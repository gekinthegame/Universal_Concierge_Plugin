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

use concierge_core::{
    cid_from_link, cid_link, default_embedder, verify_capability, verify_membership, Cid, CidOrName,
    CoreBinding, Depth, EgressOperation, EgressPlan, Error, Librarian, MemCli, Node, PrivateSharePlan,
    Record, Result as CoreResult, RevocationSet, SharedEmbedder, UserId,
};
use concierge_net::{
    content_message_id, peer_id_from_ed25519_hex, store_provider, ConciergeNode, Multiaddr,
    NodeConfig, NodeEvent,
};
use rand_core::{OsRng, RngCore};

const INDEX_HTML: &str = include_str!("index.html");
const BRAIN_PNG: &[u8] = include_bytes!("brain.png");
const LOGO_PNG: &[u8] = include_bytes!("logo.png");
const SWARM_PNG: &[u8] = include_bytes!("swarm.png");
const YARAX_PNG: &[u8] = include_bytes!("yarax.png");
const IMPECCABLE_PNG: &[u8] = include_bytes!("impeccable.png");
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024;
/// Ingest uploads a whole JSONL session, so it gets a much larger body budget
/// than the small JSON control mutations.
const MAX_INGEST_BODY_BYTES: usize = 64 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const GRAPH_NODE_LIMIT: usize = 500;
const MUTATION_RATE_WINDOW: Duration = Duration::from_secs(10);
const MUTATION_RATE_MAX: usize = 60;
const REVIEW_TOKEN_TTL: Duration = Duration::from_secs(300);

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
}

impl std::fmt::Debug for ChatNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatNode").field("peer_id", &self.peer_id).finish()
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
        }
    }
}

impl GuiOptions {
    pub fn new(mounted_model: String, store_label: String, open_browser: bool, watch_pid: Option<u32>) -> Self {
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
            CachedReviewPlan::Private(_) => None,
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
            CachedReviewPlan::Egress(_) => None,
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
                .map(|ch| if ch == '"' || ch == '\\' || ch.is_control() { '_' } else { ch })
                .collect();
            let disposition = if download { "attachment" } else { "inline" };
            response
                .headers
                .push(("Content-Disposition".to_string(), format!("{disposition}; filename=\"{safe}\"")));
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
        return canvas_preview_serve(options, rest);
    }
    match path {
        "/" => Response::html(INDEX_HTML),
        "/api/brain.png" => Response::new(200, "image/png", BRAIN_PNG.to_vec()),
        "/api/logo.png" => Response::new(200, "image/png", LOGO_PNG.to_vec()),
        "/api/swarm.png" => Response::new(200, "image/png", SWARM_PNG.to_vec()),
        "/api/yarax.png" => Response::new(200, "image/png", YARAX_PNG.to_vec()),
        "/api/impeccable.png" => Response::new(200, "image/png", IMPECCABLE_PNG.to_vec()),
        "/api/meta" => to_response(meta_json(mem, options)),
        "/api/me" => me_response(mem, options),
        "/api/sites" => to_response(sites_json(mem)),
        "/api/canvas/files" => canvas_files_get(options, query),
        "/api/canvas/mtime" => canvas_mtime_get(options, query),
        "/api/canvas/draft" => canvas_draft_get(mem),
        "/api/canvas/signal" => canvas_signal_get(options, query),
        "/api/requests" => to_response(requests_json(mem)),
        "/api/names" => to_response(names_json(mem)),
        "/api/record" => record_response(mem, query),
        "/api/blob" => blob_response(mem, query),
        "/api/checkpoints" => to_response(checkpoints_json(mem)),
        "/api/graph" => graph_response(mem, query),
        "/api/stats" => stats_response(mem, options, query),
        "/api/rooms" => to_response(rooms_json(mem)),
        "/api/network" => to_response(network_json(mem)),
        "/api/thread" => thread_response(mem, query),
        "/api/privacy" => privacy_response(mem, query),
        "/api/search" => search_response(mem, options, query),
        "/api/sidekick/status" => to_response(sidekick_status_json(mem)),
        "/api/claude-code/status" => to_response(claude_code_status_json(mem)),
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

    // Drain inbound network events into the store. A received message is a signed
    // envelope (verified by `accept_message`); a `Listening` event records a
    // dialable address for `/api/me`.
    let drain_mem = mem.clone();
    let drain_addrs = addrs.clone();
    let drain_canvas = options.canvas.clone();
    runtime.spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                // A direct (1:1) message over the concierge-only request-response
                // protocol. Live-canvas WebRTC signaling rides the same channel — it
                // is routed to the signaling relay, not a chat thread. Everything else
                // is consent-gated: only an approved contact's message enters a thread.
                NodeEvent::DirectMessage { data, .. } => {
                    let json = String::from_utf8_lossy(&data).into_owned();
                    if let Some(signal) = parse_canvas_signal(&json) {
                        if let Some(session) =
                            signal.get("session").and_then(|s| s.as_str()).map(str::to_string)
                        {
                            if let Ok(mut store) = drain_canvas.lock() {
                                store.entry(session).or_default().push(signal);
                            }
                        }
                    } else {
                        let _ = drain_mem.receive_message(&json);
                    }
                }
                // A group-room message over gossipsub — same consent gate.
                NodeEvent::Message { data, .. } => {
                    let json = String::from_utf8_lossy(&data).into_owned();
                    let _ = drain_mem.receive_message(&json);
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
            let pending = match retry_mem.pending_outbound() {
                Ok(pending) if !pending.is_empty() => pending,
                _ => continue,
            };
            if let Ok(guard) = retry_opts.chat.lock() {
                if let Some(chat) = guard.as_ref() {
                    for (_id, recipient, envelope) in pending {
                        if let Some(peer) = peer_id_from_ed25519_hex(&recipient) {
                            let _ = chat.node.find_peer(peer);
                            let _ = chat.node.send_dm(peer, envelope.into_bytes());
                        }
                    }
                }
            }
        }
    });

    *guard = Some(ChatNode {
        _runtime: runtime,
        node,
        agent_id,
        peer_id,
        addrs,
    });
    Ok(())
}

/// `/api/me`: the local username (shareable AgentID), its derived PeerID, and
/// the chat node's online state + listen addresses. Computing the username does
/// not require the node to be running, so this is safe to poll before any send.
fn me_response(mem: &MemCli, options: &GuiOptions) -> Response {
    // Bring the chat node online while the app is open so the user is reachable —
    // a recipient must be running to receive (incl. store-and-forward retries),
    // not only after they send. Idempotent + best-effort.
    let _ = ensure_chat_node(mem, options);
    let fallback_username = mem
        .identity()
        .map(|identity| identity.agent_id().0)
        .unwrap_or_default();
    // When the node is up its own values are authoritative; otherwise derive the
    // username + PeerID from the persisted identity (no node needed to show them).
    let (online, username, peer_id, addresses) = match options.chat.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(chat) => (
                true,
                chat.agent_id.clone(),
                chat.peer_id.clone(),
                chat.addrs.lock().map(|a| a.clone()).unwrap_or_default(),
            ),
            None => (
                false,
                fallback_username.clone(),
                peer_id_from_ed25519_hex(&fallback_username)
                    .map(|peer| peer.to_string())
                    .unwrap_or_default(),
                Vec::new(),
            ),
        },
        Err(_) => (false, fallback_username, String::new(), Vec::new()),
    };
    Response::json(
        serde_json::json!({
            "username": username,
            "peer_id": peer_id,
            "online": online,
            "addresses": addresses,
        })
        .to_string(),
    )
}

fn meta_json(mem: &MemCli, options: &GuiOptions) -> CoreResult<String> {
    let config = mem.config()?;
    Ok(serde_json::json!({
        "mounted_model": options.mounted_model,
        "store": options.store_label,
        "host": config.host.id,
        "adapter": config.host.adapter,
        "identity_kind": config.identity.kind,
        // Reads stay read-only; privacy mutations are gated by this token.
        "read_only": true,
        "csrf_token": options.csrf_token,
        "password_set": mem.password_is_set().unwrap_or(false),
    })
    .to_string())
}

fn names_json(mem: &MemCli) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    // Many names resolve to one record: content-addressed dedupe collapses
    // byte-identical events, and "latest"/pointer names alias a checkpoint. The
    // human timeline shows one row per CID — the leaf the user reasons about —
    // and keeps every alias for that row's tooltip.
    let mut aliases: BTreeMap<Cid, Vec<String>> = BTreeMap::new();
    for (name, cid) in mem.names()? {
        aliases.entry(cid).or_default().push(name);
    }
    let mut entries = Vec::new();
    for (cid, names) in aliases {
        let mut entry = name_node_summary(mem, &privacy, &cid)?;
        entry["name"] = serde_json::Value::String(names.first().cloned().unwrap_or_default());
        entry["names"] =
            serde_json::Value::Array(names.into_iter().map(serde_json::Value::String).collect());
        entry["cid"] = serde_json::Value::String(cid.0);
        entries.push(entry);
    }
    Ok(serde_json::Value::Array(entries).to_string())
}

/// Human-facing summary for the Names timeline: a coarse `created_at` (epoch
/// seconds, used only to place the entry on a date), the node `kind`, and a
/// short description. Locked nodes keep their timestamp — the graph view
/// already exposes their existence and position — but redact the preview body.
fn name_node_summary(
    mem: &MemCli,
    privacy: &PrivacyOverlay,
    cid: &Cid,
) -> CoreResult<serde_json::Value> {
    match mem.get(&CidOrName::Cid(cid.clone()))? {
        Record::Live {
            kind, body_json, ..
        } => {
            let value: serde_json::Value =
                serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
            let created_at = value.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(serde_json::json!({
                // Content is shown locally; the lock surfaces only as a badge (it
                // guards egress, not viewing).
                "kind": kind,
                "created_at": created_at,
                "preview": record_preview(&value),
                "locked": privacy.is_fenced(cid),
                "live": true,
            }))
        }
        Record::Tombstone { .. } => Ok(serde_json::json!({
            "kind": "tombstone",
            "created_at": 0,
            "preview": "Tombstoned record",
            "locked": privacy.is_fenced(cid),
            "live": false,
        })),
    }
}

/// Device-local privacy overlay used by every GUI preview route. The map may
/// still show CIDs and topology while locked bodies/previews remain redacted.
struct PrivacyOverlay {
    // Decision 0026: everything is fenced from egress by default. The overlay
    // tracks the *exceptions* — roots explicitly cleared for export, and roots
    // already known-public — not what is "locked" (that is the default).
    cleared_roots: BTreeSet<String>,
    cleared_cids: BTreeSet<String>,
    known_public: BTreeSet<String>,
    /// Guardian-quarantined CIDs (§3). Surfaced as a badge locally; excluded from
    /// retrieval/relay. Local view stays transparent — you can see + release them.
    quarantined: BTreeSet<String>,
}

impl PrivacyOverlay {
    fn load(mem: &MemCli) -> CoreResult<Self> {
        let cleared = mem.cleared_roots()?;
        let cleared_roots = cleared
            .iter()
            .map(|record| record.root.clone())
            .collect::<BTreeSet<_>>();
        let mut cleared_cids = BTreeSet::new();
        for record in cleared {
            cleared_cids.extend(
                mem.walk(&Cid(record.root.clone()))
                    .map_err(|error| {
                        Error::SecurityPolicy(format!(
                            "cannot verify cleared root {}: {error}",
                            record.root
                        ))
                    })?
                    .into_iter()
                    .map(|cid| cid.0),
            );
        }
        let known_public = mem
            .publish_receipts()?
            .into_iter()
            .map(|receipt| receipt.root)
            .collect();
        let quarantined = mem
            .quarantine_registry()
            .map(|reg| reg.list().map(|(cid, _)| cid.clone()).collect())
            .unwrap_or_default();
        Ok(Self {
            cleared_roots,
            cleared_cids,
            known_public,
            quarantined,
        })
    }

    fn is_quarantined(&self, cid: &str) -> bool {
        self.quarantined.contains(cid)
    }

    /// A CID is fenced from egress unless it has been explicitly cleared or is
    /// already known-public. Fenced is the default — this is what badges read.
    fn is_fenced(&self, cid: &Cid) -> bool {
        !self.cleared_cids.contains(&cid.0) && !self.known_public.contains(&cid.0)
    }

    fn is_cleared(&self, cid: &Cid) -> bool {
        self.cleared_cids.contains(&cid.0)
    }
}

fn record_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let Some(key) = query_key(&params) else {
        return Response::bad_request("need ?name= or ?cid=");
    };
    to_response(record_json(mem, key))
}

fn record_json(mem: &MemCli, key: CidOrName) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    match mem.get(&key)? {
        Record::Live {
            cid,
            kind,
            body_json,
        } => {
            // Content shown locally; `locked` is an egress badge, not a view gate.
            let mut value: serde_json::Value =
                serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
            // Blob nodes carry the file bytes as a giant JSON array — replace it
            // with a byte count so the record payload stays small (preview the
            // bytes via /api/blob instead).
            if kind == "blob" {
                if let Some(object) = value.get_mut("body").and_then(serde_json::Value::as_object_mut) {
                    let length = object
                        .get("bytes")
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::len);
                    object.remove("bytes");
                    if let Some(length) = length {
                        object.insert("size".to_string(), serde_json::json!(length));
                    }
                }
            }
            let links: Vec<_> = link_details(&value, mem.outbound_links(&cid).unwrap_or_default())
                .into_iter()
                .map(|(relation, target)| {
                    serde_json::json!({ "relation": relation, "cid": target.0 })
                })
                .collect();
            Ok(serde_json::json!({
                "cid": cid.0,
                "kind": kind,
                "live": true,
                "locked": privacy.is_fenced(&cid),
                "record": value,
                "links": links,
            })
            .to_string())
        }
        Record::Tombstone { cid, receipt_json } => {
            // Content shown locally; the lock is an egress badge.
            Ok(serde_json::json!({
                "cid": cid.0,
                "live": false,
                "locked": privacy.is_fenced(&cid),
                "tombstone": receipt_json,
            })
            .to_string())
        }
    }
}

/// Serve a stored blob's raw bytes for inline media preview / download. Accepts
/// a `blob` CID directly or a `file_ref` CID (whose `content` link is followed).
/// Respects the privacy overlay — locked content is forbidden until unlocked.
fn blob_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let Some(cid) = params.get("cid").filter(|cid| !cid.is_empty()) else {
        return Response::bad_request("need ?cid=");
    };
    let download = params.get("download").map(String::as_str) == Some("1");
    match blob_bytes(mem, &Cid(cid.clone())) {
        Ok(Some((media_type, filename, bytes))) => {
            Response::asset(&media_type, bytes, filename.as_deref(), download)
        }
        Ok(None) => Response::forbidden(),
        Err(error) => Response::bad_request(&error.to_string()),
    }
}

/// Resolve a CID to `(media_type, filename, bytes)`. Follows one `file_ref` →
/// `content` hop. A lock guards egress, not local viewing, so blob bytes are
/// always served to the local Data Platter.
fn blob_bytes(mem: &MemCli, cid: &Cid) -> CoreResult<Option<(String, Option<String>, Vec<u8>)>> {
    let Record::Live { body_json, .. } = mem.get(&CidOrName::Cid(cid.clone()))? else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
    // Stored records nest the node fields under `body`.
    let fields = value.get("body").unwrap_or(&value).clone();

    // A file_ref points at its blob via `content`; follow one hop with its name.
    if fields.get("bytes").is_none() {
        if let Some(link) = fields.get("content") {
            let blob_cid = cid_from_link(link)?;
            let filename = fields
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(|path| path.rsplit('/').next().unwrap_or(path).to_string());
            let media_type = fields
                .get("media_type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let Record::Live { body_json, .. } = mem.get(&CidOrName::Cid(blob_cid))? else {
                return Ok(None);
            };
            let blob: serde_json::Value =
                serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
            let blob_fields = blob.get("body").unwrap_or(&blob);
            let media_type = media_type
                .or_else(|| blob_fields.get("media_type").and_then(serde_json::Value::as_str).map(str::to_string))
                .unwrap_or_else(|| "application/octet-stream".to_string());
            return Ok(blob_byte_array(blob_fields).map(|bytes| (media_type, filename, bytes)));
        }
        return Ok(None);
    }

    let media_type = fields
        .get("media_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("application/octet-stream")
        .to_string();
    Ok(blob_byte_array(&fields).map(|bytes| (media_type, None, bytes)))
}

fn blob_byte_array(fields: &serde_json::Value) -> Option<Vec<u8>> {
    let array = fields.get("bytes")?.as_array()?;
    let mut bytes = Vec::with_capacity(array.len());
    for entry in array {
        bytes.push(u8::try_from(entry.as_u64()?).ok()?);
    }
    Some(bytes)
}

fn checkpoints_json(mem: &MemCli) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let mut aliases: BTreeMap<Cid, Vec<String>> = BTreeMap::new();
    for (name, cid) in mem.names()? {
        aliases.entry(cid).or_default().push(name);
    }

    let mut checkpoints = Vec::new();
    for (cid, names) in aliases {
        let Record::Live {
            kind, body_json, ..
        } = mem.get(&CidOrName::Cid(cid.clone()))?
        else {
            continue;
        };
        if kind != "checkpoint" {
            continue;
        }
        // Content shown locally; `locked` is an egress badge.
        let value: serde_json::Value =
            serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
        let body = value.get("body").unwrap_or(&serde_json::Value::Null);
        checkpoints.push(serde_json::json!({
            "cid": cid.0,
            "label": body.get("label").and_then(|v| v.as_str()).unwrap_or("checkpoint"),
            "root": body.get("root").and_then(decode_link).map(|cid| cid.0),
            "parent": body.get("parent").and_then(decode_link).map(|cid| cid.0),
            "created_at": value.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
            "names": names,
            "locked": privacy.is_fenced(&cid),
        }));
    }
    checkpoints.sort_by_key(|checkpoint| {
        checkpoint
            .get("created_at")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    });
    Ok(serde_json::Value::Array(checkpoints).to_string())
}

fn graph_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let has_target = params
        .get("cid")
        .or_else(|| params.get("root"))
        .or_else(|| params.get("name"))
        .is_some_and(|value| !value.is_empty());
    // With no explicit target, show the whole store as a session-grouped forest
    // instead of just the `latest` checkpoint's tiny subgraph.
    if !has_target {
        return to_response(forest_graph_json(mem));
    }
    match resolve_target(mem, &params) {
        Ok(root) => to_response(graph_json(mem, root)),
        Err(error) => Response::bad_request(&error.to_string()),
    }
}

/// Parse the session id out of a harness event name of the shape
/// `host:<host>:session:<id>:event:<hash>`. Returns `None` for names that do
/// not carry a session segment (checkpoints, manual bindings, `latest`).
fn session_of(name: &str) -> Option<String> {
    let rest = name.split(":session:").nth(1)?;
    let id = rest.split(':').next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Civil date `YYYY-MM-DD` → Unix seconds (UTC midnight). Lets the calendar tiers
/// be ordered without fetching any record. `None` for a malformed date.
fn date_to_unix(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    // days-from-civil (Howard Hinnant), proleptic Gregorian calendar.
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

/// Friendly leaf label for a session bucket. Session ids are long uuids, so show a
/// short, stable prefix. `"loose"` collects events whose name carries no session.
fn session_label(session: &str) -> String {
    if session == "loose" {
        return "loose records".to_string();
    }
    let short = session.get(0..8).unwrap_or(session);
    format!("session {short}")
}

/// Build the JSON node object plus its outbound `(relation, target)` link
/// details for a single CID. Shared by the rooted graph and the forest view so
/// both render nodes identically (kind, preview, lock badges).
fn node_and_links(
    mem: &MemCli,
    privacy: &PrivacyOverlay,
    encrypted_plaintext_roots: &BTreeSet<String>,
    cid: &Cid,
) -> CoreResult<(serde_json::Value, Vec<(String, Cid)>)> {
    let record = mem.get(&CidOrName::Cid(cid.clone()))?;
    Ok(node_and_links_from_record(
        mem,
        privacy,
        encrypted_plaintext_roots,
        cid,
        &record,
    ))
}

/// Same shape as [`node_and_links`] but from a record already in hand — used by
/// the whole-store forest, which batch-fetches every node in one `mem get-many`
/// rather than spawning a `mem` process per node.
fn node_and_links_from_record(
    mem: &MemCli,
    privacy: &PrivacyOverlay,
    encrypted_plaintext_roots: &BTreeSet<String>,
    cid: &Cid,
    record: &Record,
) -> (serde_json::Value, Vec<(String, Cid)>) {
    // Decision 0026: fenced from egress by default; the exceptions (cleared for
    // export, already known-public) are what we badge. Content + metadata are
    // always shown locally — a fence is an egress safeguard, never a view-hider.
    let fenced = privacy.is_fenced(cid);
    let cleared = privacy.is_cleared(cid);
    let known_public = privacy.known_public.contains(&cid.0);
    let quarantined = privacy.is_quarantined(&cid.0);
    match record {
        Record::Live { kind, body_json, .. } => {
            let value = serde_json::from_str::<serde_json::Value>(body_json)
                .unwrap_or(serde_json::Value::Null);
            let created_at = value.get("created_at").and_then(serde_json::Value::as_i64);
            let node = serde_json::json!({
                "cid": cid.0,
                "kind": kind.as_str(),
                "preview": record_preview(&value),
                "created_at": created_at,
                "fenced": fenced,
                "cleared": cleared,
                "known_public": known_public,
                "quarantined": quarantined,
                "encrypted_private": encrypted_plaintext_roots.contains(&cid.0),
            });
            // Links come from the record JSON we already have — no extra `mem` call.
            let outbound = mem.links_from_record_json(body_json).unwrap_or_default();
            let details = link_details(&value, outbound);
            (node, details)
        }
        Record::Tombstone { receipt_json, .. } => (
            serde_json::json!({
                "cid": cid.0,
                "kind": "tombstone",
                "preview": receipt_json.clone(),
                "fenced": fenced,
                "cleared": cleared,
                "known_public": known_public,
                "quarantined": quarantined,
                "encrypted_private": encrypted_plaintext_roots.contains(&cid.0),
            }),
            Vec::new(),
        ),
    }
}

/// A store-wide "forest": a synthetic store root fans out to the calendar tiers
/// `store → year → month → day → session` (Phase A.5, DECISIONS.md 0014). The
/// graph stops at the session, not the individual record — drilling into a
/// session's records is the Records tab's job — which keeps the canvas legible
/// and lets the whole timeline render without a node cap. Sessions are bucketed
/// by their events' occurrence day (the `day-` bindings), not ingest time.
/// File/folder imports stay their own expandable roots. Synthetic ids are
/// prefixed (`store:`/`year:`/`month:`/`day:`/`session:`) so the front end skips
/// record/privacy fetches for them. This is the default graph when no root is
/// selected.
fn forest_graph_json(mem: &MemCli) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let encrypted_plaintext_roots = mem
        .private_conversions()?
        .into_iter()
        .map(|conversion| conversion.plaintext_root)
        .collect::<BTreeSet<_>>();

    // The graph stops at the SESSION tier — store → year → month → day → session.
    // Rendering every record would clutter the canvas (thousands of leaves) and
    // balloon memory; to inspect a session's records the user opens the Records
    // tab. Sessions are placed by their events' *occurrence* day (the `day-`
    // bindings built at ingest, DECISIONS.md 0014), not by `created_at` (ingest
    // wall-clock), so the timeline reflects when things actually happened. Because
    // a session is summarised by a count rather than fetched per-record, there is
    // no node cap — every month/day/session is shown.
    let mut imports: Vec<(String, Cid)> = Vec::new();
    let mut cid_session: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_import: BTreeSet<String> = BTreeSet::new();
    for (name, cid) in mem.names()? {
        if let Some(label) = name.strip_prefix("import:") {
            if seen_import.insert(cid.0.clone()) {
                imports.push((label.to_string(), cid));
            }
            continue;
        }
        // Structural pointers (day/month/year calendar manifests, `latest`,
        // checkpoints) are not session events and stay out of the timeline.
        if name.starts_with("day-") || name.starts_with("month-") || name.starts_with("year-") {
            continue;
        }
        // Remember which session each event belongs to (from its name).
        if let Some(session) = session_of(&name) {
            cid_session.entry(cid.0.clone()).or_insert(session);
        }
    }

    // Date each named event by its record's `created_at` — the same axis the
    // Records tab groups by, so the graph timeline and the Records list agree.
    // One batched fetch over just the named events (+ imports), never the whole
    // store, and the leaves are sessions, so there is no node cap.
    let mut batch: Vec<Cid> = cid_session.keys().map(|c| Cid(c.clone())).collect();
    batch.extend(imports.iter().map(|(_, cid)| cid.clone()));
    let records = mem.get_many(&batch).unwrap_or_default();
    let created_day = |cid: &str| -> String {
        match records.get(cid) {
            Some(Record::Live { body_json, .. }) => {
                serde_json::from_str::<serde_json::Value>(body_json)
                    .ok()
                    .and_then(|v| v.get("created_at").and_then(serde_json::Value::as_u64))
                    .map(concierge_core::utc_date)
                    .unwrap_or_else(|| "undated".to_string())
            }
            _ => "undated".to_string(),
        }
    };

    // Bucket named events into (day, session) leaves; tally each day's total.
    let mut buckets: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut day_total: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for (cid, session) in &cid_session {
        let date = created_day(cid);
        *buckets.entry((date.clone(), session.clone())).or_default() += 1;
        *day_total.entry(date.clone()).or_default() += 1;
        total += 1;
    }

    let store_cid = "store:root";
    let mut nodes = vec![serde_json::json!({
        "cid": store_cid,
        "kind": "store",
        "preview": "The Data Platter",
        "synthetic": true,
        "fenced": false, "cleared": false,
        "known_public": false, "encrypted_private": false,
    })];
    let mut edges = Vec::new();

    // Emit the calendar tiers + session leaves, creating each synthetic node once.
    let mut years: BTreeSet<String> = BTreeSet::new();
    let mut months: BTreeSet<String> = BTreeSet::new();
    let mut emitted_days: BTreeSet<String> = BTreeSet::new();
    let tier_node = |cid: String, kind: &str, preview: &str, count: usize, started: Option<i64>| {
        serde_json::json!({
            "cid": cid, "kind": kind, "preview": preview, "count": count,
            "started_at": started, "synthetic": true,
            "fenced": false, "cleared": false,
            "known_public": false, "encrypted_private": false,
        })
    };
    for ((date, session), count) in &buckets {
        let (year, month) = if date.len() >= 7 {
            (date[0..4].to_string(), date[0..7].to_string())
        } else {
            ("undated".to_string(), "undated".to_string())
        };
        let started = date_to_unix(date);
        if years.insert(year.clone()) {
            nodes.push(tier_node(format!("year:{year}"), "year", &year, 0, started));
            edges.push(serde_json::json!({ "from": store_cid, "to": format!("year:{year}"), "relation": "year" }));
        }
        if months.insert(month.clone()) {
            nodes.push(tier_node(format!("month:{month}"), "month", &month, 0, started));
            edges.push(serde_json::json!({ "from": format!("year:{year}"), "to": format!("month:{month}"), "relation": "month" }));
        }
        let day_cid = format!("day:{date}");
        if emitted_days.insert(date.clone()) {
            let dcount = *day_total.get(date).unwrap_or(&0);
            nodes.push(tier_node(day_cid.clone(), "day", date, dcount, started));
            edges.push(serde_json::json!({ "from": format!("month:{month}"), "to": day_cid.clone(), "relation": "day" }));
        }
        let session_cid = format!("session:{date}:{session}");
        nodes.push(tier_node(session_cid.clone(), "session", &session_label(session), *count, started));
        edges.push(serde_json::json!({ "from": day_cid, "to": session_cid, "relation": "session" }));
    }

    // Each ingested file/folder is a real root: show it under the store, marked
    // expandable so a click lazy-loads its file tree (manifest → file_refs).
    for (label, cid) in &imports {
        let (mut node, _) = match records.get(&cid.0) {
            Some(record) => {
                node_and_links_from_record(mem, &privacy, &encrypted_plaintext_roots, cid, record)
            }
            None => node_and_links(mem, &privacy, &encrypted_plaintext_roots, cid)?,
        };
        // `import:<unix>-<basename>` → show the basename.
        let display = label.splitn(2, '-').nth(1).unwrap_or(label);
        node["preview"] = serde_json::Value::String(display.to_string());
        node["expandable"] = serde_json::Value::Bool(true);
        nodes.push(node);
        edges.push(serde_json::json!({ "from": store_cid, "to": cid.0, "relation": "import" }));
    }

    Ok(serde_json::json!({
        "root": store_cid,
        "nodes": nodes,
        "edges": edges,
        "total": total,
        "truncated": false,
        "forest": true,
        "edge_vocabulary": [
            "year", "month", "day", "session", "event", "checkpoint_root", "checkpoint_parent", "content",
            "spec", "parent", "turn", "supersedes", "file_ref", "manifest", "derived_from", "links_to"
        ],
    })
    .to_string())
}

fn graph_json(mem: &MemCli, root: Cid) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let encrypted_plaintext_roots = mem
        .private_conversions()?
        .into_iter()
        .map(|conversion| conversion.plaintext_root)
        .collect::<BTreeSet<_>>();
    // BFS the subgraph, fetching each level in a single batched `mem get-many`
    // and reusing the records — instead of `mem.walk` (one spawn per node)
    // followed by a second per-node fetch. We stop once GRAPH_NODE_LIMIT nodes
    // are collected: a rooted subgraph can fan out across the whole store via
    // shared blob CIDs (thousands of nodes), and the view only ever renders the
    // first GRAPH_NODE_LIMIT. Tombstones are skipped rather than aborting.
    let mut visited: BTreeSet<Cid> = BTreeSet::new();
    let mut included: Vec<Cid> = Vec::new();
    let mut records: std::collections::HashMap<String, Record> = std::collections::HashMap::new();
    let mut truncated = false;
    let mut frontier: Vec<Cid> = vec![root.clone()];
    'bfs: while !frontier.is_empty() {
        let level: Vec<Cid> = frontier
            .drain(..)
            .filter(|cid| visited.insert(cid.clone()))
            .collect();
        if level.is_empty() {
            continue;
        }
        let fetched = mem.get_many(&level)?;
        for cid in level {
            let Some(record) = fetched.get(&cid.0) else {
                continue;
            };
            let Record::Live { body_json, .. } = record else {
                continue;
            };
            if included.len() >= GRAPH_NODE_LIMIT {
                truncated = true;
                break 'bfs;
            }
            for child in mem.links_from_record_json(body_json).unwrap_or_default() {
                if !visited.contains(&child) {
                    frontier.push(child);
                }
            }
            records.insert(cid.0.clone(), record.clone());
            included.push(cid);
        }
    }
    let total = included.len();
    let included_set: BTreeSet<Cid> = included.iter().cloned().collect();

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for cid in &included {
        let (node, details) = node_and_links_from_record(
            mem,
            &privacy,
            &encrypted_plaintext_roots,
            cid,
            &records[&cid.0],
        );
        nodes.push(node);
        for (relation, target) in details {
            if included_set.contains(&target) {
                edges.push(serde_json::json!({
                    "from": cid.0,
                    "to": target.0,
                    "relation": relation,
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "root": root.0,
        "nodes": nodes,
        "edges": edges,
        "total": total,
        "truncated": truncated,
        "edge_vocabulary": [
            "checkpoint_root", "checkpoint_parent", "content", "spec", "parent",
            "turn", "supersedes", "file_ref", "manifest", "derived_from", "links_to"
        ],
    })
    .to_string())
}

fn stats_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    to_response(stats_json(mem, options, &params))
}

fn stats_json(
    mem: &MemCli,
    options: &GuiOptions,
    params: &HashMap<String, String>,
) -> CoreResult<String> {
    let stats = mem.store_stats()?;
    let config = mem.config()?;
    let target = resolve_target(mem, params).ok();
    let (car_blocks, car_size) = target
        .as_ref()
        .and_then(|root| mem.export_car_manifest(root).ok())
        .map(|(cids, bytes)| (cids.len(), bytes))
        .unwrap_or((0, 0));
    let receipts = mem.publish_receipts().unwrap_or_default();
    // Phase B: publishing is opt-in. Probe the *selected* backend so the UI can
    // say "not set up (publishing is optional)" instead of only erroring on a
    // publish attempt. This is a quick probe on the background stats refresh,
    // never the startup path.
    let publishing_ready = concierge_core::selected_backend_reachable(&config);
    let backends: Vec<_> = mem
        .list_backends()?
        .into_iter()
        .map(|backend| {
            let selected = backend.name == config.publishing.backend;
            let pinned = target.as_ref().is_some_and(|root| {
                receipts
                    .iter()
                    .any(|receipt| receipt.root == root.0 && receipt.backend == backend.name)
            });
            let status = if pinned {
                "receipt recorded".to_string()
            } else if selected && !publishing_ready {
                "not set up — publishing is optional (everything works offline)".to_string()
            } else if selected && publishing_ready {
                "node reachable — ready to publish".to_string()
            } else {
                "no local pin receipt".to_string()
            };
            serde_json::json!({
                "name": backend.name,
                "blurb": backend.blurb,
                "selected": selected,
                "reachable": selected && publishing_ready,
                "pin_status": status,
                "requirements": backend.requirements_summary(),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "names": stats.names,
        "blocks": stats.blocks,
        "reachable": stats.reachable,
        "orphans": stats.orphans,
        "tombstones": stats.tombstones,
        "car_blocks": car_blocks,
        "car_size": car_size,
        "root": target.map(|cid| cid.0),
        "backend": config.publishing.backend,
        "publishing_ready": publishing_ready,
        "backends": backends,
        "store": options.store_label,
    })
    .to_string())
}

fn rooms_json(mem: &MemCli) -> CoreResult<String> {
    let mut rooms: BTreeSet<String> = mem.room_book()?.rooms.keys().cloned().collect();
    for (name, _) in mem.names()? {
        if let Some(room) = name.strip_prefix("room-latest-") {
            rooms.insert(room.to_string());
        }
    }
    Ok(
        serde_json::Value::Array(rooms.into_iter().map(serde_json::Value::String).collect())
            .to_string(),
    )
}

/// The Private Network Map (Phase N · Phase H): this device's identity hierarchy,
/// the networks it belongs to, its granted scopes + their validity, the membership/
/// capability epoch, and who is revoked — everything the Data Platter needs to show
/// *what a device can access* and *who has been cut off*, without the CLI.
fn network_json(mem: &MemCli) -> CoreResult<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let user = mem.user_identity().ok().map(|u| u.agent_id().0);
    let device = mem.identity().ok().map(|d| d.agent_id().0);

    let mut networks = Vec::new();
    for descriptor in mem.networks()? {
        let nid = descriptor.network_id.clone();
        let revoked = mem.revocation_set(&nid).unwrap_or_default();
        let is_root = user
            .as_ref()
            .map(|u| descriptor.is_root(&UserId(u.clone())))
            .unwrap_or(false);

        let device_membership = match mem.device_membership(&nid) {
            Ok(Some(cert)) => {
                let valid = verify_membership(&cert, &descriptor, now, &revoked).is_ok();
                Some(serde_json::json!({
                    "subject": cert.subject_id,
                    "valid": valid,
                    "capabilities": cert.capabilities,
                }))
            }
            _ => None,
        };

        let capabilities: Vec<serde_json::Value> = mem
            .device_capabilities(&nid)
            .unwrap_or_default()
            .into_iter()
            .map(|cap| {
                let valid = verify_capability(&cap, &[], &descriptor, now, &revoked).is_ok();
                serde_json::json!({
                    "namespace": cap.namespace.canonical(),
                    "operations": cap.operations.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>(),
                    "valid": valid,
                })
            })
            .collect();

        let revoked_subjects: Vec<String> = mem
            .revocation_ledger(&nid)
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.verify(&descriptor).is_ok())
            .map(|record| record.subject_id)
            .collect();

        networks.push(serde_json::json!({
            "name": descriptor.name,
            "network_id": nid.0,
            "membership_epoch": descriptor.membership_epoch,
            "descriptor_valid": descriptor.verify().is_ok(),
            "is_root": is_root,
            "device_membership": device_membership,
            "capabilities": capabilities,
            "revoked": revoked_subjects,
        }));
    }

    Ok(serde_json::json!({
        "user_id": user,
        "device_id": device,
        "networks": networks,
    })
    .to_string())
}

fn thread_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let Some(room) = params.get("room").filter(|room| !room.is_empty()) else {
        return Response::bad_request("need ?room=");
    };
    to_response(thread_json(mem, room))
}

fn thread_json(mem: &MemCli, room: &str) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let social = mem.social_book().unwrap_or_default();
    let policy = mem.room_book()?.policy(room);
    // Phase N · Phase I — social legibility, all strictly local.
    let this_agent = mem.identity().ok().map(|id| id.agent_id().0).unwrap_or_default();
    let messages: Vec<_> = mem
        .room_thread(room)?
        .into_iter()
        .map(|(cid, envelope)| {
            // Message body shown locally; the lock is an egress badge.
            let tier = concierge_core::message_trust_tier(&envelope, &this_agent);
            serde_json::json!({
                "cid": cid.0,
                "author": envelope.author(),
                "nickname": social.nickname_of(envelope.author()),
                "clock": envelope.clock,
                "payload": envelope.payload,
                "parents": envelope.next,
                "locked": privacy.is_fenced(&cid),
                // Trust thermometer: the authentication tier this message crossed.
                "trust_tier": tier,
                "trust_label": tier.label(),
                // Structural importance: how many things it ties together (not popularity).
                "importance": concierge_core::structural_importance(&envelope),
                // Personal follow-lens: is the author someone *you* follow?
                "followed": social.is_following(envelope.author()),
            })
        })
        .collect();
    // Moderator badge data (Phase 8 §3/§4): the Guardian's room policy plus whether
    // the thread is now long enough to be a synthesis candidate (§4 threshold).
    let synthesis_candidate = messages.len() >= concierge_core::SYNTHESIS_THRESHOLD;
    Ok(serde_json::json!({
        "room": room,
        "policy": {
            "ai_send": policy.ai_send,
            "muted": policy.muted,
        },
        "moderation": {
            "guardian": "active",
            "ai_send": policy.ai_send,
            "muted_count": policy.muted.len(),
            "message_count": messages.len(),
            "synthesis_candidate": synthesis_candidate,
            "synthesis_threshold": concierge_core::SYNTHESIS_THRESHOLD,
        },
        "messages": messages,
    })
    .to_string())
}

fn egress_plan_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let Some(target) = params
        .get("cid")
        .or_else(|| params.get("root"))
        .or_else(|| params.get("name"))
        .filter(|target| !target.is_empty())
    else {
        return Response::bad_request("egress plan requires an explicit ?cid= or ?name=");
    };
    if params.get("op").map(String::as_str) == Some("private") {
        let Some(namespace) = params.get("namespace").filter(|value| !value.is_empty()) else {
            return Response::bad_request("private-share review requires ?namespace=");
        };
        let recipients = params
            .get("recipients")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return to_response(
            mem.build_encrypt_and_share_plan(target, namespace, &recipients)
                .and_then(|plan| {
                    let review_token = options.cache_private_review(plan.clone())?;
                    let mut value = serde_json::to_value(plan).map_err(|error| {
                        Error::Io(format!("serialize private-share plan: {error}"))
                    })?;
                    value
                        .as_object_mut()
                        .ok_or_else(|| {
                            Error::Io(
                                "private-share plan did not serialize as an object".to_string(),
                            )
                        })?
                        .insert(
                            "review_token".to_string(),
                            serde_json::Value::String(review_token),
                        );
                    Ok(value.to_string())
                }),
        );
    }
    // `?op=publish` reviews a public publication; the default reviews plaintext
    // CAR export. Every review is read-only.
    let plan = if params.get("op").map(String::as_str) == Some("publish") {
        mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish)
    } else {
        mem.build_egress_plan_for_target_and_backend(
            target,
            EgressOperation::PlaintextCarExport,
            "browser-download",
            "browser-download",
            "plaintext-portable",
        )
    };
    to_response(plan.and_then(|plan| {
        let review_token = options.cache_review(plan.clone())?;
        let mut value = serde_json::to_value(plan)
            .map_err(|error| Error::Io(format!("serialize egress plan: {error}")))?;
        value
            .as_object_mut()
            .ok_or_else(|| Error::Io("egress plan did not serialize as an object".to_string()))?
            .insert(
                "review_token".to_string(),
                serde_json::Value::String(review_token),
            );
        Ok(value.to_string())
    }))
}

/// Read-only privacy state for one target, for the drawer: whether the target's
/// own root is directly locked, whether its manifest reaches *any* lock
/// (inherited), the blocking lock roots, and whether it has a known-public
/// receipt. Never exposes record bodies.
fn privacy_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let Some(target) = params
        .get("cid")
        .or_else(|| params.get("root"))
        .or_else(|| params.get("name"))
        .filter(|target| !target.is_empty())
    else {
        return Response::bad_request("privacy state requires an explicit ?cid= or ?name=");
    };
    to_response(privacy_json(mem, target))
}

fn privacy_json(mem: &MemCli, target: &str) -> CoreResult<String> {
    let plan = mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish)?;
    let privacy = PrivacyOverlay::load(mem)?;
    // Decision 0026: fenced from egress by default. The panel reports whether the
    // root has been explicitly cleared for export (the exception) vs. fenced.
    let cleared = privacy.cleared_roots.contains(&plan.root.0);
    let fenced = !cleared && plan.known_public_receipts == 0;
    let known_public = plan.known_public_receipts > 0;
    let encrypted_private_copy = mem.private_copy_of(&plan.root.0)?;
    let blocked_node_count = plan
        .blocking_locks
        .iter()
        .flat_map(|hit| hit.intersecting_cids.iter())
        .collect::<BTreeSet<_>>()
        .len();
    let blocked_file_count = plan
        .blocking_locks
        .iter()
        .flat_map(|hit| hit.intersecting_file_paths.iter())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(serde_json::json!({
        "root": plan.root.0,
        "fenced": fenced,
        "cleared": cleared,
        "blocked": plan.is_blocked(),
        "reachable_node_count": plan.block_count,
        "file_count": plan.file_paths.len(),
        "blocked_node_count": blocked_node_count,
        "blocked_file_count": blocked_file_count,
        "blocking_locks": plan.blocking_locks,
        "sensitivity_warnings": plan.sensitivity_warnings,
        "known_public": known_public,
        "password_set": mem.password_is_set().unwrap_or(false),
        // Encrypted-private state is distinct from the policy lock: this root
        // has a capability-encrypted ciphertext copy (Phase E).
        "encrypted_private_copy": encrypted_private_copy,
    })
    .to_string())
}

/// Phase 8 §1 semantic-search endpoint. Builds (and caches, on a short TTL) the
/// Librarian index over the local store, then returns ranked CIDs for `?q=`.
/// Default embedder is the zero-dependency lexical one; a semantic backend swaps
/// in behind the same trait when its feature is enabled.
fn search_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let q = params.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if q.is_empty() {
        return Response::bad_request("search requires a non-empty ?q=");
    }
    let budget = params
        .get("budget")
        .and_then(|b| b.parse::<usize>().ok())
        .unwrap_or(4000);
    let depth = match params.get("depth").map(String::as_str) {
        Some("brief") => Depth::Brief,
        Some("full") => Depth::Full,
        _ => Depth::Summary,
    };
    // Multi-hop: pull in related context by following links from matches (capped).
    let hops = params
        .get("hops")
        .and_then(|h| h.parse::<u8>().ok())
        .unwrap_or(0)
        .min(3);
    // Optional comma-separated kind filter for direct matches (e.g. decision,memory).
    let kinds: Option<Vec<String>> = params.get("kinds").map(|k| {
        k.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });

    let mut guard = options
        .librarian
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Build the embedder once, per the librarian config (model is config-selected,
    // not baked in — lexical / fastembed:<model> / http:<url>).
    if guard.embedder.is_none() {
        let librarian_config = mem.config().map(|c| c.librarian).unwrap_or_default();
        guard.embedder = Some(default_embedder(&librarian_config));
    }
    let embedder = guard.embedder.clone().expect("embedder built above");
    let stale = guard
        .cache
        .as_ref()
        .map(|c| c.built_at.elapsed() >= LIBRARIAN_TTL)
        .unwrap_or(true);
    if stale {
        match Librarian::index_all_persistent(mem, embedder) {
            Ok(librarian) => {
                guard.cache = Some(LibrarianCache {
                    librarian,
                    built_at: Instant::now(),
                })
            }
            Err(error) => return Response::error(error.to_string()),
        }
    }
    let cache = guard.cache.as_ref().expect("index built above");
    let result = cache
        .librarian
        .retrieve_multihop(&q, budget, &[], depth, hops, kinds.as_deref());
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
    Response::json(
        serde_json::json!({
            "query": q,
            "indexed": cache.librarian.len(),
            "used_tokens": result.used_tokens,
            "budget_tokens": result.budget_tokens,
            "items": items,
        })
        .to_string(),
    )
}

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

/// Route a gated `POST` mutation. Bodies are JSON. Passwords are read straight
/// into the core call and never logged or echoed.
fn handle_mutation(mem: &MemCli, options: &GuiOptions, path: &str, body: &str) -> Response {
    match path {
        "/api/ingest" => mutation_ingest(mem, options, body),
        "/api/ingest-path" => mutation_ingest_path(mem, body),
        "/api/lock" => mutation_lock(mem, body),
        "/api/unlock" => mutation_unlock(mem, body),
        "/api/clear-for-egress" => mutation_clear_for_egress(mem, body),
        "/api/refence" => mutation_refence(mem, body),
        "/api/claude-code/attach" => mutation_claude_code_attach(mem, true),
        "/api/claude-code/detach" => mutation_claude_code_attach(mem, false),
        "/api/sidekick/enable" => mutation_sidekick(mem, true),
        "/api/sidekick/disable" => mutation_sidekick(mem, false),
        "/api/set-password" => mutation_set_password(mem, body),
        "/api/authorize-publish" => mutation_authorize_publish(mem, options, body),
        "/api/convert-private" => mutation_convert_private(mem, options, body),
        "/api/message" => mutation_post_message(mem, options, body),
        "/api/site/publish" => mutation_publish_site(mem, body),
        "/api/canvas/open" => mutation_canvas_open(options, body),
        "/api/canvas/signal" => mutation_canvas_signal(mem, options, body),
        "/api/canvas/snapshot" => mutation_canvas_snapshot(mem, body),
        "/api/requests/accept" => mutation_request_decision(mem, body, true),
        "/api/requests/decline" => mutation_request_decision(mem, body, false),
        "/api/network/create" => mutation_network_create(mem, body),
        "/api/network/revoke" => mutation_network_revoke(mem, body),
        "/api/network/rotate" => mutation_network_rotate(mem, body),
        _ => Response::not_found(),
    }
}

/// Rotate a private graph's capability key (Phase N · Phase G) after a revocation,
/// so the revoked holder's old key cannot decrypt the re-rooted ciphertext. Password
/// travels in the loopback body (same pattern as convert-private), never the URL.
fn mutation_network_rotate(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let ciphertext_root = match body_str(&value, "ciphertext_root") {
        Ok(root) => root,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(pw) => pw,
        Err(response) => return response,
    };
    match mem.rotate_private_capability(ciphertext_root, password) {
        Ok(result) => Response::json(
            serde_json::json!({
                "old_ciphertext_root": result.old_ciphertext_root,
                "new_ciphertext_root": result.new_ciphertext_root,
                "capability_epoch": result.capability_epoch,
                "block_count": result.block_count,
            })
            .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Post a direct private chat message into a local room thread (RoomBook). The
/// message is authored locally and appended to the room; the client re-fetches the
/// thread via `/api/thread`. Bodies travel in the loopback POST body, never the URL.
fn mutation_post_message(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let room = match body_str(&value, "room") {
        Ok(room) => room.trim(),
        Err(response) => return response,
    };
    let text = match body_str(&value, "body") {
        Ok(text) => text.trim(),
        Err(response) => return response,
    };
    if room.is_empty() || text.is_empty() {
        return Response::bad_request("recipient and message are required");
    }
    // A 64-hex "to" is a username (a direct message); anything else is a room name
    // (a group thread). Direct messages are stored under a shared dm-room id and
    // delivered to the recipient's personal inbox topic.
    if looks_like_username(room) {
        let me = match mem.identity() {
            Ok(identity) => identity.agent_id().0,
            Err(error) => return Response::error(error.to_string()),
        };
        if room == me {
            return Response::bad_request("cannot send a direct message to yourself");
        }
        let dm_room = dm_room_id(&me, room);
        let cid = match mem.post_message(&dm_room, text) {
            Ok(cid) => cid,
            Err(error) => return Response::error(error.to_string()),
        };
        // Initiating a conversation implies trust: approve the recipient so their
        // replies are accepted into the thread (not held as a request).
        let _ = mem.add_contact(room);
        let delivered = deliver_to_user(mem, options, room, &cid);
        return Response::json(
            serde_json::json!({
                "ok": true, "room": dm_room, "cid": cid.0,
                "delivered": delivered, "direct": true,
            })
            .to_string(),
        );
    }
    let cid = match mem.post_message(room, text) {
        Ok(cid) => cid,
        Err(error) => return Response::error(error.to_string()),
    };
    // Group room: publish the signed envelope to the room's gossipsub topic.
    let delivered = deliver_message(mem, options, room, &cid);
    Response::json(
        serde_json::json!({ "ok": true, "room": room, "cid": cid.0, "delivered": delivered })
            .to_string(),
    )
}

/// Deliver a direct message to a username: ensure the node is up, locate the peer
/// globally via the DHT (mDNS covers the LAN), and publish the signed envelope to
/// the recipient's inbox topic. Best-effort — if the peer is offline/unreachable
/// the message is still recorded locally (store-and-forward is a later stage).
fn deliver_to_user(mem: &MemCli, options: &GuiOptions, target_username: &str, cid: &Cid) -> bool {
    if let Err(error) = ensure_chat_node(mem, options) {
        eprintln!("chat node unavailable: {error}");
        return false;
    }
    let Ok(env) = mem.read_message(cid) else {
        return false;
    };
    let Ok(bytes) = serde_json::to_string(&env) else {
        return false;
    };
    let Some(peer) = peer_id_from_ed25519_hex(target_username) else {
        return false;
    };
    // Queue for store-and-forward retry: if the peer is offline now, the retry
    // loop re-sends until they ack (the ack clears the entry). Keyed by the same
    // content id the transport reports back on delivery.
    let bytes = bytes.into_bytes();
    let message_id = content_message_id(&bytes);
    let _ = mem.queue_outbound(&message_id, target_username, &String::from_utf8_lossy(&bytes));
    if let Ok(guard) = options.chat.lock() {
        if let Some(chat) = guard.as_ref() {
            // Locate the peer (DHT for global; mDNS already covers the LAN), then
            // deliver point-to-point over the concierge-only protocol.
            let _ = chat.node.find_peer(peer);
            return chat.node.send_dm(peer, bytes).is_ok();
        }
    }
    false
}

/// `/api/sites`: the user's published websites (the Planet Pattern registry).
fn sites_json(mem: &MemCli) -> CoreResult<String> {
    let sites: Vec<serde_json::Value> = mem
        .site_list()?
        .into_iter()
        .map(|site| {
            serde_json::json!({
                "name": site.name,
                "ipns": site.ipns,
                "dir": site.dir,
                "last_cid": site.last_cid,
                "published_at": site.published_at,
                "url": format!("https://ipfs.io/ipns/{}", site.ipns),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "sites": sites,
        "kubo_installed": concierge_core::kubo_installed(),
    })
    .to_string())
}

/// A site name is also the public Kubo IPNS key name — keep it to safe characters.
fn valid_site_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `/api/site/publish`: publish (or update) a folder as a website. Password-gated
/// egress (the password travels in the loopback body, never the URL). Publishing
/// is the deliberate act; the AI only *staged* the folder.
fn mutation_publish_site(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = match body_str(&value, "name") {
        Ok(name) => name.trim(),
        Err(response) => return response,
    };
    let folder = match body_str(&value, "folder") {
        Ok(folder) => folder.trim(),
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let kind = value.get("kind").and_then(|k| k.as_str()).unwrap_or("site");
    if !valid_site_name(name) {
        return Response::bad_request("site name must be letters, digits, - _ . (max 64)");
    }
    if folder.is_empty() {
        return Response::bad_request("a folder path is required");
    }
    match mem.publish_site(name, folder, kind, password) {
        Ok(receipt) => Response::json(
            serde_json::json!({
                "ok": true,
                "name": receipt.site_name,
                "ipns": receipt.ipns_name,
                "cid": receipt.root,
                "url": receipt.gateway_url,
            })
            .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

// ── Live Collaborative Canvas (WebRTC signaling relay + snapshot) ───────────
//
// The canvas runs in the browser over a native WebRTC data channel (content never
// touches the server — ephemeral, peer-to-peer). The Rust node only relays the
// WebRTC handshake (offer/answer/ICE): same-machine peers poll this in-memory
// relay; remote peers' signals arrive over the libp2p DM channel and land here
// too. "Snapshot" stages the current HTML into a folder for the Phase-1 publish
// gate — the only thing that crosses into permanence (Decision 0026/0030).

/// Recognise a DM payload that is a canvas-signal envelope (`{"type":"canvas-signal"}`)
/// rather than a chat `MessageEnvelope`, returning the inner signal.
fn parse_canvas_signal(json: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("type").and_then(|t| t.as_str()) == Some("canvas-signal") {
        value.get("signal").cloned()
    } else {
        None
    }
}

/// `GET /api/canvas/signal?session=&me=`: drain pending signaling messages
/// addressed to `me` (or broadcast `*`) for this session.
fn canvas_signal_get(options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let session = params.get("session").cloned().unwrap_or_default();
    let me = params.get("me").cloned().unwrap_or_default();
    if session.is_empty() || me.is_empty() {
        return Response::bad_request("session and me are required");
    }
    let mut delivered = Vec::new();
    if let Ok(mut store) = options.canvas.lock() {
        if let Some(queue) = store.get_mut(&session) {
            let mut keep = Vec::new();
            for msg in queue.drain(..) {
                let to = msg.get("to").and_then(|v| v.as_str()).unwrap_or("");
                if to == me || to == "*" {
                    delivered.push(msg);
                } else {
                    keep.push(msg);
                }
            }
            *queue = keep;
        }
    }
    Response::json(serde_json::json!({ "messages": delivered }).to_string())
}

/// `POST /api/canvas/signal`: relay one WebRTC signaling message. Pushed to the
/// local relay (same-machine peers) and, if `to` is a username, sent over the DM
/// channel to that peer's node.
fn mutation_canvas_signal(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let session = value.get("session").and_then(|v| v.as_str()).unwrap_or("").trim();
    if session.is_empty() {
        return Response::bad_request("session is required");
    }
    let to = value.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if let Ok(mut store) = options.canvas.lock() {
        let queue = store.entry(session.to_string()).or_default();
        queue.push(value.clone());
        if queue.len() > 500 {
            let excess = queue.len() - 500;
            queue.drain(0..excess);
        }
    }
    if looks_like_username(&to) && ensure_chat_node(mem, options).is_ok() {
        if let Some(peer) = peer_id_from_ed25519_hex(&to) {
            if let Ok(guard) = options.chat.lock() {
                if let Some(chat) = guard.as_ref() {
                    let wire = serde_json::json!({ "type": "canvas-signal", "signal": value });
                    let _ = chat.node.find_peer(peer);
                    let _ = chat.node.send_dm(peer, wire.to_string().into_bytes());
                }
            }
        }
    }
    Response::json(serde_json::json!({ "ok": true }).to_string())
}

/// `POST /api/canvas/snapshot`: stage the canvas's current HTML into a folder under
/// the store, ready to publish through the Phase-1 gate. Returns the folder + a
/// suggested site name; publishing remains the user's explicit, password-gated act.
fn mutation_canvas_snapshot(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let session = value.get("session").and_then(|v| v.as_str()).unwrap_or("snapshot");
    let html = value.get("html").and_then(|v| v.as_str()).unwrap_or("");
    if html.is_empty() {
        return Response::bad_request("html is required");
    }
    let store = match mem.store_dir() {
        Ok(dir) => dir,
        Err(error) => return Response::error(error.to_string()),
    };
    let safe: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(48)
        .collect();
    let safe = if safe.is_empty() { "snapshot".to_string() } else { safe };
    let folder = store.join("canvas").join(&safe);
    if let Err(error) = std::fs::create_dir_all(&folder) {
        return Response::error(format!("create snapshot dir: {error}"));
    }
    if let Err(error) = std::fs::write(folder.join("index.html"), html) {
        return Response::error(format!("write snapshot: {error}"));
    }
    Response::json(
        serde_json::json!({
            "ok": true,
            "folder": folder.to_string_lossy(),
            "name": format!("canvas-{safe}"),
        })
        .to_string(),
    )
}

// ── Live website-builder: folder preview (multi-file, hot-reloading) ────────

/// Proper **web** content types for serving a site to a browser (the explorer's
/// `guess_media_type_path` serves source as `text/plain` to *display* it — here we
/// need the browser to *render* it, so html/css/js get their real types).
fn site_media_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// A stable short token for a folder path (the preview route key).
fn preview_token(dir: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Minimal percent-decoder for a URL path segment (`%20` → space, …).
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn _skip_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "node_modules" | "target" | "dist" | "build")
}

/// Relative paths of every file under `dir` (the AI-written site files), sorted.
fn folder_files(dir: &std::path::Path) -> Vec<String> {
    fn walk(base: &std::path::Path, cur: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(cur) else { return };
        for entry in entries.flatten() {
            if out.len() > 2000 {
                return;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if _skip_dir(&name) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The newest modification time (unix secs) across the folder — the hot-reload signal.
fn folder_mtime(dir: &std::path::Path) -> u64 {
    fn walk(cur: &std::path::Path, max: &mut u64) {
        let Ok(entries) = std::fs::read_dir(cur) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if _skip_dir(&name.to_string_lossy()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(&path, max);
            } else if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    *max = (*max).max(dur.as_secs());
                }
            }
        }
    }
    let mut max = 0;
    walk(dir, &mut max);
    max
}

/// `GET /api/canvas/draft`: the HTML the AI staged via the MCP `concierge.write_site`
/// tool (`<store>/canvas/draft/index.html`), so the Studio's Write tab can surface
/// the AI's page live. Returns `{html, mtime}` (html null if none yet).
fn canvas_draft_get(mem: &MemCli) -> Response {
    let path = mem
        .store_dir()
        .ok()
        .map(|dir| dir.join("canvas").join("draft").join("index.html"));
    if let Some(path) = path {
        if let Ok(html) = std::fs::read_to_string(&path) {
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Response::json(serde_json::json!({ "html": html, "mtime": mtime }).to_string());
        }
    }
    Response::json(serde_json::json!({ "html": serde_json::Value::Null, "mtime": 0 }).to_string())
}

fn canvas_dir(options: &GuiOptions, query: &str) -> Option<std::path::PathBuf> {
    let token = parse_query(query).get("token").cloned()?;
    options.preview_dirs.lock().ok()?.get(&token).cloned()
}

/// `POST /api/canvas/open`: register a site source folder for live preview. Returns
/// a token (used in `/canvas-preview/<token>/…`), the file list, and the mtime.
fn mutation_canvas_open(options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let folder = match body_str(&value, "folder") {
        Ok(folder) => folder.trim(),
        Err(response) => return response,
    };
    if folder.is_empty() {
        return Response::bad_request("a folder path is required");
    }
    let canon = match std::path::Path::new(folder).canonicalize() {
        Ok(canon) if canon.is_dir() => canon,
        _ => return Response::error(format!("not a folder: {folder}")),
    };
    let token = preview_token(&canon);
    if let Ok(mut dirs) = options.preview_dirs.lock() {
        dirs.insert(token.clone(), canon.clone());
    }
    Response::json(
        serde_json::json!({
            "ok": true,
            "token": token,
            "files": folder_files(&canon),
            "mtime": folder_mtime(&canon),
            "has_index": canon.join("index.html").is_file(),
        })
        .to_string(),
    )
}

/// `GET /api/canvas/files?token=`: the folder's file list + mtime + whether it has
/// an index.html (so the builder can show the AI's files and a publish-readiness hint).
fn canvas_files_get(options: &GuiOptions, query: &str) -> Response {
    match canvas_dir(options, query) {
        Some(dir) => Response::json(
            serde_json::json!({
                "files": folder_files(&dir),
                "mtime": folder_mtime(&dir),
                "has_index": dir.join("index.html").is_file(),
            })
            .to_string(),
        ),
        None => Response::bad_request("unknown or missing preview token"),
    }
}

/// `GET /api/canvas/mtime?token=`: the folder's newest mtime, for hot-reload polling.
fn canvas_mtime_get(options: &GuiOptions, query: &str) -> Response {
    match canvas_dir(options, query) {
        Some(dir) => Response::json(serde_json::json!({ "mtime": folder_mtime(&dir) }).to_string()),
        None => Response::bad_request("unknown or missing preview token"),
    }
}

/// Serve `/canvas-preview/<token>/<relpath>` from the registered folder, read-only
/// and fenced to that folder (no path traversal). This is what the preview iframe
/// loads, so a multi-file site renders with correct relative links.
fn canvas_preview_serve(options: &GuiOptions, rest: &str) -> Response {
    let (token, relpath) = rest.split_once('/').unwrap_or((rest, ""));
    let relpath = percent_decode(relpath);
    let relpath = if relpath.trim_matches('/').is_empty() { "index.html".to_string() } else { relpath };
    let Some(dir) = options.preview_dirs.lock().ok().and_then(|d| d.get(token).cloned()) else {
        return Response::not_found();
    };
    let candidate = dir.join(&relpath);
    let canon = match candidate.canonicalize() {
        Ok(canon) => canon,
        Err(_) => return Response::not_found(),
    };
    if !canon.starts_with(&dir) {
        return Response::forbidden();
    }
    if !canon.is_file() {
        return Response::not_found();
    }
    match std::fs::read(&canon) {
        Ok(bytes) => Response::new(200, site_media_type(&canon.to_string_lossy()), bytes),
        Err(_) => Response::not_found(),
    }
}

/// `/api/requests`: pending direct-message requests from senders the user has not
/// yet approved (the consent gate's holding area).
fn requests_json(mem: &MemCli) -> CoreResult<String> {
    let items: Vec<serde_json::Value> = mem
        .message_requests()?
        .into_iter()
        .map(|(username, count, preview)| {
            serde_json::json!({ "username": username, "count": count, "preview": preview })
        })
        .collect();
    Ok(serde_json::json!({ "requests": items }).to_string())
}

/// Accept (approve sender + flush their held messages into the thread) or decline
/// (drop their held messages, stay blocked) a pending message request.
fn mutation_request_decision(mem: &MemCli, body: &str, accept: bool) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let username = match body_str(&value, "username") {
        Ok(username) => username.trim(),
        Err(response) => return response,
    };
    if username.is_empty() {
        return Response::bad_request("username is required");
    }
    if accept {
        match mem.accept_contact(username) {
            Ok(delivered) => {
                Response::json(serde_json::json!({ "ok": true, "delivered": delivered }).to_string())
            }
            Err(error) => Response::error(error.to_string()),
        }
    } else {
        match mem.decline_contact(username) {
            Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
            Err(error) => Response::error(error.to_string()),
        }
    }
}

/// Best-effort peer delivery of a just-posted message: ensure the chat node is up
/// and publish the signed envelope to the room topic. Returns whether it was
/// handed to the transport — *not* whether a peer received it. The message is
/// already recorded locally, so offline / no-peer cases never fail the post.
fn deliver_message(mem: &MemCli, options: &GuiOptions, room: &str, cid: &Cid) -> bool {
    if let Err(error) = ensure_chat_node(mem, options) {
        eprintln!("chat node unavailable: {error}");
        return false;
    }
    let Ok(env) = mem.read_message(cid) else {
        return false;
    };
    let Ok(bytes) = serde_json::to_string(&env) else {
        return false;
    };
    if let Ok(guard) = options.chat.lock() {
        if let Some(chat) = guard.as_ref() {
            let _ = chat.node.subscribe(room);
            return chat.node.publish(room, bytes.into_bytes()).is_ok();
        }
    }
    false
}

/// Found a new private network from the Data Platter (Phase N · Phase H). Returns
/// the refreshed network map.
fn mutation_network_create(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = value.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
    if name.is_empty() {
        return Response::bad_request("network name is required");
    }
    match mem.create_network(name) {
        Ok(_) => to_response(network_json(mem)),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Revoke a subject from the Data Platter (advances the epoch; remaining members
/// must be re-granted). Returns the refreshed network map.
fn mutation_network_revoke(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let subject = value.get("subject").and_then(|s| s.as_str()).unwrap_or("").trim();
    if subject.is_empty() {
        return Response::bad_request("subject id is required");
    }
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        return Response::bad_request("no network on this device");
    };
    match mem.revoke(&descriptor.network_id, subject) {
        Ok(_) => to_response(network_json(mem)),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Convert a reviewed plaintext root into a capability-encrypted private graph,
/// immediately consume the exact private-share grant, and return the read-only
/// bearer capability to the local Data Platter handoff.
fn mutation_convert_private(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let review_token = match body_str(&value, "review_token") {
        Ok(token) => token,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if value
        .get("acknowledge_private")
        .and_then(|value| value.as_bool())
        != Some(true)
    {
        return Response::bad_request(
            "private sharing requires destination and recipient acknowledgement",
        );
    }
    let plan = match options.reviewed_private_plan(review_token) {
        Some(plan) => plan,
        None => {
            return Response::bad_request(
                "private-share review expired or was not created by this Data Platter",
            )
        }
    };
    if let Err(error) = mem.create_encrypt_and_share_private_grant(&plan, password) {
        return mutation_error(&error);
    }
    match mem.convert_and_share_private(&plan, password) {
        Ok(result) => {
            options.discard_review(review_token);
            Response::json(
                serde_json::json!({
                    "converted": true,
                    "ciphertext_root": result.conversion.ciphertext_root,
                    "plaintext_root": result.conversion.plaintext_root,
                    "block_count": result.conversion.block_count,
                    "plaintext_locked": result.conversion.plaintext_locked,
                    "destination_namespace": result.conversion.destination_namespace,
                    "recipients": result.conversion.recipients,
                    "capability": result.capability,
                    "egress_receipt": result.receipt,
                })
                .to_string(),
            )
        }
        Err(error) => mutation_error(&error),
    }
}

fn parse_body(body: &str) -> Result<serde_json::Value, Response> {
    serde_json::from_str(body).map_err(|_| Response::bad_request("invalid JSON body"))
}

fn body_str<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, Response> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| Response::bad_request("missing required field"))
}

/// Directory names skipped when ingesting a folder/repo: build output and VCS
/// internals, which are noise rather than content.
const INGEST_SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", ".next", "dist", "build", ".venv", "__pycache__", ".cache",
];

/// Accumulator for a file/folder ingest: the manifest entries plus tallies.
#[derive(Default)]
struct PathIngest {
    files: usize,
    bytes: u64,
    ignored: usize,
    ignored_examples: Vec<String>,
    entries: Vec<serde_json::Value>,
}

/// Extension → media type, covering the documents/images/video/audio the user
/// is likely to ingest. Unknown types fall back to `application/octet-stream`.
fn guess_media_type_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" | "rs" | "toml" | "json" | "jsonl" | "ndjson" | "js" | "mjs"
        | "ts" | "tsx" | "jsx" | "py" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "kt"
        | "rb" | "sh" | "bash" | "zsh" | "yml" | "yaml" | "html" | "htm" | "css" | "scss" | "csv"
        | "tsv" | "log" | "xml" | "ini" | "cfg" | "conf" | "sql" | "lock" | "gitignore" => {
            "text/plain"
        }
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "heic" => "image/heic",
        "tiff" | "tif" => "image/tiff",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => "application/octet-stream",
    }
}

/// Store one file as a `blob` + `file_ref`, returning the `file_ref` CID. There
/// is no size cap — any file is ingested whole. Unreadable files (or
/// directories/special files that `read` rejects) are tallied as ignored and
/// return `None`. Note blobs are stored as JSON byte arrays (~4× on disk), so
/// very large files amplify on-disk storage accordingly.
fn ingest_one_file(
    mem: &MemCli,
    abs: &std::path::Path,
    rel: &str,
    acc: &mut PathIngest,
) -> CoreResult<Option<Cid>> {
    let bytes = match std::fs::read(abs) {
        Ok(bytes) => bytes,
        Err(_) => {
            acc.ignored += 1;
            return Ok(None);
        }
    };
    let media_type = guess_media_type_path(rel);
    let blob = mem.put_blob(&bytes, media_type)?;
    let fields = serde_json::json!({
        "path": rel,
        "size": bytes.len() as u64,
        "media_type": media_type,
        "content": cid_link(&blob)?,
    });
    let file_ref = mem.put_node(&Node {
        kind: "file_ref".to_string(),
        fields_json: fields.to_string(),
    })?;
    acc.entries.push(serde_json::json!({ "path": rel, "file_ref": cid_link(&file_ref)? }));
    acc.files += 1;
    acc.bytes += bytes.len() as u64;
    Ok(Some(file_ref))
}

/// Recursively store every regular file under `dir`, skipping symlinks and the
/// `INGEST_SKIP_DIRS` denylist. Paths in the manifest are relative to `base`.
fn walk_dir(
    mem: &MemCli,
    dir: &std::path::Path,
    base: &std::path::Path,
    acc: &mut PathIngest,
) -> CoreResult<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return Ok(()),
    };
    let mut children: Vec<_> = read.filter_map(std::result::Result::ok).collect();
    children.sort_by_key(std::fs::DirEntry::file_name);
    for entry in children {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if INGEST_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_dir(mem, &path, base, acc)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            ingest_one_file(mem, &path, &rel, acc)?;
        }
    }
    Ok(())
}

/// A stable, groupable binding name for an ingest: `import:<unix>-<basename>`.
fn import_binding_name(path: &std::path::Path) -> String {
    let base = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "import".to_string());
    let safe: String = base
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' { ch } else { '-' })
        .collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("import:{ts}-{safe}")
}

/// Ingest a file or folder from a path on disk. The GUI is loopback-only on the
/// user's own machine, so the server reads the path directly — this is what
/// makes whole repos and large media practical (no browser upload). A single
/// `.jsonl`/`.ndjson` file is treated as a harness session stream; anything else
/// is stored as content-addressed blobs under a walkable `ingest_run` root.
fn mutation_ingest_path(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let raw = match body_str(&value, "path") {
        Ok(path) => path.trim(),
        Err(response) => return response,
    };
    if raw.is_empty() {
        return Response::bad_request("provide an absolute path to a file or folder");
    }
    let path = std::path::PathBuf::from(raw);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) => return Response::bad_request(&format!("cannot access {raw}: {error}")),
    };

    if meta.is_dir() {
        let mut acc = PathIngest::default();
        if let Err(error) = walk_dir(mem, &path, &path, &mut acc) {
            return mutation_error(&error);
        }
        if acc.entries.is_empty() {
            return Response::bad_request(&format!(
                "no ingestible files under {raw} ({} ignored)",
                acc.ignored
            ));
        }
        let manifest_fields = serde_json::json!({ "root_path": raw, "entries": acc.entries });
        let manifest = match mem.put_node(&Node {
            kind: "directory_manifest".to_string(),
            fields_json: manifest_fields.to_string(),
        }) {
            Ok(cid) => cid,
            Err(error) => return mutation_error(&error),
        };
        let manifest_link = match cid_link(&manifest) {
            Ok(link) => link,
            Err(error) => return mutation_error(&error),
        };
        let run_fields = serde_json::json!({
            "source_path": raw,
            "manifest": manifest_link,
            "file_count": acc.files as u64,
            "byte_count": acc.bytes,
            "ignored_count": acc.ignored as u64,
            "plugin_records": 0,
            "plugin_failures": 0,
            "per_file_plugin_records": {},
            "per_file_plugin_failures": {},
        });
        let run = match mem.put_node(&Node {
            kind: "ingest_run".to_string(),
            fields_json: run_fields.to_string(),
        }) {
            Ok(cid) => cid,
            Err(error) => return mutation_error(&error),
        };
        let name = import_binding_name(&path);
        if let Err(error) = mem.bind(&name, &run) {
            return mutation_error(&error);
        }
        return Response::json(
            serde_json::json!({
                "ok": true, "kind": "folder", "root": run.0, "name": name,
                "files": acc.files, "bytes": acc.bytes,
                "ignored": acc.ignored, "ignored_examples": acc.ignored_examples,
            })
            .to_string(),
        );
    }

    // Single file. A JSONL/NDJSON file is a harness session stream.
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "jsonl" || ext == "ndjson" {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) => return Response::bad_request(&format!("cannot open {raw}: {error}")),
        };
        let base_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let report =
            concierge_adapter_jsonl::ingest(std::io::BufReader::new(file), mem, &base_dir);
        return Response::json(
            serde_json::json!({
                "ok": true, "kind": "session",
                "events": report.events, "nodes_written": report.nodes_written,
                "names_bound": report.names_bound, "checkpoints": report.checkpoints,
                "skipped": report.skipped.len(),
            })
            .to_string(),
        );
    }

    let rel = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| raw.to_string());
    let mut acc = PathIngest::default();
    let file_ref = match ingest_one_file(mem, &path, &rel, &mut acc) {
        Ok(Some(cid)) => cid,
        Ok(None) => {
            return Response::bad_request(&format!("skipped {raw}: unreadable"));
        }
        Err(error) => return mutation_error(&error),
    };
    let name = import_binding_name(&path);
    if let Err(error) = mem.bind(&name, &file_ref) {
        return mutation_error(&error);
    }
    Response::json(
        serde_json::json!({
            "ok": true, "kind": "file", "root": file_ref.0, "name": name,
            "files": acc.files, "bytes": acc.bytes, "ignored": acc.ignored,
        })
        .to_string(),
    )
}

/// Ingest an uploaded JSONL event stream into the store. The body is
/// `{ "content": "<jsonl text>" }`. File paths inside `file_*` events resolve
/// against the mounted store directory; missing files are skipped, never fatal.
fn mutation_ingest(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let content = match body_str(&value, "content") {
        Ok(content) => content,
        Err(response) => return response,
    };
    let base_dir = std::path::PathBuf::from(&options.store_label);
    let report =
        concierge_adapter_jsonl::ingest(std::io::BufReader::new(content.as_bytes()), mem, &base_dir);
    Response::json(
        serde_json::json!({
            "ok": true,
            "lines": report.lines,
            "events": report.events,
            "nodes_written": report.nodes_written,
            "checkpoints": report.checkpoints,
            "names_bound": report.names_bound,
            "blobs_written": report.blobs_written.len(),
            "skipped": report.skipped.len(),
        })
        .to_string(),
    )
}

fn mutation_lock(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let label = value
        .get("label")
        .and_then(|item| item.as_str())
        .unwrap_or("");

    // Bulk path: `{ "session": "<id>" }` locks every named record in one session.
    if let Some(session) = value
        .get("session")
        .and_then(|item| item.as_str())
        .filter(|session| !session.is_empty())
    {
        match mem.password_is_set() {
            Ok(true) => {}
            Ok(false) => {
                return Response::bad_request(
                    "set and confirm a store password before creating the first GUI lock",
                );
            }
            Err(error) => return mutation_error(&error),
        }
        let names = match mem.names() {
            Ok(names) => names,
            Err(error) => return mutation_error(&error),
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut locked_count = 0usize;
        for (name, cid) in names {
            if session_of(&name).as_deref() != Some(session) {
                continue;
            }
            if !seen.insert(cid.0.clone()) {
                continue;
            }
            if mem.lock_subgraph(&cid, label).is_ok() {
                locked_count += 1;
            }
        }
        return Response::json(
            serde_json::json!({
                "locked": true,
                "session": session,
                "locked_count": locked_count,
            })
            .to_string(),
        );
    }

    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.password_is_set() {
        Ok(true) => {}
        Ok(false) => {
            return Response::bad_request(
                "set and confirm a store password before creating the first GUI lock",
            );
        }
        Err(error) => return mutation_error(&error),
    }
    let plan = match mem.build_egress_plan(&root, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(error) => return mutation_error(&error),
    };
    match mem.lock_subgraph(&root, label) {
        Ok(()) => Response::json(
            serde_json::json!({
                "locked": true,
                "root": root.0,
                "reachable_node_count": plan.block_count,
                "file_count": plan.file_paths.len(),
            })
            .to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

/// Permanently lift a lock (the egress-unlock) after the store password — this is
/// what allows a previously-locked subgraph to be published/shared/exported. The
/// bulk `{ "session": "<id>" }` form lifts the lock on every record in the
/// session; the single form takes a `{ "target": "<cid|name>" }`. Locks only ever
/// guarded egress, never local viewing.
fn mutation_unlock(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };

    // Bulk path: unlock every locked record in one session.
    if let Some(session) = value
        .get("session")
        .and_then(|item| item.as_str())
        .filter(|session| !session.is_empty())
    {
        let names = match mem.names() {
            Ok(names) => names,
            Err(error) => return mutation_error(&error),
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut unlocked_count = 0usize;
        for (name, cid) in names {
            if session_of(&name).as_deref() != Some(session) {
                continue;
            }
            if !seen.insert(cid.0.clone()) {
                continue;
            }
            // A bad/rate-limited password fails the whole batch; otherwise skip
            // records that simply have no direct lock to remove.
            match mem.unlock_subgraph(&cid, password) {
                Ok(()) => unlocked_count += 1,
                Err(error @ (Error::AuthenticationFailed | Error::AuthenticationRateLimited { .. })) => {
                    return mutation_error(&error);
                }
                Err(_) => {}
            }
        }
        return Response::json(
            serde_json::json!({
                "unlocked": true,
                "session": session,
                "unlocked_count": unlocked_count,
            })
            .to_string(),
        );
    }

    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.unlock_subgraph(&root, password) {
        Ok(()) => Response::json(
            serde_json::json!({ "unlocked": true, "root": root.0 }).to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

/// Decision 0026: everything is fenced from egress by default. Clearing a root
/// is the explicit, password-gated exception that lets it be published / shared /
/// exported. Takes `{ "target": "<cid|name>", "password": "…", "label"?: "…" }`.
/// The password is read straight into the core call and never echoed.
fn mutation_clear_for_egress(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let label = value
        .get("label")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    let plan = match mem.build_egress_plan(&root, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(error) => return mutation_error(&error),
    };
    match mem.clear_for_egress(&root, label, password) {
        Ok(()) => Response::json(
            serde_json::json!({
                "cleared": true,
                "root": root.0,
                "reachable_node_count": plan.block_count,
                "file_count": plan.file_paths.len(),
            })
            .to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

/// Restore the default fence on a previously-cleared root (the safe direction —
/// no password needed to make data *more* private). Takes `{ "target": "<cid|name>" }`.
fn mutation_refence(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.refence(&root) {
        Ok(()) => Response::json(
            serde_json::json!({ "refenced": true, "root": root.0 }).to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

fn mutation_set_password(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let confirm_password = match body_str(&value, "confirm_password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if password != confirm_password {
        return Response::bad_request("password confirmation does not match");
    }
    match mem.set_password(password) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => mutation_error(&error),
    }
}

/// Publish exactly the short-lived server-cached plan identified by the review
/// drawer token. Locked plans mint and immediately consume a one-shot grant;
/// clear plans still require the store password and the same exact-plan check.
fn mutation_authorize_publish(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if value
        .get("acknowledge_irreversible")
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        return Response::bad_request("publication requires an irreversibility acknowledgement");
    }
    let review_token = match body_str(&value, "review_token") {
        Ok(token) => token,
        Err(response) => return response,
    };
    let plan = match options.reviewed_plan(review_token) {
        Some(plan) => plan,
        None => {
            return Response::bad_request("review expired or was not created by this Data Platter")
        }
    };
    if plan.operation != EgressOperation::PublicPublish {
        return Response::bad_request("reviewed plan is not a public publication");
    }
    let authorization = if plan.is_blocked() {
        mem.create_publish_grant(&plan, password).map(|_| ())
    } else {
        mem.verify_password(password)
    };
    if let Err(error) = authorization {
        return mutation_error(&error);
    }
    match mem.publish_public(&plan) {
        Ok(receipt) => {
            options.discard_review(review_token);
            Response::json(
                serde_json::json!({
                    "published": true,
                    "root": receipt.root,
                    "backend": receipt.backend,
                    "gateway_url": receipt.gateway_url,
                    "authorization_consumed": true,
                })
                .to_string(),
            )
        }
        Err(error) => mutation_error(&error),
    }
}

fn resolve_target_string(mem: &MemCli, target: &str) -> CoreResult<Cid> {
    if target.parse::<cid::Cid>().is_ok() {
        Ok(Cid(target.to_string()))
    } else {
        mem.resolve(target)
    }
}

/// Map a core error to an HTTP status, never leaking secret material.
fn mutation_error(error: &Error) -> Response {
    let (status, message): (u16, String) = match error {
        Error::AuthenticationFailed => (401, "store password authentication failed".to_string()),
        Error::AuthenticationRateLimited { retry_after_secs } => (
            429,
            format!("authentication rate limited; retry in {retry_after_secs}s"),
        ),
        Error::PublicationBlocked { .. }
        | Error::SensitiveContentBlocked { .. }
        | Error::SecurityPolicy(_)
        | Error::GrantIntegrity(_)
        | Error::ExplicitPublicPublishRequired => (403, error.to_string()),
        Error::EgressPlanChanged(_) => (409, error.to_string()),
        // A closed/wrong-password vault surfaces as an encryption error; treat it
        // as forbidden rather than a server fault.
        Error::Encryption(_) => (403, error.to_string()),
        Error::NameUnbound(_) | Error::CidNotFound(_) | Error::Tombstoned(_) => {
            (404, error.to_string())
        }
        Error::BackendDown(_) => (502, error.to_string()),
        Error::Unsupported { .. } => (400, error.to_string()),
        _ => (500, error.to_string()),
    };
    Response::json_error(status, &message)
}

/// Start the explorer server on `addr` (blocking).
pub fn serve(mem: MemCli, addr: &str) -> CoreResult<()> {
    serve_with_options(mem, addr, GuiOptions::default())
}

/// Start the explorer with harness display metadata and optional browser open.
pub fn serve_with_options(mem: MemCli, addr: &str, options: GuiOptions) -> CoreResult<()> {
    mem.clear_all_grants()?;
    // Mint a fresh CSRF token for this process; privacy mutations require it.
    let mut options = options;
    options.csrf_token = new_csrf_token();
    let listener =
        TcpListener::bind(addr).map_err(|error| Error::Io(format!("gui bind {addr}: {error}")))?;
    // Record {pid, port} so a second mount of the same store reuses this server
    // instead of spawning a duplicate (the uniform "auto-open" contract).
    if let Ok(local) = listener.local_addr() {
        write_gui_lock(&mem, local.port());
    }
    if options.open_browser {
        let _ = open_browser(&format!("http://{addr}"));
    }
    
    if let Some(pid) = options.watch_pid {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                #[cfg(unix)]
                let alive = unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) };
                #[cfg(not(unix))]
                let alive = true;
                if !alive { std::process::exit(0); }
            }
        });
    }

    // Phase C: while the app is open, continuously capture Claude Code sessions —
    // but only once the user has explicitly attached (consent-gated, opt-in).
    spawn_claude_code_capture(mem.clone());

    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let mem = mem.clone();
        let options = options.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(&mem, &options, stream);
        });
    }
    Ok(())
}

// ── Phase C: Claude Code auto-capture (Decision 0013) ───────────────────────
//
// Capture is opt-in and consent-gated: a sentinel file under the store records
// that the user attached. While attached, a low-priority background loop ingests
// any newly-appended transcript lines across `~/.claude/projects` (the first pass
// backfills the whole history, then it tails). Ingest is content-addressed, so
// re-reads dedupe by CID — it is safe to run on a short interval.

/// The consent sentinel: its presence means "attached / capturing".
fn capture_flag_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir().ok().map(|dir| dir.join("capture-claude-code"))
}

fn claude_code_attached(mem: &MemCli) -> bool {
    capture_flag_path(mem)
        .map(|path| path.exists())
        .unwrap_or(false)
}

fn set_claude_code_attached(mem: &MemCli, attached: bool) -> std::io::Result<()> {
    let Some(path) = capture_flag_path(mem) else {
        return Ok(());
    };
    if attached {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, b"attached")
    } else if path.exists() {
        std::fs::remove_file(&path)
    } else {
        Ok(())
    }
}

/// Where per-file ingest offsets are persisted across relaunches, so a restart
/// resumes the tail instead of re-scanning every session from byte 0.
fn capture_offsets_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir().ok().map(|dir| dir.join("capture-offsets.json"))
}

fn load_capture_offsets(mem: &MemCli) -> std::collections::HashMap<std::path::PathBuf, u64> {
    let Some(path) = capture_offsets_path(mem) else {
        return Default::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    serde_json::from_str::<std::collections::BTreeMap<String, u64>>(&text)
        .map(|map| {
            map.into_iter()
                .map(|(k, v)| (std::path::PathBuf::from(k), v))
                .collect()
        })
        .unwrap_or_default()
}

fn save_capture_offsets(mem: &MemCli, offsets: &std::collections::HashMap<std::path::PathBuf, u64>) {
    let Some(path) = capture_offsets_path(mem) else {
        return;
    };
    let map: std::collections::BTreeMap<String, u64> = offsets
        .iter()
        .map(|(k, v)| (k.display().to_string(), *v))
        .collect();
    if let Ok(text) = serde_json::to_string(&map) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, text);
    }
}

/// Spawn the continuous capture loop. No-op if `~/.claude/projects` can't be
/// resolved. Ticks every few seconds; does work only while attached, so an
/// un-attached node costs a stat per tick and nothing else. Per-file offsets are
/// loaded at start and persisted whenever they advance, so a relaunch resumes the
/// tail rather than re-scanning the whole history.
fn spawn_claude_code_capture(mem: MemCli) {
    use concierge_adapter_claude_code::{capture_once, discovery};
    let Some(projects_dir) = discovery::claude_projects_dir() else {
        return;
    };
    let base = mem.working_dir().to_path_buf();
    std::thread::spawn(move || {
        let mut offsets = load_capture_offsets(&mem);
        loop {
            if claude_code_attached(&mem) {
                let ingested = capture_once(&projects_dir, &mut offsets, &mem, &base);
                if ingested > 0 {
                    save_capture_offsets(&mem, &offsets);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}

/// Sidekick/node status for the Data Platter (the embedding model + its private
/// Kubo node, coupled). Read-only.
fn sidekick_status_json(mem: &MemCli) -> CoreResult<String> {
    let status = mem.sidekick_status();
    serde_json::to_string(&status).map_err(|e| Error::Io(format!("serialize sidekick status: {e}")))
}

/// Enable/disable the Sidekick (launches/winds down the private Kubo node).
/// CSRF-gated like every mutation; the password is not involved (local-only).
fn mutation_sidekick(mem: &MemCli, enable: bool) -> Response {
    let result = if enable {
        mem.enable_sidekick()
    } else {
        mem.disable_sidekick()
    };
    match result {
        Ok(status) => match serde_json::to_string(&status) {
            Ok(body) => Response::json(body),
            Err(e) => Response::error(e.to_string()),
        },
        Err(error) => Response::error(error.to_string()),
    }
}

/// Read-only onboarding/status: are Claude Code sessions present, and are we
/// attached? Drives the first-run "Found N sessions — attach?" card.
fn claude_code_status_json(mem: &MemCli) -> CoreResult<String> {
    use concierge_adapter_claude_code::discovery;
    let projects_dir = discovery::claude_projects_dir();
    let sessions = projects_dir
        .as_ref()
        .map(|dir| discovery::enumerate_sessions(dir))
        .unwrap_or_default();
    // How many of those sessions belong to *this* project (the launch cwd) — a
    // hint so the banner can foreground the most relevant ones. Capture still
    // covers the whole projects dir (Decision 0013); this only sharpens the copy.
    let current_slug = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.to_str().map(discovery::slug_for_path));
    let current_project_sessions = current_slug
        .as_ref()
        .map(|slug| sessions.iter().filter(|s| &s.project_slug == slug).count())
        .unwrap_or(0);
    Ok(serde_json::json!({
        "available": projects_dir.is_some(),
        "session_count": sessions.len(),
        "current_project_sessions": current_project_sessions,
        "attached": claude_code_attached(mem),
        "projects_dir": projects_dir.map(|p| p.display().to_string()),
    })
    .to_string())
}

/// Attach/detach capture (consent). CSRF-gated like every mutation; the password
/// is not involved — capture is local-only and writes only to the user's store.
fn mutation_claude_code_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_claude_code_attached(mem, attached) {
        return Response::error(format!("could not update capture state: {error}"));
    }
    match claude_code_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

/// The lockfile recording the Data Platter serving this store: `.concierge/gui.json`.
fn gui_lock_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir().ok().map(|dir| dir.join("gui.json"))
}

fn write_gui_lock(mem: &MemCli, port: u16) {
    let Some(path) = gui_lock_path(mem) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "pid": std::process::id(), "port": port }).to_string();
    let _ = std::fs::write(path, body);
}

/// If a Data Platter is already serving this store, its port. Verified by
/// probing the recorded port for *our* server (a `/api/meta` that returns a
/// `csrf_token`), so a stale lockfile or an unrelated app never matches.
pub fn running_gui_port(mem: &MemCli) -> Option<u16> {
    let path = gui_lock_path(mem)?;
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    if probe_is_concierge_gui(port) {
        Some(port)
    } else {
        None
    }
}

fn probe_is_concierge_gui(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut stream) = TcpStream::connect(&addr) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /api/meta HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.contains("csrf_token")
}

/// Pick a bindable loopback port, starting at `preferred` and scanning upward a
/// little. Returns `preferred` if nothing is free (the caller's bind then errors
/// with a clear message).
pub fn pick_free_port(preferred: u16) -> u16 {
    for candidate in preferred..preferred.saturating_add(16) {
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
    preferred
}

/// Open the explorer URL with the platform's default browser.
pub fn open_browser(url: &str) -> CoreResult<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| Error::Io(format!("open browser: {error}")))
}

fn serve_connection(
    mem: &MemCli,
    options: &GuiOptions,
    mut stream: TcpStream,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let request = match read_request(&mut stream)? {
        RequestOutcome::Parsed(request) => request,
        RequestOutcome::Empty => {
            return write_response(&mut stream, Response::bad_request("empty request"));
        }
        RequestOutcome::HeadersTooLarge => {
            return write_response(&mut stream, Response::header_too_large());
        }
        RequestOutcome::BodyTooLarge => {
            return write_response(&mut stream, Response::payload_too_large());
        }
    };
    let response = route_request(mem, options, &request);
    write_response(&mut stream, response)
}

/// Apply the loopback gate, then route reads (`GET`) or privacy mutations
/// (`POST`). The single seam both the socket loop and tests go through.
fn route_request(mem: &MemCli, options: &GuiOptions, request: &ParsedRequest) -> Response {
    if let Some(rejection) = loopback_gate(request, &options.csrf_token) {
        return rejection;
    }
    if request.method == "POST" && !options.allow_mutation() {
        return Response::too_many_requests();
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    match request.method.as_str() {
        "GET" => handle_with_options(mem, options, path, query),
        "POST" => handle_mutation(mem, options, path, &request.body),
        _ => Response::method_not_allowed(),
    }
}

/// The result of reading one request off the socket.
enum RequestOutcome {
    Parsed(ParsedRequest),
    Empty,
    HeadersTooLarge,
    BodyTooLarge,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<RequestOutcome> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES {
            return Ok(RequestOutcome::HeadersTooLarge);
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break Some(index + 4);
        }
    };
    if bytes.is_empty() {
        return Ok(RequestOutcome::Empty);
    }
    let Some(header_end) = header_end else {
        return Ok(RequestOutcome::HeadersTooLarge);
    };

    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let mut lines = header_text.lines();
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_string();
    let target = request_line.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    // Read the body (if any). Ingest uploads get a larger budget than the small
    // control mutations.
    let body_limit = if target.split('?').next() == Some("/api/ingest") {
        MAX_INGEST_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    };
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > body_limit {
        return Ok(RequestOutcome::BodyTooLarge);
    }
    let mut body = bytes[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > body_limit {
            return Ok(RequestOutcome::BodyTooLarge);
        }
    }
    body.truncate(content_length);
    let body = String::from_utf8(body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;

    Ok(RequestOutcome::Parsed(ParsedRequest {
        method,
        target,
        headers,
        body,
    }))
}

fn write_response(stream: &mut TcpStream, response: Response) -> std::io::Result<()> {
    // Blob assets may be framed same-origin so PDFs render in an <iframe>; every
    // other response keeps the strict deny-all framing posture.
    let (frame_options, frame_ancestors) = if response.embeddable {
        ("SAMEORIGIN", "'self'")
    } else {
        ("DENY", "'none'")
    };
    let content_type = response
        .content_type_owned
        .as_deref()
        .unwrap_or(response.content_type);
    let mut header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: {}\r\nContent-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors {}; object-src 'none'; base-uri 'none'; form-action 'self'\r\n",
        response.status,
        reason_phrase(response.status),
        content_type,
        response.body.len(),
        frame_options,
        frame_ancestors,
    );
    for (name, value) in response.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{cid_link, CoreBinding, GcPolicy, Node};
    use std::io::{Read, Write};
    use std::path::Path;

    fn store() -> (tempfile::TempDir, MemCli) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = MemCli::new(dir.path());
        (dir, mem)
    }

    fn body(response: &Response) -> String {
        String::from_utf8_lossy(&response.body).into_owned()
    }

    fn put_named(mem: &MemCli, name: &str, text: &str) -> Cid {
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({ "text": text, "kind": "project" }).to_string(),
            })
            .expect("put");
        mem.bind(name, &cid).expect("bind");
        cid
    }

    fn configure_fake_ipfs_backend(
        mem: &MemCli,
        dir: &Path,
        expected_requests: usize,
    ) -> std::thread::JoinHandle<()> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");
        let join = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buf).expect("read request");
                    request.extend_from_slice(&buf[..read]);
                    if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let headers_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("headers end")
                    + 4;
                let header_text = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(key, value)| {
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .expect("content length");
                let remaining = content_length.saturating_sub(request.len() - headers_end);
                if remaining > 0 {
                    let mut body = vec![0u8; remaining];
                    stream.read_exact(&mut body).expect("read body");
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .expect("write response");
            }
        });
        let mut config = mem.config().expect("config");
        config.publishing.backend = "ipfs".to_string();
        config.publishing.ipfs_api = api_url;
        config.save_to_project_root(dir).expect("save config");
        join
    }

    #[test]
    fn index_is_the_safe_live_explorer_shell() {
        let (_dir, mem) = store();
        let response = handle(&mem, "/", "");
        let page = body(&response);
        assert_eq!(response.status, 200);
        assert!(page.contains("The Data Platter"));
        assert!(page.contains("Export-CAR"));
        assert!(
            !page.contains("innerHTML"),
            "store data must never enter innerHTML"
        );
    }

    #[test]
    fn names_record_and_meta_endpoints_return_live_data() {
        let (_dir, mem) = store();
        let cid = put_named(&mem, "latest", "<img src=x onerror=alert(1)>");
        let names = body(&handle(&mem, "/api/names", ""));
        let record = body(&handle(&mem, "/api/record", "name=latest"));
        let options = GuiOptions::new("hermes-model".to_string(), "/tmp/store".to_string(), false, None);
        let meta = body(&handle_with_options(&mem, &options, "/api/meta", ""));
        assert!(names.contains("latest"));
        // The Names timeline needs a date, a kind, and a human description per
        // binding — not just the raw name/CID — to fold records by month/day.
        let names_value: serde_json::Value = serde_json::from_str(&names).expect("names json");
        let entry = &names_value.as_array().expect("array")[0];
        assert!(entry.get("created_at").and_then(|v| v.as_u64()).unwrap() > 0);
        assert_eq!(entry.get("kind").and_then(|v| v.as_str()), Some("memory"));
        assert!(entry
            .get("preview")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("<img src=x onerror=alert(1)>"));
        assert!(record.contains(&cid.0));
        assert!(record.contains("<img src=x onerror=alert(1)>"));
        assert!(meta.contains("hermes-model"));
        assert!(meta.contains("/tmp/store"));
    }

    #[test]
    fn a_fence_is_an_egress_badge_not_a_local_view_hider() {
        let (_dir, mem) = store();
        let cid = put_named(&mem, "secret", "sensitive text");
        let record = mem.get(&CidOrName::Cid(cid.clone())).unwrap();

        // Decision 0026: fenced from egress by default, nothing cleared.
        let privacy = PrivacyOverlay {
            cleared_roots: BTreeSet::new(),
            cleared_cids: BTreeSet::new(),
            known_public: BTreeSet::new(),
            quarantined: BTreeSet::new(),
        };
        let (node, _) =
            node_and_links_from_record(&mem, &privacy, &BTreeSet::new(), &cid, &record);

        // A fence is an EGRESS safeguard, not a local-view control — the user sees
        // their own data on their own device. So content + metadata are fully
        // visible locally …
        assert_eq!(node["preview"], "sensitive text");
        assert_ne!(node["kind"], "locked", "real kind shown locally");
        assert!(node["created_at"].as_i64().is_some(), "timestamp visible");
        // … and the fence surfaces only as a badge (the default, not cleared).
        assert_eq!(node["fenced"], true);
        assert_eq!(node["cleared"], false);
    }

    #[test]
    fn forest_groups_sessions_into_calendar_tiers() {
        let (_dir, mem) = store();
        // Two session-named events under one session. The forest groups by
        // session (store → year → month → day → session), not by record.
        let e1 = put_named(&mem, "host:test:session:S1:event:E1", "first");
        let e2 = put_named(&mem, "host:test:session:S1:event:E2", "second");

        let forest = body(&handle(&mem, "/api/graph", ""));
        let today = concierge_core::utc_today();
        let year = &today[0..4];
        let month = &today[0..7];

        assert!(forest.contains("\"cid\":\"store:root\""));
        assert!(forest.contains(&format!("year:{year}")), "year tier present");
        assert!(forest.contains(&format!("month:{month}")), "month tier present");
        assert!(forest.contains(&format!("day:{today}")), "day tier present");
        assert!(forest.contains("\"relation\":\"year\""));
        assert!(forest.contains("\"relation\":\"day\""));
        // The leaf is the SESSION, not the individual records.
        assert!(forest.contains("\"relation\":\"session\""), "session relation");
        assert!(forest.contains("\"kind\":\"session\""), "session leaf present");
        // Individual event records are not drawn — the Records tab goes deeper.
        assert!(
            !forest.contains(&e1.0) && !forest.contains(&e2.0),
            "records are not drawn as graph leaves"
        );
    }

    #[test]
    fn graph_checkpoint_stats_and_guarded_car_preview_cover_the_plan_views() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "root", "explore me");
        let public = put_named(&mem, "public", "safe export");
        let checkpoint = mem.checkpoint("head", &root, None).expect("checkpoint");
        mem.bind("latest", &checkpoint).expect("latest");
        mem.set_password("pw").expect("password");

        let graph = body(&handle(&mem, "/api/graph", "name=latest"));
        let checkpoints = body(&handle(&mem, "/api/checkpoints", ""));
        let stats = body(&handle(&mem, "/api/stats", "name=latest"));
        let public_plan = mem
            .build_egress_plan_for_target_and_backend(
                "public",
                concierge_core::EgressOperation::PlaintextCarExport,
                "browser-download",
                "browser-download",
                "plaintext-portable",
            )
            .expect("public plan");
        let plan_response = handle(&mem, "/api/egress-plan", "name=public");
        let locked_car = handle(&mem, "/api/export-car", "name=latest");
        let car = handle(&mem, "/api/export-car", "name=public");
        let unreviewed = handle(&mem, "/api/export-car", "name=public");
        let missing_target = handle(&mem, "/api/export-car", "");

        assert!(graph.contains(&checkpoint.0));
        assert!(graph.contains(&root.0));
        assert!(graph.contains("checkpoint_root"));
        assert!(checkpoints.contains("\"label\":\"head\""));
        assert!(stats.contains("\"car_size\":"));
        assert!(stats.contains("\"pin_status\":"));
        // Phase B: stats always reports publishing readiness (opt-in signal).
        assert!(stats.contains("\"publishing_ready\":"));
        assert_eq!(locked_car.status, 400);
        assert_eq!(missing_target.status, 400);
        assert_eq!(unreviewed.status, 400);
        assert_eq!(plan_response.status, 200);
        assert!(body(&plan_response).contains(&public_plan.manifest_digest));
        assert!(body(&plan_response).contains("\"review_token\":"));
        assert_eq!(car.status, 400);
        assert_ne!(public, checkpoint);
    }

    #[test]
    fn publishing_reads_as_opt_in_when_no_node_is_running() {
        // Phase B: an absent publishing node is a normal "not set up yet" state,
        // surfaced as opt-in guidance — never a startup failure or error status.
        let (dir, mem) = store();
        // Pin the backend at a guaranteed-closed local port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let dead_port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = mem.config().expect("config");
        config.publishing.ipfs_api = format!("http://127.0.0.1:{dead_port}/api/v0");
        config.save_to_project_root(dir.path()).expect("save config");

        let response = handle(&mem, "/api/stats", "");
        assert_eq!(response.status, 200, "stats must never fail when the node is down");
        let stats = body(&response);
        assert!(stats.contains("\"publishing_ready\":false"));
        assert!(stats.contains("\"reachable\":false"));
        assert!(stats.contains("publishing is optional"));
    }

    #[test]
    fn network_map_surfaces_membership_capabilities_and_revocation() {
        let (_dir, mem) = store();
        let opts = GuiOptions::default();

        // Empty before any network exists.
        assert!(body(&handle(&mem, "/api/network", "")).contains("\"networks\":[]"));

        // Found a network from the Data Platter (no CLI).
        let created = handle_mutation(&mem, &opts, "/api/network/create", r#"{"name":"research-team"}"#);
        assert_eq!(created.status, 200);
        let map = body(&created);
        assert!(map.contains("research-team"));
        assert!(map.contains("\"is_root\":true"), "this device founded it");
        assert!(map.contains("\"membership_epoch\":0"));
        assert!(map.contains("\"descriptor_valid\":true"));
        assert!(map.contains("\"valid\":true"), "the founding device's membership/capabilities verify");

        // Revoke a subject → the epoch advances and the subject is listed revoked.
        let subject = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa7777bbbb8888";
        let revoked = handle_mutation(&mem, &opts, "/api/network/revoke", &format!(r#"{{"subject":"{subject}"}}"#));
        assert_eq!(revoked.status, 200);
        let after = body(&revoked);
        assert!(after.contains("\"membership_epoch\":1"), "revocation advanced the epoch");
        assert!(after.contains(subject), "the revoked subject is surfaced in the map");
    }

    #[test]
    fn network_rotate_requires_the_ciphertext_root_and_password_in_the_body() {
        // The rotation crypto is proven in core; here we check the endpoint guards
        // its required fields and never takes the password in the URL.
        let (_dir, mem) = store();
        let opts = GuiOptions::default();
        assert_eq!(handle_mutation(&mem, &opts, "/api/network/rotate", r#"{"password":"pw"}"#).status, 400);
        assert_eq!(handle_mutation(&mem, &opts, "/api/network/rotate", r#"{"ciphertext_root":"bafyX"}"#).status, 400);
        // Well-formed but unknown root → a clean error, not a panic.
        let resp = handle_mutation(&mem, &opts, "/api/network/rotate", r#"{"ciphertext_root":"bafyUNKNOWN","password":"pw"}"#);
        assert_ne!(resp.status, 200);
    }

    #[test]
    fn tombstone_record_returns_a_receipt_instead_of_an_error() {
        let (_dir, mem) = store();
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"orphan","kind":"project"}"#.to_string(),
            })
            .expect("put");
        mem.gc(&GcPolicy {
            keep_checkpoints: Some(0),
        })
        .expect("gc");
        let response = handle(&mem, "/api/record", &format!("cid={}", cid.0));
        assert_eq!(response.status, 200);
        assert!(body(&response).contains("\"live\":false"));
    }

    #[test]
    fn thread_endpoint_includes_policy_participants_and_hidden_cids() {
        let (_dir, mem) = store();
        let cid = mem
            .post_message("conservation", "protect the wetlands")
            .expect("post");
        let response = handle(&mem, "/api/thread", "room=conservation");
        let text = body(&response);
        assert!(text.contains("protect the wetlands"));
        assert!(text.contains(&cid.0));
        assert!(text.contains("\"ai_send\":\"on\""));
        // Moderator badge data (Phase 8 §3/§4): Guardian status + synthesis flag.
        assert!(text.contains("\"guardian\":\"active\""), "Guardian badge present");
        assert!(text.contains("\"synthesis_candidate\":false"), "short thread is not a candidate");
        assert!(text.contains("\"message_count\":1"));
        // Phase N · Phase I — social legibility: a self-authored message is `Local`,
        // carries a structural-importance count, and the follow-lens flag.
        assert!(text.contains("\"trust_tier\":\"local\""), "own message is the Local tier");
        assert!(text.contains("\"trust_label\":\"Local\""));
        assert!(text.contains("\"importance\":0"), "an orphan message ties nothing together yet");
        assert!(text.contains("\"followed\":false"));
    }

    #[test]
    fn malformed_unicode_query_never_panics() {
        let (_dir, mem) = store();
        let response = handle(&mem, "/api/record", "name=%a%C3%A9");
        assert_eq!(response.status, 500);
    }

    #[test]
    fn missing_parameters_and_unknown_paths_have_specific_statuses() {
        let (_dir, mem) = store();
        assert_eq!(handle(&mem, "/api/record", "").status, 400);
        assert_eq!(handle(&mem, "/api/thread", "").status, 400);
        assert_eq!(handle(&mem, "/nope", "").status, 404);
    }

    #[test]
    fn socket_responses_use_correct_reason_phrases_and_bound_headers() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            serve_connection(&mem, &options, stream).expect("serve");
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .write_all(b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        server.join().expect("join");
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("frame-ancestors 'none'"));
        assert!(response.contains("X-Frame-Options: DENY"));
        assert!(response.contains("Cache-Control: no-store"));
        assert!(!response.contains("Access-Control-Allow-Origin"));
        assert!(!response.contains("Set-Cookie"));
    }

    #[test]
    fn oversized_headers_receive_431_without_unbounded_reads() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            serve_connection(&mem, &options, stream).expect("serve");
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        let request = format!(
            "GET / HTTP/1.1\r\nX-Large: {}\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        client.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        server.join().expect("join");
        assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    }

    // ---- Phase D: loopback gate + privacy mutations --------------------------

    fn options_with_csrf(token: &str) -> GuiOptions {
        GuiOptions {
            csrf_token: token.to_string(),
            ..GuiOptions::default()
        }
    }

    fn post(
        path: &str,
        body: &str,
        host: Option<&str>,
        origin: Option<&str>,
        csrf: Option<&str>,
    ) -> ParsedRequest {
        let mut headers = HashMap::new();
        if let Some(host) = host {
            headers.insert("host".to_string(), host.to_string());
        }
        if let Some(origin) = origin {
            headers.insert("origin".to_string(), origin.to_string());
        }
        if let Some(csrf) = csrf {
            headers.insert("x-csrf-token".to_string(), csrf.to_string());
        }
        headers.insert("content-type".to_string(), "application/json".to_string());
        ParsedRequest {
            method: "POST".to_string(),
            target: path.to_string(),
            headers,
            body: body.to_string(),
        }
    }

    #[test]
    fn semantic_search_returns_ranked_hits_for_a_query() {
        let (_dir, mem) = store();
        let rustdoc = put_named(&mem, "rustdoc", "the rust borrow checker enforces ownership and lifetimes");
        put_named(&mem, "cooking", "sourdough fermentation needs a live starter and time");
        let body = body(&handle(&mem, "/api/search", "q=rust%20ownership&budget=2000&depth=summary"));
        assert!(body.contains("\"indexed\":"), "reports index size");
        assert!(body.contains("\"items\":"), "returns a ranked item list");
        assert!(
            body.contains(&rustdoc.0),
            "the rust node is retrieved for a rust query: {body}"
        );
    }

    #[test]
    fn semantic_search_requires_a_query() {
        let (_dir, mem) = store();
        assert_eq!(handle(&mem, "/api/search", "q=").status, 400);
        assert_eq!(handle(&mem, "/api/search", "").status, 400);
    }

    #[test]
    fn claude_code_capture_is_opt_in_and_toggles_on_attach() {
        let (_dir, mem) = store();
        // Phase C: capture is consent-gated — off until the user attaches.
        assert!(!claude_code_attached(&mem));
        let status = body(&handle(&mem, "/api/claude-code/status", ""));
        assert!(status.contains("\"attached\":false"));

        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let attach = route_request(
            &mem,
            &options,
            &post("/api/claude-code/attach", "{}", local, origin, Some("tok")),
        );
        assert_eq!(attach.status, 200, "{}", body(&attach));
        assert!(body(&attach).contains("\"attached\":true"));
        assert!(claude_code_attached(&mem), "consent persisted to the store");
        assert!(body(&handle(&mem, "/api/claude-code/status", "")).contains("\"attached\":true"));

        // Detaching is the safe direction; no password needed.
        let detach = route_request(
            &mem,
            &options,
            &post("/api/claude-code/detach", "{}", local, origin, Some("tok")),
        );
        assert!(body(&detach).contains("\"attached\":false"));
        assert!(!claude_code_attached(&mem));
    }

    #[test]
    fn capture_offsets_persist_across_relaunch() {
        let (_dir, mem) = store();
        // No file yet → empty.
        assert!(load_capture_offsets(&mem).is_empty());
        // Save a couple of offsets, then load them back (simulating a relaunch).
        let mut offsets = std::collections::HashMap::new();
        offsets.insert(std::path::PathBuf::from("/p/a.jsonl"), 128u64);
        offsets.insert(std::path::PathBuf::from("/p/b.jsonl"), 4096u64);
        save_capture_offsets(&mem, &offsets);
        let reloaded = load_capture_offsets(&mem);
        assert_eq!(reloaded.get(std::path::Path::new("/p/a.jsonl")), Some(&128));
        assert_eq!(reloaded.get(std::path::Path::new("/p/b.jsonl")), Some(&4096));
    }

    #[test]
    fn status_reports_current_project_session_count() {
        let (_dir, mem) = store();
        // The field is always present so the banner can foreground this project.
        let status = body(&handle(&mem, "/api/claude-code/status", ""));
        assert!(status.contains("\"current_project_sessions\":"));
        assert!(status.contains("\"session_count\":"));
    }

    #[test]
    fn claude_code_attach_requires_csrf_like_every_mutation() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        let no_token = route_request(
            &mem,
            &options,
            &post(
                "/api/claude-code/attach",
                "{}",
                Some("127.0.0.1"),
                Some("http://127.0.0.1"),
                None,
            ),
        );
        assert_eq!(no_token.status, 403, "attach must be CSRF-gated");
        assert!(!claude_code_attached(&mem));
    }

    #[test]
    fn loopback_gate_blocks_cross_site_missing_csrf_and_bad_host() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "lock me");
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let body = r#"{"target":"latest"}"#;
        let local = Some("127.0.0.1:4173");
        let local_origin = Some("http://127.0.0.1:4173");

        // A fully valid same-origin request with the CSRF token locks the root.
        let ok = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, Some("tok")),
        );
        assert_eq!(ok.status, 200, "valid same-origin POST should lock");

        // Each missing/forged credential is forbidden.
        let no_csrf = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, None),
        );
        let bad_csrf = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, Some("nope")),
        );
        let cross_origin = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                body,
                local,
                Some("http://evil.example"),
                Some("tok"),
            ),
        );
        let rebinding_host = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                body,
                Some("evil.example"),
                local_origin,
                Some("tok"),
            ),
        );
        for blocked in [&no_csrf, &bad_csrf, &cross_origin, &rebinding_host] {
            assert_eq!(blocked.status, 403, "credential check must forbid");
        }
    }

    #[test]
    fn get_cannot_reach_a_mutation_route_and_rebinding_host_is_forbidden() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        // GET on a mutation path is simply not a route (read router owns GET).
        let mut get_lock = post("/api/lock", "", Some("127.0.0.1"), None, None);
        get_lock.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &get_lock).status, 404);

        // A read with a non-loopback Host (DNS rebinding) is forbidden.
        let mut rebinding = post("/", "", Some("attacker.example"), None, None);
        rebinding.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &rebinding).status, 403);

        let mut missing_host = post("/", "", None, None, None);
        missing_host.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &missing_host).status, 403);
    }

    #[test]
    fn mutations_are_disabled_when_no_csrf_token_is_configured() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "x");
        // Default options have an empty token => every POST is refused.
        let options = GuiOptions::default();
        let request = post(
            "/api/lock",
            r#"{"target":"latest"}"#,
            Some("127.0.0.1"),
            Some("http://127.0.0.1"),
            Some(""),
        );
        assert_eq!(route_request(&mem, &options, &request).status, 403);
    }

    #[test]
    fn password_is_never_echoed_by_mutation_responses() {
        let (_dir, mem) = store();
        let fenced = put_named(&mem, "fenced", "x");
        mem.lock_subgraph(&fenced, "fence").expect("lock");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");
        let secret = "hunter2-very-secret";

        let set = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                &format!(r#"{{"password":"{secret}","confirm_password":"{secret}"}}"#),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(set.status, 200);
        assert!(
            !body(&set).contains(secret),
            "set-password must not echo the password"
        );

        // A wrong-password egress-unlock attempt fails 401 and never reflects the input.
        let wrong = route_request(
            &mem,
            &options,
            &post(
                "/api/unlock",
                r#"{"target":"fenced","password":"WRONG"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong.status, 401);
        assert!(!body(&wrong).contains("WRONG"));
    }

    #[test]
    fn password_confirmation_and_first_gui_lock_fail_closed() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "lock me");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let premature_lock = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                r#"{"target":"latest"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(premature_lock.status, 400);
        assert!(mem.locks().expect("locks").is_empty());

        let mismatch = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                r#"{"password":"one","confirm_password":"two"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(mismatch.status, 400);
        assert!(!mem.password_is_set().expect("password state"));
    }

    #[test]
    fn authorize_publish_requires_acknowledgement_then_password() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "secret", "classified");
        mem.lock_subgraph(&root, "private").expect("lock");
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");
        let plan = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .unwrap();
        let review_token = options.cache_review(plan.clone()).expect("cache review");

        // No acknowledgement => 400 before any password handling.
        let no_ack_body = serde_json::json!({
            "review_token": &review_token,
            "password": "pw",
        })
        .to_string();
        let no_ack = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &no_ack_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(no_ack.status, 400);

        // Acknowledged but wrong password => 401, no grant minted, still blocked.
        let wrong_body = serde_json::json!({
            "review_token": &review_token,
            "password": "WRONG",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let wrong_pw = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &wrong_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong_pw.status, 401);
        // No grant was minted by the failed attempt: the root is still blocked.
        let plan = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .unwrap();
        assert!(matches!(
            mem.publish_public(&plan),
            Err(Error::PublicationBlocked { .. })
        ));
    }

    #[test]
    fn the_fence_badges_a_subgraph_for_egress_without_hiding_local_view() {
        let (_dir, mem) = store();
        let content = mem
            .put_blob(b"hidden-body-value", "text/plain")
            .expect("put blob");
        let secret = mem
            .put_node(&Node {
                kind: "file_ref".to_string(),
                fields_json: serde_json::json!({
                    "path": "docs/notes.txt",
                    "size": 17,
                    "content": cid_link(&content).expect("content link"),
                })
                .to_string(),
            })
            .expect("put");
        mem.bind("secret", &secret).expect("bind");
        let checkpoint = mem
            .checkpoint("private", &secret, None)
            .expect("checkpoint");
        mem.bind("latest", &checkpoint).expect("bind checkpoint");

        let locked_record = body(&handle(&mem, "/api/record", &format!("cid={}", secret.0)));
        let locked_graph = body(&handle(
            &mem,
            "/api/graph",
            &format!("cid={}", checkpoint.0),
        ));
        let privacy = body(&handle(&mem, "/api/privacy", &format!("cid={}", secret.0)));
        // Content is fully visible locally — the fence guards egress, not viewing …
        assert!(locked_record.contains("docs/notes.txt"));
        assert!(locked_graph.contains("docs/notes.txt"));
        // … and surfaces only as a fence badge (the default under Decision 0026).
        assert!(locked_record.contains("\"locked\":true"));
        assert!(locked_graph.contains("\"fenced\":true"));
        assert!(locked_graph.contains("\"cleared\":false"));
        // The egress-side privacy summary still reports what is fenced from export.
        assert!(privacy.contains("\"fenced\":true"));
        assert!(privacy.contains("\"reachable_node_count\":2"));
        assert!(privacy.contains("\"file_count\":1"));
        assert!(privacy.contains("\"blocked_file_count\":1"));
    }

    #[test]
    fn locked_room_messages_stay_visible_locally_with_an_egress_badge() {
        let (_dir, mem) = store();
        let cid = mem
            .post_message("private-room", "hidden-message-body")
            .expect("post");
        mem.lock_subgraph(&cid, "private room").expect("lock");
        let thread = body(&handle(&mem, "/api/thread", "room=private-room"));
        // The body is shown locally — the lock only fences it from egress …
        assert!(thread.contains("hidden-message-body"));
        // … and surfaces as a lock badge on the message.
        assert!(thread.contains("\"locked\":true"));
    }

    #[test]
    fn gui_publishes_clear_and_locked_exact_reviewed_plans() {
        let (dir, mem) = store();
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let clear = put_named(&mem, "clear", "public body");
        let clear_backend = configure_fake_ipfs_backend(&mem, dir.path(), 1);
        let clear_plan = mem
            .build_egress_plan_for_target("clear", EgressOperation::PublicPublish)
            .expect("clear plan");
        let clear_token = options
            .cache_review(clear_plan.clone())
            .expect("cache clear review");
        let clear_body = serde_json::json!({
            "review_token": clear_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let clear_response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &clear_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(clear_response.status, 200, "{}", body(&clear_response));
        clear_backend.join().expect("clear backend");

        let locked = put_named(&mem, "locked", "locked body");
        mem.lock_subgraph(&locked, "private").expect("lock");
        let locked_backend = configure_fake_ipfs_backend(&mem, dir.path(), 1);
        let locked_plan = mem
            .build_egress_plan_for_target("locked", EgressOperation::PublicPublish)
            .expect("locked plan");
        let locked_token = options
            .cache_review(locked_plan.clone())
            .expect("cache locked review");
        let locked_body = serde_json::json!({
            "review_token": locked_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let locked_response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &locked_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(locked_response.status, 200, "{}", body(&locked_response));
        assert!(body(&locked_response).contains("\"authorization_consumed\":true"));
        locked_backend.join().expect("locked backend");

        let privacy = body(&handle(&mem, "/api/privacy", &format!("cid={}", locked.0)));
        assert!(privacy.contains("\"known_public\":true"));
        let graph = body(&handle(&mem, "/api/graph", &format!("cid={}", locked.0)));
        assert!(graph.contains("\"known_public\":true"));
        let current = mem
            .build_egress_plan_for_target("locked", EgressOperation::PublicPublish)
            .expect("current locked plan");
        assert!(matches!(
            mem.publish_public(&current),
            Err(Error::PublicationBlocked { .. })
        ));
        assert_ne!(clear, locked);
    }

    #[test]
    fn authorize_publish_rejects_a_modified_reviewed_plan() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "secret", "classified");
        mem.set_password("pw").expect("password");
        mem.lock_subgraph(&root, "private").expect("lock");
        let mut reviewed = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .expect("plan");
        reviewed.byte_size += 1;
        let options = options_with_csrf("tok");
        let review_token = options
            .cache_review(reviewed)
            .expect("cache modified review");
        let request_body = serde_json::json!({
            "review_token": review_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &request_body,
                Some("127.0.0.1"),
                Some("http://127.0.0.1"),
                Some("tok"),
            ),
        );
        assert_eq!(response.status, 409);
    }

    #[test]
    fn loopback_gate_requires_host_matching_origin_json_and_rate_limits() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        let valid = post(
            "/api/nope",
            "{}",
            Some("127.0.0.1:4173"),
            Some("http://127.0.0.1:4173"),
            Some("tok"),
        );

        let missing_host = post(
            "/api/nope",
            "{}",
            None,
            Some("http://127.0.0.1:4173"),
            Some("tok"),
        );
        assert_eq!(route_request(&mem, &options, &missing_host).status, 403);

        let mismatched_origin = post(
            "/api/nope",
            "{}",
            Some("127.0.0.1:4173"),
            Some("http://127.0.0.1:4174"),
            Some("tok"),
        );
        assert_eq!(
            route_request(&mem, &options, &mismatched_origin).status,
            403
        );

        let mut wrong_type = valid;
        wrong_type
            .headers
            .insert("content-type".to_string(), "text/plain".to_string());
        assert_eq!(route_request(&mem, &options, &wrong_type).status, 415);

        for _ in 0..MUTATION_RATE_MAX {
            assert_eq!(
                route_request(
                    &mem,
                    &options,
                    &post(
                        "/api/nope",
                        "{}",
                        Some("127.0.0.1:4173"),
                        Some("http://127.0.0.1:4173"),
                        Some("tok"),
                    ),
                )
                .status,
                404
            );
        }
        assert_eq!(
            route_request(
                &mem,
                &options,
                &post(
                    "/api/nope",
                    "{}",
                    Some("127.0.0.1:4173"),
                    Some("http://127.0.0.1:4173"),
                    Some("tok"),
                ),
            )
            .status,
            429
        );
    }

    #[test]
    fn browser_shell_contains_phase_d_secret_and_state_safeguards() {
        let (_dir, mem) = store();
        let page = body(&handle(&mem, "/", ""));
        assert!(!page.contains(r#"autocomplete = "off""#));
        assert!(page.contains(r#"autocomplete = "new-password""#));
        assert!(page.contains(r#"autocomplete = "current-password""#));
        assert!(page.contains("finally { input.value = \"\"; }"));
        assert!(page.contains("review_token: plan.review_token"));
        assert!(page.contains("Exact CID manifest"));
        assert!(page.contains("cleared-root"));
        assert!(page.contains("partial-cleared"));
        assert!(page.contains("known-public"));
    }

    #[test]
    fn meta_exposes_a_csrf_token_for_the_page() {
        let (_dir, mem) = store();
        let options = options_with_csrf("page-token");
        let meta = body(&handle_with_options(&mem, &options, "/api/meta", ""));
        assert!(meta.contains("page-token"));
        assert!(meta.contains("\"password_set\""));
    }

    #[test]
    fn convert_private_is_gated_password_protected_and_surfaces_in_privacy() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "secret content");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let set = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                r#"{"password":"pw","confirm_password":"pw"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(set.status, 200);

        let review = handle_with_options(
            &mem,
            &options,
            "/api/egress-plan",
            "op=private&name=latest&namespace=team%3Awetlands&recipients=agent-recipient",
        );
        assert_eq!(review.status, 200);
        let review: serde_json::Value = serde_json::from_slice(&review.body).unwrap();
        let review_token = review["review_token"].as_str().unwrap();

        // Missing CSRF is forbidden by the gate (never reaches the handler).
        let no_csrf = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                None,
            ),
        );
        assert_eq!(no_csrf.status, 403);

        // Destination and recipient review must be explicitly acknowledged.
        let no_acknowledgement = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(no_acknowledgement.status, 400);

        // Wrong password is rejected (authentication failed).
        let wrong = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "WRONG",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong.status, 401);

        // A valid request converts and the privacy endpoint then shows the copy.
        let ok = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(ok.status, 200);
        assert!(body(&ok).contains("ciphertext_root"));
        assert!(body(&ok).contains("\"capability\""));

        let privacy = body(&handle(&mem, "/api/privacy", "name=latest"));
        assert!(privacy.contains("encrypted_private_copy"));
        assert!(privacy.contains("\"baf"));
        assert!(!privacy.contains("read_key"));
        let graph = body(&handle(&mem, "/api/graph", "name=latest"));
        assert!(graph.contains("\"encrypted_private\":true"));
    }

    #[test]
    fn no_running_gui_means_no_reuse() {
        let (_dir, mem) = store();
        assert!(running_gui_port(&mem).is_none());
    }

    #[test]
    fn stale_lockfile_does_not_match_a_dead_server() {
        let (_dir, mem) = store();
        // A lockfile pointing at a port nothing serves must not be reused.
        let path = mem.store_dir().unwrap().join("gui.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"pid":999999,"port":59123}"#).unwrap();
        assert!(running_gui_port(&mem).is_none());
    }

    #[test]
    fn pick_free_port_returns_a_bindable_port() {
        let port = pick_free_port(48910);
        // Whatever it returns must actually be bindable now.
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }
}
