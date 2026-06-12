//! Local IPFS (Kubo) backend plugin (feature "ipfs"). The free, CID-preserving
//! network surface: it talks to a **user-run** Kubo node over its HTTP API.
//!
//! `push` uploads the same CARv1 `mem` builds to `POST /api/v0/dag/import` with
//! `pin-roots=true`, so blocks are ingested under their original CIDs and the
//! root is pinned — surviving node restarts. `get_block` reads a raw block back
//! via `POST /api/v0/block/get` and verifies the hash. See `ADDING_A_BACKEND.md`.
//!
//! Opt-in by construction: compiled only with `--features ipfs`, dormant until
//! `[backend].name = "ipfs"`, and it pins *only* the subgraph a `share` names.
//! The node is the user's to run; we never start or manage it — if it isn't up,
//! `push`/`get_block` fail with a clear "is your node running?" message.

use crate::backend::{Backend, BackendManifest};
use crate::backends::car::build_car;
use crate::blockstore::Blockstore;
use crate::cid::Cid;
use crate::config::Config;

pub struct Ipfs {
    api: String,
    http: reqwest::blocking::Client,
}

impl Ipfs {
    fn with_api(api: String) -> Self {
        Self {
            api,
            http: reqwest::blocking::Client::new(),
        }
    }

    /// Import a CAR into the local node, pinning the root so it persists across
    /// restarts (Kubo's `pin-roots` defaults to true; we pass it explicitly to
    /// make the persistence promise local to this code).
    fn dag_import(&self, root: &Cid, car: Vec<u8>) -> anyhow::Result<()> {
        let part = reqwest::blocking::multipart::Part::bytes(car)
            .file_name(format!("{root}.car"))
            .mime_str("application/vnd.ipld.car")
            .map_err(|e| anyhow::anyhow!("car part: {e}"))?;
        let form = reqwest::blocking::multipart::Form::new().part("file", part);

        let resp = self
            .http
            .post(format!(
                "{}/api/v0/dag/import?pin-roots=true",
                self.api.trim_end_matches('/')
            ))
            .multipart(form)
            .send()
            .map_err(|e| {
                anyhow::anyhow!("ipfs dag import failed (is your node running? `ipfs daemon`): {e}")
            })?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("ipfs dag import failed with {status}: {body}");
        }
        Ok(())
    }
}

impl Backend for Ipfs {
    fn manifest() -> BackendManifest {
        BackendManifest {
            name: "ipfs",
            label: "Local IPFS / Kubo node (set IPFS_API to override http://127.0.0.1:5001)",
            // No secrets: it talks to your own node, not a third-party service.
            requires: vec![],
        }
    }

    fn from_config(_cfg: &Config) -> anyhow::Result<Self> {
        // The node API is local and optional to override; default to Kubo's.
        let api = std::env::var("IPFS_API").unwrap_or_else(|_| "http://127.0.0.1:5001".to_string());
        Ok(Self::with_api(api))
    }

    fn push(&self, local: &dyn Blockstore, root: &Cid) -> anyhow::Result<()> {
        let cids = crate::dag::reachable_from(local, root)?;
        let mut blocks = Vec::with_capacity(cids.len());
        for cid in &cids {
            blocks.push((*cid, local.get(cid)?));
        }
        let car = build_car(root, &blocks)?;
        self.dag_import(root, car)?;
        crate::trace::sync("ipfs", cids.len(), root);
        Ok(())
    }

    fn get_block(&self, cid: &Cid) -> anyhow::Result<Vec<u8>> {
        let resp = self
            .http
            .post(format!(
                "{}/api/v0/block/get?arg={cid}",
                self.api.trim_end_matches('/')
            ))
            .send()
            .map_err(|e| anyhow::anyhow!("ipfs block get failed (is your node running?): {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .map_err(|e| anyhow::anyhow!("ipfs block read failed: {e}"))?
            .to_vec();
        if !status.is_success() {
            anyhow::bail!("ipfs node returned {status} for {cid}");
        }
        anyhow::ensure!(
            crate::cid::compute(&bytes) == *cid,
            "ipfs node returned bytes that don't hash to {cid}"
        );
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockstore::LocalBlocks;
    use crate::node::{Checkpoint, Memory, MemoryKind, Node, Record, Source};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use tempfile::TempDir;

    fn record(body: Node) -> Vec<u8> {
        crate::node::encode(&Record {
            schema_version: crate::node::CURRENT_SCHEMA_VERSION,
            created_at: 0,
            source: Source::System,
            edges: vec![],
            body,
        })
        .unwrap()
    }

    fn small_graph() -> (TempDir, LocalBlocks, Cid) {
        let dir = TempDir::new().unwrap();
        let bs = LocalBlocks::new(dir.path().join("blocks"));
        let leaf = bs
            .put(&record(Node::Memory(Memory {
                text: "leaf".into(),
                kind: MemoryKind::Project,
            })))
            .unwrap();
        let cp = bs
            .put(&record(Node::Checkpoint(Checkpoint {
                label: "c".into(),
                root: leaf,
                parent: None,
            })))
            .unwrap();
        (dir, bs, cp)
    }

    /// A one-shot mock HTTP server that captures the request and returns a fixed
    /// response. Mirrors the local node's API for `push`/`get_block` tests.
    fn serve_once(
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> (String, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request_is_complete(&request) {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            request
        });
        (host, handle)
    }

    fn request_is_complete(request: &[u8]) -> bool {
        let text = String::from_utf8_lossy(request);
        let Some(headers_end) = text.find("\r\n\r\n").map(|i| i + 4) else {
            return false;
        };
        let content_len = text[..headers_end]
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(k, v)| {
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        request.len() >= headers_end + content_len
    }

    #[test]
    fn push_imports_a_pinned_car_into_the_local_node() {
        let (_d, bs, cp) = small_graph();
        let (host, handle) = serve_once(200, "application/json", br#"{"Root":{}}"#.to_vec());
        let ipfs = Ipfs::with_api(host);

        ipfs.push(&bs, &cp).unwrap();

        let request = handle.join().unwrap();
        let text = String::from_utf8_lossy(&request);
        assert!(
            text.starts_with("POST /api/v0/dag/import"),
            "first line: {:?}",
            text.lines().next()
        );
        assert!(
            text.contains("pin-roots=true"),
            "import must pin the root so it survives node restarts"
        );
        assert!(text.contains("name=\"file\""));
        assert!(text.contains("application/vnd.ipld.car"));
    }

    #[test]
    fn push_propagates_a_node_failure() {
        let (_d, bs, cp) = small_graph();
        let (host, handle) = serve_once(500, "application/json", br#"{"Message":"boom"}"#.to_vec());
        let ipfs = Ipfs::with_api(host);

        let err = ipfs.push(&bs, &cp).unwrap_err().to_string();
        assert!(err.contains("500"));
        handle.join().unwrap();
    }

    #[test]
    fn get_block_returns_verified_bytes() {
        let block = record(Node::Memory(Memory {
            text: "hi".into(),
            kind: MemoryKind::Project,
        }));
        let cid = crate::cid::compute(&block);
        let (host, handle) = serve_once(200, "application/octet-stream", block.clone());
        let ipfs = Ipfs::with_api(host);

        assert_eq!(ipfs.get_block(&cid).unwrap(), block);
        handle.join().unwrap();
    }

    #[test]
    fn get_block_rejects_bytes_that_dont_hash_to_the_cid() {
        let cid = crate::cid::compute(b"the real block");
        let (host, handle) = serve_once(200, "application/octet-stream", b"tampered".to_vec());
        let ipfs = Ipfs::with_api(host);

        let err = ipfs.get_block(&cid).unwrap_err().to_string();
        assert!(err.contains("don't hash to"));
        handle.join().unwrap();
    }

    #[test]
    fn manifest_requires_no_secrets() {
        let m = Ipfs::manifest();
        assert_eq!(m.name, "ipfs");
        assert!(
            m.requires.is_empty(),
            "talking to your own node needs no secret"
        );
    }

    #[test]
    fn from_config_defaults_to_localhost_when_unset() {
        if std::env::var("IPFS_API").is_err() {
            let ipfs = Ipfs::from_config(&Config::default()).unwrap();
            assert_eq!(ipfs.api, "http://127.0.0.1:5001");
        }
    }
}
