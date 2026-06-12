//! Shared CARv1 construction for backends that ingest a CAR (Pinata's CAR
//! upload, a local Kubo node's `dag/import`). The block CIDs are *our* CIDs, so
//! every target stores each block under the CID we already computed — no
//! re-CID. One implementation, so the backends can't drift apart.

use crate::cid::Cid;
use iroh_car::{CarHeader, CarWriter};

/// Build a CARv1 with `root` in the header and `blocks` in body order. The
/// `iroh-car` writer is async; drive it on a lightweight current-thread runtime
/// writing to an in-memory `Vec`, so no real I/O happens.
pub(crate) fn build_car(root: &Cid, blocks: &[(Cid, Vec<u8>)]) -> anyhow::Result<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .map_err(|e| anyhow::anyhow!("car runtime: {e}"))?;
    rt.block_on(async {
        let header = CarHeader::new_v1(vec![*root]);
        let mut writer = CarWriter::new(header, Vec::new());
        for (cid, data) in blocks {
            writer
                .write(*cid, data)
                .await
                .map_err(|e| anyhow::anyhow!("car block write {cid}: {e}"))?;
        }
        writer
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("car finish: {e}"))
    })
}
