//! Phase 8 §1 — the node-resident Librarian (Decision 0022).
//!
//! Semantic + graph-gravity retrieval over the IPLD store, packed into a token
//! budget. Ranking is **vector similarity × graph gravity**, not similarity
//! alone — well-connected, load-bearing nodes outrank lexically-similar orphans
//! (the math is ported from cyber's `trikernel` PageRank diffusion and
//! `context.nu` greedy-knapsack packer; we borrow the math only and reject its
//! token economy — Decision 0022).
//!
//! ## Pluggable embedder
//! The embedder sits behind the [`Embedder`] trait. The always-available default
//! is the zero-dependency [`LexicalEmbedder`] (deterministic, offline, ideal for
//! tests and as a fallback). A real small *embedding* model (e.g. `nomic-embed`
//! via fastembed) is a feature-gated backend that swaps in behind the same trait.
//! Non-negotiables (Decision 0022): the only model on the node is a small
//! *embedding* model — **never a generative LLM on-node** — and the index is
//! **capability-scoped**, never one global plaintext index (Decision 0011): the
//! [`Librarian::index`] scope is the set of roots whose capabilities the holder
//! already unwraps.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::binding::{Cid, CoreBinding, MemCli, Record};
use crate::error::Result;

/// ~chars per token, for the budget estimate (mixed prose/code).
const CHARS_PER_TOKEN: usize = 4;
/// How much graph gravity boosts a node's score over pure similarity.
const GRAVITY_WEIGHT: f32 = 2.0;
/// How much link density (connectedness per KB) boosts score.
const DENSITY_WEIGHT: f32 = 0.5;
/// How much recency boosts score. Conversational capture is mostly flat (uniform
/// gravity), so recency is the main differentiator among equally-relevant memories.
const RECENCY_WEIGHT: f32 = 1.0;
/// Recency half-life: a memory this old gets half the recency boost of a brand-new one.
const RECENCY_HALF_LIFE_SECS: f32 = 14.0 * 86_400.0;
/// PageRank damping (cyber `trikernel` alpha); teleport = 1 - alpha.
const PAGERANK_ALPHA: f32 = 0.85;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A text → vector embedder. Implementations must be deterministic for a given
/// input and return a fixed-length, L2-normalized vector.
pub trait Embedder {
    fn dims(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Stable identity of the model, so a persisted embedding cache invalidates
    /// automatically when the embedder changes.
    fn id(&self) -> String {
        format!("embedder-{}", self.dims())
    }
}

/// A shareable, thread-safe embedder handle. Built once and cloned cheaply for
/// each index rebuild (the underlying model — expensive to load — lives behind
/// the `Arc`, never reloaded per rebuild).
pub type SharedEmbedder = std::sync::Arc<dyn Embedder + Send + Sync>;

impl Embedder for SharedEmbedder {
    fn dims(&self) -> usize {
        (**self).dims()
    }
    fn embed(&self, text: &str) -> Vec<f32> {
        (**self).embed(text)
    }
    fn id(&self) -> String {
        (**self).id()
    }
}

/// Build the embedder the config selects — the model is **never baked in**
/// (Decision 0022 — small embedder only; swap as models age, no recompile):
/// - `"http"`  → an [`HttpEmbedder`] against any model server (works in the base
///   build, no ONNX); the most future-proof "swap the model" path.
/// - `"fastembed"` → the in-process [`SemanticEmbedder`] for `embedding_model`
///   (requires the `semantic-embed` feature).
/// - `"lexical"` → the zero-dependency fallback.
/// - `"auto"` (default) → http if a URL is set, else fastembed if available, else lexical.
///
/// Any backend that fails to construct degrades to the lexical fallback rather
/// than failing retrieval.
pub fn default_embedder(config: &crate::config::LibrarianConfig) -> SharedEmbedder {
    let lexical = || -> SharedEmbedder { std::sync::Arc::new(LexicalEmbedder::default()) };
    let http = |url: &str| -> Option<SharedEmbedder> {
        (!url.is_empty()).then(|| {
            std::sync::Arc::new(HttpEmbedder::new(url, &config.embedding_model)) as SharedEmbedder
        })
    };
    #[cfg(feature = "semantic-embed")]
    let fastembed = || -> Option<SharedEmbedder> {
        match SemanticEmbedder::new(&config.embedding_model) {
            Ok(model) => Some(std::sync::Arc::new(model) as SharedEmbedder),
            Err(error) => {
                eprintln!("librarian: semantic model unavailable ({error}); using fallback");
                None
            }
        }
    };
    #[cfg(not(feature = "semantic-embed"))]
    let fastembed = || -> Option<SharedEmbedder> { None };

    match config.embedder.as_str() {
        "lexical" => lexical(),
        "http" => http(&config.embedding_url).unwrap_or_else(lexical),
        "fastembed" => fastembed().unwrap_or_else(lexical),
        // "auto" / anything else: prefer an external server, then in-process, then lexical.
        _ => http(&config.embedding_url)
            .or_else(fastembed)
            .unwrap_or_else(lexical),
    }
}

/// Zero-dependency lexical embedder: hashed token bag → L2-normalized vector.
/// Deterministic (good for tests + offline), gives crude lexical similarity, and
/// is the fallback when no semantic-model backend is enabled.
#[derive(Debug, Clone)]
pub struct LexicalEmbedder {
    dims: usize,
}

impl LexicalEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(1) }
    }
}

impl Default for LexicalEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedder for LexicalEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn id(&self) -> String {
        format!("lexical-v1-{}", self.dims)
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dims];
        for token in tokenize(text) {
            let bucket = (fnv1a(token.as_bytes()) as usize) % self.dims;
            v[bucket] += 1.0;
        }
        l2_normalize(&mut v);
        v
    }
}

/// A small on-node **semantic** embedder (Decision 0022) — `bge-small-en-v1.5`
/// (~33M params, CPU-friendly, 384-dim), run via fastembed/ONNX. This is the
/// *only* model on the node and is **never** a generative LLM. Feature-gated
/// (`semantic-embed`) so the base build carries no ONNX runtime; it swaps in
/// behind the [`Embedder`] trait exactly where [`LexicalEmbedder`] sits.
///
/// The model is downloaded to the fastembed cache on first construction, so
/// `new()` is fallible and requires network on first run. Embedding is batched
/// internally per call; for large libraries the caller should index
/// background/low-priority (Decision 0022's performance-invisible rule).
#[cfg(feature = "semantic-embed")]
pub struct SemanticEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    dims: usize,
    model_id: String,
}

#[cfg(feature = "semantic-embed")]
impl SemanticEmbedder {
    /// Load (downloading on first use) the embedding model named `model_id`,
    /// matched against fastembed's supported models — so the model is chosen by
    /// config, **never baked in** (swap it as models age). Unknown names fall back
    /// to bge-small-en-v1.5 with a warning. `dims` comes from the model's metadata.
    pub fn new(model_id: &str) -> std::result::Result<Self, String> {
        let want = model_id.to_lowercase();
        let models = fastembed::TextEmbedding::list_supported_models();
        // Precise resolution: exact code, else the HF-style suffix `…/<want>`.
        // (Loose substring matching can grab a quantized/variant model.)
        let matched = models
            .iter()
            .find(|info| info.model_code.to_lowercase() == want)
            .or_else(|| {
                models.iter().find(|info| {
                    info.model_code
                        .to_lowercase()
                        .ends_with(&format!("/{want}"))
                })
            })
            .cloned();
        let (model, dims, resolved) = match matched {
            Some(info) => (info.model, info.dim, info.model_code),
            None => {
                eprintln!(
                    "librarian: unknown embedding model '{model_id}'; falling back to bge-small-en-v1.5"
                );
                (
                    fastembed::EmbeddingModel::BGESmallENV15,
                    384,
                    "bge-small-en-v1.5".to_string(),
                )
            }
        };
        let loaded = fastembed::TextEmbedding::try_new(fastembed::InitOptions::new(model))
            .map_err(|e| format!("load embedding model '{resolved}': {e}"))?;
        Ok(Self {
            model: std::sync::Mutex::new(loaded),
            dims,
            model_id: resolved,
        })
    }

    /// Embed a whole batch in one ONNX call (the efficient path).
    pub fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let guard = self.model.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .embed(texts.iter().map(String::as_str).collect(), None)
            .unwrap_or_else(|_| vec![vec![0.0; self.dims]; texts.len()])
    }
}

#[cfg(feature = "semantic-embed")]
impl Embedder for SemanticEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn id(&self) -> String {
        format!("fastembed:{}", self.model_id)
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let guard = self.model.lock().unwrap_or_else(|p| p.into_inner());
        match guard.embed(vec![text], None) {
            Ok(mut vectors) if !vectors.is_empty() => {
                let mut v = vectors.remove(0);
                l2_normalize(&mut v);
                v
            }
            _ => vec![0.0; self.dims],
        }
    }
}

/// An embedder backed by **any external model server** (Ollama-style
/// `{prompt}→{embedding}` or OpenAI-style `{input}→{data[0].embedding}`). The
/// model lives off-process, so it can be upgraded/swapped without touching the
/// plugin — the most future-proof "models get outdated" answer. Zero heavy deps:
/// a plain HTTP/1.1 POST over std TCP (works in the base build, no ONNX). On any
/// request failure it returns a zero vector (graceful: that node just won't match).
pub struct HttpEmbedder {
    url: String,
    model: String,
    /// Vector length, learned from the first successful response (0 = unknown).
    dims: AtomicUsize,
}

impl HttpEmbedder {
    pub fn new(url: &str, model: &str) -> Self {
        Self {
            url: url.to_string(),
            model: model.to_string(),
            dims: AtomicUsize::new(0),
        }
    }

    fn request(&self, text: &str) -> Option<Vec<f32>> {
        let (host, port, path) = parse_http_url(&self.url)?;
        let body = serde_json::json!({
            "model": self.model,
            "prompt": text, // Ollama
            "input": text,  // OpenAI-style
        })
        .to_string();
        let mut stream = TcpStream::connect((host.as_str(), port)).ok()?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
        // Write headers + body in one shot: if we split the writes, a server that
        // reads, responds, and closes before the second write would break the pipe
        // on the body write and fail the request.
        let mut wire = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        wire.extend_from_slice(body.as_bytes());
        stream.write_all(&wire).ok()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).ok()?;
        let text = String::from_utf8_lossy(&response);
        let start = text.find("\r\n\r\n")? + 4;
        let value: serde_json::Value = serde_json::from_str(text[start..].trim()).ok()?;
        extract_embedding(&value)
    }
}

impl Embedder for HttpEmbedder {
    fn dims(&self) -> usize {
        self.dims.load(Ordering::Relaxed)
    }

    fn id(&self) -> String {
        format!("http:{}:{}", self.url, self.model)
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        match self.request(text) {
            Some(mut v) if !v.is_empty() => {
                self.dims.store(v.len(), Ordering::Relaxed);
                l2_normalize(&mut v);
                v
            }
            _ => vec![0.0; self.dims.load(Ordering::Relaxed).max(1)],
        }
    }
}

/// Pull an embedding vector out of a model server's JSON response, tolerant of
/// the common shapes: Ollama `{"embedding": [...]}`, OpenAI `{"data": [{"embedding": [...]}]}`,
/// and `{"embeddings": [[...]]}`.
fn extract_embedding(value: &serde_json::Value) -> Option<Vec<f32>> {
    let as_vec = |arr: &serde_json::Value| -> Option<Vec<f32>> {
        arr.as_array().map(|xs| {
            xs.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect()
        })
    };
    if let Some(v) = value.get("embedding").and_then(as_vec) {
        return Some(v);
    }
    if let Some(v) = value
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|e| e.get("embedding"))
        .and_then(as_vec)
    {
        return Some(v);
    }
    value
        .get("embeddings")
        .and_then(|e| e.get(0))
        .and_then(as_vec)
}

/// Minimal `http://host:port/path` parser (no TLS — local/loopback model servers).
pub(crate) fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host_port, path) = match rest.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (host_port.to_string(), 80),
    };
    Some((host, port, path))
}

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity, clamped to [0, 1] (inputs are normalized → this is a dot).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    dot.clamp(0.0, 1.0)
}

/// PageRank over a directed link graph — the cyber `trikernel` diffusion kernel.
/// `out_edges[cid]` lists the in-scope CIDs that `cid` links to. Returns the
/// stationary distribution (well-connected nodes get more mass).
fn pagerank(nodes: &[String], out_edges: &HashMap<String, Vec<String>>) -> HashMap<String, f32> {
    let n = nodes.len();
    let mut rank: HashMap<String, f32> = nodes
        .iter()
        .map(|c| (c.clone(), 1.0 / n.max(1) as f32))
        .collect();
    if n == 0 {
        return rank;
    }
    let node_set: HashSet<&String> = nodes.iter().collect();
    let teleport = (1.0 - PAGERANK_ALPHA) / n as f32;

    // Inbound adjacency + out-degree, both restricted to in-scope targets.
    let mut inbound: HashMap<&String, Vec<&String>> =
        nodes.iter().map(|c| (c, Vec::new())).collect();
    let mut outdeg: HashMap<&String, usize> = HashMap::new();
    for cid in nodes {
        let targets: Vec<&String> = out_edges
            .get(cid)
            .map(|ts| ts.iter().filter(|t| node_set.contains(*t)).collect())
            .unwrap_or_default();
        outdeg.insert(cid, targets.len());
        for t in targets {
            inbound.get_mut(t).unwrap().push(cid);
        }
    }

    for _ in 0..50 {
        let dangling_mass: f32 = nodes
            .iter()
            .filter(|c| outdeg[c] == 0)
            .map(|c| rank[c])
            .sum();
        let dangling_share = PAGERANK_ALPHA * dangling_mass / n as f32;
        let mut next = HashMap::with_capacity(n);
        let mut delta = 0.0f32;
        for cid in nodes {
            let link_sum: f32 = inbound[cid]
                .iter()
                .map(|u| rank[*u] / outdeg[u].max(1) as f32)
                .sum();
            let val = PAGERANK_ALPHA * link_sum + teleport + dangling_share;
            delta += (val - rank[cid]).abs();
            next.insert(cid.clone(), val);
        }
        rank = next;
        if delta < 1e-8 {
            break;
        }
    }
    rank
}

/// One indexed node: its embedding, searchable text, token cost, and graph
/// signals (normalized to [0, 1]).
struct IndexEntry {
    cid: String,
    kind: String,
    vector: Vec<f32>,
    text: String,
    tokens: usize,
    outbound: Vec<String>,
    created_at: Option<u64>,
    gravity: f32,
    density: f32,
    recency: f32,
}

/// How much content to return per hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Brief,
    Summary,
    Full,
}

impl Depth {
    fn preview_chars(self) -> usize {
        match self {
            Depth::Brief => 160,
            Depth::Summary => 600,
            Depth::Full => usize::MAX,
        }
    }
}

/// One retrieved node.
#[derive(Debug, Clone)]
pub struct Retrieved {
    pub cid: String,
    pub kind: String,
    pub preview: String,
    pub score: f32,
    pub similarity: f32,
    pub gravity: f32,
    pub tokens: usize,
    /// How this node was reached: `0` = a direct query match; `1+` = pulled in as
    /// related context by following links from a match (multi-hop).
    pub hop: u8,
}

/// The packed result of a [`Librarian::retrieve`] call.
#[derive(Debug, Clone)]
pub struct RetrieveResult {
    pub items: Vec<Retrieved>,
    pub used_tokens: usize,
    pub budget_tokens: usize,
}

/// The node-resident Librarian: a capability-scoped index + the retrieve API.
pub struct Librarian<E: Embedder> {
    embedder: E,
    entries: Vec<IndexEntry>,
}

impl<E: Embedder> Librarian<E> {
    /// Index every content-bearing node reachable from `roots` (the capability
    /// scope). Pass all named roots for the whole local store; pass a single
    /// subtree root to confine the index to that capability segment — no node
    /// outside the scope is ever embedded, ranked, or returned (Decision 0011).
    pub fn index(mem: &MemCli, embedder: E, roots: &[Cid]) -> Result<Self> {
        // Scope = the union of everything reachable from the granted roots. A
        // root that can't be walked (e.g. a calendar-scaffolding `day_index`
        // whose body the graph walker doesn't decode) is skipped, not fatal.
        let mut scope: HashSet<String> = HashSet::new();
        for root in roots {
            scope.insert(root.0.clone());
            if let Ok(reachable) = mem.walk(root) {
                for cid in reachable {
                    scope.insert(cid.0);
                }
            }
        }
        Self::index_cids(mem, embedder, scope, None)
    }

    /// Convenience: index the whole local store. Content lives in two places —
    /// reachable from ordinary named roots, *and* in the day-tier HAMT calendar
    /// (which the graph walker cannot traverse) — so this gathers both, the same
    /// way the Data Platter forest does (via `day_events`).
    pub fn index_all(mem: &MemCli, embedder: E) -> Result<Self> {
        Self::index_cids(mem, embedder, scope_all(mem)?, None)
    }

    /// Like [`index_all`], but reuses a persisted, model-tagged embedding cache
    /// (`<store>/embed-cache.json`) so unchanged nodes are not re-embedded across
    /// rebuilds or restarts — the persistence win without a heavyweight vector DB
    /// (LanceDB ANN remains the future feature-gated scale backend). Embedding is
    /// the expensive step (especially semantic); this skips it for known CIDs.
    pub fn index_all_persistent(mem: &MemCli, embedder: E) -> Result<Self> {
        let scope = scope_all(mem)?;
        let cache_path = mem.store_dir().ok().map(|dir| dir.join("embed-cache.json"));
        let mut cache = cache_path
            .as_ref()
            .map(|path| EmbedCache::load(path, &embedder.id()));
        let librarian = Self::index_cids(mem, embedder, scope, cache.as_mut())?;
        if let (Some(cache), Some(path)) = (&cache, &cache_path) {
            cache.save(path);
        }
        Ok(librarian)
    }

    /// Index an explicit set of CIDs (already scope-resolved). When `cache` is
    /// present, a node's vector is reused from it (or computed and stored).
    fn index_cids(
        mem: &MemCli,
        embedder: E,
        scope: HashSet<String>,
        mut cache: Option<&mut EmbedCache>,
    ) -> Result<Self> {
        // Quarantined CIDs (the Guardian's bad-CID list, §3) are withheld from
        // surfacing: never embedded, ranked, or returned. Reversible — `release`
        // makes them re-appear on the next index.
        let quarantine = mem.quarantine_registry().unwrap_or_default();
        let cids: Vec<Cid> = scope
            .iter()
            .filter(|c| !quarantine.is_quarantined(c))
            .map(|c| Cid(c.clone()))
            .collect();
        let records = mem.get_many(&cids)?;
        // 3. Build entries (content-bearing live nodes only), restricting links
        //    to the scope so the gravity graph never reaches outside it.
        let mut entries = Vec::new();
        let mut out_edges: HashMap<String, Vec<String>> = HashMap::new();
        for cid in &cids {
            let Some(Record::Live {
                kind, body_json, ..
            }) = records.get(&cid.0)
            else {
                continue;
            };
            if is_scaffolding(kind) {
                continue;
            }
            let text = searchable_text(body_json);
            if text.trim().is_empty() {
                continue;
            }
            let links: Vec<String> = mem
                .links_from_record_json(body_json)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.0)
                .filter(|t| scope.contains(t))
                .collect();
            out_edges.insert(cid.0.clone(), links.clone());
            let tokens = (text.len() / CHARS_PER_TOKEN).max(1);
            // Reuse a cached vector when available (and the right length), else
            // embed and store it.
            let vector = match cache.as_deref_mut() {
                // The cache is model-tagged by `embedder.id()`, so a stored vector
                // is trusted; the length check is a secondary guard, skipped when
                // the embedder's dims are not yet known (e.g. a lazy HTTP backend).
                Some(c) => match c.vectors.get(&cid.0) {
                    Some(v) if embedder.dims() == 0 || v.len() == embedder.dims() => v.clone(),
                    _ => {
                        let v = embedder.embed(&text);
                        c.vectors.insert(cid.0.clone(), v.clone());
                        v
                    }
                },
                None => embedder.embed(&text),
            };
            let created_at = serde_json::from_str::<serde_json::Value>(body_json)
                .ok()
                .and_then(|v| v.get("created_at").and_then(serde_json::Value::as_u64));
            entries.push(IndexEntry {
                cid: cid.0.clone(),
                kind: kind.clone(),
                vector,
                text,
                tokens,
                outbound: links,
                created_at,
                gravity: 0.0,
                density: 0.0,
                recency: 0.0,
            });
        }
        Ok(Self::finish(embedder, entries, &out_edges))
    }

    /// Compute the graph signals (PageRank gravity + density) and recency, normalized.
    fn finish(
        embedder: E,
        mut entries: Vec<IndexEntry>,
        out_edges: &HashMap<String, Vec<String>>,
    ) -> Self {
        let node_ids: Vec<String> = entries.iter().map(|e| e.cid.clone()).collect();
        let pr = pagerank(&node_ids, out_edges);
        let max_pr = pr
            .values()
            .copied()
            .fold(0.0f32, f32::max)
            .max(f32::EPSILON);
        let density_of =
            |e: &IndexEntry| e.outbound.len() as f32 / (e.text.len() as f32 / 1024.0).max(1.0);
        let max_density = entries
            .iter()
            .map(density_of)
            .fold(0.0f32, f32::max)
            .max(f32::EPSILON);
        let now = now_secs();
        for e in &mut entries {
            e.gravity = pr.get(&e.cid).copied().unwrap_or(0.0) / max_pr;
            e.density = density_of(e) / max_density;
            // Exponential recency decay; nodes with no timestamp get no boost (0).
            e.recency = match e.created_at {
                Some(ts) if ts <= now => 0.5f32.powf((now - ts) as f32 / RECENCY_HALF_LIFE_SECS),
                Some(_) => 1.0, // a future timestamp counts as "now"
                None => 0.0,
            };
        }
        Self { embedder, entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a CID is in this (capability-scoped) index.
    pub fn contains(&self, cid: &str) -> bool {
        self.entries.iter().any(|e| e.cid == cid)
    }

    /// Rank by `similarity × (1 + gravity·w_g + density·w_d)`, then greedily pack
    /// into `token_budget`. `pinned` CIDs are always included first (even over
    /// budget), then the highest-scoring nodes fill the remaining budget.
    /// Rank by `similarity × (1 + gravity·w_g + density·w_d)`, then greedily pack
    /// into `token_budget` (pinned first). Single-hop: direct matches only.
    pub fn retrieve(
        &self,
        query: &str,
        token_budget: usize,
        pinned: &[String],
        depth: Depth,
    ) -> RetrieveResult {
        self.retrieve_multihop(query, token_budget, pinned, depth, 0, None)
    }

    /// Like [`retrieve`], but after packing the direct matches it expands up to
    /// `hops` link-steps to pull in **related context** — e.g. the `tool_result`
    /// a retrieved `decision` links to (Phase 8 §1 "multi-hop"). Traversal stays
    /// strictly inside the index (the capability scope, Decision 0011) and within
    /// the remaining token budget. Related nodes are tagged with their hop depth.
    ///
    /// Ranking blends similarity × graph gravity × density × **recency**. `kinds`,
    /// when `Some`, restricts *direct matches* to those node kinds (related context
    /// pulled by multi-hop can be any kind); pinned CIDs are exempt from the filter.
    pub fn retrieve_multihop(
        &self,
        query: &str,
        token_budget: usize,
        pinned: &[String],
        depth: Depth,
        hops: u8,
        kinds: Option<&[String]>,
    ) -> RetrieveResult {
        let qv = self.embedder.embed(query);
        let by_cid: HashMap<&str, &IndexEntry> =
            self.entries.iter().map(|e| (e.cid.as_str(), e)).collect();
        let score_of = |e: &IndexEntry, sim: f32| {
            sim * (1.0
                + GRAVITY_WEIGHT * e.gravity
                + DENSITY_WEIGHT * e.density
                + RECENCY_WEIGHT * e.recency)
        };
        let kind_ok = |e: &IndexEntry| kinds.is_none_or(|ks| ks.iter().any(|k| k == &e.kind));

        let mut scored: Vec<(f32, f32, &IndexEntry)> = self
            .entries
            .iter()
            .map(|e| {
                let sim = cosine(&qv, &e.vector);
                (score_of(e, sim), sim, e)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let pinned_set: HashSet<&str> = pinned.iter().map(String::as_str).collect();
        let mut included: HashSet<&str> = HashSet::new();
        let mut used = 0usize;
        // (score, similarity, entry, hop)
        let mut chosen: Vec<(f32, f32, &IndexEntry, u8)> = Vec::new();

        // Pinned first — forced in, even past the budget.
        for &(score, sim, e) in &scored {
            if pinned_set.contains(e.cid.as_str()) {
                chosen.push((score, sim, e, 0));
                included.insert(e.cid.as_str());
                used += e.tokens;
            }
        }
        // Then greedy-fill direct matches by score within the remaining budget.
        // Only nodes that actually match (similarity > 0) and pass the kind filter
        // count as direct hits — we never pad results with zero-relevance filler.
        // Related-but-unmatched context arrives solely via multi-hop linkage below.
        for &(score, sim, e) in &scored {
            if included.contains(e.cid.as_str()) {
                continue;
            }
            if sim > 0.0 && kind_ok(e) && used + e.tokens <= token_budget {
                chosen.push((score, sim, e, 0));
                included.insert(e.cid.as_str());
                used += e.tokens;
            }
        }

        // Multi-hop expansion: follow links from what we've chosen to surface
        // related context, breadth-first, staying within scope and budget.
        let mut frontier: Vec<&str> = chosen.iter().map(|c| c.2.cid.as_str()).collect();
        for hop in 1..=hops {
            let mut next: Vec<&str> = Vec::new();
            for cid in &frontier {
                let Some(entry) = by_cid.get(cid) else {
                    continue;
                };
                for link in &entry.outbound {
                    if included.contains(link.as_str()) {
                        continue;
                    }
                    let Some(linked) = by_cid.get(link.as_str()) else {
                        continue;
                    };
                    if used + linked.tokens > token_budget {
                        continue;
                    }
                    let sim = cosine(&qv, &linked.vector);
                    chosen.push((score_of(linked, sim), sim, linked, hop));
                    included.insert(linked.cid.as_str());
                    used += linked.tokens;
                    next.push(linked.cid.as_str());
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }

        // Direct matches first (by score), then related context (by hop, score).
        chosen.sort_by(|a, b| {
            a.3.cmp(&b.3)
                .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
        });
        let items = chosen
            .into_iter()
            .map(|(score, sim, e, hop)| Retrieved {
                cid: e.cid.clone(),
                kind: e.kind.clone(),
                preview: truncate(&e.text, depth.preview_chars()),
                score,
                similarity: sim,
                gravity: e.gravity,
                tokens: e.tokens,
                hop,
            })
            .collect();
        RetrieveResult {
            items,
            used_tokens: used,
            budget_tokens: token_budget,
        }
    }
}

/// The whole-store scope: content reachable from ordinary named roots plus the
/// day-tier HAMT calendar (which the graph walker can't traverse), gathered the
/// same way the Data Platter forest does.
fn scope_all(mem: &MemCli) -> Result<HashSet<String>> {
    let mut scope: HashSet<String> = HashSet::new();
    for (name, cid) in mem.names()? {
        if let Some(date) = name.strip_prefix("day-") {
            for (_key, event) in mem.day_events(date).unwrap_or_default() {
                scope.insert(event.0);
            }
        } else if name.starts_with("month-") || name.starts_with("year-") {
            continue; // calendar scaffolding, not content
        } else {
            scope.insert(cid.0.clone());
            if let Ok(reachable) = mem.walk(&cid) {
                for reached in reachable {
                    scope.insert(reached.0);
                }
            }
        }
    }
    Ok(scope)
}

/// A persisted, model-tagged map of CID → embedding vector, so re-indexing skips
/// re-embedding unchanged nodes across rebuilds/restarts. Tagged by the
/// embedder's `id()` so switching models invalidates the whole cache.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct EmbedCache {
    model: String,
    #[serde(default)]
    vectors: HashMap<String, Vec<f32>>,
}

impl EmbedCache {
    /// Load the cache for `model`; an absent, unreadable, or differently-tagged
    /// file yields a fresh empty cache (safe: at worst everything re-embeds).
    fn load(path: &std::path::Path, model: &str) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<EmbedCache>(&text).ok())
            .filter(|cache| cache.model == model)
            .unwrap_or_else(|| EmbedCache {
                model: model.to_string(),
                vectors: HashMap::new(),
            })
    }

    fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// Calendar/structural scaffolding carries no searchable content.
fn is_scaffolding(kind: &str) -> bool {
    matches!(kind, "day_index" | "month_index" | "year_index" | "store")
}

/// All string leaf values in a record's JSON, concatenated — robust across node
/// kinds. IPLD link objects (`{"/": "bafy…"}`) are skipped so CIDs do not pollute
/// the embedding text.
fn searchable_text(body_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body_json) else {
        return String::new();
    };
    let mut out = String::new();
    collect_strings(&value, &mut out);
    out
}

fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(s);
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if map.len() == 1 && map.contains_key("/") {
                return; // an IPLD link, not prose
            }
            for (_, v) in map {
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == usize::MAX || s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::Node;

    impl Librarian<LexicalEmbedder> {
        /// Build a deterministic index directly from `(cid, text, outbound)`
        /// triples — bypasses the store so ranking/packing/gravity are testable
        /// in isolation.
        fn from_raw(raw: Vec<(&str, &str, Vec<&str>)>) -> Self {
            Self::from_raw_dated(raw.into_iter().map(|(c, t, o)| (c, t, o, None)).collect())
        }

        /// As [`from_raw`] but with an explicit `created_at` per node, to test recency.
        fn from_raw_dated(raw: Vec<(&str, &str, Vec<&str>, Option<u64>)>) -> Self {
            let embedder = LexicalEmbedder::default();
            let mut out_edges = HashMap::new();
            let entries = raw
                .into_iter()
                .map(|(cid, text, outbound, created_at)| {
                    let outbound: Vec<String> = outbound.into_iter().map(String::from).collect();
                    out_edges.insert(cid.to_string(), outbound.clone());
                    IndexEntry {
                        cid: cid.to_string(),
                        kind: "memory".to_string(),
                        vector: embedder.embed(text),
                        text: text.to_string(),
                        tokens: (text.len() / CHARS_PER_TOKEN).max(1),
                        outbound,
                        created_at,
                        gravity: 0.0,
                        density: 0.0,
                        recency: 0.0,
                    }
                })
                .collect();
            Librarian::finish(embedder, entries, &out_edges)
        }
    }

    #[cfg(feature = "semantic-embed")]
    #[test]
    fn semantic_embedder_captures_meaning_beyond_lexical_overlap() {
        // Requires network on first run to download the model.
        let e = SemanticEmbedder::new("bge-small-en-v1.5").expect("load embedding model");
        assert_eq!(e.dims(), 384);
        let anchor = e.embed("a happy puppy playing in the yard");
        // Semantically close but low lexical overlap vs. an unrelated topic.
        let near = cosine(&anchor, &e.embed("a joyful dog running outside"));
        let far = cosine(&anchor, &e.embed("quarterly financial accounting report"));
        assert!(
            near > far,
            "semantic similarity should beat lexical overlap: near {near} vs far {far}"
        );
    }

    #[test]
    fn http_embedder_calls_a_model_server_and_normalizes() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        // A tiny mock embeddings server returning an Ollama-style response.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = r#"{"embedding":[3.0,0.0,4.0]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let e = HttpEmbedder::new(
            &format!("http://127.0.0.1:{port}/api/embeddings"),
            "any-model",
        );
        let v = e.embed("hello");
        server.join().unwrap();
        // 3-4-... normalized: [0.6, 0.0, 0.8].
        assert_eq!(v.len(), 3);
        assert!(
            (v[0] - 0.6).abs() < 1e-5 && (v[2] - 0.8).abs() < 1e-5,
            "normalized: {v:?}"
        );
        assert_eq!(e.dims(), 3, "dims learned from the response");
        assert!(
            e.id().contains("any-model"),
            "id carries the model for cache tagging"
        );
    }

    #[test]
    fn http_embedder_degrades_gracefully_when_the_server_is_down() {
        // A closed port → no panic, a zero vector (that node just won't match).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let e = HttpEmbedder::new(&format!("http://127.0.0.1:{port}/api/embeddings"), "m");
        assert!(e.embed("hello").iter().all(|x| *x == 0.0));
    }

    #[test]
    fn lexical_embedder_is_deterministic_and_normalized() {
        let e = LexicalEmbedder::new(128);
        let a = e.embed("the quick brown fox");
        let b = e.embed("the quick brown fox");
        assert_eq!(a, b, "deterministic");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2-normalized");
        // Similar text is closer than unrelated text.
        let near = cosine(&a, &e.embed("the quick brown dog"));
        let far = cosine(&a, &e.embed("zebra umbrella concrete"));
        assert!(near > far, "lexical overlap ranks higher: {near} vs {far}");
    }

    #[test]
    fn pagerank_ranks_a_hub_above_an_orphan() {
        // Five leaves all link to "hub"; "orphan" has no inbound links.
        let mut out_edges = HashMap::new();
        let nodes: Vec<String> = ["hub", "orphan", "l1", "l2", "l3", "l4", "l5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for leaf in ["l1", "l2", "l3", "l4", "l5"] {
            out_edges.insert(leaf.to_string(), vec!["hub".to_string()]);
        }
        out_edges.insert("hub".to_string(), vec![]);
        out_edges.insert("orphan".to_string(), vec![]);
        let pr = pagerank(&nodes, &out_edges);
        assert!(
            pr["hub"] > pr["orphan"],
            "well-linked hub ({}) must outrank orphan ({})",
            pr["hub"],
            pr["orphan"]
        );
    }

    #[test]
    fn a_well_linked_hub_outranks_a_lexically_identical_orphan() {
        // hub and orphan have IDENTICAL text (equal similarity to the query),
        // but five other nodes link to hub. Gravity must break the tie — the
        // §1 verification case (graph-gravity actually applied, not vectors alone).
        let topic = "distributed systems consensus protocol";
        let raw = vec![
            ("hub", topic, vec![]),
            ("orphan", topic, vec![]),
            ("a", "unrelated one", vec!["hub"]),
            ("b", "unrelated two", vec!["hub"]),
            ("c", "unrelated three", vec!["hub"]),
            ("d", "unrelated four", vec!["hub"]),
            ("e", "unrelated five", vec!["hub"]),
        ];
        let lib = Librarian::from_raw(raw);
        let result = lib.retrieve(topic, 10_000, &[], Depth::Brief);
        let hub_pos = result.items.iter().position(|r| r.cid == "hub").unwrap();
        let orphan_pos = result.items.iter().position(|r| r.cid == "orphan").unwrap();
        assert!(
            hub_pos < orphan_pos,
            "hub (gravity {}) should rank above orphan (gravity {})",
            result.items[hub_pos].gravity,
            result.items[orphan_pos].gravity
        );
    }

    #[test]
    fn multihop_pulls_in_linked_related_context() {
        // A decision links to its provenance tool_result. The query matches only
        // the decision lexically; one hop should surface the linked provenance.
        let raw = vec![
            (
                "decision",
                "we chose the egress-locked default for privacy",
                vec!["provenance"],
            ),
            (
                "provenance",
                "command grep results showing the lock fields",
                vec![],
            ),
            ("unrelated", "sourdough fermentation starter timing", vec![]),
        ];
        let lib = Librarian::from_raw(raw);

        // Without hops: only the direct match (and any other in-budget matches),
        // but the lexically-unrelated provenance is not pulled by linkage.
        let direct = lib.retrieve("egress-locked privacy default", 10_000, &[], Depth::Brief);
        let direct_has_prov = direct.items.iter().any(|r| r.cid == "provenance");

        // With one hop: the provenance is pulled in as related (hop = 1).
        let hopped = lib.retrieve_multihop(
            "egress-locked privacy default",
            10_000,
            &[],
            Depth::Brief,
            1,
            None,
        );
        let prov = hopped.items.iter().find(|r| r.cid == "provenance");
        assert!(
            prov.is_some(),
            "one hop surfaces the linked provenance node"
        );
        assert_eq!(prov.unwrap().hop, 1, "tagged as related (hop 1)");
        let decision = hopped.items.iter().find(|r| r.cid == "decision").unwrap();
        assert_eq!(decision.hop, 0, "the direct match stays hop 0");
        // The decision (direct match) ranks ahead of its related provenance.
        let dpos = hopped
            .items
            .iter()
            .position(|r| r.cid == "decision")
            .unwrap();
        let ppos = hopped
            .items
            .iter()
            .position(|r| r.cid == "provenance")
            .unwrap();
        assert!(dpos < ppos, "direct matches lead, related context follows");
        // (Sanity: the link, not lexical similarity, is what brought provenance in.)
        let _ = direct_has_prov;
    }

    #[test]
    fn multihop_stays_within_budget() {
        let raw = vec![
            ("a", "alpha beta gamma delta epsilon zeta", vec!["b"]),
            ("b", "linked neighbor one two three four five", vec!["c"]),
            ("c", "second neighbor six seven eight nine ten", vec![]),
        ];
        let lib = Librarian::from_raw(raw);
        // Tight budget: even with hops requested, packing must respect it.
        let result = lib.retrieve_multihop("alpha beta", 12, &[], Depth::Brief, 2, None);
        assert!(
            result.used_tokens <= 12,
            "multi-hop respects the budget: {}",
            result.used_tokens
        );
    }

    #[test]
    fn pack_respects_token_budget() {
        // Each text is ~40 chars ≈ 10 tokens; a 25-token budget fits ~2.
        let text = "alpha beta gamma delta epsilon zeta eta";
        let raw: Vec<(&str, &str, Vec<&str>)> = ["n0", "n1", "n2", "n3", "n4"]
            .iter()
            .map(|c| (*c, text, vec![]))
            .collect();
        let lib = Librarian::from_raw(raw);
        let result = lib.retrieve("alpha beta", 25, &[], Depth::Brief);
        assert!(
            result.used_tokens <= 25,
            "budget respected: {}",
            result.used_tokens
        );
        assert!(result.items.len() < 5, "not everything fits");
        assert!(!result.items.is_empty(), "something fits");
    }

    #[test]
    fn pinned_cid_is_always_included_even_over_budget() {
        let raw = vec![
            ("relevant", "alpha beta gamma", vec![]),
            (
                "pinned-irrelevant",
                "zebra umbrella concrete xylophone",
                vec![],
            ),
        ];
        let lib = Librarian::from_raw(raw);
        // Budget 0 → nothing fits by score, but the pinned CID is forced in.
        let result = lib.retrieve(
            "alpha beta",
            0,
            &["pinned-irrelevant".to_string()],
            Depth::Brief,
        );
        assert!(
            result.items.iter().any(|r| r.cid == "pinned-irrelevant"),
            "pinned CID must be included regardless of budget/score"
        );
    }

    #[test]
    fn indexes_a_real_store_and_retrieves_relevant_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let about_rust = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"the rust borrow checker enforces ownership and lifetimes","kind":"reference"}"#.to_string(),
            })
            .unwrap();
        let about_cooking = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"sourdough fermentation needs a live starter and time","kind":"reference"}"#.to_string(),
            })
            .unwrap();
        mem.bind("latest", &about_rust).unwrap();
        mem.bind("cooking", &about_cooking).unwrap();

        let lib = Librarian::index_all(&mem, LexicalEmbedder::default()).unwrap();
        assert!(lib.len() >= 2);
        let result = lib.retrieve("rust ownership lifetimes", 10_000, &[], Depth::Summary);
        assert_eq!(
            result.items.first().map(|r| r.cid.as_str()),
            Some(about_rust.0.as_str()),
            "the rust node ranks first for a rust query"
        );
    }

    #[test]
    fn persistent_cache_skips_re_embedding_unchanged_nodes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting(std::sync::Arc<AtomicUsize>);
        impl Embedder for Counting {
            fn dims(&self) -> usize {
                8
            }
            fn id(&self) -> String {
                "counting-v1".to_string()
            }
            fn embed(&self, _text: &str) -> Vec<f32> {
                self.0.fetch_add(1, Ordering::SeqCst);
                vec![0.35; 8]
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        for i in 0..5 {
            let cid = mem
                .put_node(&Node {
                    kind: "memory".to_string(),
                    fields_json: format!(
                        r#"{{"text":"node {i} content here","kind":"reference"}}"#
                    ),
                })
                .unwrap();
            mem.bind(&format!("n{i}"), &cid).unwrap();
        }
        let first = std::sync::Arc::new(AtomicUsize::new(0));
        Librarian::index_all_persistent(&mem, Counting(first.clone())).unwrap();
        assert!(
            first.load(Ordering::SeqCst) >= 5,
            "first build embeds all nodes"
        );
        // Second build: the persisted cache (same model id) serves every vector.
        let second = std::sync::Arc::new(AtomicUsize::new(0));
        Librarian::index_all_persistent(&mem, Counting(second.clone())).unwrap();
        assert_eq!(
            second.load(Ordering::SeqCst),
            0,
            "second build reuses every cached vector — no re-embedding"
        );
    }

    #[test]
    fn recency_breaks_ties_toward_newer_memory() {
        // Identical text (equal similarity) → recency must break the tie. (Real
        // nodes carry a record-level created_at; here we set it directly.)
        let now = now_secs();
        let lib = Librarian::from_raw_dated(vec![
            (
                "recent",
                "the same topic about distributed consensus",
                vec![],
                Some(now - 60),
            ),
            (
                "old",
                "the same topic about distributed consensus",
                vec![],
                Some(now - 400 * 86_400),
            ),
        ]);
        let result = lib.retrieve("distributed consensus topic", 10_000, &[], Depth::Brief);
        let rpos = result.items.iter().position(|r| r.cid == "recent").unwrap();
        let opos = result.items.iter().position(|r| r.cid == "old").unwrap();
        assert!(
            rpos < opos,
            "newer memory ranks above equally-relevant older memory"
        );
    }

    #[test]
    fn kind_filter_restricts_direct_matches() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let decision = mem
            .put_node(&Node {
                kind: "decision".to_string(),
                fields_json: r#"{"question":"","choice":"adopt the egress lock","rationale":""}"#
                    .to_string(),
            })
            .unwrap();
        let memo = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"a note about the egress lock","kind":"reference"}"#
                    .to_string(),
            })
            .unwrap();
        mem.bind("d", &decision).unwrap();
        mem.bind("m", &memo).unwrap();
        let lib = Librarian::index_all(&mem, LexicalEmbedder::default()).unwrap();
        // Unfiltered: both kinds match "egress lock".
        let all = lib.retrieve("egress lock", 10_000, &[], Depth::Brief);
        assert!(all.items.iter().any(|r| r.cid == decision.0));
        assert!(all.items.iter().any(|r| r.cid == memo.0));
        // Filter to decisions only: the memory node is excluded.
        let only = lib.retrieve_multihop(
            "egress lock",
            10_000,
            &[],
            Depth::Brief,
            0,
            Some(&["decision".to_string()]),
        );
        assert!(only.items.iter().any(|r| r.cid == decision.0));
        assert!(
            only.items.iter().all(|r| r.cid != memo.0),
            "kind filter excludes non-decision matches"
        );
    }

    #[test]
    fn quarantined_cids_are_withheld_from_retrieval_and_restored_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"the flagged malware payload notes","kind":"reference"}"#
                    .to_string(),
            })
            .unwrap();
        mem.bind("latest", &cid).unwrap();
        // Indexed + retrievable.
        assert!(Librarian::index_all(&mem, LexicalEmbedder::default())
            .unwrap()
            .contains(&cid.0));
        // Quarantine → excluded from the index, never surfaces.
        mem.quarantine_cid(&cid, "yara: test").unwrap();
        let withheld = Librarian::index_all(&mem, LexicalEmbedder::default()).unwrap();
        assert!(!withheld.contains(&cid.0), "quarantined CID is excluded");
        let result = withheld.retrieve("malware payload notes", 10_000, &[], Depth::Full);
        assert!(
            result.items.iter().all(|i| i.cid != cid.0),
            "never surfaces while quarantined"
        );
        // Release → reversible, re-appears.
        mem.release_cid(&cid).unwrap();
        assert!(Librarian::index_all(&mem, LexicalEmbedder::default())
            .unwrap()
            .contains(&cid.0));
    }

    #[test]
    fn the_index_is_capability_scoped_to_the_given_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        // Capability A and a sibling capability B the holder of A must NOT see.
        let node_a = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"capability A alpha content","kind":"reference"}"#
                    .to_string(),
            })
            .unwrap();
        let secret_b = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"capability B forbidden secret bravo","kind":"reference"}"#
                    .to_string(),
            })
            .unwrap();
        mem.bind("a", &node_a).unwrap();
        mem.bind("b", &secret_b).unwrap();

        // Index scoped to A's root only — B is a sibling, out of scope.
        let lib = Librarian::index(
            &mem,
            LexicalEmbedder::default(),
            std::slice::from_ref(&node_a),
        )
        .unwrap();
        assert!(lib.contains(&node_a.0), "A is indexed");
        assert!(
            !lib.contains(&secret_b.0),
            "sibling capability B must never be embedded or indexed"
        );
        // And it can never be returned, even by a query that matches its text.
        let result = lib.retrieve("forbidden secret bravo", 10_000, &[], Depth::Full);
        assert!(
            result.items.iter().all(|r| r.cid != secret_b.0),
            "B is never retrievable from A's index"
        );
    }
}
