//! Phase 4 — CARv1 codec + verification.
//!
//! Pure framing only: it turns `(root, blocks)` into CARv1 bytes and back, with
//! per-block CID verification on read. It knows nothing about the store — the
//! [`crate::MemCli`] methods (`export_car` / `import_car`) supply the blocks and
//! handle the on-disk blockstore.
//!
//! We use **`iroh-car`** (the same writer `mem` uses for its Pinata/IPFS
//! backends), so a CAR the plugin writes is byte-identical to one `mem` writes —
//! no re-CID, full ecosystem interop. The async writer/reader are driven on a
//! lightweight current-thread runtime, exactly as `mem`'s `build_car` does.
//!
//! On read we recompute every block's CID (sha2-256 / its codec) and reject any
//! mismatch — the streaming, verify-each-block discipline from atproto's
//! `verifyIncomingCarBlocks`.

use iroh_car::{CarHeader, CarReader, CarWriter};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The roots and verified `(cid, bytes)` blocks read from a CARv1.
pub(crate) type CarContents = (Vec<cid::Cid>, Vec<(cid::Cid, Vec<u8>)>);

/// Build a CARv1 with `root` in the header and `blocks` in body order.
pub(crate) fn build_car(root: &cid::Cid, blocks: &[(cid::Cid, Vec<u8>)]) -> Result<Vec<u8>> {
    runtime()?.block_on(async {
        let header = CarHeader::new_v1(vec![*root]);
        let mut writer = CarWriter::new(header, Vec::new());
        for (cid, data) in blocks {
            writer
                .write(*cid, data)
                .await
                .map_err(|e| Error::Io(format!("CAR write {cid}: {e}")))?;
        }
        writer
            .finish()
            .await
            .map_err(|e| Error::Io(format!("CAR finish: {e}")))
    })
}

/// Read a CARv1, **verifying every block's CID**. Returns the header roots and
/// the verified `(cid, bytes)` blocks. A block whose bytes don't hash to its
/// claimed CID fails the whole read loudly.
pub(crate) fn read_car_verified(bytes: &[u8]) -> Result<CarContents> {
    runtime()?.block_on(async {
        let mut reader = CarReader::new(bytes)
            .await
            .map_err(|e| Error::Io(format!("CAR read header: {e}")))?;
        let roots = reader.header().roots().to_vec();
        let mut blocks = Vec::new();
        while let Some((cid, data)) = reader
            .next_block()
            .await
            .map_err(|e| Error::Io(format!("CAR read block: {e}")))?
        {
            verify_block(&cid, &data)?;
            blocks.push((cid, data));
        }
        Ok((roots, blocks))
    })
}

/// A current-thread runtime to drive the async CAR reader/writer over in-memory
/// buffers (no real I/O happens).
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| Error::Io(format!("CAR runtime: {e}")))
}

/// Recompute a block's CID and compare it to the claimed one.
pub(crate) fn verify_block(expected: &cid::Cid, data: &[u8]) -> Result<()> {
    let computed = compute_cid(expected.codec(), expected.hash().code(), data)?;
    if &computed != expected {
        return Err(Error::Io(format!(
            "CAR block CID mismatch: claimed {expected}, computed {computed}"
        )));
    }
    Ok(())
}

/// Recompute a CIDv1 for `data` under the given codec and multihash code. Only
/// sha2-256 (0x12) — what `mem` writes — is verified; anything else is rejected
/// rather than silently trusted.
pub(crate) fn compute_cid(codec: u64, hash_code: u64, data: &[u8]) -> Result<cid::Cid> {
    const SHA2_256: u64 = 0x12;
    if hash_code != SHA2_256 {
        return Err(Error::Io(format!(
            "unsupported multihash code 0x{hash_code:x} in CAR (only sha2-256 is verified)"
        )));
    }
    let digest = Sha256::digest(data);
    let mh = multihash::Multihash::<64>::wrap(SHA2_256, digest.as_slice())
        .map_err(|e| Error::Io(format!("multihash wrap: {e}")))?;
    Ok(cid::Cid::new_v1(codec, mh))
}
