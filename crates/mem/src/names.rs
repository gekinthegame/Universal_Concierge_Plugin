//! The mutable name index — THE one place mutation is legal. Immutable CIDs
//! can't answer "my *current* project"; this maps human/model-friendly names to
//! the latest root CID. Lives beside the blocks, NOT inside the DAG.
//!
//! Well-known names (conventions, not hardcoded types):
//! `current-project`, `active-agent-memory`, `user-profile`, `task-head`,
//! `convo:<id>`, `skill:<name>`.
//!
//! Tracing of a bind happens one layer up in `store` (the orchestration layer);
//! this layer stays pure so it can be tested in isolation.

use crate::cid::Cid;
use atomic_write_file::AtomicWriteFile;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;

/// Persisted as `.concierge/names.json` (name -> cid string). Allowed to be
/// boring. Writes go through `AtomicWriteFile` (temp + rename), so this — the
/// only mutable state in the system — can never be left partially written.
pub struct NameIndex {
    path: PathBuf,
    map: HashMap<String, Cid>,
}

impl NameIndex {
    /// Load the index from `path`, or start empty if the file doesn't exist.
    pub fn load(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let map = if path.try_exists()? {
            let content = std::fs::read_to_string(&path)?;
            // On disk, CIDs are stored as their canonical strings; parse them
            // back explicitly rather than relying on a format-specific serde.
            let raw: BTreeMap<String, String> = serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("names index parse: {e}"))?;
            let mut map = HashMap::with_capacity(raw.len());
            for (name, cid_str) in raw {
                let cid: Cid = cid_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("names index: bad CID for {name:?}: {e}"))?;
                map.insert(name, cid);
            }
            map
        } else {
            HashMap::new()
        };
        Ok(Self { path, map })
    }

    /// The only mutable operation in the system. Point a name at a root CID and
    /// persist atomically.
    pub fn bind(&mut self, name: &str, root: Cid) -> anyhow::Result<()> {
        let mut next = self.map.clone();
        next.insert(name.to_string(), root);
        self.save(&next)?;
        self.map = next;
        Ok(())
    }

    /// Resolve a name to its current root CID.
    pub fn resolve(&self, name: &str) -> anyhow::Result<Cid> {
        self.map
            .get(name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no name bound: {name:?}"))
    }

    /// All current bindings, for listing (`mem ls`).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Cid)> {
        self.map.iter().map(|(k, v)| (k.as_str(), v))
    }

    fn save(&self, map: &HashMap<String, Cid>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // BTreeMap → deterministic key order, so the file is stable across saves.
        let serializable: BTreeMap<&str, String> = map
            .iter()
            .map(|(k, v)| (k.as_str(), v.to_string()))
            .collect();
        let json = serde_json::to_string_pretty(&serializable)?;
        let mut f = AtomicWriteFile::open(&self.path)?;
        f.write_all(json.as_bytes())?;
        f.commit()?; // temp + atomic rename — a partial bind can never be observed
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn cid(b: &[u8]) -> Cid {
        crate::cid::compute(b)
    }

    fn fresh() -> (TempDir, NameIndex) {
        let dir = TempDir::new().unwrap();
        let idx = NameIndex::load(dir.path().join(".concierge/names.json")).unwrap();
        (dir, idx)
    }

    #[test]
    fn bind_then_resolve() {
        let (_d, mut idx) = fresh();
        let c = cid(b"root");
        idx.bind("current-project", c).unwrap();
        assert_eq!(idx.resolve("current-project").unwrap(), c);
    }

    #[test]
    fn resolve_unknown_name_errors() {
        let (_d, idx) = fresh();
        assert!(idx.resolve("nope").is_err());
    }

    #[test]
    fn survives_process_restart() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        let c = cid(b"persisted root");
        {
            let mut idx = NameIndex::load(path.clone()).unwrap();
            idx.bind("user-profile", c).unwrap();
        } // index dropped — only the on-disk file remains
        let reloaded = NameIndex::load(path.clone()).unwrap();
        assert_eq!(reloaded.resolve("user-profile").unwrap(), c);
    }

    #[test]
    fn rebind_takes_latest_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        let (c1, c2) = (cid(b"v1"), cid(b"v2"));
        {
            let mut idx = NameIndex::load(path.clone()).unwrap();
            idx.bind("head", c1).unwrap();
            idx.bind("head", c2).unwrap();
            assert_eq!(idx.resolve("head").unwrap(), c2);
        }
        let reloaded = NameIndex::load(path.clone()).unwrap();
        assert_eq!(reloaded.resolve("head").unwrap(), c2);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let (_d, idx) = fresh();
        assert_eq!(idx.iter().count(), 0);
        assert!(idx.resolve("x").is_err());
    }

    #[test]
    fn iter_lists_all_bindings() {
        let (_d, mut idx) = fresh();
        idx.bind("a", cid(b"a")).unwrap();
        idx.bind("b", cid(b"b")).unwrap();
        let names: std::collections::BTreeSet<&str> = idx.iter().map(|(n, _)| n).collect();
        assert_eq!(idx.iter().count(), 2);
        assert!(names.contains("a") && names.contains("b"));
    }

    #[test]
    fn load_malformed_json_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{").unwrap();

        let err = match NameIndex::load(path) {
            Ok(_) => panic!("malformed names.json must not load"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("names index parse"),
            "malformed names.json must fail explicitly: {err}"
        );
    }

    #[test]
    fn load_bad_cid_string_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"head":"not-a-cid"}"#).unwrap();

        let err = match NameIndex::load(path) {
            Ok(_) => panic!("bad CID strings must not load"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("bad CID"),
            "malformed CID strings must fail explicitly: {err}"
        );
    }

    #[test]
    fn interrupted_uncommitted_write_preserves_committed_index() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        let c = cid(b"committed");

        let mut idx = NameIndex::load(path.clone()).unwrap();
        idx.bind("head", c).unwrap();
        {
            let mut f = AtomicWriteFile::open(&path).unwrap();
            std::io::Write::write_all(&mut f, b"{").unwrap();
        } // dropped before commit, simulating interruption before rename

        let reloaded = NameIndex::load(path).unwrap();
        assert_eq!(reloaded.resolve("head").unwrap(), c);
    }

    #[test]
    fn failed_bind_does_not_advance_in_memory_pointer() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".concierge/names.json");
        let (c1, c2) = (cid(b"committed"), cid(b"not committed"));

        let mut idx = NameIndex::load(path.clone()).unwrap();
        idx.bind("head", c1).unwrap();

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        assert!(idx.bind("head", c2).is_err());
        assert_eq!(idx.resolve("head").unwrap(), c1);
    }
}
