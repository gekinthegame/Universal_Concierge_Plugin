//! Phase 7 explorer + Phase D Data Platter privacy controls.
//!
//! Reads (`GET`) are the read-only Visual Memory Explorer. Phase D adds the
//! privacy *mutations* — lock a subgraph, unlock for viewing, set the store
//! password, and authorize exactly one reviewed public publication — exposed as
//! `POST` routes behind a loopback security gate: a per-process CSRF token
//! required in a custom header (cross-origin pages cannot set it without a CORS
//! preflight we never answer), plus loopback `Host`/`Origin` validation (DNS
//! rebinding + cross-site defense), no CORS headers, and no request-body
//! logging (passwords never touch logs). Password verification rate-limiting
//! lives in the core (`verify_password`).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use concierge_core::deploy::SiteDeployPlan;
use concierge_core::{
    cid_from_link, default_embedder, verify_capability, verify_membership, Cid, CidOrName,
    CoreBinding, Depth, EgressOperation, EgressPlan, Error, Librarian, MemCli, PrivateSharePlan,
    Record, Result as CoreResult, SharedEmbedder, UserId,
};
use concierge_net::{
    ed25519_hex_from_peer_id, peer_id_from_ed25519_hex, store_provider, ConciergeNode, Multiaddr,
    NodeConfig, NodeEvent,
};
use rand_core::{OsRng, RngCore};

mod canvas;
mod mutations;
mod read_routes;
mod server;

#[cfg(test)]
use canvas::preview_token;
use canvas::{
    approved_agent_matches_peer, approved_contact_card_author, canvas_draft_get, canvas_file_get,
    canvas_files_get, canvas_mtime_get, canvas_preview_serve, canvas_projects_get,
    canvas_signal_get, mutation_canvas_delete, mutation_canvas_new, mutation_canvas_open,
    mutation_canvas_pwa, mutation_canvas_signal, mutation_canvas_snapshot, mutation_canvas_write,
    mutation_save_checkpoint, parse_canvas_signal, parse_contact_card, queue_canvas_signal,
    record_site_checkpoint, site_checkpoint_response, site_checkpoints_json,
};
use mutations::{
    body_str, contacts_json, deploy_status_json, handle_mutation, mcp_status_json,
    oauth_status_json, parse_body, pin_status_json, profile_json, reachability_json, requests_json,
    resolve_response, sites_json, valid_site_name, wallet_json, wallet_proposals_json,
};
use read_routes::{
    activity_response, blob_response, checkpoints_json, egress_plan_response, graph_response,
    me_response, meta_json, names_json, network_json, peers_response, privacy_response,
    record_response, rooms_json, search_response, session_of, stats_response, thread_response,
};
#[cfg(test)]
use read_routes::{node_and_links_from_record, PrivacyOverlay};
pub use server::{
    brave_path, open_app, open_browser, opera_path, pick_free_port, running_gui_port, serve,
    serve_with_options, wallet_browser, WalletBrowser,
};
#[cfg(test)]
use server::{
    claude_code_attached, load_capture_offsets, route_request, save_capture_offsets,
    serve_connection,
};
use server::{
    claude_code_status_json, mutation_claude_code_attach, mutation_sidekick, sidekick_status_json,
};

const INDEX_HTML: &str = include_str!("index.html");
const APP_CSS: &str = include_str!("app.css");
const APP_JS: &str = include_str!("app.js");
const WALLET_JS: &str = include_str!("wallet.js");
const STUDIO_JS: &str = include_str!("studio.js");
const WORLDMAP_JSON: &str = include_str!("worldmap.json");
const BRAIN_PNG: &[u8] = include_bytes!("brain.png");
const LOGO_PNG: &[u8] = include_bytes!("logo.png");
const SWARM_PNG: &[u8] = include_bytes!("swarm.png");
const YARAX_PNG: &[u8] = include_bytes!("yarax.png");
const IMPECCABLE_PNG: &[u8] = include_bytes!("impeccable.png");
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024;
/// Ingest and Site publishing get a much larger body budget than small control mutations.
const MAX_LARGE_BODY_BYTES: usize = 100 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const GRAPH_NODE_LIMIT: usize = 500;
const MUTATION_RATE_WINDOW: Duration = Duration::from_secs(10);
const MUTATION_RATE_MAX: usize = 60;
const REVIEW_TOKEN_TTL: Duration = Duration::from_secs(300);
const MAX_CANVAS_SESSIONS: usize = 64;
const MAX_CANVAS_SIGNAL_QUEUE: usize = 128;
const MAX_CANVAS_SIGNAL_BYTES: usize = 64 * 1024;
const MAX_CANVAS_SESSION_LEN: usize = 128;
const MAX_PREVIEW_DIRS: usize = 64;
const MAX_DISCOVERY_PEERS: usize = 256;
const DISCOVERY_PEER_TTL_SECS: u64 = 600;

/// A fresh, unguessable CSRF token for one server process.
fn new_csrf_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The libp2p chat node hosted inside the GUI process. Created lazily on the
/// first send (or `/api/me` view) so a user who never chats never starts a
/// network node — the "it just happens in the background" contract. The node's
/// key is the install's AgentID, so its PeerID *is* the user's username. The
/// owned tokio `Runtime` keeps the swarm + the inbound-drain task alive for the
/// life of the server; dropping the `ChatNode` shuts the node down.
struct ChatNode {
    _runtime: tokio::runtime::Runtime,
    node: ConciergeNode,
    /// The username we share: hex Ed25519 public key (the AgentID).
    agent_id: String,
    /// The libp2p PeerID derived from that same key.
    peer_id: String,
    /// Dialable listen addresses, filled in as the swarm reports them.
    addrs: Arc<Mutex<Vec<String>>>,
    /// Peers this node has discovered/connected to, keyed by PeerID — the live
    /// source for the Network tab's discovery map. Filled from swarm events.
    peers: Arc<Mutex<std::collections::BTreeMap<String, PeerInfo>>>,
}

/// One peer on the discovery map: who it is, how we found it, and whether we're
/// connected. Rendered to JSON by `/api/peers`.
#[derive(Debug, Clone)]
struct PeerInfo {
    peer_id: String,
    /// "connected" (a live connection) or "discovered" (located, not yet/no longer connected).
    status: &'static str,
    /// Best-effort discovery channel: "lan/direct", "relay", "rendezvous", or "dht".
    source: String,
    /// Whether the live connection is via a relay circuit (vs a direct connection).
    relayed: bool,
    /// Dialable addresses we learned (for rendezvous/DHT-routed peers).
    addresses: Vec<String>,
    /// Unix seconds we last saw activity from this peer.
    last_seen: u64,
}

fn prune_discovery_peers(map: &mut std::collections::BTreeMap<String, PeerInfo>, now: u64) {
    map.retain(|_, peer| {
        peer.status == "connected" || now.saturating_sub(peer.last_seen) < DISCOVERY_PEER_TTL_SECS
    });
    if map.len() <= MAX_DISCOVERY_PEERS {
        return;
    }
    let mut removable: Vec<(String, u64)> = map
        .iter()
        .filter(|(_, peer)| peer.status != "connected")
        .map(|(id, peer)| (id.clone(), peer.last_seen))
        .collect();
    removable.sort_by_key(|(_, last_seen)| *last_seen);
    for (id, _) in removable {
        if map.len() <= MAX_DISCOVERY_PEERS {
            break;
        }
        map.remove(&id);
    }
}

impl std::fmt::Debug for ChatNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatNode")
            .field("peer_id", &self.peer_id)
            .finish()
    }
}

/// Display-only details supplied by the harness or manual GUI command, plus the
/// per-process CSRF token that gates privacy mutations. The token is empty by
/// default (mutations disabled); `serve_with_options` fills it with a fresh
/// random value, so a bare `GuiOptions` cannot accidentally permit a `POST`.
#[derive(Debug, Clone)]
pub struct GuiOptions {
    pub mounted_model: String,
    pub store_label: String,
    pub open_browser: bool,
    pub watch_pid: Option<u32>,
    pub csrf_token: String,
    mutation_limiter: Arc<Mutex<MutationRateLimiter>>,
    review_cache: Arc<Mutex<HashMap<String, CachedReview>>>,
    /// Lazily-built, cached Librarian index for the semantic-search bar (Phase 8
    /// §1). The embedder is built once (semantic model if the `semantic-embed`
    /// feature is on, else lexical) and reused; the index is rebuilt on a short
    /// TTL so search reflects fresh capture without re-indexing every keystroke.
    librarian: Arc<Mutex<LibrarianState>>,
    /// The lazily-spawned libp2p chat node (private peer messaging). `None` until
    /// the first send or `/api/me`, then shared across all connection threads.
    chat: Arc<Mutex<Option<ChatNode>>>,
    /// Live Canvas signaling relay: `session -> pending signal messages`. Browsers
    /// exchange WebRTC offer/answer/ICE through here (same-machine), and remote
    /// peers' signals arrive over the libp2p DM channel and land here too. The
    /// canvas *content* never touches this — it flows peer-to-peer over WebRTC.
    canvas: Arc<Mutex<HashMap<String, Vec<serde_json::Value>>>>,
    /// Live website-builder preview roots: `token -> site source folder`. The
    /// preview iframe loads `/canvas-preview/<token>/…` so a multi-file site (HTML +
    /// CSS + JS + assets) renders with correct relative paths and hot-reloads as the
    /// folder changes. Read-only file serving, fenced to the folder.
    preview_dirs: Arc<Mutex<HashMap<String, std::path::PathBuf>>>,
    /// The System Console feed: a rolling, monotonic record of what the concierge
    /// actually does in-process — embedder load + indexing + retrieval, every
    /// mutation (publish, sidekick, MCP writes, canvas drafts), chat node lifecycle.
    /// The GUI polls `/api/activity?since=<seq>` and prints it so the user can see
    /// the plugin does what it says (no hidden network or model activity).
    activity: Arc<Mutex<ActivityLog>>,
    /// Open GUI windows. Each window heartbeats `/api/heartbeat` while it's open and
    /// beacons `/api/closing` on unload; the lifecycle watchdog (only when this process
    /// opened a browser) shuts the whole server down — stopping the detached Kubo node —
    /// once the last window closes, so hitting the GUI's X fully exits with no orphaned
    /// background processes. Maps window-id → last seen; `seen_any` guards the startup race.
    clients: Arc<Mutex<ClientPresence>>,
}

/// Window-presence tracker behind [`GuiOptions::clients`].
#[derive(Debug, Default)]
struct ClientPresence {
    /// True once any window has heartbeated — until then the watchdog must not shut down
    /// (the server is up before the first window has loaded).
    seen_any: bool,
    /// window-id → last heartbeat instant. Pruned by the watchdog; emptied → time to exit.
    last_seen: HashMap<String, Instant>,
}

/// One line in the System Console feed.
#[derive(Debug, Clone)]
struct ActivityEntry {
    seq: u64,
    ts_unix: u64,
    /// Severity/colour bucket the GUI maps to a console class: `ok` | `ev` | `wn`.
    level: &'static str,
    text: String,
}

/// A bounded, monotonic activity feed. New entries get an ever-increasing `seq`
/// so the GUI can poll incrementally (`?since=<last seq>`); the oldest are dropped
/// past the cap so memory stays bounded for a long-running session.
#[derive(Debug, Default)]
struct ActivityLog {
    next_seq: u64,
    entries: VecDeque<ActivityEntry>,
}

/// Wall-clock seconds since the Unix epoch, for stamping console lines.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn atomic_local_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

impl ActivityLog {
    /// Keep roughly the last ten minutes of a busy session on screen.
    const CAP: usize = 500;

    fn push(&mut self, level: &'static str, text: String) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push_back(ActivityEntry {
            seq,
            ts_unix: now_unix(),
            level,
            text,
        });
        while self.entries.len() > Self::CAP {
            self.entries.pop_front();
        }
    }
}

/// The Data Platter's retrieval state: the shared embedder (built once) and a
/// cached index (rebuilt on a TTL). Lazily initialised on first search.
struct LibrarianState {
    embedder: Option<SharedEmbedder>,
    cache: Option<LibrarianCache>,
}

struct LibrarianCache {
    librarian: Librarian<SharedEmbedder>,
    built_at: Instant,
}

impl std::fmt::Debug for LibrarianState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibrarianState")
            .field("indexed", &self.cache.as_ref().map(|c| c.librarian.len()))
            .finish()
    }
}

/// How long a built search index is reused before a rebuild (capture is
/// continuous, so a short staleness window is fine).
const LIBRARIAN_TTL: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct MutationRateLimiter {
    attempts: VecDeque<Instant>,
}

#[derive(Debug, Clone)]
struct CachedReview {
    plan: CachedReviewPlan,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
enum CachedReviewPlan {
    Egress(EgressPlan),
    Private(PrivateSharePlan),
    SiteDeploy(SiteDeployPlan),
}

impl MutationRateLimiter {
    fn allow(&mut self, now: Instant) -> bool {
        while self
            .attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= MUTATION_RATE_WINDOW)
        {
            self.attempts.pop_front();
        }
        if self.attempts.len() >= MUTATION_RATE_MAX {
            return false;
        }
        self.attempts.push_back(now);
        true
    }
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            mounted_model: "not declared".to_string(),
            store_label: "current store".to_string(),
            open_browser: false,
            watch_pid: None,
            csrf_token: String::new(),
            mutation_limiter: Arc::new(Mutex::new(MutationRateLimiter {
                attempts: VecDeque::new(),
            })),
            review_cache: Arc::new(Mutex::new(HashMap::new())),
            librarian: Arc::new(Mutex::new(LibrarianState {
                embedder: None,
                cache: None,
            })),
            chat: Arc::new(Mutex::new(None)),
            canvas: Arc::new(Mutex::new(HashMap::new())),
            preview_dirs: Arc::new(Mutex::new(HashMap::new())),
            activity: Arc::new(Mutex::new(ActivityLog::default())),
            clients: Arc::new(Mutex::new(ClientPresence::default())),
        }
    }
}

impl GuiOptions {
    pub fn new(
        mounted_model: String,
        store_label: String,
        open_browser: bool,
        watch_pid: Option<u32>,
    ) -> Self {
        Self {
            mounted_model,
            store_label,
            open_browser,
            watch_pid,
            ..Self::default()
        }
    }

    fn allow_mutation(&self) -> bool {
        self.mutation_limiter
            .lock()
            .map(|mut limiter| limiter.allow(Instant::now()))
            .unwrap_or(false)
    }

    /// Record one line in the System Console feed. `level` is the colour bucket the
    /// GUI maps to a console class (`ok` | `ev` | `wn`). Never blocks the request on
    /// a poisoned lock — transparency is best-effort, never load-bearing.
    fn log(&self, level: &'static str, text: impl Into<String>) {
        if let Ok(mut feed) = self.activity.lock() {
            feed.push(level, text.into());
        }
    }

    fn cache_review(&self, plan: EgressPlan) -> CoreResult<String> {
        let token = new_csrf_token();
        let mut cache = self
            .review_cache
            .lock()
            .map_err(|_| Error::SecurityPolicy("review cache lock poisoned".to_string()))?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.insert(
            token.clone(),
            CachedReview {
                plan: CachedReviewPlan::Egress(plan),
                expires_at: now + REVIEW_TOKEN_TTL,
            },
        );
        Ok(token)
    }

    fn reviewed_plan(&self, token: &str) -> Option<EgressPlan> {
        let mut cache = self.review_cache.lock().ok()?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.get(token).and_then(|review| match &review.plan {
            CachedReviewPlan::Egress(plan) => Some(plan.clone()),
            CachedReviewPlan::Private(_) | CachedReviewPlan::SiteDeploy(_) => None,
        })
    }

    fn cache_private_review(&self, plan: PrivateSharePlan) -> CoreResult<String> {
        let token = new_csrf_token();
        let mut cache = self
            .review_cache
            .lock()
            .map_err(|_| Error::SecurityPolicy("review cache lock poisoned".to_string()))?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.insert(
            token.clone(),
            CachedReview {
                plan: CachedReviewPlan::Private(plan),
                expires_at: now + REVIEW_TOKEN_TTL,
            },
        );
        Ok(token)
    }

    fn reviewed_private_plan(&self, token: &str) -> Option<PrivateSharePlan> {
        let mut cache = self.review_cache.lock().ok()?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.get(token).and_then(|review| match &review.plan {
            CachedReviewPlan::Private(plan) => Some(plan.clone()),
            CachedReviewPlan::Egress(_) | CachedReviewPlan::SiteDeploy(_) => None,
        })
    }

    fn cache_site_deploy_review(&self, plan: SiteDeployPlan) -> CoreResult<String> {
        let token = new_csrf_token();
        let mut cache = self
            .review_cache
            .lock()
            .map_err(|_| Error::SecurityPolicy("review cache lock poisoned".to_string()))?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.insert(
            token.clone(),
            CachedReview {
                plan: CachedReviewPlan::SiteDeploy(plan),
                expires_at: now + REVIEW_TOKEN_TTL,
            },
        );
        Ok(token)
    }

    fn reviewed_site_deploy(&self, token: &str) -> Option<SiteDeployPlan> {
        let mut cache = self.review_cache.lock().ok()?;
        let now = Instant::now();
        cache.retain(|_, review| review.expires_at > now);
        cache.get(token).and_then(|review| match &review.plan {
            CachedReviewPlan::SiteDeploy(plan) => Some(plan.clone()),
            CachedReviewPlan::Egress(_) | CachedReviewPlan::Private(_) => None,
        })
    }

    fn discard_review(&self, token: &str) {
        if let Ok(mut cache) = self.review_cache.lock() {
            cache.remove(token);
        }
    }
}

/// An HTTP response.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    /// Overrides `content_type` when set (for dynamic media types on blob assets).
    pub content_type_owned: Option<String>,
    /// Overrides the default GUI CSP when a route needs a narrower browser sandbox.
    pub csp: Option<&'static str>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// When true, the response may be framed same-origin (so a PDF blob can
    /// render in an `<iframe>`); otherwise framing is denied as usual.
    pub embeddable: bool,
}

impl Response {
    fn html(body: &str) -> Self {
        Self::new(200, "text/html; charset=utf-8", body.as_bytes().to_vec())
    }

    fn json(body: String) -> Self {
        Self::new(200, "application/json", body.into_bytes())
    }

    fn bad_request(message: &str) -> Self {
        Self::json_error(400, message)
    }

    fn not_found() -> Self {
        Self::json_error(404, "not found")
    }

    fn method_not_allowed() -> Self {
        Self::json_error(405, "method not allowed")
    }

    fn unsupported_media_type() -> Self {
        Self::json_error(415, "mutations require application/json")
    }

    fn too_many_requests() -> Self {
        Self::json_error(429, "mutation rate limit exceeded")
    }

    /// A generic loopback-gate rejection. Intentionally detail-free so a probing
    /// page learns nothing about which check failed.
    fn forbidden() -> Self {
        Self::json_error(403, "forbidden")
    }

    fn forbidden_with_message(message: String) -> Self {
        Self::json_error(403, &message)
    }

    fn header_too_large() -> Self {
        Self::json_error(431, "request headers too large")
    }

    fn payload_too_large() -> Self {
        Self::json_error(413, "request body too large")
    }

    fn error(message: String) -> Self {
        Self::json_error(500, &message)
    }

    fn json_error(status: u16, message: &str) -> Self {
        Self::new(
            status,
            "application/json",
            serde_json::json!({ "error": message })
                .to_string()
                .into_bytes(),
        )
    }

    fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            content_type_owned: None,
            csp: None,
            headers: Vec::new(),
            body,
            embeddable: false,
        }
    }

    /// Serve raw bytes (a stored blob) with a dynamic media type. Same-origin
    /// framing is permitted so PDFs render inline; `filename` adds a
    /// `Content-Disposition` (inline, or attachment when `download`).
    fn asset(media_type: &str, body: Vec<u8>, filename: Option<&str>, download: bool) -> Self {
        let mut response = Self::new(200, "application/octet-stream", body);
        response.content_type_owned = Some(media_type.to_string());
        response.embeddable = true;
        if let Some(name) = filename {
            let safe: String = name
                .chars()
                .map(|ch| {
                    if ch == '"' || ch == '\\' || ch.is_control() {
                        '_'
                    } else {
                        ch
                    }
                })
                .collect();
            let disposition = if download { "attachment" } else { "inline" };
            response.headers.push((
                "Content-Disposition".to_string(),
                format!("{disposition}; filename=\"{safe}\""),
            ));
        }
        response
    }
}

/// Route a GET request with default display metadata. Kept as a small pure seam
/// for callers and tests that do not need harness-specific mounted details.
pub fn handle(mem: &MemCli, path: &str, query: &str) -> Response {
    handle_with_options(mem, &GuiOptions::default(), path, query)
}

/// Route a GET request. Every route is read-only over the store.
pub fn handle_with_options(
    mem: &MemCli,
    options: &GuiOptions,
    path: &str,
    query: &str,
) -> Response {
    // Live-builder preview: serve a registered site folder's files so a multi-file
    // site renders with correct relative paths.
    if let Some(rest) = path.strip_prefix("/canvas-preview/") {
        return canvas_preview_serve(mem, options, rest);
    }
    match path {
        "/" => Response::html(INDEX_HTML),
        "/app.css" => Response::new(200, "text/css; charset=utf-8", APP_CSS.as_bytes().to_vec()),
        "/app.js" => Response::new(
            200,
            "text/javascript; charset=utf-8",
            APP_JS.as_bytes().to_vec(),
        ),
        "/wallet.js" => Response::new(
            200,
            "text/javascript; charset=utf-8",
            WALLET_JS.as_bytes().to_vec(),
        ),
        "/studio.js" => Response::new(
            200,
            "text/javascript; charset=utf-8",
            STUDIO_JS.as_bytes().to_vec(),
        ),
        "/worldmap.json" => Response::new(
            200,
            "application/json; charset=utf-8",
            WORLDMAP_JSON.as_bytes().to_vec(),
        ),
        "/api/brain.png" => Response::new(200, "image/png", BRAIN_PNG.to_vec()),
        "/api/logo.png" => Response::new(200, "image/png", LOGO_PNG.to_vec()),
        "/api/swarm.png" => Response::new(200, "image/png", SWARM_PNG.to_vec()),
        "/api/yarax.png" => Response::new(200, "image/png", YARAX_PNG.to_vec()),
        "/api/impeccable.png" => Response::new(200, "image/png", IMPECCABLE_PNG.to_vec()),
        "/api/meta" => to_response(meta_json(mem, options)),
        "/api/me" => me_response(mem, options),
        "/api/sites" => to_response(sites_json(mem)),
        "/api/publish/reachability" => to_response(reachability_json(mem)),
        "/api/site/checkpoints" => to_response(site_checkpoints_json(mem)),
        "/api/site/checkpoint" => site_checkpoint_response(mem, query),
        "/api/mcp/status" => to_response(mcp_status_json(mem)),
        "/api/canvas/files" => canvas_files_get(mem, options, query),
        "/api/canvas/file" => canvas_file_get(mem, options, query),
        "/api/canvas/projects" => canvas_projects_get(mem),
        "/api/canvas/mtime" => canvas_mtime_get(mem, options, query),
        "/api/canvas/draft" => canvas_draft_get(mem),
        "/api/canvas/signal" => canvas_signal_get(options, query),
        "/api/requests" => to_response(requests_json(mem)),
        "/api/contacts" => to_response(contacts_json(mem)),
        "/api/profile" => to_response(profile_json(mem)),
        "/api/resolve" => resolve_response(mem, query),
        "/api/names" => to_response(names_json(mem)),
        "/api/record" => record_response(mem, query),
        "/api/blob" => blob_response(mem, query),
        "/api/checkpoints" => to_response(checkpoints_json(mem)),
        "/api/graph" => graph_response(mem, query),
        "/api/stats" => stats_response(mem, options, query),
        "/api/activity" => activity_response(mem, options, query),
        "/api/rooms" => to_response(rooms_json(mem)),
        "/api/network" => to_response(network_json(mem)),
        "/api/peers" => peers_response(mem, options),
        "/api/hot-pins" => match mem.hot_pins() {
            Ok(pins) => Response::json(serde_json::json!({ "pins": pins }).to_string()),
            Err(error) => Response::error(error.to_string()),
        },
        "/api/thread" => thread_response(mem, query),
        "/api/privacy" => privacy_response(mem, query),
        "/api/search" => search_response(mem, options, query),
        "/api/sidekick/status" => to_response(sidekick_status_json(mem)),
        "/api/claude-code/status" => to_response(claude_code_status_json(mem)),
        "/api/deploy/credentials" => to_response(deploy_status_json(mem)),
        "/api/deploy/cloudflare/oauth-status" => Response::json(oauth_status_json("cloudflare")),
        "/api/deploy/firebase/oauth-status" => Response::json(oauth_status_json("firebase")),
        "/api/pin/credentials" => to_response(pin_status_json(mem)),
        "/api/wallet" => to_response(wallet_json(mem)),
        "/api/wallet/proposals" => to_response(wallet_proposals_json(mem)),
        "/api/egress-plan" => egress_plan_response(mem, options, query),
        "/api/export-car" => Response::bad_request(
            "browser plaintext CAR download is intentionally disabled; use the reviewed CLI export flow",
        ),
        _ => Response::not_found(),
    }
}

fn to_response(result: CoreResult<String>) -> Response {
    match result {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

/// A username is the hex-encoded 32-byte Ed25519 public key (the AgentID): 64
/// lowercase hex chars. Used to tell a direct-message recipient from a room name
/// in the chat bar's single "to" field.
fn looks_like_username(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The shared, deterministic 1:1 thread id for two usernames (order-independent),
/// so both sides store the conversation under the same room.
fn dm_room_id(a: &str, b: &str) -> String {
    let mut pair = [a, b];
    pair.sort_unstable();
    format!("dm:{}-{}", pair[0], pair[1])
}

/// Every room this install knows, so the chat node can subscribe to them on
/// start and receive inbound messages on existing threads.
fn known_rooms(mem: &MemCli) -> Vec<String> {
    let mut rooms: BTreeSet<String> = BTreeSet::new();
    if let Ok(book) = mem.room_book() {
        rooms.extend(book.rooms.keys().cloned());
    }
    if let Ok(names) = mem.names() {
        for (name, _) in names {
            if let Some(room) = name.strip_prefix("room-latest-") {
                rooms.insert(room.to_string());
            }
        }
    }
    rooms.into_iter().collect()
}

/// Lazily spawn the libp2p chat node (idempotent). The node's key is the
/// install's AgentID, so its PeerID is the user's username; it listens on an
/// ephemeral port, joins mDNS (LAN) + the public DHT (global) for discovery, and
/// a background task drains inbound gossipsub messages into the store via
/// [`MemCli::accept_message`]. Best-effort: a failure to start is surfaced to the
/// caller but never panics the request.
fn ensure_chat_node(mem: &MemCli, options: &GuiOptions) -> Result<(), String> {
    let mut guard = options
        .chat
        .lock()
        .map_err(|_| "chat node lock poisoned".to_string())?;
    if guard.is_some() {
        return Ok(());
    }
    let identity = mem.identity().map_err(|e| e.to_string())?;
    let secret = identity.secret_bytes();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("chat runtime: {e}"))?;
    let provider = store_provider(Arc::new(mem.clone()));
    let (node, mut events) = {
        // `spawn_with_provider` calls `tokio::spawn` internally, so it must run
        // inside the runtime context.
        let _enter = runtime.enter();
        ConciergeNode::spawn_with_provider(secret, NodeConfig::default(), provider)?
    };
    // Listen on TCP and QUIC (UDP). QUIC avoids the TCP port-reuse collision when
    // two nodes share a host and traverses NAT better.
    for listen in ["/ip4/0.0.0.0/tcp/0", "/ip4/0.0.0.0/udp/0/quic-v1"] {
        if let Ok(addr) = listen.parse::<Multiaddr>() {
            let _ = node.listen(addr);
        }
    }
    for room in known_rooms(mem) {
        let _ = node.subscribe(&room);
    }
    let agent_id = identity.agent_id().0;
    let peer_id = node.peer_id.to_string();
    let addrs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let peers: Arc<Mutex<std::collections::BTreeMap<String, PeerInfo>>> =
        Arc::new(Mutex::new(std::collections::BTreeMap::new()));

    // Drain inbound network events into the store. A received message is a signed
    // envelope (verified by `accept_message`); a `Listening` event records a
    // dialable address for `/api/me`.
    let drain_mem = mem.clone();
    let drain_addrs = addrs.clone();
    let drain_peers = peers.clone();
    let drain_canvas = options.canvas.clone();
    let drain_activity = options.activity.clone();
    let drain_node = node.clone();
    runtime.spawn(async move {
        // Upsert a peer into the discovery map, preserving the strongest known state
        // (a live connection outranks a bare discovery; a direct link outranks relayed).
        let touch_peer = |id: String,
                          status: &'static str,
                          source: &str,
                          relayed: bool,
                          addresses: Vec<String>| {
            if let Ok(mut map) = drain_peers.lock() {
                let now = now_unix();
                let entry = map.entry(id.clone()).or_insert_with(|| PeerInfo {
                    peer_id: id,
                    status: "discovered",
                    source: source.to_string(),
                    relayed: true,
                    addresses: Vec::new(),
                    last_seen: now,
                });
                if status == "connected" || entry.status != "connected" {
                    entry.status = status;
                }
                if !source.is_empty() {
                    entry.source = source.to_string();
                }
                if status == "connected" {
                    entry.relayed = relayed;
                }
                if !addresses.is_empty() {
                    entry.addresses = addresses;
                }
                entry.last_seen = now;
                prune_discovery_peers(&mut map, now);
            }
        };
        while let Some(event) = events.recv().await {
            match event {
                // A direct (1:1) message over the concierge-only request-response
                // protocol. Live-canvas WebRTC signaling rides the same channel — it
                // is routed to the signaling relay, not a chat thread. Everything else
                // is consent-gated: only an approved contact's message enters a thread.
                NodeEvent::DirectMessage {
                    from_peer,
                    data,
                    delivery_id,
                } => {
                    let json = String::from_utf8_lossy(&data).into_owned();
                    let mut accepted = false;
                    if let Some(signal) = parse_canvas_signal(&json) {
                        let claimed = signal
                            .get("from")
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        if approved_agent_matches_peer(&drain_mem, claimed, &from_peer) {
                            if let Ok(mut store) = drain_canvas.lock() {
                                accepted = queue_canvas_signal(&mut store, signal);
                            }
                        }
                    } else if let Some(card_json) = parse_contact_card(&json) {
                        // A peer's signed contact card (Layer 2) — verify + cache.
                        if let Some(aid) =
                            approved_contact_card_author(&drain_mem, &card_json, &from_peer)
                        {
                            if drain_mem.import_card(&card_json).is_ok() {
                                accepted = true;
                                if let Ok(mut feed) = drain_activity.lock() {
                                    feed.push(
                                        "ev",
                                        format!(
                                            "received a contact card · {}…",
                                            &aid[..aid.len().min(10)]
                                        ),
                                    );
                                }
                            }
                        }
                    } else if drain_mem.receive_message(&json).is_ok() {
                        accepted = true;
                        if let Ok(mut feed) = drain_activity.lock() {
                            feed.push(
                                "ev",
                                "received a private message from an approved contact".into(),
                            );
                        }
                    }
                    let _ = drain_node.acknowledge_dm(delivery_id, accepted);
                }
                // A group-room message over gossipsub — same consent gate.
                NodeEvent::Message { data, .. } => {
                    let json = String::from_utf8_lossy(&data).into_owned();
                    if drain_mem.receive_message(&json).is_ok() {
                        if let Ok(mut feed) = drain_activity.lock() {
                            feed.push("ev", "received a room message".into());
                        }
                    }
                }
                // A peer acked a direct message — clear it from the retry outbox.
                NodeEvent::DirectMessageDelivered { message_id, .. } => {
                    let _ = drain_mem.mark_outbound_delivered(&message_id);
                }
                NodeEvent::Listening(addr) => {
                    if let Ok(mut list) = drain_addrs.lock() {
                        if !list.contains(&addr) {
                            list.push(addr);
                        }
                    }
                }
                // ── Discovery-map signals ──
                NodeEvent::ConnectionEstablished { peer_id, relayed } => {
                    let source = if relayed { "relay" } else { "lan/direct" };
                    touch_peer(peer_id, "connected", source, relayed, Vec::new());
                }
                NodeEvent::DirectConnectionUpgrade {
                    peer_id,
                    succeeded: true,
                    ..
                } => touch_peer(peer_id, "connected", "lan/direct", false, Vec::new()),
                NodeEvent::DirectConnectionUpgrade { .. } => {}
                NodeEvent::RendezvousDiscovered { peer_id, addresses } => {
                    touch_peer(peer_id, "discovered", "rendezvous", true, addresses);
                }
                NodeEvent::PeerRouted { peer_id, addresses } => {
                    touch_peer(peer_id, "discovered", "dht", true, addresses);
                }
                _ => {}
            }
        }
    });

    // Store-and-forward retry: every 30s re-send each undelivered direct message
    // (and on startup, flushing anything queued in a previous session). The
    // recipient de-dupes by signature, and the delivery ack clears the entry, so
    // a message sent while the peer was offline lands once they come online.
    let retry_mem = mem.clone();
    let retry_opts = options.clone();
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            // Store-and-forward retry for undelivered DMs.
            let pending = retry_mem.pending_outbound().unwrap_or_default();
            // Best-effort contact-card sync: share my signed card with each approved
            // contact (Layer 2, no Kubo needed). Re-tried each tick so it lands once
            // a peer is online; import on the other side is idempotent.
            let card_env = retry_mem.my_card().ok().map(|card| {
                serde_json::json!({ "type": "contact-card", "card": card }).to_string()
            });
            let contacts = retry_mem.approved_contacts().unwrap_or_default();
            if let Ok(guard) = retry_opts.chat.lock() {
                if let Some(chat) = guard.as_ref() {
                    for (_id, recipient, envelope) in pending {
                        if let Some(peer) = peer_id_from_ed25519_hex(&recipient) {
                            let _ = chat.node.find_peer(peer);
                            let _ = chat.node.send_dm(peer, envelope.into_bytes());
                        }
                    }
                    if let Some(env) = &card_env {
                        for recipient in &contacts {
                            if let Some(peer) = peer_id_from_ed25519_hex(recipient) {
                                let _ = chat.node.find_peer(peer);
                                let _ = chat.node.send_dm(peer, env.clone().into_bytes());
                            }
                        }
                    }
                }
            }
        }
    });

    options.log(
        "ok",
        "peer messaging node online · listening for approved-contact messages",
    );
    *guard = Some(ChatNode {
        _runtime: runtime,
        node,
        agent_id,
        peer_id,
        addrs,
        peers,
    });
    Ok(())
}

/// `/api/peers`: the live network-discovery map data — this node + every peer it
/// has discovered or connected to. Brings the chat node online (so discovery is
/// actually running) before reading the registry. Discovered-but-stale peers
/// (located, never connected, not seen in 10 min) are pruned from the view.
fn query_key(params: &HashMap<String, String>) -> Option<CidOrName> {
    params
        .get("cid")
        .filter(|cid| !cid.is_empty())
        .map(|cid| CidOrName::Cid(Cid(cid.clone())))
        .or_else(|| {
            params
                .get("name")
                .filter(|name| !name.is_empty())
                .map(|name| CidOrName::Name(name.clone()))
        })
}

fn resolve_target(mem: &MemCli, params: &HashMap<String, String>) -> CoreResult<Cid> {
    if let Some(cid) = params
        .get("cid")
        .or_else(|| params.get("root"))
        .filter(|cid| !cid.is_empty())
    {
        return Ok(Cid(cid.clone()));
    }
    if let Some(name) = params.get("name").filter(|name| !name.is_empty()) {
        return mem.resolve(name);
    }
    mem.resolve("latest").or_else(|_| {
        mem.names()?
            .into_iter()
            .next()
            .map(|(_, cid)| cid)
            .ok_or_else(|| Error::NameUnbound("store has no named roots".to_string()))
    })
}

fn parse_query(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn link_details(record: &serde_json::Value, outbound_links: Vec<Cid>) -> Vec<(String, Cid)> {
    let mut details: Vec<(String, Cid)> = Vec::new();
    let body = record.get("body").unwrap_or(&serde_json::Value::Null);
    let kind = body
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    let mut push = |relation: &str, value: Option<&serde_json::Value>| {
        if let Some(cid) = value.and_then(decode_link) {
            details.push((normalize_relation(relation), cid));
        }
    };

    match kind {
        "checkpoint" => {
            push("checkpoint_root", body.get("root"));
            push("checkpoint_parent", body.get("parent"));
        }
        "file_ref" => push("content", body.get("content")),
        "plan" => push("spec", body.get("spec")),
        "task" => push("parent", body.get("parent")),
        "skill" => push("supersedes", body.get("supersedes")),
        "directory_manifest" => {
            if let Some(entries) = body.get("entries").and_then(|value| value.as_array()) {
                for entry in entries {
                    push("file_ref", entry.get("file_ref"));
                }
            }
        }
        "ingest_run" => push("manifest", body.get("manifest")),
        "conversation" => {
            if let Some(turns) = body.get("turns").and_then(|value| value.as_array()) {
                for turn in turns {
                    push("turn", Some(turn));
                }
            }
            push("parent", body.get("parent"));
        }
        _ => {}
    }

    if let Some(edges) = record.get("edges").and_then(|value| value.as_array()) {
        for edge in edges {
            let relation = edge
                .get("rel")
                .and_then(relation_name)
                .unwrap_or_else(|| "links_to".to_string());
            push(&relation, edge.get("to"));
        }
    }

    if record
        .get("source")
        .and_then(|source| source.get("kind"))
        .and_then(|value| value.as_str())
        == Some("derived")
    {
        if let Some(from) = record
            .get("source")
            .and_then(|source| source.get("from"))
            .and_then(|value| value.as_array())
        {
            for source in from {
                push("derived_from", Some(source));
            }
        }
    }

    for cid in outbound_links {
        if !details.iter().any(|(_, existing)| existing == &cid) {
            details.push(("links_to".to_string(), cid));
        }
    }
    details.sort();
    details.dedup();
    details
}

fn relation_name(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            value
                .get("type")
                .and_then(|kind| kind.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("kind")
                .and_then(|kind| kind.as_str())
                .map(str::to_string)
        })
}

fn normalize_relation(relation: &str) -> String {
    match relation.to_ascii_lowercase().replace('-', "_").as_str() {
        "checkpoint_root" | "root" => "checkpoint_root",
        "checkpoint_parent" => "checkpoint_parent",
        "content" => "content",
        "spec" => "spec",
        "parent" => "parent",
        "turn" | "turns" => "turn",
        "supersedes" => "supersedes",
        "file_ref" => "file_ref",
        "manifest" => "manifest",
        "derived_from" => "derived_from",
        _ => "links_to",
    }
    .to_string()
}

fn decode_link(value: &serde_json::Value) -> Option<Cid> {
    if value.is_null() {
        return None;
    }
    cid_from_link(value).ok()
}

fn record_preview(record: &serde_json::Value) -> String {
    let body = record.get("body").unwrap_or(record);
    for key in [
        "text",
        "path",
        "label",
        "title",
        "question",
        "tool",
        "name",
        "root_path",
    ] {
        if let Some(value) = body.get(key).and_then(|value| value.as_str()) {
            return truncate(value, 100);
        }
    }
    truncate(&body.to_string(), 100)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

// ---------------------------------------------------------------------------
// Phase D - privacy mutations behind the loopback security gate.
// ---------------------------------------------------------------------------

/// The minimal request facts the gate and mutation router need.
struct ParsedRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}

impl ParsedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

fn loopback_authority(authority: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(&format!("http://{authority}")).ok()?;
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return None;
    }
    Some((host, parsed.port_or_known_default()?))
}

fn loopback_origin_authority(origin: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(origin).ok()?;
    if parsed.scheme() != "http"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return None;
    }
    Some((host, parsed.port_or_known_default()?))
}

/// The loopback security gate. Returns `Some(rejection)` to refuse, `None` to
/// allow. Reads (`GET`/`HEAD`) need only a loopback `Host` (DNS-rebinding
/// defense). Mutations (`POST`) additionally require a loopback `Origin` and a
/// matching CSRF token in a custom header — neither of which a cross-origin page
/// can supply without a CORS preflight this server never answers.
fn loopback_gate(request: &ParsedRequest, csrf_token: &str) -> Option<Response> {
    // HTTP/1.1 requires Host. Fail closed if it is absent or non-loopback.
    let Some(host) = request.header("host").and_then(loopback_authority) else {
        return Some(Response::forbidden());
    };
    if request.method == "GET" || request.method == "HEAD" {
        return None;
    }
    if request.method != "POST" {
        return Some(Response::method_not_allowed());
    }
    // Mutations require a configured token (empty => mutations disabled).
    if csrf_token.is_empty() {
        return Some(Response::forbidden());
    }
    if request
        .header("origin")
        .and_then(loopback_origin_authority)
        .as_ref()
        != Some(&host)
    {
        return Some(Response::forbidden());
    }
    // Constant-ish comparison is unnecessary here (the token is per-process and
    // not a long-lived secret), but require an exact match.
    if request.header("x-csrf-token") != Some(csrf_token) {
        return Some(Response::forbidden());
    }
    if request
        .header("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_none_or(|content_type| !content_type.eq_ignore_ascii_case("application/json"))
    {
        return Some(Response::unsupported_media_type());
    }
    None
}

#[cfg(test)]
include!("tests.rs");
