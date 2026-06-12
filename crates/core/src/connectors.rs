//! Phase 8 §6 — External Knowledge Connectors (Distributed Knowledge).
//!
//! Extend the Librarian's reach beyond the local store to external, decentralized
//! indices (e.g. an STC-style scientific index published on IPFS). The node stays
//! a **small sidekick**: it federates a *query* to registered sources and returns
//! **External CID References** — it never ingests the remote bytes into the local
//! DAG, and it never generates. The host harness resolves the returned CIDs from
//! the global IPFS network if (and only if) it wants them.
//!
//! ## Trust + privacy boundary (this is the load-bearing part)
//! - **Querying an external source is egress** — the query text leaves the device.
//!   So sources are *opt-in* (explicitly registered) and federation is *off by
//!   default* in retrieval; nothing is queried until the user asks.
//! - **External results are untrusted.** They are kept in their own section,
//!   labeled with their `source_alias`, and are **never merged into local
//!   graph-gravity** (local provenance is trusted; a stranger's index is not) and
//!   **never auto-injected** (the §2 trusted-authority gate still applies
//!   downstream). They are *references*, not stored memories.
//! - **Quarantine still applies**: a quarantined CID coming back from an external
//!   source is withheld, exactly as for local content.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::binding::MemCli;
use crate::error::{Error, Result};
use crate::moderation::QuarantineRegistry;
use crate::retrieval::parse_http_url;

/// On-disk registry of external knowledge sources, persisted at
/// `<store>/connectors.json`.
pub const CONNECTORS_VERSION: u32 = 1;

/// A registered external knowledge source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSource {
    /// Local petname the user federates against.
    pub alias: String,
    /// Where to query. For `http-index`: an `http://host:port/path` search endpoint.
    pub url: String,
    /// Connector kind. v1 ships `http-index`; `ipns`/`stc` are future kinds behind
    /// the same trait.
    pub kind: String,
}

/// The set of external sources this install will federate to. Opt-in: empty until
/// the user explicitly connects one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorRegistry {
    #[serde(default)]
    pub sources: BTreeMap<String, ExternalSource>,
}

impl ConnectorRegistry {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::Io(format!("parse connectors.json: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Io(format!("read connectors.json: {e}"))),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("create store dir: {e}")))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Io(format!("serialize connectors: {e}")))?;
        std::fs::write(path, text).map_err(|e| Error::Io(format!("write connectors.json: {e}")))
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn list(&self) -> impl Iterator<Item = &ExternalSource> {
        self.sources.values()
    }
}

/// One result from an external index: an **External CID Reference**. Untrusted by
/// construction — the `source_alias` makes its provenance explicit to the host/user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalHit {
    /// The content address the host can resolve from the global IPFS network.
    pub cid: String,
    pub title: String,
    pub snippet: String,
    /// Which registered source returned this — never blank, so external content is
    /// always distinguishable from local memory.
    pub source_alias: String,
    pub score: f32,
}

/// A connector that can answer a query against one external source. Implementations
/// only *search and return references*; they never fetch/store remote bytes locally.
pub trait ExternalConnector {
    fn alias(&self) -> &str;
    fn search(&self, query: &str, limit: usize) -> Result<Vec<ExternalHit>>;
}

/// The v1 connector: POSTs `{"query","limit"}` to an HTTP search endpoint and
/// reads back CID references. Tolerant of the common index response shapes — hits
/// under `results` / `hits` / `data`, each with `cid` (required) plus optional
/// `title` / `snippet` (or `text`/`abstract`) / `score`. Best-effort over std TCP
/// (no async runtime); a down source yields an error the federation swallows.
pub struct HttpIndexConnector {
    alias: String,
    url: String,
}

impl HttpIndexConnector {
    pub fn new(alias: &str, url: &str) -> Self {
        Self {
            alias: alias.to_string(),
            url: url.to_string(),
        }
    }
}

impl ExternalConnector for HttpIndexConnector {
    fn alias(&self) -> &str {
        &self.alias
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<ExternalHit>> {
        let (host, port, path) = parse_http_url(&self.url).ok_or_else(|| {
            Error::Io(format!(
                "connector `{}`: bad http url {}",
                self.alias, self.url
            ))
        })?;
        let body = serde_json::json!({ "query": query, "limit": limit }).to_string();
        let mut stream = TcpStream::connect((host.as_str(), port)).map_err(|e| {
            Error::BackendDown(format!("connector `{}` unreachable: {e}", self.alias))
        })?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
        // Headers + body in one write: a server that responds and closes before a
        // second write would break the pipe (same fix as the HTTP embedder).
        let mut wire = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        wire.extend_from_slice(body.as_bytes());
        stream.write_all(&wire).map_err(|e| {
            Error::BackendDown(format!("connector `{}` write failed: {e}", self.alias))
        })?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).map_err(|e| {
            Error::BackendDown(format!("connector `{}` read failed: {e}", self.alias))
        })?;
        let text = String::from_utf8_lossy(&response);
        let start = text.find("\r\n\r\n").ok_or_else(|| {
            Error::Io(format!(
                "connector `{}`: malformed HTTP response",
                self.alias
            ))
        })? + 4;
        let value: serde_json::Value = serde_json::from_str(text[start..].trim())
            .map_err(|e| Error::Io(format!("connector `{}`: bad JSON: {e}", self.alias)))?;
        Ok(parse_hits(&value, &self.alias))
    }
}

/// Pull `ExternalHit`s out of an index response, tolerant of the common shapes.
fn parse_hits(value: &serde_json::Value, source_alias: &str) -> Vec<ExternalHit> {
    let array = ["results", "hits", "data", "items"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| v.as_array()))
        .or_else(|| value.as_array());
    let Some(array) = array else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|item| {
            let cid = item
                .get("cid")
                .or_else(|| item.get("link"))
                .and_then(|v| v.as_str())?;
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet = ["snippet", "text", "abstract", "summary"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let score = item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            Some(ExternalHit {
                cid: cid.to_string(),
                title,
                snippet,
                source_alias: source_alias.to_string(),
                score,
            })
        })
        .collect()
}

/// Federate one `query` across `connectors`, returning ranked External CID
/// References with quarantined CIDs withheld. Best-effort: a source that errors
/// contributes nothing rather than failing the whole search.
pub fn federate(
    connectors: &[Box<dyn ExternalConnector>],
    query: &str,
    limit: usize,
    quarantine: &QuarantineRegistry,
) -> Vec<ExternalHit> {
    let mut out: Vec<ExternalHit> = Vec::new();
    for connector in connectors {
        if let Ok(hits) = connector.search(query, limit) {
            for hit in hits {
                if !quarantine.is_quarantined(&hit.cid) {
                    out.push(hit);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

impl MemCli {
    fn connectors_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("connectors.json"))
    }

    /// The registered external sources (opt-in; empty by default).
    pub fn connector_registry(&self) -> Result<ConnectorRegistry> {
        ConnectorRegistry::load(&self.connectors_path()?)
    }

    /// Register an external source under `alias`. Idempotent on alias (re-connecting
    /// updates the URL). Registering does **not** query it — federation is explicit.
    pub fn connect_external(&self, url: &str, alias: &str) -> Result<()> {
        if parse_http_url(url).is_none() {
            return Err(Error::Io(format!(
                "external source url must be http://… (got `{url}`)"
            )));
        }
        let path = self.connectors_path()?;
        let mut registry = ConnectorRegistry::load(&path)?;
        registry.sources.insert(
            alias.to_string(),
            ExternalSource {
                alias: alias.to_string(),
                url: url.to_string(),
                kind: "http-index".to_string(),
            },
        );
        registry.save(&path)
    }

    /// Remove a registered source. Returns whether it existed.
    pub fn disconnect_external(&self, alias: &str) -> Result<bool> {
        let path = self.connectors_path()?;
        let mut registry = ConnectorRegistry::load(&path)?;
        let existed = registry.sources.remove(alias).is_some();
        if existed {
            registry.save(&path)?;
        }
        Ok(existed)
    }

    /// Federate `query` across all registered sources, returning External CID
    /// References (untrusted, quarantine-filtered). This is the explicit egress
    /// path — calling it sends the query to each connected source.
    pub fn federate_search(&self, query: &str, limit: usize) -> Result<Vec<ExternalHit>> {
        let registry = self.connector_registry()?;
        let quarantine = self.quarantine_registry().unwrap_or_default();
        let connectors: Vec<Box<dyn ExternalConnector>> = registry
            .list()
            .map(|s| {
                Box::new(HttpIndexConnector::new(&s.alias, &s.url)) as Box<dyn ExternalConnector>
            })
            .collect();
        Ok(federate(&connectors, query, limit, &quarantine))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A tiny mock index server that returns `body` once, then closes.
    fn mock_index(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/search"), handle)
    }

    #[test]
    fn registry_round_trips_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        assert!(
            mem.connector_registry().unwrap().is_empty(),
            "opt-in: empty by default"
        );
        mem.connect_external("http://127.0.0.1:9/search", "stc")
            .unwrap();
        let reg = mem.connector_registry().unwrap();
        assert_eq!(reg.sources.len(), 1);
        assert_eq!(reg.sources["stc"].url, "http://127.0.0.1:9/search");
        assert!(
            mem.disconnect_external("stc").unwrap(),
            "remove reports it existed"
        );
        assert!(mem.connector_registry().unwrap().is_empty());
        assert!(
            !mem.disconnect_external("stc").unwrap(),
            "second remove is a no-op"
        );
    }

    #[test]
    fn connect_external_rejects_non_http_urls() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        assert!(
            mem.connect_external("ipns://libstc.cc", "stc").is_err(),
            "v1 is http-index only"
        );
    }

    #[test]
    fn http_connector_returns_external_cid_references() {
        let body = r#"{"results":[
            {"cid":"bafyA","title":"Paper A","abstract":"about widgets","score":0.9},
            {"cid":"bafyB","title":"Paper B","text":"about gadgets","score":0.4}
        ]}"#;
        let (url, server) = mock_index(body);
        let connector = HttpIndexConnector::new("stc", &url);
        let hits = connector.search("widgets", 10).unwrap();
        server.join().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].cid, "bafyA");
        assert_eq!(
            hits[0].source_alias, "stc",
            "external content is always attributed"
        );
        assert_eq!(hits[0].snippet, "about widgets", "tolerant snippet field");
    }

    #[test]
    fn federate_withholds_quarantined_cids_and_ranks_by_score() {
        let body = r#"{"hits":[
            {"cid":"good","score":0.2},
            {"cid":"bad","score":0.99}
        ]}"#;
        let (url, server) = mock_index(body);
        let connectors: Vec<Box<dyn ExternalConnector>> =
            vec![Box::new(HttpIndexConnector::new("src", &url))];
        let mut quarantine = QuarantineRegistry::default();
        quarantine.quarantine("bad", "unsafe", 0);
        let hits = federate(&connectors, "q", 10, &quarantine);
        server.join().unwrap();
        assert_eq!(
            hits.len(),
            1,
            "the high-scoring but quarantined hit is withheld"
        );
        assert_eq!(hits[0].cid, "good");
    }

    #[test]
    fn a_down_source_yields_nothing_rather_than_failing() {
        // Bind then drop → a closed port. federate swallows the error.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/search",
            listener.local_addr().unwrap().port()
        );
        drop(listener);
        let connectors: Vec<Box<dyn ExternalConnector>> =
            vec![Box::new(HttpIndexConnector::new("dead", &url))];
        let hits = federate(&connectors, "q", 10, &QuarantineRegistry::default());
        assert!(hits.is_empty());
    }
}
