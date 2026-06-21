//! The kernel's owned state: the IPLD store binding plus the single warm index
//! that every client (GUI, CLI, MCP) shares. The index is built lazily on the
//! first search, restored from a snapshot on daemon start, and appended to by
//! kernel-owned capture loops. This is the whole point of the kernel: build the
//! index at most once and let everyone retrieve against the same warm copy.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use concierge_core::{Cid, Depth, Librarian, MemCli, SharedEmbedder};
use concierge_kernel::protocol::Response;

use crate::node::KernelNode;

/// How long a built index is reused before a search rebuilds it. Generous: the
/// shared warm index is the common case, and a rebuild only ever happens on a
/// search (never idle, never on a timer).
const INDEX_TTL: Duration = Duration::from_secs(300);

pub const KERNEL_ROUTES: &[&str] = &[
    "/__kernel/ping",
    "/__kernel/status",
    "/api/search",
    "/api/peers",
    "/api/meta",
    "/api/resolve",
    "/api/names",
    "/api/record",
    "/api/checkpoints",
    "/api/graph",
    "/api/stats",
    "/api/hot-pins",
    "/api/privacy",
];

/// Map a depth string to the retrieval [`Depth`]; unknown/absent → summary.
pub fn parse_depth(depth: Option<&str>) -> Depth {
    match depth {
        Some("brief") => Depth::Brief,
        Some("full") => Depth::Full,
        _ => Depth::Summary,
    }
}

pub struct KernelState {
    mem: MemCli,
    index: Mutex<Index>,
    node: Mutex<Option<KernelNode>>,
}

#[derive(Default)]
struct Index {
    embedder: Option<SharedEmbedder>,
    librarian: Option<Librarian<SharedEmbedder>>,
    built_at: Option<Instant>,
    complete: bool,
}

impl KernelState {
    pub fn new(workdir: PathBuf) -> Self {
        Self {
            mem: MemCli::new(workdir),
            index: Mutex::new(Index::default()),
            node: Mutex::new(None),
        }
    }

    pub fn start_background(self: &Arc<Self>) {
        self.warm_index_from_snapshot();
        crate::capture::spawn_all(Arc::clone(self));
        // Bring the libp2p discovery node up OFF the startup path. Building the tokio
        // runtime and binding TCP/QUIC/mDNS can be slow — and because `serve()` runs
        // this before entering the accept loop, any slowness here means the socket is
        // bound but not yet accepting, so clients connect and then block forever
        // (the Windows "spins forever" symptom). The daemon must answer store reads
        // immediately; the network node warms up in the background.
        let node_state = Arc::clone(self);
        std::thread::spawn(move || {
            let _ = node_state.ensure_node();
        });
    }

    pub(crate) fn mem(&self) -> MemCli {
        self.mem.clone()
    }

    /// Load a valid index snapshot immediately on daemon start. No bulk build is
    /// attempted here; the first search still performs the full build when no
    /// snapshot exists.
    fn warm_index_from_snapshot(&self) {
        let mut guard = self.index.lock().unwrap_or_else(|p| p.into_inner());
        let embedder = self.embedder_for(&mut guard);
        if let Some(mut librarian) = Librarian::load_snapshot(&self.mem, &embedder) {
            let _ = librarian.reconcile(&self.mem);
            guard.librarian = Some(librarian);
            guard.built_at = Some(Instant::now());
            guard.complete = true;
        }
    }

    fn embedder_for(&self, guard: &mut Index) -> SharedEmbedder {
        if guard.embedder.is_none() {
            guard.embedder = Some(self.mem.librarian_embedder());
        }
        guard.embedder.clone().expect("embedder built above")
    }

    /// Ensure the search path has a complete index. A missing index loads a valid
    /// snapshot and reconciles it, or builds from the store when no snapshot exists.
    /// A stale/incomplete in-memory index is reconciled in place; it is not replaced
    /// by an older snapshot that could drop capture appends.
    fn prepare_index_for_search(&self, guard: &mut Index) -> concierge_core::Result<()> {
        let embedder = self.embedder_for(guard);
        match guard.librarian.as_mut() {
            Some(librarian) => {
                let stale = guard
                    .built_at
                    .map(|t| t.elapsed() >= INDEX_TTL)
                    .unwrap_or(true);
                if stale || !guard.complete {
                    librarian.reconcile(&self.mem)?;
                    guard.built_at = Some(Instant::now());
                    guard.complete = true;
                }
            }
            None => {
                let mut librarian = match Librarian::load_snapshot(&self.mem, &embedder) {
                    Some(librarian) => librarian,
                    None => Librarian::index_all_persistent(&self.mem, embedder)?,
                };
                librarian.reconcile(&self.mem)?;
                guard.librarian = Some(librarian);
                guard.built_at = Some(Instant::now());
                guard.complete = true;
            }
        }
        Ok(())
    }

    /// Append newly captured records to the warm index. This is the Phase 4
    /// kernel-internal path: capture hands over exact CIDs, so there is no IPC hop
    /// and no polling loop. If capture runs before the first full search, the index
    /// starts as append-only and the next search reconciles any pre-existing store
    /// records.
    pub(crate) fn append_index_records(&self, cids: &[Cid]) -> concierge_core::Result<usize> {
        if cids.is_empty() {
            return Ok(0);
        }
        let mut guard = self.index.lock().unwrap_or_else(|p| p.into_inner());
        let embedder = self.embedder_for(&mut guard);
        if guard.librarian.is_none() {
            guard.librarian = Some(
                Librarian::load_snapshot(&self.mem, &embedder)
                    .unwrap_or_else(|| Librarian::empty(embedder)),
            );
            guard.complete = false;
        }
        let added = guard
            .librarian
            .as_mut()
            .expect("index initialized above")
            .append_records(&self.mem, cids)?;
        if added > 0 {
            guard.built_at = Some(Instant::now());
        }
        Ok(added)
    }

    /// Persist the warm index to its on-disk snapshot (no-op if not built). Called
    /// on kernel shutdown so the next start loads instead of rebuilding.
    pub fn save_index_snapshot(&self) {
        let guard = self.index.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(librarian) = guard.librarian.as_ref() {
            librarian.save_snapshot(&self.mem);
        }
    }

    /// Lightweight liveness/info. Uses `try_lock` so a status check never blocks
    /// behind an in-flight build — the same discipline that keeps the GUI's poll
    /// responsive, applied at the kernel boundary.
    pub fn status(&self) -> serde_json::Value {
        let indexed = self
            .index
            .try_lock()
            .ok()
            .and_then(|g| g.librarian.as_ref().map(Librarian::len));
        serde_json::json!({
            "ok": true,
            // null until the first search builds the index; a number once warm.
            "indexed": indexed,
            "node": self
                .node
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().map(|node| node.peer_id().to_string())),
        })
    }

    pub fn handle(&self, req: &concierge_kernel::protocol::Request) -> Response {
        match req.path.as_str() {
            "/__kernel/ping" => Response::ok(req.id, serde_json::json!({ "pong": true })),
            "/__kernel/status" => Response::ok(req.id, self.status()),
            "/api/search" => self.search_response(req.id, &req.query),
            "/api/peers" => self.peers_response(req.id),
            // Phase 3: shared store-read routes (one impl in concierge-routes).
            "/api/meta" => self.meta_response(req.id),
            "/api/resolve" => json_route(
                req.id,
                concierge_routes::resolve_json_for_query(&self.mem, &req.query),
            ),
            "/api/names" => json_route(req.id, concierge_routes::names_json(&self.mem)),
            "/api/record" => self.record_response(req.id, &req.query),
            "/api/checkpoints" => json_route(req.id, concierge_routes::checkpoints_json(&self.mem)),
            "/api/graph" => json_route(
                req.id,
                concierge_routes::graph_json_for_query(&self.mem, &req.query),
            ),
            "/api/stats" => self.stats_response(req.id, &req.query),
            "/api/hot-pins" => json_route(req.id, concierge_routes::hot_pins_json(&self.mem)),
            "/api/privacy" => self.privacy_response(req.id, &req.query),
            other => Response::not_found(req.id, other),
        }
    }

    fn meta_response(&self, id: u64) -> Response {
        let store_label = self.store_label();
        json_route(
            id,
            concierge_routes::meta_json(&self.mem, "kernel", &store_label, ""),
        )
    }

    fn record_response(&self, id: u64, query: &str) -> Response {
        let params = concierge_routes::parse_query(query);
        match concierge_routes::query_key(&params) {
            Some(key) => json_route(id, concierge_routes::record_json(&self.mem, key)),
            None => Response::bad_request(id, "need ?name= or ?cid="),
        }
    }

    fn stats_response(&self, id: u64, query: &str) -> Response {
        let store_label = self.store_label();
        json_route(
            id,
            concierge_routes::stats_json_for_query(&self.mem, query, &store_label),
        )
    }

    fn privacy_response(&self, id: u64, query: &str) -> Response {
        let params = concierge_routes::parse_query(query);
        let Some(target) = params
            .get("cid")
            .or_else(|| params.get("root"))
            .or_else(|| params.get("name"))
            .filter(|target| !target.is_empty())
        else {
            return Response::bad_request(id, "privacy state requires an explicit ?cid= or ?name=");
        };
        json_route(id, concierge_routes::privacy_json(&self.mem, target))
    }

    fn store_label(&self) -> String {
        self.mem
            .store_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "kernel store".to_string())
    }

    fn search_response(&self, id: u64, query: &str) -> Response {
        let params = parse_query(query);
        let q = params
            .get("q")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if q.is_empty() {
            return Response::bad_request(id, "search requires a non-empty ?q=");
        }
        let budget = params
            .get("budget")
            .and_then(|b| b.parse::<usize>().ok())
            .unwrap_or(4000);
        let depth = parse_depth(params.get("depth").map(String::as_str));
        let hops = params
            .get("hops")
            .and_then(|h| h.parse::<u8>().ok())
            .unwrap_or(0)
            .min(3);
        let kinds: Option<Vec<String>> = params.get("kinds").map(|k| {
            k.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        });
        match self.search(&q, budget, depth, hops, kinds.as_deref()) {
            Ok(value) => Response::json(id, 200, value.to_string()),
            Err(error) => Response::json(
                id,
                500,
                serde_json::json!({ "error": error.to_string() }).to_string(),
            ),
        }
    }

    fn peers_response(&self, id: u64) -> Response {
        match self.ensure_node() {
            Ok(value) => Response::json(id, 200, value.to_string()),
            Err(error) => {
                Response::json(id, 500, serde_json::json!({ "error": error }).to_string())
            }
        }
    }

    fn ensure_node(&self) -> Result<serde_json::Value, String> {
        let mut guard = self
            .node
            .lock()
            .map_err(|_| "kernel node lock poisoned".to_string())?;
        if guard.is_none() {
            *guard = Some(KernelNode::spawn(self.mem.clone())?);
        }
        Ok(guard
            .as_ref()
            .expect("kernel node set above")
            .peers_json(&self.mem))
    }

    /// Semantic + graph-gravity retrieval over the shared warm index. Builds the
    /// index on demand (and on TTL staleness) and reuses it otherwise.
    pub fn search(
        &self,
        query: &str,
        budget: usize,
        depth: Depth,
        hops: u8,
        kinds: Option<&[String]>,
    ) -> concierge_core::Result<serde_json::Value> {
        if query.trim().is_empty() {
            return Ok(serde_json::json!({
                "query": query,
                "indexed": 0,
                "used_tokens": 0,
                "budget_tokens": budget,
                "items": []
            }));
        }
        if self.mem.names().map(|n| n.is_empty()).unwrap_or(false) {
            return Ok(serde_json::json!({
                "query": query,
                "indexed": 0,
                "used_tokens": 0,
                "budget_tokens": budget,
                "items": []
            }));
        }
        let mut guard = self.index.lock().unwrap_or_else(|p| p.into_inner());
        self.prepare_index_for_search(&mut guard)?;
        let librarian = guard.librarian.as_ref().expect("index built above");
        let result = librarian.retrieve_multihop(query, budget, &[], depth, hops.min(3), kinds);
        let items: Vec<serde_json::Value> = result
            .items
            .iter()
            .map(|hit| {
                serde_json::json!({
                    "cid": hit.cid,
                    "kind": hit.kind,
                    "preview": hit.preview,
                    "score": hit.score,
                    "similarity": hit.similarity,
                    "gravity": hit.gravity,
                    "tokens": hit.tokens,
                    "hop": hit.hop,
                })
            })
            .collect();
        Ok(serde_json::json!({
            "query": query,
            "indexed": librarian.len(),
            "used_tokens": result.used_tokens,
            "budget_tokens": result.budget_tokens,
            "items": items,
        }))
    }
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

/// Wrap a shared-handler JSON result in a kernel `Response` (200 on Ok, 500 on Err).
fn json_route(id: u64, result: concierge_core::Result<String>) -> Response {
    match result {
        Ok(body) => Response::json(id, 200, body),
        Err(error) => Response::json(
            id,
            500,
            serde_json::json!({ "error": error.to_string() }).to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{CoreBinding, Node};

    fn add_memory(mem: &MemCli, name: &str, text: &str) -> Cid {
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": text,
                    "kind": "reference",
                })
                .to_string(),
            })
            .unwrap();
        mem.bind(name, &cid).unwrap();
        cid
    }

    #[test]
    fn snapshot_warm_populates_status_and_reconciles_newer_records() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let first = add_memory(&mem, "first", "snapshot warm first memory");
        let lib = Librarian::index_all_persistent(&mem, mem.librarian_embedder()).unwrap();
        assert!(lib.contains(&first.0));
        lib.save_snapshot(&mem);

        let second = add_memory(&mem, "second", "snapshot warm second memory");
        let state = KernelState::new(dir.path().to_path_buf());
        state.warm_index_from_snapshot();

        assert_eq!(state.status()["indexed"].as_u64(), Some(2));
        let result = state
            .search("second memory", 4000, Depth::Summary, 0, None)
            .unwrap();
        assert!(
            result["items"].to_string().contains(&second.0),
            "reconciled search results should include the post-snapshot record: {result}"
        );
    }

    #[test]
    fn capture_append_updates_warm_index_without_polling() {
        let dir = tempfile::tempdir().unwrap();
        let state = KernelState::new(dir.path().to_path_buf());
        let mem = state.mem();
        let captured = add_memory(&mem, "captured", "captured comet telemetry");

        assert_eq!(
            state
                .append_index_records(std::slice::from_ref(&captured))
                .unwrap(),
            1
        );
        assert_eq!(state.status()["indexed"].as_u64(), Some(1));

        let result = state
            .search("comet telemetry", 4000, Depth::Summary, 0, None)
            .unwrap();
        assert_eq!(result["indexed"].as_u64(), Some(1));
        assert!(
            result["items"].to_string().contains(&captured.0),
            "captured record should be immediately searchable: {result}"
        );
    }
}
