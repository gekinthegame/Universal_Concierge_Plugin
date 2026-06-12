//! Pinata backend plugin (feature "pinata"). The first network backend.
//!
//! Preserves pre-computed CIDs via Pinata's CAR-upload path: `POST /v3/files`
//! with `network=public` and `car=true` ingests a CARv1 whose blocks keep their
//! original CIDs — no re-CID, no IPFS daemon, one HTTPS POST. `get_block` reads
//! a raw block from the gateway and verifies the hash. See `ADDING_A_BACKEND.md`.

use crate::backend::{Backend, BackendManifest, EnvKey};
use crate::backends::car::build_car;
use crate::blockstore::Blockstore;
use crate::cid::Cid;
use crate::config::Config;

pub struct Pinata {
    jwt: String,
    upload_base: String,
    gateway: String,
    http: reqwest::blocking::Client,
}

impl Pinata {
    fn with_endpoints(jwt: String, upload_base: String, gateway: String) -> Self {
        Self {
            jwt,
            upload_base,
            gateway,
            http: reqwest::blocking::Client::new(),
        }
    }

    fn upload_car(&self, root: &Cid, car: Vec<u8>) -> anyhow::Result<()> {
        let part = reqwest::blocking::multipart::Part::bytes(car)
            .file_name(format!("{root}.car"))
            .mime_str("application/vnd.ipld.car")
            .map_err(|e| anyhow::anyhow!("car part: {e}"))?;
        let form = reqwest::blocking::multipart::Form::new()
            .text("network", "public")
            .text("car", "true")
            .part("file", part);

        let resp = self
            .http
            .post(format!(
                "{}/v3/files",
                self.upload_base.trim_end_matches('/')
            ))
            .bearer_auth(&self.jwt)
            .multipart(form)
            .send()
            .map_err(|e| anyhow::anyhow!("pinata upload failed: {e}"))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("pinata upload failed with {status}: {body}");
        }
        Ok(())
    }
}

impl Backend for Pinata {
    fn manifest() -> BackendManifest {
        BackendManifest {
            name: "pinata",
            label: "Pinata (IPFS, CAR upload)",
            requires: vec![EnvKey {
                key: "PINATA_JWT",
                prompt: "Pinata JWT",
                url: Some("https://app.pinata.cloud/developers/api-keys"),
                secret: true,
            }],
        }
    }

    fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let jwt = cfg.require("PINATA_JWT")?;
        // Endpoints are overridable via env (handy for the 4.3 live run and for
        // pointing at a dedicated gateway); defaults target Pinata's public API.
        let upload_base = std::env::var("PINATA_UPLOAD_BASE")
            .unwrap_or_else(|_| "https://uploads.pinata.cloud".to_string());
        let gateway = std::env::var("PINATA_GATEWAY")
            .unwrap_or_else(|_| "https://gateway.pinata.cloud".to_string());
        Ok(Self::with_endpoints(jwt, upload_base, gateway))
    }

    fn push(&self, local: &dyn Blockstore, root: &Cid) -> anyhow::Result<()> {
        let cids = crate::dag::reachable_from(local, root)?;
        let mut blocks = Vec::with_capacity(cids.len());
        for cid in &cids {
            blocks.push((*cid, local.get(cid)?));
        }
        let car = build_car(root, &blocks)?;
        self.upload_car(root, car)?;
        crate::trace::sync("pinata", cids.len(), root);
        Ok(())
    }

    fn get_block(&self, cid: &Cid) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "{}/ipfs/{cid}?format=raw",
            self.gateway.trim_end_matches('/')
        );
        let resp = self
            .http
            .get(url)
            .send()
            .map_err(|e| anyhow::anyhow!("gateway request failed: {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .map_err(|e| anyhow::anyhow!("gateway read failed: {e}"))?
            .to_vec();
        if !status.is_success() {
            anyhow::bail!("gateway returned {status} for {cid}");
        }
        anyhow::ensure!(
            crate::cid::compute(&bytes) == *cid,
            "gateway returned bytes that don't hash to {cid}"
        );
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockstore::LocalBlocks;
    use crate::node::{Checkpoint, Memory, MemoryKind, Node, Record, Source};
    use iroh_car::CarReader;
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use tempfile::TempDir;

    // --- block fixtures -----------------------------------------------------

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

    // --- CARv1 parsing, using the same independent CAR crate ----------------

    fn parse_car(bytes: Vec<u8>) -> (Vec<Cid>, Vec<(Cid, Vec<u8>)>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut reader = CarReader::new(bytes.as_slice()).await.unwrap();
            let roots = reader.header().roots().to_vec();
            let mut blocks = Vec::new();
            while let Some((cid, data)) = reader.next_block().await.unwrap() {
                blocks.push((cid, data));
            }
            (roots, blocks)
        })
    }

    #[test]
    fn car_preserves_every_cid() {
        let (_d, bs, cp) = small_graph();
        let cids = crate::dag::reachable_from(&bs, &cp).unwrap();
        let blocks: Vec<(Cid, Vec<u8>)> = cids.iter().map(|c| (*c, bs.get(c).unwrap())).collect();

        let car = build_car(&cp, &blocks).unwrap();
        let (roots, parsed) = parse_car(car);

        assert_eq!(roots, vec![cp], "the root is in the CAR header");
        for (cid, data) in &parsed {
            assert_eq!(
                crate::cid::compute(data),
                *cid,
                "each CAR block keeps its pre-computed CID"
            );
        }
        let got: BTreeSet<Cid> = parsed.iter().map(|(c, _)| *c).collect();
        let want: BTreeSet<Cid> = cids.into_iter().collect();
        assert_eq!(got, want, "the CAR carries exactly the reachable subgraph");
    }

    // --- a one-shot mock HTTP server ----------------------------------------

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

    fn multipart_part(request: &[u8], name: &str) -> Option<Vec<u8>> {
        let headers_end = find_bytes(request, b"\r\n\r\n")?;
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        let boundary = headers.lines().find_map(|line| {
            let (_, value) = line.split_once(':')?;
            line[..line.find(':')?]
                .eq_ignore_ascii_case("content-type")
                .then_some(value)
        })?;
        let boundary = boundary.split("boundary=").nth(1)?.trim().trim_matches('"');
        let marker = format!("--{boundary}");
        let body = &request[headers_end + 4..];
        let mut cursor = 0;
        while let Some(relative_start) = find_bytes(&body[cursor..], marker.as_bytes()) {
            let start = cursor + relative_start + marker.len();
            if body.get(start..start + 2) == Some(b"--") {
                break;
            }
            let part_start = start + usize::from(body.get(start..start + 2) == Some(b"\r\n")) * 2;
            let part_headers_end = part_start + find_bytes(&body[part_start..], b"\r\n\r\n")?;
            let part_headers = String::from_utf8_lossy(&body[part_start..part_headers_end]);
            let content_start = part_headers_end + 4;
            let next_marker =
                content_start + find_bytes(&body[content_start..], marker.as_bytes())?;
            let mut content_end = next_marker;
            if content_end >= 2 && &body[content_end - 2..content_end] == b"\r\n" {
                content_end -= 2;
            }
            if part_headers.contains(&format!("name=\"{name}\"")) {
                return Some(body[content_start..content_end].to_vec());
            }
            cursor = next_marker;
        }
        None
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn push_uploads_a_car_to_the_files_endpoint() {
        let (_d, bs, cp) = small_graph();
        let (host, handle) =
            serve_once(200, "application/json", br#"{"data":{"cid":"x"}}"#.to_vec());
        let pinata = Pinata::with_endpoints("test-jwt".into(), host, "http://unused".into());

        pinata.push(&bs, &cp).unwrap();

        let request = handle.join().unwrap();
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("POST /v3/files HTTP/1.1"));
        assert!(
            request_text
                .to_lowercase()
                .contains("authorization: bearer test-jwt")
        );
        assert!(request_text.contains("name=\"network\""));
        assert!(request_text.contains("public"));
        assert!(request_text.contains("name=\"car\""));
        assert!(request_text.contains("name=\"file\""));
        assert!(request_text.contains("application/vnd.ipld.car"));

        let uploaded_car = multipart_part(&request, "file").expect("file part");
        let (roots, parsed) = parse_car(uploaded_car);
        assert_eq!(roots, vec![cp], "uploaded CAR root is the chosen root");
        let got: BTreeSet<Cid> = parsed.iter().map(|(c, _)| *c).collect();
        let want: BTreeSet<Cid> = crate::dag::reachable_from(&bs, &cp)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            got, want,
            "uploaded CAR carries exactly the reachable subgraph"
        );
        for (cid, data) in parsed {
            assert_eq!(
                crate::cid::compute(&data),
                cid,
                "uploaded CAR block keeps its pre-computed CID"
            );
        }
    }

    #[test]
    fn push_propagates_an_http_failure() {
        let (_d, bs, cp) = small_graph();
        let (host, handle) =
            serve_once(401, "application/json", br#"{"error":"bad jwt"}"#.to_vec());
        let pinata = Pinata::with_endpoints("nope".into(), host, "http://unused".into());

        let err = pinata.push(&bs, &cp).unwrap_err().to_string();
        assert!(err.contains("401"));
        handle.join().unwrap();
    }

    #[test]
    fn get_block_returns_verified_bytes() {
        let block = record(Node::Memory(Memory {
            text: "hi".into(),
            kind: MemoryKind::Project,
        }));
        let cid = crate::cid::compute(&block);
        let (host, handle) = serve_once(200, "application/vnd.ipld.raw", block.clone());
        let pinata = Pinata::with_endpoints("jwt".into(), "http://unused".into(), host);

        assert_eq!(pinata.get_block(&cid).unwrap(), block);
        handle.join().unwrap();
    }

    #[test]
    fn get_block_rejects_bytes_that_dont_hash_to_the_cid() {
        let cid = crate::cid::compute(b"the real block");
        let (host, handle) = serve_once(200, "application/vnd.ipld.raw", b"tampered".to_vec());
        let pinata = Pinata::with_endpoints("jwt".into(), "http://unused".into(), host);

        let err = pinata.get_block(&cid).unwrap_err().to_string();
        assert!(err.contains("don't hash to"));
        handle.join().unwrap();
    }
}
