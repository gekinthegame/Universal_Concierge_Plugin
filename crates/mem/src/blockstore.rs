//! The content-addressed block layer. Local disk and every network backend are
//! both just implementations of `Blockstore` — the CID is identical across all
//! of them, which is what makes sync trivial.

use crate::cid::Cid;
use atomic_write_file::AtomicWriteFile;
use std::io::Write;
use std::path::PathBuf;

/// A content-addressed block store.
///
/// `put` stores bytes under the CID derived from *those exact bytes*; it must
/// never re-hash or re-wrap. `get(cid)` returns bytes whose hash equals `cid`.
pub trait Blockstore {
    fn put(&self, bytes: &[u8]) -> anyhow::Result<Cid>;
    fn get(&self, cid: &Cid) -> anyhow::Result<Vec<u8>>;
    fn has(&self, cid: &Cid) -> anyhow::Result<bool>;

    /// If an absent block was intentionally pruned by GC, return its tombstone
    /// (a receipt of truth) so a DAG walk treats it as a legitimate frontier
    /// rather than corruption. The default has no tombstone knowledge — every
    /// absence is then unexplained, which the walk reports as an error.
    fn tombstone(&self, _cid: &Cid) -> Option<crate::tombstones::Tombstone> {
        None
    }
}

/// The default, always-compiled store: one file per block under a directory,
/// named by the block's CID (the `.concierge/blocks/<cid>` layout — `root` is
/// that blocks directory). Tracing of *what kind* of node a block holds happens
/// one layer up in `store`, which knows the node type; this layer is pure IO.
pub struct LocalBlocks {
    pub root: PathBuf,
}

impl LocalBlocks {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, cid: &Cid) -> PathBuf {
        // CID base32 strings are filesystem-safe (lowercase alphanumeric).
        self.root.join(cid.to_string())
    }

    /// The tombstone ledger sits beside the blocks directory (`<store>/blocks`
    /// → `<store>/tombstones.json`), matching `Config::tombstones_path`.
    fn tombstones_path(&self) -> Option<PathBuf> {
        self.root.parent().map(|p| p.join("tombstones.json"))
    }

    /// Load the tombstone ledger for this store (empty if none exists). Used by
    /// GC to record deaths and by `tombstone()` to explain a pruned absence.
    pub fn load_tombstones(&self) -> anyhow::Result<crate::tombstones::Tombstones> {
        let path = self
            .tombstones_path()
            .unwrap_or_else(|| PathBuf::from("tombstones.json"));
        crate::tombstones::Tombstones::load(path)
    }

    /// Every block CID currently on disk, for GC's sweep. Filenames that don't
    /// parse as CIDs are skipped — only blocks live in this directory.
    pub fn iter_cids(&self) -> anyhow::Result<Vec<Cid>> {
        if !self.root.try_exists()? {
            return Ok(Vec::new());
        }
        let mut cids = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(cid) = name.parse::<Cid>() {
                    cids.push(cid);
                }
            }
        }
        Ok(cids)
    }

    /// Delete a block from disk. GC-only: content addressing means re-`put`ting
    /// the same bytes restores the identical CID, so this never loses identity.
    /// Already-absent is success (idempotent sweep).
    pub fn remove(&self, cid: &Cid) -> anyhow::Result<()> {
        match std::fs::remove_file(self.path_for(cid)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::anyhow!("block {cid} not removable: {e}")),
        }
    }

    fn write_block(&self, cid: &Cid, bytes: &[u8]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut f = AtomicWriteFile::open(self.path_for(cid))?;
        f.write_all(bytes)?;
        f.commit()?; // atomic rename into place — a reader never sees a partial block
        Ok(())
    }
}

impl Blockstore for LocalBlocks {
    fn put(&self, bytes: &[u8]) -> anyhow::Result<Cid> {
        let cid = crate::cid::compute(bytes);
        // Content-addressed: identical bytes map to the same path, so an
        // existing block may be reused only if it really is this content.
        let path = self.path_for(&cid);
        if path.try_exists()? {
            let existing = std::fs::read(&path)
                .map_err(|e| anyhow::anyhow!("block {cid} not readable: {e}"))?;
            if crate::cid::compute(&existing) == cid {
                return Ok(cid);
            }
        }
        self.write_block(&cid, bytes)?;
        Ok(cid)
    }

    fn get(&self, cid: &Cid) -> anyhow::Result<Vec<u8>> {
        let path = self.path_for(cid);
        let bytes =
            std::fs::read(&path).map_err(|e| anyhow::anyhow!("block {cid} not readable: {e}"))?;
        let actual = crate::cid::compute(&bytes);
        if &actual != cid {
            anyhow::bail!("block {cid} hash mismatch: file hashes to {actual}");
        }
        Ok(bytes)
    }

    fn has(&self, cid: &Cid) -> anyhow::Result<bool> {
        let path = self.path_for(cid);
        if !path.try_exists()? {
            return Ok(false);
        }
        let bytes =
            std::fs::read(&path).map_err(|e| anyhow::anyhow!("block {cid} not readable: {e}"))?;
        Ok(crate::cid::compute(&bytes) == *cid)
    }

    /// Consult the on-disk ledger so a walk can tell "GC pruned this" (a
    /// receipt) from "this is missing" (corruption). A frontier hit is rare
    /// (only at a pruned edge), so a fresh read of the small ledger is fine; an
    /// unreadable ledger means "no tombstone", and the walk reports the absence.
    fn tombstone(&self, cid: &Cid) -> Option<crate::tombstones::Tombstone> {
        self.load_tombstones().ok()?.get(cid).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, LocalBlocks) {
        let dir = TempDir::new().unwrap();
        let bs = LocalBlocks::new(dir.path().join("blocks"));
        (dir, bs)
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_d, bs) = store();
        let data = b"hello blocks";
        let cid = bs.put(data).unwrap();
        assert_eq!(bs.get(&cid).unwrap(), data);
    }

    #[test]
    fn put_is_content_addressed_and_idempotent() {
        let (_d, bs) = store();
        let data = b"same bytes";
        let c1 = bs.put(data).unwrap();
        let c2 = bs.put(data).unwrap();
        assert_eq!(c1, c2, "same bytes must yield the same CID");

        let files = std::fs::read_dir(&bs.root).unwrap().count();
        assert_eq!(files, 1, "idempotent put must not duplicate the block");
        assert!(
            bs.root.join(c1.to_string()).exists(),
            "block file is named by its CID"
        );
    }

    #[test]
    fn has_reflects_presence() {
        let (_d, bs) = store();
        let cid = bs.put(b"x").unwrap();
        assert!(bs.has(&cid).unwrap());

        let missing = crate::cid::compute(b"never stored");
        assert!(!bs.has(&missing).unwrap());
    }

    #[test]
    fn get_missing_block_errors() {
        let (_d, bs) = store();
        let missing = crate::cid::compute(b"absent");
        assert!(bs.get(&missing).is_err());
    }

    #[test]
    fn distinct_bytes_coexist() {
        let (_d, bs) = store();
        let a = bs.put(b"alpha").unwrap();
        let b = bs.put(b"beta").unwrap();
        assert_ne!(a, b);
        assert_eq!(bs.get(&a).unwrap(), b"alpha");
        assert_eq!(bs.get(&b).unwrap(), b"beta");
    }

    #[test]
    fn get_rejects_cid_named_file_with_wrong_bytes() {
        let (_d, bs) = store();
        let cid = crate::cid::compute(b"expected bytes");

        std::fs::create_dir_all(&bs.root).unwrap();
        std::fs::write(bs.root.join(cid.to_string()), b"corrupt bytes").unwrap();

        let err = bs.get(&cid).unwrap_err().to_string();
        assert!(
            err.contains("hash mismatch"),
            "corrupt CID-named block must not be returned: {err}"
        );
    }

    #[test]
    fn has_is_false_for_cid_named_file_with_wrong_bytes() {
        let (_d, bs) = store();
        let cid = crate::cid::compute(b"expected bytes");

        std::fs::create_dir_all(&bs.root).unwrap();
        std::fs::write(bs.root.join(cid.to_string()), b"corrupt bytes").unwrap();

        assert!(!bs.has(&cid).unwrap());
    }

    #[test]
    fn put_repairs_existing_cid_named_file_with_wrong_bytes() {
        let (_d, bs) = store();
        let data = b"repair me";
        let cid = crate::cid::compute(data);

        std::fs::create_dir_all(&bs.root).unwrap();
        std::fs::write(bs.root.join(cid.to_string()), b"wrong block").unwrap();

        assert_eq!(bs.put(data).unwrap(), cid);
        assert_eq!(bs.get(&cid).unwrap(), data);
    }
}
