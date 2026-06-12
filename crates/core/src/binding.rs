//! Layer 1 — Concierge Core Binding (contract only in Phase 0).
//!
//! These are the stable operations of the Concierge memory layer, as listed in
//! the plan. Phase 0 declares the trait and its supporting types so the rest of
//! the workspace can compile and depend on a fixed shape. Phase 1 provides the
//! first implementation by shelling out to the `mem` CLI (with a seam to link
//! the Rust crate directly later).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rand_core::{OsRng, RngCore};

use crate::config::Config;
use crate::contacts::Contacts;
use crate::dm_outbox::{DmOutbox, OutboundDm};
use crate::error::{Error, Result};
use crate::identity::{AgentId, Identity};
use crate::messaging::{message_order, MessageEnvelope, RoomBook};
use crate::naming::{self, ContactCard, Introduction, NameSource, ResolvedName};
use crate::publishing::{
    available_backends, backend_exists, share_via_selected_backend, BackendInfo,
};
use crate::sites::{SiteRecord, Sites};
use crate::social::SocialBook;

const MAX_CONTACT_CARD_BYTES: usize = 128 * 1024;
const MAX_CONTACT_NAME_BYTES: usize = 128;
const MAX_CONTACT_BIO_BYTES: usize = 4 * 1024;
const MAX_CONTACT_AVATAR_BYTES: usize = 64 * 1024;
const MAX_CONTACT_SITE_BYTES: usize = 512;
const MAX_CONTACT_ROOMS: usize = 64;
const MAX_CONTACT_ROOM_BYTES: usize = 128;
const MAX_INTRODUCTION_BYTES: usize = 16 * 1024;
const MAX_INTRODUCTION_NAME_BYTES: usize = 128;

fn validate_contact_card_limits(card: &ContactCard) -> Result<()> {
    let bounded = card.display_name.len() <= MAX_CONTACT_NAME_BYTES
        && card
            .bio
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_CONTACT_BIO_BYTES)
        && card
            .avatar
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_CONTACT_AVATAR_BYTES)
        && card
            .site_ipns
            .as_ref()
            .is_none_or(|value| value.len() <= MAX_CONTACT_SITE_BYTES)
        && card.rooms.len() <= MAX_CONTACT_ROOMS
        && card
            .rooms
            .iter()
            .all(|room| room.len() <= MAX_CONTACT_ROOM_BYTES);
    if !bounded {
        return Err(Error::SecurityPolicy(
            "contact card exceeds field limits".to_string(),
        ));
    }
    let bytes =
        serde_json::to_vec(card).map_err(|e| Error::Io(format!("serialize contact card: {e}")))?;
    if bytes.len() > MAX_CONTACT_CARD_BYTES {
        return Err(Error::SecurityPolicy(
            "contact card exceeds size limit".to_string(),
        ));
    }
    Ok(())
}

fn validate_introduction_limits(intro: &Introduction) -> Result<()> {
    if intro.asserted_name.len() > MAX_INTRODUCTION_NAME_BYTES {
        return Err(Error::SecurityPolicy(
            "introduction name exceeds size limit".to_string(),
        ));
    }
    let bytes =
        serde_json::to_vec(intro).map_err(|e| Error::Io(format!("serialize introduction: {e}")))?;
    if bytes.len() > MAX_INTRODUCTION_BYTES {
        return Err(Error::SecurityPolicy(
            "introduction exceeds size limit".to_string(),
        ));
    }
    Ok(())
}

/// Content identifier.
///
/// Stored as its string form for now; Phase 1 may replace the inner type with a
/// parsed CIDv1 once the binding links `mem` directly. Records are never mutated
/// after write, so a `Cid` is a stable, portable reference.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Cid(pub String);

/// Either a content address or a human-facing name to resolve first.
#[derive(Debug, Clone)]
pub enum CidOrName {
    Cid(Cid),
    Name(String),
}

/// An IPLD node to be written. Opaque in Phase 0; Phase 1 defines the concrete
/// node taxonomy (`Prompt`, `Response`, `ToolResult`, `Checkpoint`, …) and its
/// deterministic DAG-CBOR encoding.
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: String,
    pub fields_json: String,
}

/// The result of a `get`: a live record or a tombstone receipt. A tombstoned
/// lookup returns a receipt rather than an error from `get`'s perspective —
/// callers decide how to surface it.
#[derive(Debug, Clone)]
pub enum Record {
    Live {
        cid: Cid,
        kind: String,
        body_json: String,
    },
    Tombstone {
        cid: Cid,
        receipt_json: String,
    },
}

/// Garbage-collection policy (e.g. keep N checkpoints, drop unreferenced blobs).
#[derive(Debug, Clone, Default)]
pub struct GcPolicy {
    pub keep_checkpoints: Option<u32>,
}

/// What a `gc` run did.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub removed: u64,
    pub kept: u64,
}

/// A record of a successful publish — the local receipt trail, kept beside the
/// store (`.concierge/publish-receipts.jsonl`), never inside the DAG.
/// `gateway_url` is where the root can be fetched by CID once it propagates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublishReceipt {
    pub root: String,
    pub backend: String,
    pub unix_time: u64,
    pub gateway_url: String,
    /// AgentID that signed this share (Phase 5.5); empty if unsigned.
    #[serde(default)]
    pub agent_id: String,
    /// Hex Ed25519 signature over the root CID; empty if unsigned.
    #[serde(default)]
    pub signature: String,
    /// For a website publish (Planet Pattern): the site's stable IPNS name (`k51…`).
    #[serde(default)]
    pub ipns_name: Option<String>,
    /// For a website publish: the site name this receipt is for.
    #[serde(default)]
    pub site_name: Option<String>,
}

/// Store / DAG metrics for the explorer's stats rail.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreStats {
    pub names: usize,
    pub blocks: usize,
    pub reachable: usize,
    pub orphans: usize,
    pub tombstones: usize,
}

/// A verified share that surfaced in the local "shared with me" view.
#[derive(Debug, Clone)]
pub struct SharedWithMeEntry {
    pub agent_id: String,
    pub nickname: Option<String>,
    pub root: Cid,
    pub signature: String,
    pub pointer_cid: Cid,
}

/// The stable Concierge memory operations every harness mounts through.
///
/// The host adapter never calls these directly with IPLD knowledge — it emits
/// [`crate::Event`]s and the plugin maps them onto these primitives.
pub trait CoreBinding {
    /// Write an IPLD node, returning its content address.
    fn put_node(&self, node: &Node) -> Result<Cid>;

    /// Write a raw blob with a media type, returning its content address.
    fn put_blob(&self, bytes: &[u8], media_type: &str) -> Result<Cid>;

    /// Bind a human-facing name to a CID (idempotent re-binding allowed).
    fn bind(&self, name: &str, cid: &Cid) -> Result<()>;

    /// Resolve a name to its currently bound CID.
    fn resolve(&self, name: &str) -> Result<Cid>;

    /// Fetch a record (or tombstone receipt) by CID or name.
    fn get(&self, key: &CidOrName) -> Result<Record>;

    /// Write a checkpoint over `root`, chaining `parent` if present.
    fn checkpoint(&self, label: &str, root: &Cid, parent: Option<&Cid>) -> Result<Cid>;

    /// Enumerate every CID reachable from `root` (used by CAR export and share).
    fn walk(&self, root: &Cid) -> Result<Vec<Cid>>;

    /// Run garbage collection under the given policy.
    fn gc(&self, policy: &GcPolicy) -> Result<GcReport>;

    /// Record an event record into its UTC `date`'s HAMT index and (re)bind the
    /// day root `day-YYYY-MM-DD` → `DayIndex` (Phase A.5 day tier). `event_key` is
    /// the event's stable id within the day (the HAMT key). Events stay full
    /// records; this only changes how they're reached, so the names index grows
    /// ~1/day instead of ~1/event. Returns the new `DayIndex` CID.
    fn record_event_in_day(&self, date: &str, event_key: &str, record: &Cid) -> Result<Cid>;

    /// Whether `event_key` is already recorded in `date`'s day HAMT. Cross-run
    /// idempotency: a re-ingest of an already-recorded event is a no-op, so the
    /// hook can re-read a growing session file each turn without reprocessing.
    fn day_contains(&self, date: &str, event_key: &str) -> Result<bool>;

    /// (Re)build the month/year manifest tiers from the bound `day-*` roots:
    /// `store → year → month → day` (Phase A.5, DECISIONS.md 0014.8). The tiers
    /// are derived/rebuildable, so this runs once per ingest run rather than per
    /// event — months link their days, years link their months, and re-running is
    /// idempotent (content-addressed → identical CIDs). `month-YYYY-MM` /
    /// `year-YYYY` are (re)bound to the manifests.
    fn roll_up_calendar(&self) -> Result<()>;
}

/// The UTC calendar day (`YYYY-MM-DD`) for a Unix timestamp — the day-bucket key
/// used by [`CoreBinding::record_event_in_day`] and the explorer's calendar
/// grouping. Callers with an RFC 3339 timestamp can slice its first 10 chars.
pub fn utc_date(unix_secs: u64) -> String {
    mem::tombstones::iso8601(unix_secs)[0..10].to_string()
}

/// Today's UTC date — the fallback for events that arrive without a timestamp
/// (e.g. markdown backfill sets `ts = ""`).
pub fn utc_today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_date(now)
}

fn cid_to_bytes(cid: &Cid) -> Result<Vec<u8>> {
    let parsed: cid::Cid = cid
        .0
        .parse()
        .map_err(|e| Error::Io(format!("invalid CID {}: {e}", cid.0)))?;
    Ok(parsed.to_bytes())
}

fn latest_share_name(agent_id: &str) -> String {
    format!("latest-share-{agent_id}")
}

fn shared_with_me_name(agent_id: &str) -> String {
    format!("shared-with-me-{agent_id}")
}

fn room_latest_name(room: &str) -> String {
    format!("room-latest-{room}")
}

/// Index name mapping a message's **install-independent id** (its Ed25519
/// signature — deterministic per RFC 8032) to its local block CID. Messages link
/// to parents by this id, so threads cohere across installs even though `mem`
/// stamps a per-install `created_at` (which makes block CIDs install-specific).
fn message_id_name(sig: &str) -> String {
    format!("msg-{sig}")
}

/// Pull a [`MessageEnvelope`] out of a `mem` record whose `body.text` carries it.
fn parse_message_envelope(body_json: &str) -> Result<MessageEnvelope> {
    let value: serde_json::Value = serde_json::from_str(body_json)
        .map_err(|e| Error::Io(format!("parse message record: {e}")))?;
    let text = value
        .get("body")
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| Error::Io("message record missing body.text".to_string()))?;
    serde_json::from_str(text).map_err(|e| Error::Io(format!("parse message envelope: {e}")))
}

/// Render a tombstone receipt in the same shape `mem cat` printed for a pruned
/// CID, so the in-process [`CoreBinding::get`] returns an identical
/// `Record::Tombstone` body.
fn format_receipt(cid: &Cid, t: &mem::tombstones::Tombstone) -> String {
    let see = match t.superseded_by {
        Some(next) => next.to_string(),
        None => "(nothing — orphan)".to_string(),
    };
    format!(
        "{} was pruned\n  died:   {} ({})\n  reason: {}\n  see:    {}",
        cid.0,
        mem::tombstones::iso8601(t.pruned_at),
        t.pruned_at,
        t.reason,
        see
    )
}

/// Parse a plugin [`Cid`] (string form) into the `cid` crate's typed `Cid`, as
/// the CAR codec and `iroh-car` expect.
fn to_ipld_cid(cid: &Cid) -> Result<cid::Cid> {
    cid.0
        .parse()
        .map_err(|e| Error::Io(format!("invalid CID {}: {e}", cid.0)))
}

fn bytes_to_cid(bytes: &[u8]) -> Result<Cid> {
    let parsed = <cid::Cid as std::convert::TryFrom<Vec<u8>>>::try_from(bytes.to_vec())
        .map_err(|e| Error::Io(format!("invalid CID bytes: {e}")))?;
    Ok(Cid(parsed.to_string()))
}

fn cid_to_json(cid: &Cid) -> Result<serde_json::Value> {
    let bytes = cid_to_bytes(cid)?;
    Ok(serde_json::Value::Array(
        bytes
            .into_iter()
            .map(|b| serde_json::Value::Number(serde_json::Number::from(b)))
            .collect(),
    ))
}

fn json_to_cid(value: &serde_json::Value) -> Result<Cid> {
    let bytes = value
        .as_array()
        .ok_or_else(|| Error::Io("CID link must be a JSON byte array".to_string()))?
        .iter()
        .map(|n| {
            let b = n
                .as_u64()
                .ok_or_else(|| Error::Io("CID link byte must be an integer".to_string()))?;
            if b > u8::MAX as u64 {
                return Err(Error::Io("CID link byte out of range".to_string()));
            }
            Ok(b as u8)
        })
        .collect::<Result<Vec<u8>>>()?;
    bytes_to_cid(&bytes)
}

/// Encode a [`Cid`] into the JSON link form `mem` expects for CID-typed fields
/// (a byte array). Adapters use this to build link-bearing nodes — e.g. a
/// `FileRef`'s `content` — and write them through [`CoreBinding::put_node`],
/// without re-implementing the CID→bytes encoding the binding already owns.
pub fn cid_link(cid: &Cid) -> Result<serde_json::Value> {
    cid_to_json(cid)
}

/// Decode the JSON link form `mem` uses for CID-typed fields back into a
/// [`Cid`]. CLI commands use this to read checkpoint roots from record JSON
/// without duplicating the byte-array decoding rules.
pub fn cid_from_link(value: &serde_json::Value) -> Result<Cid> {
    json_to_cid(value)
}

#[derive(Debug, Clone, serde::Deserialize)]
struct MemRecord {
    #[allow(dead_code)]
    schema_version: u16,
    #[allow(dead_code)]
    created_at: u64,
    #[allow(dead_code)]
    source: serde_json::Value,
    #[allow(dead_code)]
    edges: Vec<MemEdge>,
    body: MemNode,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct MemEdge {
    #[allow(dead_code)]
    rel: serde_json::Value,
    #[allow(dead_code)]
    to: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MemNode {
    Memory {
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        kind: String,
    },
    UserPrefs {
        #[allow(dead_code)]
        entries: Vec<(String, String)>,
    },
    Plan {
        #[allow(dead_code)]
        title: String,
        #[allow(dead_code)]
        prose: String,
        #[serde(default)]
        spec: Option<serde_json::Value>,
    },
    Decision {
        #[allow(dead_code)]
        question: String,
        #[allow(dead_code)]
        choice: String,
        #[allow(dead_code)]
        rationale: String,
    },
    Prompt {
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        #[serde(default)]
        model: Option<String>,
    },
    Response {
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        model: String,
    },
    ToolResult {
        #[allow(dead_code)]
        tool: String,
        #[allow(dead_code)]
        input: String,
        #[allow(dead_code)]
        output: String,
        #[allow(dead_code)]
        ok: bool,
    },
    Blob {
        #[allow(dead_code)]
        bytes: Vec<u8>,
        #[allow(dead_code)]
        #[serde(default)]
        media_type: Option<String>,
    },
    FileRef {
        #[allow(dead_code)]
        path: String,
        #[allow(dead_code)]
        #[serde(default)]
        size: Option<u64>,
        #[allow(dead_code)]
        #[serde(default)]
        media_type: Option<String>,
        #[allow(dead_code)]
        #[serde(default)]
        mtime: Option<u64>,
        content: serde_json::Value,
    },
    Task {
        #[allow(dead_code)]
        title: String,
        #[allow(dead_code)]
        prose: String,
        #[serde(default)]
        parent: Option<serde_json::Value>,
    },
    Conversation {
        turns: Vec<serde_json::Value>,
        #[serde(default)]
        parent: Option<serde_json::Value>,
    },
    Skill {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        body: String,
        #[serde(default)]
        supersedes: Option<serde_json::Value>,
    },
    Checkpoint {
        #[allow(dead_code)]
        label: String,
        root: serde_json::Value,
        #[serde(default)]
        parent: Option<serde_json::Value>,
    },
    DirectoryManifest {
        #[allow(dead_code)]
        root_path: String,
        entries: Vec<MemDirectoryEntry>,
    },
    IngestRun {
        #[allow(dead_code)]
        source_path: String,
        manifest: serde_json::Value,
        #[allow(dead_code)]
        file_count: u64,
        #[allow(dead_code)]
        byte_count: u64,
        #[allow(dead_code)]
        ignored_count: u64,
        #[allow(dead_code)]
        plugin_records: u64,
        #[allow(dead_code)]
        plugin_failures: u64,
        #[allow(dead_code)]
        per_file_plugin_records: std::collections::BTreeMap<String, u64>,
        #[allow(dead_code)]
        per_file_plugin_failures: std::collections::BTreeMap<String, u64>,
    },
    Symbol {
        #[allow(dead_code)]
        path: String,
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        kind: String,
        #[allow(dead_code)]
        language: String,
        #[allow(dead_code)]
        signature: String,
        #[allow(dead_code)]
        body: String,
        #[allow(dead_code)]
        start_line: u32,
        #[allow(dead_code)]
        end_line: u32,
    },
    ExtractedText {
        #[allow(dead_code)]
        path: String,
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        #[serde(default)]
        media_type: Option<String>,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
struct MemDirectoryEntry {
    #[allow(dead_code)]
    path: String,
    file_ref: serde_json::Value,
}

impl MemNode {
    fn links(&self) -> Result<Vec<Cid>> {
        let mut out = Vec::new();
        let mut push_link = |value: &serde_json::Value| -> Result<()> {
            out.push(json_to_cid(value)?);
            Ok(())
        };
        match self {
            MemNode::Plan { spec: Some(v), .. } => push_link(v)?,
            MemNode::Plan { spec: None, .. } => {}
            MemNode::FileRef { content, .. } => push_link(content)?,
            MemNode::Task {
                parent: Some(v), ..
            } => push_link(v)?,
            MemNode::Task { parent: None, .. } => {}
            MemNode::Conversation { turns, parent } => {
                for v in turns {
                    push_link(v)?;
                }
                if let Some(v) = parent {
                    push_link(v)?;
                }
            }
            MemNode::Skill {
                supersedes: Some(v),
                ..
            } => push_link(v)?,
            MemNode::Skill {
                supersedes: None, ..
            } => {}
            MemNode::Checkpoint { root, parent, .. } => {
                push_link(root)?;
                if let Some(v) = parent {
                    push_link(v)?;
                }
            }
            MemNode::DirectoryManifest { entries, .. } => {
                for entry in entries {
                    push_link(&entry.file_ref)?;
                }
            }
            MemNode::IngestRun { manifest, .. } => push_link(manifest)?,
            _ => {}
        }
        Ok(out)
    }
}

/// Phase 1 implementation: shell out to the installed `mem` CLI.
///
/// `mem` keys its `.concierge` store off the current working directory, so the
/// plugin gets store isolation by running the CLI in a distinct project root.
/// Phase 1 keeps the binding thin and explicit: wrapper methods shape the CLI
/// into the stable core API, while a direct crate-link seam remains available
/// later if we need richer calls.
#[derive(Debug, Clone)]
pub struct MemCli {
    working_dir: PathBuf,
    pub(crate) security_session_id: String,
}

impl MemCli {
    fn new_security_session_id() -> String {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Open an in-process binding to the `mem` store, scoped to `working_dir`
    /// (the parent of the `.concierge` store). The `mem` library is linked
    /// directly (Phase A) — no subprocess.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            security_session_id: Self::new_security_session_id(),
        }
    }

    /// The directory this binding is scoped to (the parent of `.concierge`).
    /// Used by the egress guard to locate the security overlay, and by the GUI's
    /// capture loop to derive the store path and the ingest base dir.
    pub fn working_dir(&self) -> &std::path::Path {
        &self.working_dir
    }

    /// Load the project config, defaulting to the plan's Phase 1 values if no
    /// config file exists yet.
    pub fn config(&self) -> Result<Config> {
        if !self.working_dir.try_exists().unwrap_or(false) || !self.working_dir.is_dir() {
            return Err(Error::StoreNotFound(self.working_dir.display().to_string()));
        }
        Config::load_from_project_root(&self.working_dir).map_err(|e| Error::Io(e.to_string()))
    }

    fn put_json_value(&self, value: serde_json::Value) -> Result<Cid> {
        let obj = value
            .as_object()
            .ok_or_else(|| Error::Io("node JSON must be a JSON object".to_string()))?;
        if !obj.contains_key("type") {
            return Err(Error::Io("node JSON is missing a `type` field".to_string()));
        }
        // In-process (Phase A): deserialize straight into mem's typed node and
        // write through the linked store. No `mem put -` subprocess, so blob nodes
        // no longer risk the OS argv limit and there is no spawn/pipe overhead.
        let node: mem::node::Node = serde_json::from_value(value)
            .map_err(|e| Error::CidNotFound(format!("invalid node json: {e}")))?;
        let store = self.open_store()?;
        let cid = store
            .put_node(node, mem::node::Source::User)
            .map_err(|e| Error::Io(format!("put node: {e}")))?;
        Ok(Cid(cid.to_string()))
    }

    /// Like [`put_json_value`](Self::put_json_value), but stamps a **deterministic**
    /// `created_at` rather than the wall clock — for derived, rebuildable nodes
    /// (the calendar manifests) whose CID must be stable across re-derivation.
    fn put_json_value_at(&self, value: serde_json::Value, created_at: u64) -> Result<Cid> {
        let obj = value
            .as_object()
            .ok_or_else(|| Error::Io("node JSON must be a JSON object".to_string()))?;
        if !obj.contains_key("type") {
            return Err(Error::Io("node JSON is missing a `type` field".to_string()));
        }
        let node: mem::node::Node = serde_json::from_value(value)
            .map_err(|e| Error::CidNotFound(format!("invalid node json: {e}")))?;
        let store = self.open_store()?;
        let cid = store
            .put_node_at(node, mem::node::Source::User, created_at)
            .map_err(|e| Error::Io(format!("put node: {e}")))?;
        Ok(Cid(cid.to_string()))
    }

    fn parse_live_record(&self, stdout: &str, cid: Cid) -> Result<Record> {
        let value: serde_json::Value = serde_json::from_str(stdout)
            .map_err(|e| Error::Io(format!("could not parse `mem cat` record JSON: {e}")))?;
        let body = value
            .get("body")
            .ok_or_else(|| Error::Io("record JSON missing body".to_string()))?;
        let kind = body
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok(Record::Live {
            cid,
            kind,
            body_json: stdout.to_string(),
        })
    }

    /// The immediate (non-recursive) links of a single block — what it points at,
    /// **without requiring those targets to be present**. Sync uses this to walk a
    /// graph downward as blocks arrive (`walk` can't: it recurses into not-yet-
    /// fetched children). Returns empty for a leaf or a block that does not decode
    /// as a linked node (e.g. opaque ciphertext).
    pub fn block_links(&self, cid: &Cid) -> Result<Vec<Cid>> {
        match self.get(&CidOrName::Cid(cid.clone())) {
            Ok(Record::Live { body_json, .. }) => {
                Ok(self.record_links(&body_json).unwrap_or_default())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn record_links(&self, stdout: &str) -> Result<Vec<Cid>> {
        let value: serde_json::Value = serde_json::from_str(stdout)
            .map_err(|e| Error::Io(format!("could not parse record JSON for walk: {e}")))?;
        let edges = value
            .get("edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let record: MemRecord = serde_json::from_value(value.clone())
            .map_err(|e| Error::Io(format!("could not decode record body: {e}")))?;
        let mut links = record.body.links()?;
        if value
            .get("source")
            .and_then(|source| source.get("kind"))
            .and_then(|kind| kind.as_str())
            == Some("derived")
        {
            if let Some(from) = value
                .get("source")
                .and_then(|source| source.get("from"))
                .and_then(|from| from.as_array())
            {
                for link in from {
                    links.push(json_to_cid(link)?);
                }
            }
        }
        for edge in edges {
            let to = edge
                .get("to")
                .ok_or_else(|| Error::Io("edge missing `to` link".to_string()))?;
            links.push(json_to_cid(to)?);
        }
        links.sort();
        links.dedup();
        Ok(links)
    }

    fn walk_inner(
        &self,
        root: &Cid,
        visited: &mut BTreeSet<Cid>,
        out: &mut Vec<Cid>,
    ) -> Result<()> {
        if !visited.insert(root.clone()) {
            return Ok(());
        }
        let record = self.get(&CidOrName::Cid(root.clone()))?;
        match record {
            Record::Live { body_json, .. } => {
                out.push(root.clone());
                for child in self.record_links(&body_json)? {
                    self.walk_inner(&child, visited, out)?;
                }
                Ok(())
            }
            Record::Tombstone { receipt_json, .. } => Err(Error::Tombstoned(receipt_json)),
        }
    }

    /// Absolute path to the content-addressed block directory (`.concierge/blocks`).
    fn blocks_dir(&self) -> Result<PathBuf> {
        Ok(self
            .working_dir
            .join(self.config()?.store.root.join("blocks")))
    }

    /// Open the `mem` store in-process, rooted at the same `.concierge` path the
    /// CLI used (`working_dir` + the config's store root). Built per call to
    /// mirror the CLI's load→op→persist model — `NameIndex` writes atomically, so
    /// concurrent writers stay safe (central-store discipline). `config()` already
    /// fails with `StoreNotFound` when `working_dir` is absent.
    fn open_store(&self) -> Result<mem::store::Store<mem::blockstore::LocalBlocks>> {
        let root = self.working_dir.join(self.config()?.store.root);
        let blocks = mem::blockstore::LocalBlocks::new(root.join("blocks"));
        let names = mem::names::NameIndex::load(root.join("names.json"))
            .map_err(|e| Error::Io(format!("open names index: {e}")))?;
        Ok(mem::store::Store::new(blocks, names))
    }

    /// A `LocalBlocks` over this store's block dir, for the day-tier HAMT (which
    /// operates on raw content-addressed blocks rather than the typed `Store`).
    fn blockstore(&self) -> Result<mem::blockstore::LocalBlocks> {
        Ok(mem::blockstore::LocalBlocks::new(self.blocks_dir()?))
    }

    /// The `(event_key, record CID)` pairs recorded in `date`'s day HAMT — for the
    /// explorer to fan a day out to its events (Phase A.5). Empty if no day root is
    /// bound for that date. Order is by key hash, not insertion.
    pub fn day_events(&self, date: &str) -> Result<Vec<(String, Cid)>> {
        let day_name = format!("day-{date}");
        let Some(hamt_root) = self.day_hamt_root(&day_name)? else {
            return Ok(Vec::new());
        };
        let blocks = self.blockstore()?;
        let hamt: mem::hamt::Hamt<_, mem::cid::Cid> = mem::hamt::Hamt::load(&blocks, &hamt_root)
            .map_err(|e| Error::Io(format!("load day hamt: {e}")))?;
        let mut out = Vec::new();
        hamt.for_each(|key, cid| {
            out.push((
                String::from_utf8_lossy(key).into_owned(),
                Cid(cid.to_string()),
            ));
            Ok(())
        })
        .map_err(|e| Error::Io(format!("hamt iterate: {e}")))?;
        Ok(out)
    }

    /// The HAMT root CID inside the currently-bound `DayIndex` for `day_name`, or
    /// `None` if no day root is bound yet.
    fn day_hamt_root(&self, day_name: &str) -> Result<Option<mem::cid::Cid>> {
        let day_cid = match self.resolve(day_name) {
            Ok(cid) => cid,
            Err(Error::NameUnbound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let body_json = match self.get(&CidOrName::Cid(day_cid))? {
            Record::Live { body_json, .. } => body_json,
            Record::Tombstone { .. } => return Ok(None),
        };
        let value: serde_json::Value = serde_json::from_str(&body_json)
            .map_err(|e| Error::Io(format!("parse day index: {e}")))?;
        let events = value
            .get("body")
            .and_then(|body| body.get("events"))
            .ok_or_else(|| Error::Io("day index missing events link".to_string()))?;
        let core_cid = cid_from_link(events)?;
        let parsed: mem::cid::Cid = core_cid
            .0
            .parse()
            .map_err(|e| Error::CidNotFound(format!("invalid day hamt cid {}: {e}", core_cid.0)))?;
        Ok(Some(parsed))
    }

    /// Read a block's raw bytes from the store by CID.
    pub(crate) fn read_block(&self, cid: &Cid) -> Result<Vec<u8>> {
        let path = self.blocks_dir()?.join(&cid.0);
        std::fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::CidNotFound(cid.0.clone()),
            _ => Error::Io(format!("read block {}: {e}", cid.0)),
        })
    }

    /// Store an already content-addressed raw block after verifying its CID.
    /// Capability-encrypted blocks use this path so they remain interoperable
    /// with CAR/Kubo while staying opaque to the plaintext `mem` decoder.
    pub(crate) fn store_verified_raw_block(&self, cid: &Cid, bytes: &[u8]) -> Result<()> {
        let parsed = to_ipld_cid(cid)?;
        crate::car::verify_block(&parsed, bytes)?;
        let blocks_dir = self.blocks_dir()?;
        std::fs::create_dir_all(&blocks_dir)
            .map_err(|error| Error::Io(format!("create blocks dir: {error}")))?;
        let path = blocks_dir.join(&cid.0);
        if path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect raw block {}: {error}", cid.0)))?
        {
            let existing = std::fs::read(&path)
                .map_err(|error| Error::Io(format!("read raw block {}: {error}", cid.0)))?;
            if existing != bytes {
                return Err(Error::SecurityPolicy(format!(
                    "existing block bytes do not match verified CID {}",
                    cid.0
                )));
            }
            return Ok(());
        }
        std::fs::write(&path, bytes)
            .map_err(|error| Error::Io(format!("write raw block {}: {error}", cid.0)))
    }

    /// Export the subgraph reachable from `root` as CARv1 bytes (root in the
    /// header). Blocks are the store's own bytes, so their CIDs are preserved.
    pub(crate) fn export_car(&self, root: &Cid) -> Result<Vec<u8>> {
        let cids = self.walk(root)?;
        let mut blocks = Vec::with_capacity(cids.len());
        for cid in &cids {
            blocks.push((to_ipld_cid(cid)?, self.read_block(cid)?));
        }
        crate::car::build_car(&to_ipld_cid(root)?, &blocks)
    }

    /// Write the exact reviewed plaintext-CAR plan while holding the
    /// cross-process policy lock through the filesystem egress.
    pub fn write_reviewed_plaintext_car(
        &self,
        reviewed: &crate::egress::EgressPlan,
        path: &std::path::Path,
    ) -> Result<u64> {
        if reviewed.operation != crate::egress::EgressOperation::PlaintextCarExport {
            return Err(Error::EgressPlanChanged(
                "reviewed plan is not a plaintext CAR export".to_string(),
            ));
        }
        if reviewed.backend != "local-file" || reviewed.backend_target != path.display().to_string()
        {
            return Err(Error::EgressPlanChanged(
                "reviewed plaintext export destination changed".to_string(),
            ));
        }
        self.execute_approved_egress(reviewed, |approved| {
            let car = self.export_car(&approved.root)?;
            std::fs::write(path, &car)
                .map_err(|error| Error::Io(format!("write plaintext CAR: {error}")))?;
            Ok(car.len() as u64)
        })
    }

    /// The CIDs and total block byte size a CAR for `root` would contain — the
    /// dry-run / manifest preview, computed without writing anything.
    /// Same reachable set as [`Binding::walk`] (the record-link closure, erroring
    /// on a tombstone or missing block within it) but fetched with one batched
    /// `get_many` per BFS level instead of one lookup per node. The set
    /// is identical; only the order differs, which is safe because every consumer
    /// (manifest digest, byte total, block shipping) is order-independent.
    pub(crate) fn walk_batched(&self, root: &Cid) -> Result<Vec<Cid>> {
        let mut visited: BTreeSet<Cid> = BTreeSet::new();
        let mut out: Vec<Cid> = Vec::new();
        visited.insert(root.clone());
        let mut frontier: Vec<Cid> = vec![root.clone()];
        while !frontier.is_empty() {
            let level = std::mem::take(&mut frontier);
            let fetched = self.get_many(&level)?;
            for cid in &level {
                match fetched.get(&cid.0) {
                    Some(Record::Live { body_json, .. }) => {
                        out.push(cid.clone());
                        for child in self.record_links(body_json)? {
                            if visited.insert(child.clone()) {
                                frontier.push(child);
                            }
                        }
                    }
                    Some(Record::Tombstone { receipt_json, .. }) => {
                        return Err(Error::Tombstoned(receipt_json.clone()));
                    }
                    // `get_many` omits a CID it could not read; `walk` would have
                    // surfaced that as a hard error, so preserve the strictness.
                    None => return Err(Error::CidNotFound(cid.0.clone())),
                }
            }
        }
        Ok(out)
    }

    pub fn export_car_manifest(&self, root: &Cid) -> Result<(Vec<Cid>, u64)> {
        let cids = self.walk_batched(root)?;
        let mut total: u64 = 0;
        for cid in &cids {
            total += self.read_block(cid)?.len() as u64;
        }
        Ok((cids, total))
    }

    /// Import CARv1 bytes: **verify every block's CID**, write the blocks into
    /// the store, bind the root under `name`, and return the root CID. A tampered
    /// block aborts the import before anything is written or bound.
    pub fn import_car(&self, car: &[u8], name: &str) -> Result<Cid> {
        let (roots, blocks) = crate::car::read_car_verified(car)?;
        let root = roots
            .first()
            .ok_or_else(|| Error::Io("CAR has no root in its header".to_string()))?;
        self.import_verified_car(root, &blocks, name)
    }

    /// Import a signed share: verify the signer before accepting the root.
    pub fn import_signed_car(
        &self,
        car: &[u8],
        name: &str,
        agent_id: &str,
        signature: &str,
    ) -> Result<Cid> {
        let (roots, blocks) = crate::car::read_car_verified(car)?;
        let root = roots
            .first()
            .ok_or_else(|| Error::Io("CAR has no root in its header".to_string()))?;
        let root_cid = Cid(root.to_string());

        let book = self.social_book()?;
        if !book.is_following(agent_id) {
            return Err(Error::Io(format!(
                "unknown signer `{agent_id}` is not on the follow list"
            )));
        }
        if !self.verify_share(&root_cid, agent_id, signature)? {
            return Err(Error::Io(format!(
                "share signature did not verify for `{agent_id}`"
            )));
        }

        let imported_root = self.import_verified_car(root, &blocks, name)?;
        self.record_shared_with_me(agent_id, &imported_root, signature)?;
        Ok(imported_root)
    }

    /// The publishing backends compiled into this build, with their
    /// requirements manifest for display.
    pub fn list_backends(&self) -> Result<Vec<BackendInfo>> {
        Ok(available_backends(&self.config()?))
    }

    /// Every name → CID binding in the store (the names browser source).
    pub fn names(&self) -> Result<Vec<(String, Cid)>> {
        let store = self.open_store()?;
        Ok(store
            .names()
            .map(|(name, cid)| (name.to_string(), Cid(cid.to_string())))
            .collect())
    }

    /// The direct outbound links of a record (its edges + structural link fields)
    /// — the clickable references in the record viewer.
    pub fn outbound_links(&self, cid: &Cid) -> Result<Vec<Cid>> {
        match self.get(&CidOrName::Cid(cid.clone()))? {
            Record::Live { body_json, .. } => self.record_links(&body_json),
            Record::Tombstone { .. } => Ok(Vec::new()),
        }
    }

    /// The direct outbound links of a record whose JSON is already in hand — the
    /// same result as [`Self::outbound_links`] but with no `mem` round-trip.
    /// Callers that have just fetched a record (e.g. via [`Self::get_many`]) use
    /// this to avoid re-`get`ting it.
    pub fn links_from_record_json(&self, record_json: &str) -> Result<Vec<Cid>> {
        self.record_links(record_json)
    }

    /// Fetch many records against a single in-process store. The block/name
    /// indexes load once and each CID is looked up directly, so building a
    /// whole-store graph costs one store open instead of one spawn per node.
    /// Tombstoned CIDs are returned alongside live ones; CIDs that fail to
    /// parse/look up are dropped. Order is not guaranteed, so the result is keyed
    /// by CID string.
    pub fn get_many(&self, cids: &[Cid]) -> Result<std::collections::HashMap<String, Record>> {
        use std::collections::HashMap;
        if cids.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.open_store()?;
        let mut out = HashMap::with_capacity(cids.len());
        for cid in cids {
            let Ok(parsed) = cid.0.parse::<mem::cid::Cid>() else {
                continue; // unparseable CID: omit (matches the old "error" entry)
            };
            match store.lookup(&parsed) {
                Ok(mem::store::Lookup::Present(record)) => {
                    let Ok(body_json) = serde_json::to_string_pretty(&record) else {
                        continue;
                    };
                    if let Ok(parsed_record) = self.parse_live_record(&body_json, cid.clone()) {
                        out.insert(cid.0.clone(), parsed_record);
                    }
                }
                Ok(mem::store::Lookup::Pruned(t)) => {
                    let receipt_json = format_receipt(cid, &t);
                    out.insert(
                        cid.0.clone(),
                        Record::Tombstone {
                            cid: cid.clone(),
                            receipt_json,
                        },
                    );
                }
                Err(_) => {} // missing / undecodable: omitted from the map
            }
        }
        Ok(out)
    }

    /// Store / DAG metrics for the explorer's stats rail.
    pub fn store_stats(&self) -> Result<StoreStats> {
        let names = self.names()?;
        let blocks_dir = self.blocks_dir()?;
        let blocks = std::fs::read_dir(&blocks_dir)
            .map(|rd| {
                rd.filter(|e| e.as_ref().map(|e| e.path().is_file()).unwrap_or(false))
                    .count()
            })
            .unwrap_or(0);
        // Reachable = the union of every named root's subgraph, computed at the
        // block level in-process (no record bodies, no spawn) — orders of
        // magnitude cheaper than the old `self.walk` per name. Every bound name
        // is a root.
        let store = self.open_store()?;
        let mut reachable: BTreeSet<String> = BTreeSet::new();
        for (_name, root) in &names {
            if let Ok(parsed) = root.0.parse::<mem::cid::Cid>() {
                if let Ok(blocks) = store.reachable(&parsed) {
                    for block in blocks {
                        reachable.insert(block.to_string());
                    }
                }
            }
        }
        let tomb_path = self
            .working_dir
            .join(self.config()?.store.root.join("tombstones.json"));
        let tombstones = std::fs::read_to_string(&tomb_path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| match v {
                serde_json::Value::Array(a) => a.len(),
                serde_json::Value::Object(o) => o.len(),
                _ => 0,
            })
            .unwrap_or(0);
        Ok(StoreStats {
            names: names.len(),
            blocks,
            reachable: reachable.len(),
            orphans: blocks.saturating_sub(reachable.len()),
            tombstones,
        })
    }

    /// Configure a backend (writes `[publishing].backend` to the local config).
    pub fn add_backend(&self, name: &str) -> Result<()> {
        if !backend_exists(name) {
            return Err(Error::BackendDown(format!(
                "backend `{name}` is not compiled in"
            )));
        }
        let mut cfg = self.config()?;
        cfg.publishing.backend = name.to_string();
        cfg.save_to_project_root(&self.working_dir)
            .map_err(Error::Io)
    }

    /// Legacy ambiguous `share` never publishes. Phase A requires callers to use
    /// an explicit reviewed `publish-public` operation.
    pub fn share(&self, target: &str) -> Result<PublishReceipt> {
        let _ = target;
        Err(Error::ExplicitPublicPublishRequired)
    }

    /// Execute one explicitly reviewed public publication.
    pub fn publish_public(&self, reviewed: &crate::egress::EgressPlan) -> Result<PublishReceipt> {
        if reviewed.operation != crate::egress::EgressOperation::PublicPublish {
            return Err(Error::EgressPlanChanged(
                "reviewed plan is not a public publication".to_string(),
            ));
        }
        self.execute_approved_egress(reviewed, |approved| {
            let cfg = self.config()?;
            let mut receipt = share_via_selected_backend(self, approved, &cfg)?;
            // Sign the shared root with the AgentID: authenticity (*who* shared it) on
            // top of the CID's integrity (*what* was shared). Phase 5.5 / Decision 0007.
            let identity = self.identity()?;
            receipt.agent_id = identity.agent_id().0;
            receipt.signature = identity.sign(approved.root.0.as_bytes());
            self.append_receipt(&receipt)?;
            self.record_latest_share(&receipt, &approved.root)?;
            Ok(receipt)
        })
    }

    /// Where the published-site registry lives.
    fn sites_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("sites.json"))
    }

    /// Wait (briefly) for the public publishing node's API to come up.
    fn wait_for_public_node(&self) -> Result<()> {
        for _ in 0..40 {
            if crate::node::public_node_running() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Err(Error::BackendDown(
            "public IPFS node did not come up in time".to_string(),
        ))
    }

    fn site_deploy_destination(&self, name: &str, platform: &str) -> Result<String> {
        let credentials = self.deploy_credentials()?;
        match platform {
            "ipfs" => Ok(format!(
                "ipfs-public:{}",
                crate::node::public_repo_for(&self.store_dir()?).display()
            )),
            "github" => credentials
                .github
                .map(|c| {
                    format!(
                        "https://api.github.com/repos/{}/{}/branches/{}",
                        c.owner, c.repo, c.branch
                    )
                })
                .ok_or_else(|| Error::Io("no github credentials yet".to_string())),
            "netlify" => credentials
                .netlify
                .map(|c| {
                    format!(
                        "https://api.netlify.com/site/{}",
                        c.site_id.unwrap_or_else(|| format!("new:{name}"))
                    )
                })
                .ok_or_else(|| Error::Io("no netlify credentials yet".to_string())),
            "vercel" => credentials
                .vercel
                .map(|c| {
                    format!(
                        "https://api.vercel.com/project/{}/team/{}",
                        c.project.unwrap_or_else(|| name.to_string()),
                        c.team_id.unwrap_or_else(|| "default".to_string())
                    )
                })
                .ok_or_else(|| Error::Io("no vercel credentials yet".to_string())),
            "cloudflare" => credentials
                .cloudflare
                .map(|c| {
                    format!(
                        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}",
                        c.account_id, c.project
                    )
                })
                .ok_or_else(|| Error::Io("no cloudflare credentials yet".to_string())),
            "ftp" => Err(Error::SecurityPolicy(
                "plaintext FTP deployment is disabled".to_string(),
            )),
            other => Err(Error::Io(format!("unsupported platform: {other}"))),
        }
    }

    fn build_site_deploy_plan(
        &self,
        name: &str,
        folder: &str,
        kind: &str,
        platform: &str,
    ) -> Result<crate::deploy::SiteDeployPlan> {
        let folder_path = std::path::Path::new(folder);
        if !folder_path.is_dir() {
            return Err(Error::Io(format!("not a folder: {folder}")));
        }
        let files = crate::deploy::walk_files(folder_path).map_err(Error::Io)?;
        crate::deploy::SiteDeployPlan::from_files(
            name,
            folder_path,
            kind,
            platform,
            &self.site_deploy_destination(name, platform)?,
            &files,
        )
        .map_err(Error::Io)
    }

    /// Build the exact website deployment plan that the user must review before
    /// entering a password. Generated gallery/player front-ends are staged before
    /// the manifest is calculated so they are included in the review.
    pub fn review_site_deploy(
        &self,
        name: &str,
        folder: &str,
        kind: &str,
        platform: &str,
    ) -> Result<crate::deploy::SiteDeployPlan> {
        let folder_path = std::path::Path::new(folder);
        if !folder_path.is_dir() {
            return Err(Error::Io(format!("not a folder: {folder}")));
        }
        crate::site::write_index(folder_path, crate::site::SiteKind::parse(kind), name)?;
        self.build_site_deploy_plan(name, folder, kind, platform)
    }

    /// Publish exactly one previously reviewed website manifest. The folder and
    /// destination are recomputed and compared while the security policy lock is
    /// held, immediately before egress.
    pub fn publish_site(
        &self,
        reviewed: &crate::deploy::SiteDeployPlan,
        password: &str,
    ) -> Result<PublishReceipt> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let current = self.build_site_deploy_plan(
            &reviewed.name,
            &reviewed.folder,
            &reviewed.kind,
            &reviewed.platform,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "website files, destination, or deployment metadata changed after review"
                    .to_string(),
            ));
        }
        let event_root = Cid(format!("external-manifest:{}", reviewed.manifest_digest));
        self.append_security_event_unlocked(
            "site_deploy_approved",
            &event_root,
            &format!("{} via {}", reviewed.name, reviewed.destination),
        )?;
        let folder_path = std::path::Path::new(&reviewed.folder);
        let files = crate::deploy::walk_files(folder_path).map_err(Error::Io)?;

        match reviewed.platform.as_str() {
            "ipfs" => {
                let store = self.store_dir()?;
                let repo = crate::node::public_repo_for(&store);
                crate::node::launch_public_node(&store)?;
                self.wait_for_public_node()?;
                let ipns = crate::node::ipns_key_gen(&repo, &reviewed.name)?;
                let cid = crate::node::unixfs_add_dir(&repo, folder_path)?;
                let published = crate::node::ipns_publish(&repo, &cid, &reviewed.name)?;
                let identity = self.identity()?;
                let receipt = PublishReceipt {
                    root: cid.clone(),
                    backend: "ipfs-public".to_string(),
                    unix_time: now_secs(),
                    gateway_url: format!("https://ipfs.io/ipns/{published}"),
                    agent_id: identity.agent_id().0,
                    signature: identity.sign(cid.as_bytes()),
                    ipns_name: Some(published.clone()),
                    site_name: Some(reviewed.name.clone()),
                };
                self.append_receipt(&receipt)?;
                self.record_publication(&receipt)?;
                // Reuse the existing IPNS address if the site was published before.
                let path = self.sites_path()?;
                crate::state::update_json::<Sites, _>(&path, |sites| {
                    let ipns = sites
                        .sites
                        .get(&reviewed.name)
                        .map(|site| site.ipns.clone())
                        .unwrap_or(ipns);
                    sites.sites.insert(
                        reviewed.name.clone(),
                        SiteRecord {
                            name: reviewed.name.clone(),
                            ipns,
                            dir: reviewed.folder.clone(),
                            last_cid: Some(cid),
                            published_at: now_secs() as i64,
                        },
                    );
                    Ok(())
                })?;
                Ok(receipt)
            }
            "github" | "netlify" | "vercel" | "cloudflare" => {
                self.publish_external(reviewed, &files)
            }
            "ftp" => Err(Error::SecurityPolicy(
                "plaintext FTP deployment is disabled".to_string(),
            )),
            _ => Err(Error::Io(format!(
                "unsupported platform: {}",
                reviewed.platform
            ))),
        }
    }

    /// Path to the on-device deploy-credentials vault (`<store>/security/deploy.json`,
    /// 0600). Tokens live here and never go anywhere but their own platform's API.
    fn deploy_credentials_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("deploy.json"))
    }

    /// Load the stored deploy credentials (empty if none configured yet).
    pub fn deploy_credentials(&self) -> Result<crate::deploy::DeployCredentials> {
        let path = self.deploy_credentials_path()?;
        if path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect deploy credentials: {error}")))?
        {
            crate::egress::validate_private_file(&path)?;
        }
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Io(format!("parse deploy credentials: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::deploy::DeployCredentials::default())
            }
            Err(e) => Err(Error::Io(format!("read deploy credentials: {e}"))),
        }
    }

    /// Set (merge) the credentials for one platform from a JSON object, written
    /// 0600. `fields_json` is the platform's credential block (e.g. the GitHub
    /// `{token,owner,repo,branch?}`); an explicit JSON `null` clears it.
    pub fn set_deploy_credentials(&self, platform: &str, fields_json: &str) -> Result<()> {
        let mut creds = self.deploy_credentials()?;
        let value: serde_json::Value = serde_json::from_str(fields_json)
            .map_err(|e| Error::Io(format!("parse credential fields: {e}")))?;
        let cleared = value.is_null();
        macro_rules! merge {
            ($field:ident) => {{
                if cleared {
                    creds.$field = None;
                } else {
                    creds.$field =
                        Some(serde_json::from_value(value.clone()).map_err(|e| {
                            Error::Io(format!("invalid {platform} credentials: {e}"))
                        })?);
                }
            }};
        }
        match platform {
            "github" => merge!(github),
            "netlify" => merge!(netlify),
            "vercel" => merge!(vercel),
            "cloudflare" => merge!(cloudflare),
            "ftp" => {
                return Err(Error::SecurityPolicy(
                    "plaintext FTP credentials are not accepted".to_string(),
                ))
            }
            other => return Err(Error::Io(format!("unknown deploy platform: {other}"))),
        }
        let required = |label: &str, value: &str| -> Result<()> {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                Err(Error::SecurityPolicy(format!(
                    "{label} must be non-empty and contain no control characters"
                )))
            } else {
                Ok(())
            }
        };
        match platform {
            "github" if !cleared => {
                let c = creds
                    .github
                    .as_ref()
                    .expect("github credentials were just parsed");
                required("github token", &c.token)?;
                required("github owner", &c.owner)?;
                required("github repository", &c.repo)?;
                required("github branch", &c.branch)?;
                if c.owner.contains('/') || c.repo.contains('/') || c.branch.contains("..") {
                    return Err(Error::SecurityPolicy(
                        "github owner, repository, or branch contains an unsafe path component"
                            .to_string(),
                    ));
                }
            }
            "netlify" if !cleared => {
                required(
                    "netlify token",
                    &creds
                        .netlify
                        .as_ref()
                        .expect("netlify credentials were just parsed")
                        .token,
                )?;
            }
            "vercel" if !cleared => {
                required(
                    "vercel token",
                    &creds
                        .vercel
                        .as_ref()
                        .expect("vercel credentials were just parsed")
                        .token,
                )?;
            }
            "cloudflare" if !cleared => {
                let c = creds
                    .cloudflare
                    .as_ref()
                    .expect("cloudflare credentials were just parsed");
                required("cloudflare token", &c.token)?;
                required("cloudflare account id", &c.account_id)?;
                required("cloudflare project", &c.project)?;
                if c.account_id.contains('/') || c.project.contains('/') {
                    return Err(Error::SecurityPolicy(
                        "cloudflare account or project contains an unsafe path component"
                            .to_string(),
                    ));
                }
            }
            _ => {}
        }
        self.ensure_security_dir()?;
        let bytes = serde_json::to_vec_pretty(&creds)
            .map_err(|e| Error::Io(format!("serialize deploy credentials: {e}")))?;
        crate::egress::atomic_private_write(&self.deploy_credentials_path()?, &bytes)
    }

    /// Non-secret status: which platforms are configured + their public fields
    /// (owner/repo/project/host). Tokens/passwords are NEVER returned to the GUI.
    pub fn deploy_status(&self) -> Result<serde_json::Value> {
        let c = self.deploy_credentials()?;
        Ok(serde_json::json!({
            "github": c.github.as_ref().map(|g| serde_json::json!({
                "owner": g.owner, "repo": g.repo, "branch": g.branch })),
            "netlify": c.netlify.as_ref().map(|n| serde_json::json!({
                "site_id": n.site_id })),
            "vercel": c.vercel.as_ref().map(|v| serde_json::json!({
                "project": v.project, "team_id": v.team_id })),
            "cloudflare": c.cloudflare.as_ref().map(|c| serde_json::json!({
                "account_id": c.account_id, "project": c.project })),
        }))
    }

    /// Verify a platform's credentials live against its API (the "Test connection"
    /// step of the connect walk-through). `fields_json` lets the GUI test *unsaved*
    /// input (a single platform's `{token,…}` block) before saving; when `None` the
    /// stored credentials are tested. Returns a short account label on success.
    pub fn verify_deploy_credentials(
        &self,
        platform: &str,
        fields_json: Option<&str>,
    ) -> Result<String> {
        let creds = match fields_json {
            Some(json) if !json.trim().is_empty() && json.trim() != "null" => {
                let value: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| Error::Io(format!("parse credential fields: {e}")))?;
                let mut c = crate::deploy::DeployCredentials::default();
                macro_rules! set {
                    ($field:ident) => {
                        c.$field = Some(serde_json::from_value(value.clone()).map_err(|e| {
                            Error::Io(format!("invalid {platform} credentials: {e}"))
                        })?)
                    };
                }
                match platform {
                    "github" => set!(github),
                    "netlify" => set!(netlify),
                    "vercel" => set!(vercel),
                    "cloudflare" => set!(cloudflare),
                    other => return Err(Error::Io(format!("unknown deploy platform: {other}"))),
                }
                c
            }
            _ => self.deploy_credentials()?,
        };
        crate::deploy::verify(platform, &creds).map_err(Error::Io)
    }

    /// Deploy the staged folder to an external Web2 host using the stored
    /// credentials. Password is already verified upstream (`publish_site`); this is
    /// explicit, gated egress. Returns a real receipt with the live URL.
    fn publish_external(
        &self,
        reviewed: &crate::deploy::SiteDeployPlan,
        files: &[crate::deploy::DeployFile],
    ) -> Result<PublishReceipt> {
        let identity = self.identity()?;
        let creds = self.deploy_credentials()?;
        let platform = reviewed.platform.as_str();

        let missing = || {
            Error::Io(format!(
                "no {platform} credentials yet — add them in Studio → Deploy settings"
            ))
        };
        let url = match platform {
            "github" => crate::deploy::deploy_github(&creds.github.ok_or_else(missing)?, files),
            "netlify" => crate::deploy::deploy_netlify(
                &creds.netlify.ok_or_else(missing)?,
                files,
                &reviewed.name,
            ),
            "vercel" => crate::deploy::deploy_vercel(
                &creds.vercel.ok_or_else(missing)?,
                files,
                &reviewed.name,
            ),
            "cloudflare" => {
                crate::deploy::deploy_cloudflare(&creds.cloudflare.ok_or_else(missing)?, files)
            }
            other => return Err(Error::Io(format!("unsupported platform: {other}"))),
        }
        .map_err(Error::Io)?;

        let signed = format!(
            "{}\n{}\n{}\n{}",
            reviewed.manifest_digest, reviewed.destination, platform, url
        );
        let receipt = PublishReceipt {
            root: format!("external-manifest:{}", reviewed.manifest_digest),
            backend: platform.to_string(),
            unix_time: now_secs(),
            gateway_url: url.clone(),
            agent_id: identity.agent_id().0,
            signature: identity.sign(signed.as_bytes()),
            ipns_name: None,
            site_name: Some(reviewed.name.clone()),
        };
        self.append_receipt(&receipt)?;
        self.record_publication(&receipt)?;
        Ok(receipt)
    }

    /// Verify that an external-site receipt authenticates this exact reviewed
    /// manifest, destination, platform, and returned live URL.
    pub fn verify_external_site_receipt(
        &self,
        receipt: &PublishReceipt,
        reviewed: &crate::deploy::SiteDeployPlan,
    ) -> Result<bool> {
        if receipt.root != format!("external-manifest:{}", reviewed.manifest_digest)
            || receipt.backend != reviewed.platform
            || receipt.site_name.as_deref() != Some(reviewed.name.as_str())
        {
            return Ok(false);
        }
        let signed = format!(
            "{}\n{}\n{}\n{}",
            reviewed.manifest_digest, reviewed.destination, reviewed.platform, receipt.gateway_url
        );
        crate::identity::verify(
            &AgentId(receipt.agent_id.clone()),
            signed.as_bytes(),
            &receipt.signature,
        )
        .map_err(Error::Io)
    }

    /// The published sites this install knows.
    pub fn site_list(&self) -> Result<Vec<SiteRecord>> {
        Ok(Sites::load(&self.sites_path()?)
            .map_err(Error::Io)?
            .sites
            .into_values()
            .collect())
    }

    /// Forget a site from the registry (does not unpin or revoke the IPNS key).
    pub fn site_unpublish(&self, name: &str) -> Result<()> {
        let path = self.sites_path()?;
        crate::state::update_json::<Sites, _>(&path, |sites| {
            sites.sites.remove(name);
            Ok(())
        })
    }

    /// Export a site's IPNS private key to `out_path` for backup/portability.
    pub fn export_site_key(&self, name: &str, out_path: &std::path::Path) -> Result<()> {
        let repo = crate::node::public_repo_for(&self.store_dir()?);
        crate::node::ipns_key_export(&repo, name, out_path)
    }

    /// Read the local publish-receipt trail. The visual explorer uses this as
    /// the read-only source of truth for whether a root has a recorded pin.
    pub fn publish_receipts(&self) -> Result<Vec<PublishReceipt>> {
        let path = self
            .working_dir
            .join(self.config()?.store.root.join("publish-receipts.jsonl"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Io(format!("read receipt trail: {e}")))?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|e| Error::Io(format!("parse publish receipt: {e}")))
            })
            .collect()
    }

    /// Load (or first-time generate + persist) this install's AgentID identity.
    pub fn identity(&self) -> Result<Identity> {
        let key_path = self.working_dir.join(self.config()?.identity.key_path);
        Identity::load_or_create(&key_path).map_err(|e| Error::Io(format!("identity: {e}")))
    }

    /// This install's public AgentID — stable across restarts.
    pub fn agent_id(&self) -> Result<AgentId> {
        Ok(self.identity()?.agent_id())
    }

    /// Verify a signed share: does `signature` over `root` come from `agent_id`?
    pub fn verify_share(&self, root: &Cid, agent_id: &str, signature: &str) -> Result<bool> {
        crate::identity::verify(&AgentId(agent_id.to_string()), root.0.as_bytes(), signature)
            .map_err(Error::Io)
    }

    /// Verified shares from followed AgentIDs, ready to display or fetch.
    pub fn shared_with_me(&self) -> Result<Vec<SharedWithMeEntry>> {
        let book = self.social_book()?;
        let mut out = Vec::new();
        for agent_id in &book.following {
            let nickname = book.nickname_of(agent_id).cloned();
            let name = shared_with_me_name(agent_id);
            let cid = match self.resolve(&name) {
                Ok(cid) => cid,
                Err(Error::NameUnbound(_)) => continue,
                Err(e) => return Err(e),
            };
            let record = self.get(&CidOrName::Cid(cid.clone()))?;
            let Record::Live { body_json, .. } = record else {
                continue;
            };
            let (pointer_agent_id, root, signature) = parse_share_pointer(&body_json)?;
            if pointer_agent_id != *agent_id {
                continue;
            }
            let verified = self.verify_share(&root, agent_id, &signature)?;
            if verified {
                out.push(SharedWithMeEntry {
                    agent_id: agent_id.clone(),
                    nickname,
                    root,
                    signature,
                    pointer_cid: cid,
                });
            }
        }
        Ok(out)
    }

    fn social_path(&self) -> Result<PathBuf> {
        Ok(self
            .working_dir
            .join(self.config()?.store.root.join("social.json")))
    }

    /// The local petname + follow book.
    pub fn social_book(&self) -> Result<SocialBook> {
        SocialBook::load(&self.social_path()?).map_err(Error::Io)
    }

    /// Follow an AgentID (persisted to the local book; also the inbound allowlist).
    pub fn follow(&self, agent_id: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.follow(agent_id);
            Ok(())
        })
    }

    /// Give an AgentID a local petname.
    pub fn set_nickname(&self, agent_id: &str, nickname: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.set_nickname(agent_id, nickname);
            Ok(())
        })
    }

    /// Remove a petname.
    pub fn remove_nickname(&self, agent_id: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.remove_nickname(agent_id);
            Ok(())
        })
    }

    // ── Sovereign naming: Layer 2 contact cards + resolution + introductions ──

    fn own_card_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("contact-card.json"))
    }
    fn cards_dir(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("cards"))
    }
    fn introductions_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("introductions.json"))
    }

    /// Edit the local user's own contact card (the self-asserted profile fields).
    /// Stored unsigned; [`MemCli::my_card`] signs a fresh copy on demand. `None`
    /// fields are left unchanged; an empty string clears an optional field.
    pub fn update_my_card(
        &self,
        display_name: Option<&str>,
        bio: Option<&str>,
        avatar: Option<&str>,
        site_ipns: Option<&str>,
    ) -> Result<()> {
        let path = self.own_card_path()?;
        crate::state::update_json::<ContactCard, _>(&path, |card| {
            if let Some(n) = display_name {
                card.display_name = n.to_string();
            }
            let opt = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
            if let Some(b) = bio {
                card.bio = opt(b);
            }
            if let Some(a) = avatar {
                card.avatar = opt(a);
            }
            if let Some(s) = site_ipns {
                card.site_ipns = opt(s);
            }
            card.sig = String::new();
            validate_contact_card_limits(card)
        })
    }

    /// Build and **sign** the user's current contact card (refreshing `updated_at`).
    pub fn my_card(&self) -> Result<ContactCard> {
        let identity = self.identity()?;
        let aid = identity.agent_id();
        let did = naming::did_key_from_agent(&aid).map_err(Error::Io)?;
        let mut card: ContactCard = std::fs::read_to_string(self.own_card_path()?)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        card.did = did;
        if card.display_name.trim().is_empty() {
            card.display_name = naming::short_agent(&aid.0);
        }
        card.updated_at = now_secs();
        card.sign(&identity);
        validate_contact_card_limits(&card)?;
        Ok(card)
    }

    /// Import a peer's signed card: verify the signature, then cache it. Only the
    /// self-authenticating part is trusted; the name stays a hint until petnamed.
    /// Returns the author's AgentID hex.
    pub fn import_card(&self, card_json: &str) -> Result<String> {
        let card: ContactCard = serde_json::from_str(card_json)
            .map_err(|e| Error::Io(format!("parse contact card: {e}")))?;
        if !card.verify() {
            return Err(Error::Io(
                "contact card signature does not verify".to_string(),
            ));
        }
        validate_contact_card_limits(&card)?;
        let aid = card.agent_id().map_err(Error::Io)?;
        let dir = self.cards_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(e.to_string()))?;
        let path = dir.join(format!("{}.json", aid.0));
        if let Some(existing) = self.card_of(&aid.0)? {
            if card.updated_at <= existing.updated_at {
                return Err(Error::SecurityPolicy(
                    "stale or duplicate contact card update rejected".to_string(),
                ));
            }
        }
        crate::state::save_json(&path, &card)?;
        Ok(aid.0)
    }

    /// The cached, verified card for an AgentID, if any.
    pub fn card_of(&self, agent_id: &str) -> Result<Option<ContactCard>> {
        let path = self.cards_dir()?.join(format!("{agent_id}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let card: ContactCard = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        // Re-verify on read — a cached file is only trusted if it still verifies.
        Ok(card.verify().then_some(card))
    }

    fn load_introductions(&self) -> Vec<Introduction> {
        std::fs::read_to_string(self.introductions_path().unwrap_or_default())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Introduction>>(&s).ok())
            .unwrap_or_default()
    }

    /// Resolve an AgentID to a display name with provenance: petname (pinned) >
    /// introduction (from someone you follow) > self-asserted card (hint) > short id.
    pub fn resolve_display(&self, agent_id: &str) -> ResolvedName {
        if let Ok(book) = self.social_book() {
            if let Some(petname) = book.nickname_of(agent_id) {
                return ResolvedName::new(petname.clone(), NameSource::Petname);
            }
            // An introduction from someone you follow is a strong hint.
            let intros = self.load_introductions();
            let from_followed = intros.iter().find(|i| {
                i.verify()
                    && i.subject_agent().ok().map(|a| a.0).as_deref() == Some(agent_id)
                    && naming::agent_id_from_did(&i.from)
                        .map(|a| book.is_following(&a.0))
                        .unwrap_or(false)
            });
            if let Some(i) = from_followed {
                return ResolvedName::new(i.asserted_name.clone(), NameSource::Introduced);
            }
        }
        if let Ok(Some(card)) = self.card_of(agent_id) {
            if !card.display_name.trim().is_empty() {
                return ResolvedName::new(card.display_name, NameSource::Card);
            }
        }
        ResolvedName::new(naming::short_agent(agent_id), NameSource::Unknown)
    }

    /// Reverse lookup: a query like `alice` → every AgentID it could mean, across
    /// petnames, introductions, and cached cards. Returns the full candidate set so
    /// the UI can disambiguate (it never silently picks one).
    pub fn resolve_name(&self, query: &str) -> Vec<(String, ResolvedName)> {
        let q = query.trim().trim_start_matches('@').to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(String, ResolvedName)> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let push = |aid: String,
                    name: ResolvedName,
                    out: &mut Vec<_>,
                    seen: &mut std::collections::BTreeSet<String>| {
            if seen.insert(format!("{aid}:{:?}", name.source)) {
                out.push((aid, name));
            }
        };
        if let Ok(book) = self.social_book() {
            for (aid, petname) in &book.nicknames {
                if petname.to_lowercase().contains(&q) {
                    push(
                        aid.clone(),
                        ResolvedName::new(petname.clone(), NameSource::Petname),
                        &mut out,
                        &mut seen,
                    );
                }
            }
        }
        for intro in self.load_introductions() {
            if intro.verify() && intro.asserted_name.to_lowercase().contains(&q) {
                if let Ok(aid) = intro.subject_agent() {
                    push(
                        aid.0,
                        ResolvedName::new(intro.asserted_name, NameSource::Introduced),
                        &mut out,
                        &mut seen,
                    );
                }
            }
        }
        if let Ok(dir) = self.cards_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        if let Ok(card) = serde_json::from_str::<ContactCard>(&text) {
                            if card.verify() && card.display_name.to_lowercase().contains(&q) {
                                if let Ok(aid) = card.agent_id() {
                                    push(
                                        aid.0,
                                        ResolvedName::new(card.display_name, NameSource::Card),
                                        &mut out,
                                        &mut seen,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Build a signed introduction vouching that `subject_agent` goes by `name`,
    /// to hand to a contact (it travels over the chat channel).
    pub fn make_introduction(&self, subject_agent: &str, name: &str) -> Result<Introduction> {
        let identity = self.identity()?;
        let from = naming::did_key_from_agent(&identity.agent_id()).map_err(Error::Io)?;
        let subject_did =
            naming::did_key_from_agent(&AgentId(subject_agent.to_string())).map_err(Error::Io)?;
        let card_ipns = self
            .card_of(subject_agent)
            .ok()
            .flatten()
            .and_then(|c| c.site_ipns);
        let mut intro = Introduction {
            from,
            subject_did,
            asserted_name: name.to_string(),
            card_ipns,
            updated_at: now_secs(),
            sig: String::new(),
        };
        intro.sign(&identity);
        Ok(intro)
    }

    /// Accept a received introduction: verify + store it as a name *candidate*
    /// (still petname-gated). Returns the subject AgentID.
    pub fn accept_introduction(&self, intro_json: &str) -> Result<String> {
        let intro: Introduction = serde_json::from_str(intro_json)
            .map_err(|e| Error::Io(format!("parse introduction: {e}")))?;
        if !intro.verify() {
            return Err(Error::Io(
                "introduction signature does not verify".to_string(),
            ));
        }
        validate_introduction_limits(&intro)?;
        let subject = intro.subject_agent().map_err(Error::Io)?.0;
        let path = self.introductions_path()?;
        crate::state::update_json::<Vec<Introduction>, _>(&path, |intros| {
            if intros.iter().any(|existing| {
                existing.from == intro.from
                    && existing.subject_did == intro.subject_did
                    && existing.updated_at >= intro.updated_at
            }) {
                return Err(Error::SecurityPolicy(
                    "stale or duplicate introduction update rejected".to_string(),
                ));
            }
            intros.retain(|existing| {
                !(existing.from == intro.from && existing.subject_did == intro.subject_did)
            });
            intros.insert(0, intro);
            intros.truncate(500);
            Ok(())
        })?;
        Ok(subject)
    }

    /// The DNSLink TXT record value for a site's IPNS — an optional Web2 bridge a
    /// domain owner pastes at their registrar (no chain, no dependency).
    pub fn dnslink_txt(&self, site_ipns: &str) -> String {
        format!("dnslink=/ipns/{}", site_ipns.trim())
    }

    fn room_book_path(&self) -> Result<PathBuf> {
        Ok(self
            .working_dir
            .join(self.config()?.store.root.join("rooms.json")))
    }

    /// The per-room participation policies (the AI-send lever + mutes).
    pub fn room_book(&self) -> Result<RoomBook> {
        RoomBook::load(&self.room_book_path()?).map_err(Error::Io)
    }

    /// Put a typed node whose **provenance is `from`** — recorded as a derived
    /// `Source`, so `walk`/`record_links` follow the links back to the sub-graph
    /// it was derived from (e.g. a §4 synthesis linking its source thread). Unlike
    /// `put_node` (which records `Source::User`), this attaches real, gravity-
    /// counted edges to the originating CIDs.
    pub fn put_node_derived(&self, node: &Node, from: &[Cid]) -> Result<Cid> {
        let mut value: serde_json::Value = serde_json::from_str(&node.fields_json)
            .map_err(|e| Error::Io(format!("node fields_json is not valid JSON: {e}")))?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| Error::Io("node fields_json must be a JSON object".to_string()))?;
        obj.insert(
            "type".to_string(),
            serde_json::Value::String(node.kind.clone()),
        );
        let typed: mem::node::Node = serde_json::from_value(value)
            .map_err(|e| Error::CidNotFound(format!("invalid node json: {e}")))?;
        let mut links = Vec::with_capacity(from.len());
        for cid in from {
            let parsed: mem::cid::Cid = cid
                .0
                .parse()
                .map_err(|e| Error::CidNotFound(format!("invalid cid {}: {e}", cid.0)))?;
            links.push(parsed);
        }
        let store = self.open_store()?;
        let cid = store
            .put_node(typed, mem::node::Source::Derived { from: links })
            .map_err(|e| Error::Io(format!("put derived node: {e}")))?;
        Ok(Cid(cid.to_string()))
    }

    /// Set a room's AI-send lever: `"off"` (Human-only), `"on"`, or `"on_mention"`.
    pub fn set_room_ai_send(&self, room: &str, value: &str) -> Result<()> {
        let path = self.room_book_path()?;
        crate::state::update_json::<RoomBook, _>(&path, |book| {
            book.set_ai_send(room, value);
            Ok(())
        })
    }

    /// Mute an AgentID in a room (receiver-side; muted messages stay in the DAG).
    pub fn mute_in_room(&self, room: &str, agent_id: &str) -> Result<()> {
        let path = self.room_book_path()?;
        crate::state::update_json::<RoomBook, _>(&path, |book| {
            book.mute(room, agent_id);
            Ok(())
        })
    }

    /// Post a signed message to a room, returning its CID. Enforces the AI-send
    /// lever (send-side): an `ai` install cannot post to a Human-only room.
    pub fn post_message(&self, room: &str, payload: &str) -> Result<Cid> {
        let cfg = self.config()?;
        let policy = self.room_book()?.policy(room);
        if !policy.may_send(&cfg.identity.kind, payload) {
            return Err(Error::Io(format!(
                "muted: room `{room}` is Human-only and this install is `{}`",
                cfg.identity.kind
            )));
        }
        let identity = self.identity()?;
        let parent = match self.resolve(&room_latest_name(room)) {
            Ok(cid) => Some(cid),
            Err(Error::NameUnbound(_)) => None,
            Err(e) => return Err(e),
        };
        // Link by the parent's *signature* (its install-independent message id),
        // not its block CID, so threads cohere across installs.
        let (clock, next) = match &parent {
            Some(p) => {
                let parent_env = self.read_message(p)?;
                (parent_env.clock + 1, vec![parent_env.sig])
            }
            None => (1, Vec::new()),
        };
        let mut env = MessageEnvelope {
            id: room.to_string(),
            payload: payload.to_string(),
            next,
            refs: Vec::new(),
            clock,
            key: identity.agent_id().0,
            sig: String::new(),
        };
        env.sig = identity.sign(&env.signing_bytes());
        let text = serde_json::to_string(&env).map_err(|e| Error::Io(e.to_string()))?;
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": text, "kind": "reference" }).to_string(),
        })?;
        self.bind(&message_id_name(&env.sig), &cid)?;
        self.bind(&room_latest_name(room), &cid)?;
        Ok(cid)
    }

    /// Read a message by CID, **verifying its signature**: a forged or tampered
    /// message (author's key doesn't sign it) is rejected.
    pub fn read_message(&self, cid: &Cid) -> Result<MessageEnvelope> {
        let record = self.get(&CidOrName::Cid(cid.clone()))?;
        let Record::Live { body_json, .. } = record else {
            return Err(Error::Io("message is tombstoned".to_string()));
        };
        let env = parse_message_envelope(&body_json)?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "message signature does not verify for author {}",
                env.key
            )));
        }
        Ok(env)
    }

    /// Accept an **inbound** signed message from a peer (gossipsub / relay): verify
    /// the author's signature, store it idempotently, and advance the room head if
    /// this message is at least as new as the current head. The room is the
    /// envelope's `id`. Returns the stored block CID (the existing one if we have
    /// already seen this message). The wire form is the bare envelope JSON — the
    /// same `text` `post_message` stores and the transport publishes.
    pub fn accept_message(&self, env_json: &str) -> Result<Cid> {
        let env: MessageEnvelope = serde_json::from_str(env_json)
            .map_err(|e| Error::Io(format!("parse inbound message: {e}")))?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "inbound message signature does not verify for author {}",
                env.key
            )));
        }
        // Idempotent: a message is identified by its signature, so re-delivery
        // (gossipsub fan-out, reconnect replay) maps to the same stored node.
        let id_name = message_id_name(&env.sig);
        if let Ok(existing) = self.resolve(&id_name) {
            return Ok(existing);
        }
        let room = env.id.clone();
        let text = serde_json::to_string(&env).map_err(|e| Error::Io(e.to_string()))?;
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": text, "kind": "reference" }).to_string(),
        })?;
        self.bind(&id_name, &cid)?;
        // Advance the room head if this message is at least as new as ours, so the
        // thread view (which walks back from the head) includes it.
        let advance = match self.resolve(&room_latest_name(&room)) {
            Ok(head) => self
                .read_message(&head)
                .map(|h| env.clock >= h.clock)
                .unwrap_or(true),
            Err(Error::NameUnbound(_)) => true,
            Err(e) => return Err(e),
        };
        if advance {
            self.bind(&room_latest_name(&room), &cid)?;
        }
        Ok(cid)
    }

    /// Where the direct-message consent allowlist + held requests live.
    fn contacts_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("contacts.json"))
    }

    /// Is `username` (a hex AgentID) an approved contact whose messages we accept?
    pub fn is_contact(&self, username: &str) -> bool {
        self.contacts_path()
            .ok()
            .and_then(|path| Contacts::load(&path).ok())
            .map(|contacts| contacts.approved.contains(username))
            .unwrap_or(false)
    }

    /// Approve `username` so their messages land in threads (idempotent). Called
    /// when *we* initiate a conversation (initiating implies trust).
    pub fn add_contact(&self, username: &str) -> Result<()> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.approved.insert(username.to_string());
            Ok(())
        })
    }

    /// The approved contacts — usernames (hex AgentIDs) whose direct messages we
    /// accept into threads. Sorted (the underlying set is ordered).
    pub fn approved_contacts(&self) -> Result<Vec<String>> {
        let path = self.contacts_path()?;
        let contacts = Contacts::load(&path).map_err(Error::Io)?;
        Ok(contacts.approved.iter().cloned().collect())
    }

    /// Revoke approval for `username` so their future messages are held as requests
    /// again (the user can re-approve). Returns whether they were approved. Thread
    /// history already received is not touched.
    pub fn remove_contact(&self, username: &str) -> Result<bool> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            Ok(contacts.approved.remove(username))
        })
    }

    /// The consent gate for **inbound** messages (the "only an approved concierge"
    /// rule). Verifies authorship, then: a message from us or an approved contact
    /// is accepted into its thread (`"accepted"`); a message from an unknown
    /// author is held as a request the user must accept/decline (`"pending"`) — a
    /// public username is never enough to land a message.
    pub fn receive_message(&self, env_json: &str) -> Result<&'static str> {
        let env: MessageEnvelope = serde_json::from_str(env_json)
            .map_err(|e| Error::Io(format!("parse inbound message: {e}")))?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "inbound message signature does not verify for author {}",
                env.key
            )));
        }
        let me = self.identity()?.agent_id().0;
        if env.key == me || self.is_contact(&env.key) {
            self.accept_message(env_json)?;
            return Ok("accepted");
        }
        // Unknown sender: hold it as a request (de-duped by signature).
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            let queue = contacts.requests.entry(env.key.clone()).or_default();
            if !queue.iter().any(|held| held.contains(&env.sig)) {
                queue.push(env_json.to_string());
            }
            Ok(())
        })?;
        Ok("pending")
    }

    /// Pending message requests: `(sender username, held count, latest preview)`.
    pub fn message_requests(&self) -> Result<Vec<(String, usize, String)>> {
        let path = self.contacts_path()?;
        let contacts = Contacts::load(&path).map_err(Error::Io)?;
        let mut out = Vec::new();
        for (username, queue) in &contacts.requests {
            let preview = queue
                .last()
                .and_then(|json| serde_json::from_str::<MessageEnvelope>(json).ok())
                .map(|env| env.payload)
                .unwrap_or_default();
            out.push((username.clone(), queue.len(), preview));
        }
        Ok(out)
    }

    /// Accept a request: approve the sender and flush every held message from them
    /// into its thread. Returns how many were delivered.
    pub fn accept_contact(&self, username: &str) -> Result<usize> {
        let path = self.contacts_path()?;
        let held = crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.approved.insert(username.to_string());
            Ok(contacts.requests.remove(username).unwrap_or_default())
        })?;
        let mut delivered = 0;
        for env_json in &held {
            if self.accept_message(env_json).is_ok() {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// Decline a request: drop every held message from `username` without
    /// approving them (they stay blocked).
    pub fn decline_contact(&self, username: &str) -> Result<()> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.requests.remove(username);
            Ok(())
        })
    }

    /// The sender-side store-and-forward outbox for undelivered direct messages.
    fn dm_outbox_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("outbox-dm.json"))
    }

    /// Queue a direct message for retry until the recipient acknowledges it,
    /// keyed by its transport content `id` (idempotent — re-queuing is a no-op).
    pub fn queue_outbound(&self, id: &str, recipient: &str, envelope: &str) -> Result<()> {
        let path = self.dm_outbox_path()?;
        crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox
                .pending
                .entry(id.to_string())
                .or_insert_with(|| OutboundDm {
                    recipient: recipient.to_string(),
                    envelope: envelope.to_string(),
                    queued_at: now_secs() as i64,
                });
            outbox.prune(now_secs() as i64);
            Ok(())
        })
    }

    /// Undelivered direct messages to retry: `(content id, recipient, envelope)`.
    pub fn pending_outbound(&self) -> Result<Vec<(String, String, String)>> {
        let path = self.dm_outbox_path()?;
        let outbox = crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox.prune(now_secs() as i64);
            Ok(outbox.clone())
        })?;
        Ok(outbox
            .pending
            .iter()
            .map(|(id, dm)| (id.clone(), dm.recipient.clone(), dm.envelope.clone()))
            .collect())
    }

    /// Clear an outbound message once its recipient acknowledged receipt.
    pub fn mark_outbound_delivered(&self, id: &str) -> Result<()> {
        let path = self.dm_outbox_path()?;
        crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox.pending.remove(id);
            Ok(())
        })
    }

    /// Assemble a room's thread in chronological order by walking parent links
    /// back from the room head, verifying every message. Muted authors are hidden
    /// (receiver-side) but still traversed — **mute ≠ deafen**.
    pub fn room_thread(&self, room: &str) -> Result<Vec<(Cid, MessageEnvelope)>> {
        let book = self.room_book()?;
        let mut out = Vec::new();
        let mut visited = BTreeSet::new();
        let root = match self.resolve(&room_latest_name(room)) {
            Ok(cid) => Some(cid),
            Err(Error::NameUnbound(_)) => None,
            Err(e) => return Err(e),
        };
        if let Some(cid) = root {
            self.collect_thread(room, &book, &cid, &mut visited, &mut out)?;
        }
        out.sort_by(|a, b| message_order(&a.1, &b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Raw stored message-envelope JSON for every message in a room — what the
    /// transport publishes to peers, byte-for-byte (so CIDs and signatures match).
    pub fn room_message_envelopes(&self, room: &str) -> Result<Vec<String>> {
        Ok(self
            .room_message_envelopes_with_cids(room)?
            .into_iter()
            .map(|(_, envelope)| envelope)
            .collect())
    }

    /// Stored message CIDs paired with their exact signed envelope JSON. Public
    /// transports use the CID to build and execute a `PublicRoomAttach` plan.
    pub fn room_message_envelopes_with_cids(&self, room: &str) -> Result<Vec<(Cid, String)>> {
        let mut out = Vec::new();
        for (cid, _) in self.room_thread(room)? {
            if let Record::Live { body_json, .. } = self.get(&CidOrName::Cid(cid.clone()))? {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_json) {
                    if let Some(text) = value
                        .get("body")
                        .and_then(|b| b.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        out.push((cid, text.to_string()));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Store a message received over the transport: **verify its signature**, gate
    /// it by the follow-list (unless `trust_all`), then persist it preserving the
    /// exact bytes (so its CID matches the sender's) and advance the room head.
    /// Returns the stored CID, or `None` if rejected (bad signature / unfollowed).
    pub fn store_inbound_message(
        &self,
        envelope_json: &str,
        trust_all: bool,
    ) -> Result<Option<Cid>> {
        let env: MessageEnvelope = serde_json::from_str(envelope_json)
            .map_err(|e| Error::Io(format!("inbound message parse: {e}")))?;
        let verified = crate::identity::verify(
            &AgentId(env.author().to_string()),
            &env.signing_bytes(),
            &env.sig,
        )
        .map_err(Error::Io)?;
        if !verified {
            return Ok(None);
        }
        if !trust_all {
            let me = self.agent_id()?.0;
            if env.author() != me && !self.social_book()?.is_following(env.author()) {
                return Ok(None);
            }
        }
        // Idempotent receive: a message's signature is its stable identity, so a
        // re-received message (e.g. periodic republish) is stored once. (mem stamps
        // `created_at`, so the *block* CID is install-specific; the signature is the
        // install-independent message id.)
        let id_name = message_id_name(&env.sig);
        if let Ok(existing) = self.resolve(&id_name) {
            return Ok(Some(existing));
        }
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": envelope_json, "kind": "reference" })
                .to_string(),
        })?;
        self.bind(&id_name, &cid)?;
        self.bind(&room_latest_name(env.room()), &cid)?;
        Ok(Some(cid))
    }

    fn collect_thread(
        &self,
        room: &str,
        book: &RoomBook,
        cid: &Cid,
        visited: &mut BTreeSet<Cid>,
        out: &mut Vec<(Cid, MessageEnvelope)>,
    ) -> Result<()> {
        if !visited.insert(cid.clone()) {
            return Ok(());
        }
        let env = self.read_message(cid)?;
        for entry in &env.next {
            // `next` entries are parent *message ids* (signatures); resolve each to
            // its local block CID via the index. Fall back to treating the entry as
            // a block CID directly (legacy/manually-built links), and skip any
            // ancestor not present locally (a partial cross-install thread).
            let parent_cid = self
                .resolve(&message_id_name(entry))
                .unwrap_or_else(|_| Cid(entry.clone()));
            if matches!(
                self.get(&CidOrName::Cid(parent_cid.clone())),
                Ok(Record::Live { .. })
            ) {
                self.collect_thread(room, book, &parent_cid, visited, out)?;
            }
        }
        if !book.is_muted(room, &env.key) {
            out.push((cid.clone(), env));
        }
        Ok(())
    }

    /// Append a publish receipt to the local trail beside the store.
    fn append_receipt(&self, receipt: &PublishReceipt) -> Result<()> {
        use std::io::Write;
        let path = self
            .working_dir
            .join(self.config()?.store.root.join("publish-receipts.jsonl"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("create receipt dir: {e}")))?;
        }
        let line = serde_json::to_string(receipt).map_err(|e| Error::Io(e.to_string()))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Io(format!("open receipt trail: {e}")))?;
        writeln!(file, "{line}").map_err(|e| Error::Io(format!("write receipt: {e}")))?;
        Ok(())
    }

    /// Record a website publish as a real DAG node, filed into the day calendar so
    /// it shows up in Records/Graph like any other node — the store is the single
    /// CID ledger. The published bytes themselves live in Kubo (UnixFS) or the
    /// external host; this `publication` node *references* that root + its IPNS/URL
    /// so the published CID is never invisible to the explorer. The receipt trail
    /// (`publish-receipts.jsonl`) stays as the signed egress log; this is the
    /// in-store, content-addressed counterpart.
    fn record_publication(&self, receipt: &PublishReceipt) -> Result<()> {
        let ts = receipt.unix_time;
        // A `memory` node of kind `reference` (a publication *is* a reference to
        // external published content). The mem node-kind enum AND its memory-kind
        // sub-enum are both closed, and the on-disk store is also read by the
        // external `mem` CLI, so we don't add a new variant — the published root
        // CID, IPNS, and URL are encoded in the `text` so the explorer surfaces them.
        let ipns_part = receipt
            .ipns_name
            .as_deref()
            .map(|ipns| format!(" · ipns {ipns}"))
            .unwrap_or_default();
        let text = format!(
            "Published \"{}\" to {} — root {}{} · {}",
            receipt.site_name.as_deref().unwrap_or("site"),
            receipt.backend,
            receipt.root,
            ipns_part,
            receipt.gateway_url,
        );
        let node = Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "kind": "reference", "text": text }).to_string(),
        };
        let cid = self.put_node(&node)?;
        // One record per publish event (ts is unique per publish; IPFS roots also
        // change each time). Files into today's day so it appears under Records.
        let event_key = format!("publication-{}-{ts}", receipt.backend);
        self.record_event_in_day(&utc_date(ts), &event_key, &cid)?;
        Ok(())
    }

    /// Save the current Studio draft as a checkpoint at any time — no publish, no
    /// egress. The HTML is content-addressed as a blob (a real CID), wrapped in a
    /// genuine `checkpoint` node (retained + egress-locked like any checkpoint), and
    /// that node is filed into the day calendar so it appears in Records. Returns the
    /// snapshot's content CID + the timestamp.
    pub fn save_site_checkpoint(&self, name: &str, html: &str) -> Result<(String, u64)> {
        let ts = now_secs();
        let root = self.put_blob(html.as_bytes(), "text/html")?;
        let checkpoint = self.checkpoint(&format!("studio:{name}"), &root, None)?;
        let event_key = format!("studio-checkpoint-{name}-{ts}");
        // Best-effort calendar filing — the node + blob are already content-addressed
        // even if the day index can't be updated.
        let _ = self.record_event_in_day(&utc_date(ts), &event_key, &checkpoint);
        Ok((root.0, ts))
    }

    /// Sync the user's wallet-browser (Brave/Opera) bookmarks into memory (Pillar A,
    /// Decision 0033). Each new bookmark (deduped by URL via a bound `bookmark:<hash>`
    /// name) becomes a `memory`/`reference` node filed into the day calendar so it
    /// shows in Records. Read-only on the browser's side; ingested content is an
    /// **untrusted source** — retrievable, never auto-injected. Returns count added.
    pub fn sync_browser_bookmarks(&self) -> Result<usize> {
        let mut added = 0;
        for bm in crate::browser::read_bookmarks() {
            let key = crate::browser::url_key(&bm.url);
            let dedup_name = format!("bookmark:{key}");
            if self.resolve(&dedup_name).is_ok() {
                continue; // already ingested
            }
            let ts = if bm.added_unix > 0 { bm.added_unix } else { now_secs() };
            let location = if bm.folder.is_empty() {
                String::new()
            } else {
                format!("\n(in {})", bm.folder)
            };
            let text = format!("Bookmark — {}\n{}{}", bm.title, bm.url, location);
            let node = Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({ "kind": "reference", "text": text }).to_string(),
            };
            let cid = self.put_node(&node)?;
            self.bind(&dedup_name, &cid)?;
            let _ = self.record_event_in_day(&utc_date(ts), &format!("bookmark-{key}"), &cid);
            added += 1;
        }
        Ok(added)
    }

    // ── Wallet attestation + settings (Pillar C, Decision 0033) ──────────────
    // The browser (Brave/Opera) custodies keys and signs; we only verify + store.

    fn wallet_state_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("wallet.json"))
    }

    /// Load the on-device wallet state (links + settings), empty if none yet.
    pub fn wallet_state(&self) -> Result<crate::wallet::WalletState> {
        match std::fs::read(self.wallet_state_path()?) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Io(format!("parse wallet state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::wallet::WalletState::default())
            }
            Err(e) => Err(Error::Io(format!("read wallet state: {e}"))),
        }
    }

    fn save_wallet_state(&self, state: &crate::wallet::WalletState) -> Result<()> {
        let dir = self.store_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(format!("create store dir: {e}")))?;
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| Error::Io(format!("serialize wallet state: {e}")))?;
        std::fs::write(self.wallet_state_path()?, bytes)
            .map_err(|e| Error::Io(format!("write wallet state: {e}")))
    }

    /// Verify a browser-wallet `personal_sign` over our AgentID and record the link
    /// (idempotent per address). Returns the stored link. We hold no keys — this only
    /// confirms the address controls a key that signed our AgentID.
    pub fn link_wallet(
        &self,
        address: &str,
        chain: &str,
        signature: &str,
    ) -> Result<crate::wallet::WalletLink> {
        let agent_id = self.identity()?.agent_id().0;
        let message = crate::wallet::link_message(&agent_id);
        let recovered = crate::wallet::recover_eth_personal_sign(&message, signature)
            .map_err(Error::Io)?;
        if recovered.to_lowercase() != address.trim().to_lowercase() {
            return Err(Error::Io(format!(
                "signature does not match {address} (recovered {recovered})"
            )));
        }
        let link = crate::wallet::WalletLink {
            address: recovered.to_lowercase(),
            chain: if chain.is_empty() { "evm".to_string() } else { chain.to_string() },
            agent_id,
            signature: signature.to_string(),
            linked_at: now_secs(),
        };
        let mut state = self.wallet_state()?;
        state.links.retain(|l| l.address != link.address);
        state.links.push(link.clone());
        self.save_wallet_state(&state)?;
        Ok(link)
    }

    pub fn wallet_links(&self) -> Result<Vec<crate::wallet::WalletLink>> {
        Ok(self.wallet_state()?.links)
    }

    pub fn unlink_wallet(&self, address: &str) -> Result<()> {
        let mut state = self.wallet_state()?;
        let want = address.trim().to_lowercase();
        state.links.retain(|l| l.address != want);
        self.save_wallet_state(&state)
    }

    pub fn wallet_settings(&self) -> Result<crate::wallet::WalletSettings> {
        Ok(self.wallet_state()?.settings)
    }

    /// Replace the wallet settings from a JSON object (agent_access / spend_cap /
    /// allowlist / preferred_chain).
    pub fn set_wallet_settings(&self, settings_json: &str) -> Result<()> {
        let settings: crate::wallet::WalletSettings = serde_json::from_str(settings_json)
            .map_err(|e| Error::Io(format!("parse wallet settings: {e}")))?;
        let mut state = self.wallet_state()?;
        state.settings = settings;
        self.save_wallet_state(&state)
    }

    /// Stage a transaction the host AI *proposes* (the agent-propose tier). We never
    /// send it — the GUI surfaces it and the user approves it in their browser wallet,
    /// which confirms again. All guards are enforced HERE, before staging:
    /// `agent_access` must be on, the spend cap must cover the value, and (if set) the
    /// recipient must be allowlisted.
    pub fn propose_wallet_tx(
        &self,
        to: &str,
        value: &str,
        data: &str,
        reason: &str,
    ) -> Result<crate::wallet::WalletProposal> {
        let s = self.wallet_settings()?;
        if !s.agent_access {
            return Err(Error::Io(
                "AI wallet access is off — enable it in the Wallet tab to let the AI propose transactions".to_string(),
            ));
        }
        let to_l = to.trim().to_lowercase();
        if !to_l.starts_with("0x") || to_l.len() != 42 {
            return Err(Error::Io(format!("invalid recipient address: {to}")));
        }
        if !s.allowlist.is_empty()
            && !s.allowlist.iter().any(|a| a.trim().to_lowercase() == to_l)
        {
            return Err(Error::Io(format!("recipient {to} is not in your allowlist")));
        }
        let cap: f64 = s
            .spend_cap
            .trim()
            .parse()
            .map_err(|_| Error::Io("no per-transaction spend cap set — AI sends are disabled".to_string()))?;
        let amount: f64 = value
            .trim()
            .parse()
            .map_err(|_| Error::Io(format!("invalid value: {value}")))?;
        if amount <= 0.0 || amount > cap {
            return Err(Error::Io(format!(
                "value {value} exceeds your per-transaction cap of {}",
                s.spend_cap
            )));
        }
        let proposed_at = now_secs();
        let id = format!(
            "tx-{}",
            &crate::browser::url_key(&format!("{to_l}{value}{data}{proposed_at}"))[..12]
        );
        let proposal = crate::wallet::WalletProposal {
            id,
            to: to_l,
            value: value.trim().to_string(),
            data: data.trim().to_string(),
            reason: reason.trim().to_string(),
            proposed_at,
            status: "pending".to_string(),
            tx_hash: String::new(),
        };
        let mut state = self.wallet_state()?;
        state.proposals.push(proposal.clone());
        // Bound the history.
        if state.proposals.len() > 50 {
            let drop = state.proposals.len() - 50;
            state.proposals.drain(0..drop);
        }
        self.save_wallet_state(&state)?;
        Ok(proposal)
    }

    /// Pending (not-yet-approved/rejected) proposals, newest first.
    pub fn pending_wallet_proposals(&self) -> Result<Vec<crate::wallet::WalletProposal>> {
        let mut out: Vec<_> = self
            .wallet_state()?
            .proposals
            .into_iter()
            .filter(|p| p.status == "pending")
            .collect();
        out.reverse();
        Ok(out)
    }

    /// Record the user's decision on a proposal (`approved` with a tx hash, or
    /// `rejected`).
    pub fn resolve_wallet_proposal(&self, id: &str, status: &str, tx_hash: &str) -> Result<()> {
        let mut state = self.wallet_state()?;
        let p = state
            .proposals
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::Io(format!("no such proposal: {id}")))?;
        p.status = status.to_string();
        p.tx_hash = tx_hash.to_string();
        self.save_wallet_state(&state)
    }

    fn import_verified_car(
        &self,
        root: &cid::Cid,
        blocks: &[(cid::Cid, Vec<u8>)],
        name: &str,
    ) -> Result<Cid> {
        let blocks_dir = self.blocks_dir()?;
        std::fs::create_dir_all(&blocks_dir)
            .map_err(|e| Error::Io(format!("create blocks dir: {e}")))?;
        for (cid, data) in blocks {
            std::fs::write(blocks_dir.join(cid.to_string()), data)
                .map_err(|e| Error::Io(format!("write block {cid}: {e}")))?;
        }
        let root_cid = Cid(root.to_string());
        self.bind(name, &root_cid)?;
        Ok(root_cid)
    }

    fn record_latest_share(&self, receipt: &PublishReceipt, root: &Cid) -> Result<()> {
        let payload = serde_json::json!({
            "agent_id": receipt.agent_id,
            "root": cid_to_json(root)?,
            "signature": receipt.signature,
            "published_at": receipt.unix_time,
        });
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::to_string(&serde_json::json!({
                "text": payload.to_string(),
                "kind": "reference",
            }))
            .map_err(|e| Error::Io(format!("serialize share pointer: {e}")))?,
        })?;
        self.bind(&latest_share_name(&receipt.agent_id), &cid)
    }

    fn record_shared_with_me(&self, agent_id: &str, root: &Cid, signature: &str) -> Result<()> {
        let payload = serde_json::json!({
            "agent_id": agent_id,
            "root": cid_to_json(root)?,
            "signature": signature,
            "received_at": now_secs(),
        });
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::to_string(&serde_json::json!({
                "text": payload.to_string(),
                "kind": "reference",
            }))
            .map_err(|e| Error::Io(format!("serialize incoming share: {e}")))?,
        })?;
        self.bind(&shared_with_me_name(agent_id), &cid)
    }
}

#[derive(Debug, serde::Deserialize)]
struct SharePointerBody {
    agent_id: String,
    root: serde_json::Value,
    signature: String,
}

fn parse_share_pointer(body_json: &str) -> Result<(String, Cid, String)> {
    let value: serde_json::Value = serde_json::from_str(body_json)
        .map_err(|e| Error::Io(format!("parse share pointer JSON: {e}")))?;
    let body = value
        .get("body")
        .ok_or_else(|| Error::Io("share pointer JSON missing body".to_string()))?;
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Io("share pointer JSON missing text".to_string()))?;
    let pointer: SharePointerBody = serde_json::from_str(text)
        .map_err(|e| Error::Io(format!("parse share pointer body: {e}")))?;
    Ok((
        pointer.agent_id,
        cid_from_link(&pointer.root)?,
        pointer.signature,
    ))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days from the Unix epoch to a civil date (Howard Hinnant's algorithm).
/// Deterministic; no wall clock.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The deterministic `created_at` for a calendar manifest: midnight UTC of the
/// **start of the period** it represents (a month `YYYY-MM` → its 1st; a year
/// `YYYY` → Jan 1). A derived manifest's timestamp is a function of *which period
/// it indexes*, not of when it happened to be rebuilt — so re-deriving it yields
/// an identical CID (idempotent rollup).
fn period_start_unix(label: &str) -> u64 {
    let mut parts = label.split('-');
    let year: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1970);
    let month: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    (days_from_civil(year, month, 1).max(0) as u64) * 86_400
}

impl CoreBinding for MemCli {
    fn put_node(&self, node: &Node) -> Result<Cid> {
        let mut value: serde_json::Value = serde_json::from_str(&node.fields_json)
            .map_err(|e| Error::Io(format!("node fields_json is not valid JSON: {e}")))?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| Error::Io("node fields_json must be a JSON object".to_string()))?;
        obj.insert(
            "type".to_string(),
            serde_json::Value::String(node.kind.clone()),
        );
        self.put_json_value(value)
    }

    fn put_blob(&self, bytes: &[u8], media_type: &str) -> Result<Cid> {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".to_string(),
            serde_json::Value::String("blob".to_string()),
        );
        map.insert(
            "bytes".to_string(),
            serde_json::Value::Array(
                bytes
                    .iter()
                    .copied()
                    .map(|b| serde_json::Value::Number(serde_json::Number::from(b)))
                    .collect(),
            ),
        );
        map.insert(
            "media_type".to_string(),
            serde_json::Value::String(media_type.to_string()),
        );
        // A blob's identity is its bytes plus media type. Wall-clock metadata
        // would make identical content produce different CIDs across seconds.
        self.put_json_value_at(serde_json::Value::Object(map), 0)
    }

    fn bind(&self, name: &str, cid: &Cid) -> Result<()> {
        let parsed: mem::cid::Cid = cid
            .0
            .parse()
            .map_err(|e| Error::CidNotFound(format!("invalid cid {}: {e}", cid.0)))?;
        let mut store = self.open_store()?;
        store
            .bind(name, parsed)
            .map_err(|e| Error::Io(format!("bind {name}: {e}")))
    }

    fn resolve(&self, name: &str) -> Result<Cid> {
        let store = self.open_store()?;
        store
            .resolve(name)
            .map(|cid| Cid(cid.to_string()))
            .map_err(|_| Error::NameUnbound(name.to_string()))
    }

    fn get(&self, key: &CidOrName) -> Result<Record> {
        let cid = match key {
            CidOrName::Cid(c) => c.clone(),
            CidOrName::Name(n) => self.resolve(n)?,
        };
        let parsed: mem::cid::Cid = cid
            .0
            .parse()
            .map_err(|e| Error::CidNotFound(format!("invalid cid {}: {e}", cid.0)))?;
        let store = self.open_store()?;
        match store
            .lookup(&parsed)
            .map_err(|e| Error::CidNotFound(format!("{e}")))?
        {
            mem::store::Lookup::Present(record) => {
                let body_json = serde_json::to_string_pretty(&record)
                    .map_err(|e| Error::Io(format!("serialize record: {e}")))?;
                self.parse_live_record(&body_json, cid)
            }
            mem::store::Lookup::Pruned(t) => {
                let receipt_json = format_receipt(&cid, &t);
                Ok(Record::Tombstone { cid, receipt_json })
            }
        }
    }

    fn checkpoint(&self, label: &str, root: &Cid, parent: Option<&Cid>) -> Result<Cid> {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".to_string(),
            serde_json::Value::String("checkpoint".to_string()),
        );
        map.insert(
            "label".to_string(),
            serde_json::Value::String(label.to_string()),
        );
        map.insert("root".to_string(), cid_to_json(root)?);
        map.insert(
            "parent".to_string(),
            match parent {
                Some(cid) => cid_to_json(cid)?,
                None => serde_json::Value::Null,
            },
        );
        let checkpoint = self.put_json_value(serde_json::Value::Object(map))?;
        self.lock_default_checkpoint(&checkpoint, label)?;
        Ok(checkpoint)
    }

    fn walk(&self, root: &Cid) -> Result<Vec<Cid>> {
        let mut visited = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        self.walk_inner(root, &mut visited, &mut out)?;
        Ok(out)
    }

    fn gc(&self, policy: &GcPolicy) -> Result<GcReport> {
        let keep = policy
            .keep_checkpoints
            .unwrap_or(self.config()?.checkpoint.keep_checkpoints);
        let store = self.open_store()?;
        // `checkpoint_name` is mem's well-known chain-head pointer (default
        // `latest`); core does not override it, matching the prior CLI behavior.
        let report = store
            .gc(&mem::gc::RetentionPolicy {
                keep_checkpoints: keep as usize,
                checkpoint_name: "latest".to_string(),
            })
            .map_err(|e| Error::Io(format!("gc: {e}")))?;
        Ok(GcReport {
            removed: (report.pruned_checkpoints.len() + report.pruned_orphans.len()) as u64,
            kept: report.kept as u64,
        })
    }

    fn record_event_in_day(&self, date: &str, event_key: &str, record: &Cid) -> Result<Cid> {
        let day_name = format!("day-{date}");
        let blocks = self.blockstore()?;
        let record_cid: mem::cid::Cid = record
            .0
            .parse()
            .map_err(|e| Error::CidNotFound(format!("invalid cid {}: {e}", record.0)))?;

        // Load the day's existing HAMT (via the bound day root) or start fresh.
        let mut hamt = match self.day_hamt_root(&day_name)? {
            Some(hamt_root) => mem::hamt::Hamt::load(&blocks, &hamt_root)
                .map_err(|e| Error::Io(format!("load day hamt: {e}")))?,
            None => mem::hamt::Hamt::new(&blocks),
        };
        hamt.set(event_key.as_bytes(), record_cid)
            .map_err(|e| Error::Io(format!("hamt set: {e}")))?;
        let hamt_root = hamt
            .flush()
            .map_err(|e| Error::Io(format!("hamt flush: {e}")))?;

        // Wrap the HAMT root in a DayIndex node and (re)bind the day root name.
        let events_link = cid_link(&Cid(hamt_root.to_string()))?;
        let day_node = Node {
            kind: "day_index".to_string(),
            fields_json: serde_json::json!({ "date": date, "events": events_link }).to_string(),
        };
        let day_cid = self.put_node(&day_node)?;
        self.bind(&day_name, &day_cid)?;
        Ok(day_cid)
    }

    fn day_contains(&self, date: &str, event_key: &str) -> Result<bool> {
        let day_name = format!("day-{date}");
        match self.day_hamt_root(&day_name)? {
            None => Ok(false),
            Some(hamt_root) => {
                let blocks = self.blockstore()?;
                let hamt: mem::hamt::Hamt<_, mem::cid::Cid> =
                    mem::hamt::Hamt::load(&blocks, &hamt_root)
                        .map_err(|e| Error::Io(format!("load day hamt: {e}")))?;
                Ok(hamt
                    .get(event_key.as_bytes())
                    .map_err(|e| Error::Io(format!("hamt get: {e}")))?
                    .is_some())
            }
        }
    }

    fn roll_up_calendar(&self) -> Result<()> {
        // Group the bound day roots by month, then months by year.
        let mut by_month: BTreeMap<String, Vec<(String, Cid)>> = BTreeMap::new();
        for (name, cid) in self.names()? {
            if let Some(date) = name.strip_prefix("day-") {
                if date.len() >= 7 {
                    by_month
                        .entry(date[0..7].to_string())
                        .or_default()
                        .push((date.to_string(), cid));
                }
            }
        }

        // Each month: a manifest linking its (key-sorted) day roots.
        let mut by_year: BTreeMap<String, Vec<(String, Cid)>> = BTreeMap::new();
        for (month, mut days) in by_month {
            days.sort();
            let mut entries = Vec::with_capacity(days.len());
            for (date, day_cid) in &days {
                entries.push(serde_json::json!({ "label": date, "node": cid_link(day_cid)? }));
            }
            // Derived manifest: a deterministic, period-derived `created_at` (not the
            // wall clock) so re-rolling produces the identical CID (idempotent).
            let month_value =
                serde_json::json!({ "type": "month_index", "month": month, "days": entries });
            let month_cid = self.put_json_value_at(month_value, period_start_unix(&month))?;
            self.bind(&format!("month-{month}"), &month_cid)?;
            by_year
                .entry(month[0..4].to_string())
                .or_default()
                .push((month, month_cid));
        }

        // Each year: a manifest linking its (key-sorted) month roots.
        for (year, mut months) in by_year {
            months.sort();
            let mut entries = Vec::with_capacity(months.len());
            for (month, month_cid) in &months {
                entries.push(serde_json::json!({ "label": month, "node": cid_link(month_cid)? }));
            }
            let year_value =
                serde_json::json!({ "type": "year_index", "year": year, "months": entries });
            let year_cid = self.put_json_value_at(year_value, period_start_unix(&year))?;
            self.bind(&format!("year-{year}"), &year_cid)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;

    /// A unique scratch working dir under the OS temp dir, so `mem`'s
    /// cwd-scoped `.concierge` store never touches the user's real store.
    fn temp_workdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!(
            "concierge-core-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn publish(mem: &MemCli, target: &str) -> Result<PublishReceipt> {
        // Decision 0026: everything is fenced from egress by default, so the
        // egress-unlock (set password + clear) is a precondition of publishing.
        // Already-public or already-cleared roots clear idempotently.
        if let Ok(root) = mem.resolve(target) {
            let _ = mem.set_password("pw");
            let _ = mem.clear_for_egress(&root, "test", "pw");
        }
        let plan = mem
            .build_egress_plan_for_target(target, crate::egress::EgressOperation::PublicPublish)?;
        mem.publish_public(&plan)
    }

    fn configure_fake_ipfs_backend(
        mem: &MemCli,
        dir: &Path,
        expected_requests: usize,
    ) -> (std::thread::JoinHandle<()>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");

        let join = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).expect("read request");
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let headers_end = request
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .expect("headers end")
                    + 4;
                let header_text = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(k, v)| {
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                    })
                    .expect("content length");
                let already = request.len().saturating_sub(headers_end);
                let remaining = content_length.saturating_sub(already);
                if remaining > 0 {
                    let mut body = vec![0u8; remaining];
                    stream.read_exact(&mut body).expect("read body");
                }
                let response =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                stream.write_all(response).expect("write response");
            }
        });

        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = api_url.clone();
        cfg.save_to_project_root(dir).expect("save config");
        (join, api_url)
    }

    #[test]
    fn naming_petname_precedence_card_import_and_resolution() {
        let dir = temp_workdir("naming");
        let mem = MemCli::new(&dir);
        let me = mem.agent_id().unwrap().0;

        // A peer signs their own card; we import it (verifies).
        let peer = crate::identity::Identity::generate();
        let peer_aid = peer.agent_id().0;
        let mut card = crate::naming::ContactCard::new(&peer.agent_id(), "Jason", 100).unwrap();
        card.site_ipns = Some("k51peer".into());
        card.sign(&peer);
        assert_eq!(
            mem.import_card(&serde_json::to_string(&card).unwrap())
                .unwrap(),
            peer_aid
        );

        // No petname yet → the card name is a Card *hint* (unverified).
        let r = mem.resolve_display(&peer_aid);
        assert_eq!(r.text, "Jason");
        assert_eq!(r.source, NameSource::Card);
        assert!(!r.verified);

        // Petname wins and is verified (anti-spoofing precedence).
        mem.set_nickname(&peer_aid, "J-dawg").unwrap();
        let r = mem.resolve_display(&peer_aid);
        assert_eq!(r.text, "J-dawg");
        assert_eq!(r.source, NameSource::Petname);
        assert!(r.verified);

        // A tampered card is rejected at import.
        let mut forged = card.clone();
        forged.display_name = "Mallory".into();
        assert!(mem
            .import_card(&serde_json::to_string(&forged).unwrap())
            .is_err());

        // The user's own card builds + self-verifies for their AgentID.
        mem.update_my_card(Some("Me"), Some("hi"), None, Some("k51mine"))
            .unwrap();
        let my = mem.my_card().unwrap();
        assert_eq!(my.display_name, "Me");
        assert_eq!(my.site_ipns.as_deref(), Some("k51mine"));
        assert!(my.verify());
        assert_eq!(my.agent_id().unwrap().0, me);

        // Reverse lookup finds the petname candidate.
        assert!(mem
            .resolve_name("@j-dawg")
            .iter()
            .any(|(a, n)| a == &peer_aid && n.text == "J-dawg"));

        // A signed introduction round-trips through accept_introduction.
        let intro = mem.make_introduction(&peer_aid, "Jason from work").unwrap();
        assert_eq!(
            mem.accept_introduction(&serde_json::to_string(&intro).unwrap())
                .unwrap(),
            peer_aid
        );
    }

    #[test]
    fn put_bind_resolve_get_roundtrip_survives_restart() {
        let dir = temp_workdir("restart");

        // "Process 1": write a node and bind a name to it.
        let cid = {
            let mem = MemCli::new(&dir);
            let node = Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"phase 1 lives","kind":"project"}"#.to_string(),
            };
            let cid = mem.put_node(&node).expect("put_node");
            mem.bind("latest", &cid).expect("bind");
            assert_eq!(mem.resolve("latest").expect("resolve same-process"), cid);
            cid
        };

        // "Process 2": a brand-new binding over the same on-disk store. Because
        // each call is a fresh `mem` process reading `.concierge`, this is a real
        // restart — the Phase 1 exit criterion.
        let mem2 = MemCli::new(&dir);
        assert_eq!(
            mem2.resolve("latest").expect("resolve after restart"),
            cid,
            "a bound name must resolve to the same CID after restart"
        );
        match mem2
            .get(&CidOrName::Name("latest".to_string()))
            .expect("get after restart")
        {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, cid);
                assert_eq!(kind, "memory");
                assert!(body_json.contains("phase 1 lives"), "body must round-trip");
            }
            other => panic!("expected a live record, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_unbound_name_is_typed_error() {
        let dir = temp_workdir("unbound");
        let mem = MemCli::new(&dir);
        match mem.resolve("never-bound") {
            Err(Error::NameUnbound(_)) => {}
            other => panic!("expected NameUnbound, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_workdir_is_store_not_found() {
        let dir = temp_workdir("missing-store");
        let missing = dir.join("does-not-exist");
        let mem = MemCli::new(&missing);
        match mem.resolve("latest") {
            Err(Error::StoreNotFound(_)) => {}
            other => panic!("expected StoreNotFound, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_blob_roundtrips_and_walk_sees_it() {
        let dir = temp_workdir("blob");
        let mem = MemCli::new(&dir);
        let cid = mem.put_blob(b"hi", "text/plain").expect("put_blob");
        match mem.get(&CidOrName::Cid(cid.clone())).expect("get blob") {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, cid);
                assert_eq!(kind, "blob");
                let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
                assert_eq!(value["created_at"], serde_json::json!(0));
                assert_eq!(value["body"]["bytes"], serde_json::json!([104, 105]));
                assert_eq!(value["body"]["media_type"], serde_json::json!("text/plain"));
            }
            other => panic!("expected live blob record, got {other:?}"),
        }
        assert_eq!(
            mem.put_blob(b"hi", "text/plain").expect("repeat put_blob"),
            cid,
            "identical blobs have one stable content address"
        );
        assert_eq!(mem.walk(&cid).expect("walk blob"), vec![cid]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn day_tier_buckets_events_under_one_day_root() {
        let dir = temp_workdir("day-tier");
        let mem = MemCli::new(&dir);

        let prompt = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "hi" }).to_string(),
            })
            .expect("put prompt");
        let response = mem
            .put_node(&Node {
                kind: "response".into(),
                fields_json: serde_json::json!({ "text": "yo", "model": "unknown" }).to_string(),
            })
            .expect("put response");

        let ts = 1_749_470_400u64; // a fixed point inside one UTC day
        let date = mem::tombstones::iso8601(ts)[0..10].to_string();
        let day_name = format!("day-{date}");

        // Two events on the same day fold into ONE re-bound day root.
        mem.record_event_in_day(&date, "evt-1", &prompt)
            .expect("record evt-1");
        let day_cid = mem
            .record_event_in_day(&date, "evt-2", &response)
            .expect("record evt-2");
        assert_eq!(mem.resolve(&day_name).expect("resolve day"), day_cid);

        // The day's HAMT holds both events, keyed by their stable ids.
        let blocks = mem.blockstore().unwrap();
        let hamt_root = mem
            .day_hamt_root(&day_name)
            .unwrap()
            .expect("day hamt root");
        let hamt: mem::hamt::Hamt<_, mem::cid::Cid> =
            mem::hamt::Hamt::load(&blocks, &hamt_root).unwrap();
        let prompt_cid: mem::cid::Cid = prompt.0.parse().unwrap();
        let response_cid: mem::cid::Cid = response.0.parse().unwrap();
        assert_eq!(hamt.get(b"evt-1").unwrap(), Some(prompt_cid));
        assert_eq!(hamt.get(b"evt-2").unwrap(), Some(response_cid));

        // The day fans out to its events for the explorer.
        let mut events = mem.day_events(&date).expect("day events");
        events.sort();
        assert_eq!(
            events,
            vec![
                ("evt-1".to_string(), prompt.clone()),
                ("evt-2".to_string(), response.clone()),
            ]
        );

        // A different UTC day gets its own root (not the same day index).
        let date2 = mem::tombstones::iso8601(ts + 86_400)[0..10].to_string();
        let next_day = mem
            .record_event_in_day(&date2, "evt-3", &prompt)
            .expect("record next day");
        assert_ne!(next_day, day_cid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_is_recorded_as_a_store_node_in_the_day_calendar() {
        // A published site's CID must also be a node in the store so it appears in
        // Records — not just in the receipt trail / Studio sidecar.
        let dir = temp_workdir("publish-record");
        let mem = MemCli::new(&dir);
        let ts = 1_700_000_000u64;
        let receipt = PublishReceipt {
            root: "bafyTESTsiteroot".to_string(),
            backend: "ipfs-public".to_string(),
            unix_time: ts,
            gateway_url: "https://ipfs.io/ipns/k51TESTipns".to_string(),
            agent_id: "deadbeef".to_string(),
            signature: String::new(),
            ipns_name: Some("k51TESTipns".to_string()),
            site_name: Some("ConciergeSideKick".to_string()),
        };
        mem.record_publication(&receipt)
            .expect("record publication");

        // It lands in today's day calendar under a stable per-publish key.
        let date = utc_date(ts);
        let events = mem.day_events(&date).expect("day events");
        let (_key, node_cid) = events
            .iter()
            .find(|(k, _)| k == "publication-ipfs-public-1700000000")
            .expect("publication event filed in the day");

        // And the node is a real, content-addressed `publication` record carrying
        // the published root + IPNS so the explorer can surface the CID.
        match mem
            .get(&CidOrName::Cid(node_cid.clone()))
            .expect("get node")
        {
            Record::Live {
                kind, body_json, ..
            } => {
                assert_eq!(kind, "memory");
                assert!(body_json.contains("Published"), "reads as a publication");
                assert!(
                    body_json.contains("bafyTESTsiteroot"),
                    "carries the published root CID"
                );
                assert!(body_json.contains("k51TESTipns"), "carries the IPNS name");
                assert!(body_json.contains("ipfs-public"), "carries the platform");
            }
            other => panic!("expected live publication record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn studio_checkpoint_saves_a_content_addressed_node_in_the_day_calendar() {
        // "Save checkpoint" snapshots the draft any time — a real CID + a checkpoint
        // node filed into Records, with no publish/egress.
        let dir = temp_workdir("studio-ckpt");
        let mem = MemCli::new(&dir);
        let (cid, ts) = mem
            .save_site_checkpoint("portfolio", "<h1>draft v1</h1>")
            .expect("save checkpoint");
        assert!(!cid.is_empty(), "snapshot is content-addressed");

        // Filed into today's calendar under a studio-checkpoint key so Records shows it.
        let events = mem.day_events(&utc_date(ts)).expect("day events");
        let (_key, node_cid) = events
            .iter()
            .find(|(k, _)| k.starts_with("studio-checkpoint-portfolio-"))
            .expect("studio checkpoint filed in the day");

        // The node is a real `checkpoint` over the snapshot blob.
        match mem
            .get(&CidOrName::Cid(node_cid.clone()))
            .expect("get node")
        {
            Record::Live {
                kind, body_json, ..
            } => {
                assert_eq!(kind, "checkpoint");
                assert!(
                    body_json.contains("studio:portfolio"),
                    "labelled as the studio checkpoint"
                );
            }
            other => panic!("expected live checkpoint record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bookmark_sync_ingests_dedupes_and_files_into_records() {
        // Pillar A: a wallet-browser bookmark becomes a retrievable memory node, once.
        let dir = temp_workdir("bookmarks");
        let mem = MemCli::new(&dir);
        let bm_file = dir.join("Bookmarks");
        std::fs::write(
            &bm_file,
            r#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks bar","children":[
                {"type":"url","name":"IPFS paper","url":"https://ipfs.tech/paper"},
                {"type":"url","name":"libp2p","url":"https://libp2p.io"},
                {"type":"url","name":"dup","url":"https://ipfs.tech/paper"}
            ]},"other":{"type":"folder","name":"Other","children":[]}}}"#,
        )
        .unwrap();
        std::env::set_var("CONCIERGE_BOOKMARKS_FILE", &bm_file);

        // Two unique URLs ingested (the duplicate is deduped).
        assert_eq!(mem.sync_browser_bookmarks().expect("sync"), 2);
        // Re-sync adds nothing (URL-keyed dedup via the bound name).
        assert_eq!(mem.sync_browser_bookmarks().expect("re-sync"), 0);

        // Each is a retrievable `memory` node bound under bookmark:<url-hash>.
        let key = crate::browser::url_key("https://libp2p.io");
        let cid = mem.resolve(&format!("bookmark:{key}")).expect("bound bookmark");
        match mem.get(&CidOrName::Cid(cid)).expect("get bookmark") {
            Record::Live { kind, body_json, .. } => {
                assert_eq!(kind, "memory");
                assert!(body_json.contains("libp2p"), "carries the bookmark");
            }
            other => panic!("expected live bookmark record, got {other:?}"),
        }

        std::env::remove_var("CONCIERGE_BOOKMARKS_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallet_link_verifies_a_signature_over_the_agent_id_and_rejects_mismatch() {
        use k256::ecdsa::{RecoveryId, Signature, SigningKey};
        use sha3::{Digest, Keccak256};
        let dir = temp_workdir("wallet-link");
        let mem = MemCli::new(&dir);
        let agent_id = mem.identity().unwrap().agent_id().0;
        let message = crate::wallet::link_message(&agent_id);

        // An external EVM key signs the link message (EIP-191 personal_sign).
        let key = SigningKey::from_bytes(&[9u8; 32].into()).unwrap();
        let point = key.verifying_key().to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let address: String =
            format!("0x{}", hash[12..].iter().map(|b| format!("{b:02x}")).collect::<String>());
        let prefixed = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
        let (sig, recid): (Signature, RecoveryId) =
            key.sign_digest_recoverable(Keccak256::new_with_prefix(prefixed.as_bytes())).unwrap();
        let mut raw = sig.to_bytes().to_vec();
        raw.push(recid.to_byte() + 27);
        let sig_hex: String =
            format!("0x{}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>());

        // The matching address links; the signature ties it to our AgentID.
        let link = mem.link_wallet(&address, "evm", &sig_hex).expect("link");
        assert_eq!(link.address, address.to_lowercase());
        assert_eq!(link.agent_id, agent_id);
        assert_eq!(mem.wallet_links().unwrap().len(), 1);
        // A different claimed address with the same signature is rejected.
        assert!(mem.link_wallet("0x000000000000000000000000000000000000dEaD", "evm", &sig_hex).is_err());
        // Unlink removes it.
        mem.unlink_wallet(&address).unwrap();
        assert!(mem.wallet_links().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallet_propose_enforces_guards_and_stages_for_approval() {
        let dir = temp_workdir("wallet-propose");
        let mem = MemCli::new(&dir);
        let to = "0x000000000000000000000000000000000000bEEF";

        // Off by default → refused.
        assert!(mem.propose_wallet_tx(to, "0.01", "", "pay").is_err());

        // Enable access, cap 0.05, allowlist the recipient.
        mem.set_wallet_settings(
            &serde_json::json!({ "agent_access": true, "spend_cap": "0.05",
                "allowlist": [to.to_lowercase()], "preferred_chain": "" })
            .to_string(),
        )
        .unwrap();

        // Over the cap → refused; off-allowlist → refused; bad address → refused.
        assert!(mem.propose_wallet_tx(to, "0.10", "", "pay").is_err());
        assert!(mem.propose_wallet_tx("0x0000000000000000000000000000000000001234", "0.01", "", "x").is_err());
        assert!(mem.propose_wallet_tx("not-an-address", "0.01", "", "x").is_err());

        // Valid → staged pending; resolving clears it from pending.
        let p = mem.propose_wallet_tx(to, "0.01", "", "pay for X").unwrap();
        assert_eq!(p.status, "pending");
        assert_eq!(mem.pending_wallet_proposals().unwrap().len(), 1);
        mem.resolve_wallet_proposal(&p.id, "approved", "0xhash").unwrap();
        assert!(mem.pending_wallet_proposals().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_rollup_links_days_into_months_and_years() {
        let dir = temp_workdir("calendar-rollup");
        let mem = MemCli::new(&dir);
        let rec = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "x" }).to_string(),
            })
            .expect("put");

        // Two June days + one July day.
        mem.record_event_in_day("2026-06-09", "e1", &rec).unwrap();
        mem.record_event_in_day("2026-06-10", "e2", &rec).unwrap();
        mem.record_event_in_day("2026-07-01", "e3", &rec).unwrap();
        mem.roll_up_calendar().unwrap();

        // Helper: the `label`s a manifest links, in order.
        let labels = |name: &str, field: &str| -> Vec<String> {
            let body_json = match mem.get(&CidOrName::Name(name.to_string())).unwrap() {
                Record::Live { body_json, .. } => body_json,
                other => panic!("expected live manifest, got {other:?}"),
            };
            let v: serde_json::Value = serde_json::from_str(&body_json).unwrap();
            v["body"][field]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["label"].as_str().unwrap().to_string())
                .collect()
        };

        assert_eq!(
            labels("month-2026-06", "days"),
            ["2026-06-09", "2026-06-10"]
        );
        assert_eq!(labels("month-2026-07", "days"), ["2026-07-01"]);
        assert_eq!(labels("year-2026", "months"), ["2026-06", "2026-07"]);

        // Re-running is idempotent (content-addressed): same year root CID.
        let year_first = mem.resolve("year-2026").unwrap();
        mem.roll_up_calendar().unwrap();
        assert_eq!(mem.resolve("year-2026").unwrap(), year_first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_manifests_use_a_deterministic_period_timestamp_not_the_wall_clock() {
        // Regression: a derived month/year manifest must stamp a `created_at`
        // derived from the *period it indexes* (midnight UTC of its start), not the
        // wall clock — otherwise re-rolling a second later produces a new CID and the
        // rollup is non-idempotent (the calendar flake).
        let dir = temp_workdir("calendar-deterministic");
        let mem = MemCli::new(&dir);
        let rec = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "x" }).to_string(),
            })
            .expect("put");
        mem.record_event_in_day("2026-06-09", "e1", &rec).unwrap();
        mem.roll_up_calendar().unwrap();

        let created_at = |name: &str| -> u64 {
            match mem.get(&CidOrName::Name(name.to_string())).unwrap() {
                Record::Live { body_json, .. } => {
                    serde_json::from_str::<serde_json::Value>(&body_json).unwrap()["created_at"]
                        .as_u64()
                        .unwrap()
                }
                other => panic!("expected live manifest, got {other:?}"),
            }
        };
        // Year 2026 → 2026-01-01T00:00:00Z; month 2026-06 → 2026-06-01T00:00:00Z.
        assert_eq!(created_at("year-2026"), period_start_unix("2026"));
        assert_eq!(created_at("year-2026"), 1_767_225_600, "Jan 1 2026 UTC");
        assert_eq!(created_at("month-2026-06"), period_start_unix("2026-06"));
        assert_eq!(created_at("month-2026-06"), 1_780_272_000, "Jun 1 2026 UTC");
        // It is the period start, deterministically — never the wall clock.
        assert!(
            created_at("year-2026") < now_secs(),
            "a fixed past timestamp, not 'now'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_roundtrips_with_parent_and_walks_subgraph() {
        let dir = temp_workdir("checkpoint");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"phase 1 lives","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let checkpoint = mem.checkpoint("latest", &root, None).expect("checkpoint");

        match mem
            .get(&CidOrName::Cid(checkpoint.clone()))
            .expect("get checkpoint")
        {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, checkpoint);
                assert_eq!(kind, "checkpoint");
                let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
                assert_eq!(value["body"]["label"], serde_json::json!("latest"));
                assert_eq!(value["body"]["root"], cid_to_json(&root).unwrap());
                assert!(value["body"]["parent"].is_null());
            }
            other => panic!("expected live checkpoint record, got {other:?}"),
        }

        let walked = mem.walk(&checkpoint).expect("walk checkpoint");
        let walked: std::collections::BTreeSet<_> = walked.into_iter().collect();
        assert!(walked.contains(&checkpoint));
        assert!(walked.contains(&root));
        assert_eq!(walked.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_returns_a_parsed_summary() {
        let dir = temp_workdir("gc");
        let mem = MemCli::new(&dir);
        let _orphan = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"throwaway","kind":"project"}"#.to_string(),
            })
            .expect("put_node");

        let report = mem.gc(&GcPolicy::default()).expect("gc");
        assert_eq!(report.removed, 1);
        assert_eq!(report.kept, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 4 exit criterion: a memory graph moves between machines without
    /// losing identity. Export a subgraph from store A, import into a fresh
    /// store B, and assert the root CID and the whole reachable set are identical.
    #[test]
    fn car_roundtrip_preserves_root_and_subgraph() {
        let dir_a = temp_workdir("car-a");
        let mem_a = MemCli::new(&dir_a);
        let m1 = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"shared artifact","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem_a.checkpoint("snap", &m1, None).expect("checkpoint");
        let mut walk_a = mem_a.walk(&cp).expect("walk a");
        walk_a.sort();
        assert!(walk_a.len() >= 2, "checkpoint reaches its root node");

        let car = mem_a.export_car(&cp).expect("export_car");

        // A fresh store on "another machine".
        let dir_b = temp_workdir("car-b");
        let mem_b = MemCli::new(&dir_b);
        let root = mem_b.import_car(&car, "imported").expect("import_car");

        assert_eq!(root, cp, "root CID is preserved across export/import");
        assert_eq!(mem_b.resolve("imported").expect("resolve imported"), cp);
        let mut walk_b = mem_b.walk(&cp).expect("walk b");
        walk_b.sort();
        assert_eq!(walk_b, walk_a, "the full subgraph moves intact");

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn car_import_rejects_a_tampered_block() {
        let dir_a = temp_workdir("car-tamper-a");
        let mem_a = MemCli::new(&dir_a);
        let m1 = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"trust me","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem_a.checkpoint("snap", &m1, None).expect("checkpoint");
        let mut car = mem_a.export_car(&cp).expect("export_car");

        // Flip a byte in the block region — its CID will no longer verify.
        let last = car.len() - 1;
        car[last] ^= 0xFF;

        let dir_b = temp_workdir("car-tamper-b");
        let mem_b = MemCli::new(&dir_b);
        assert!(
            mem_b.import_car(&car, "x").is_err(),
            "a tampered CAR must be rejected, not silently imported"
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_imports_only_for_followed_signers_and_surfaces_in_shared_with_me() {
        let dir_a = temp_workdir("signed-share-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"shared root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let dir_b = temp_workdir("signed-share-b");
        let mem_b = MemCli::new(&dir_b);
        mem_b.follow(&receipt.agent_id).expect("follow signer");
        let imported = mem_b
            .import_signed_car(&car, "inbox", &receipt.agent_id, &receipt.signature)
            .expect("import signed car");
        assert_eq!(imported, root);

        let shared = mem_b.shared_with_me().expect("shared with me");
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].agent_id, receipt.agent_id);
        assert_eq!(shared[0].root, root);
        assert_eq!(shared[0].signature, receipt.signature);
        assert_eq!(shared[0].nickname, None);

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_rejects_unknown_signer() {
        let dir_a = temp_workdir("signed-share-reject-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let dir_b = temp_workdir("signed-share-reject-b");
        let mem_b = MemCli::new(&dir_b);
        assert!(
            matches!(
                mem_b.import_signed_car(&car, "inbox", &receipt.agent_id, &receipt.signature),
                Err(Error::Io(_))
            ),
            "imports from unknown signers must be rejected until they are on the follow list"
        );

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_rejects_wrong_signature() {
        let dir_a = temp_workdir("signed-share-wrong-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let wrong_key_dir = temp_workdir("signed-share-wrong-key");
        let wrong_key = wrong_key_dir.join("identity.key");
        let wrong_identity = crate::identity::Identity::load_or_create(&wrong_key).expect("key");
        let wrong_signature = wrong_identity.sign(root.0.as_bytes());

        let dir_b = temp_workdir("signed-share-wrong-b");
        let mem_b = MemCli::new(&dir_b);
        mem_b.follow(&receipt.agent_id).expect("follow signer");
        assert!(
            matches!(
                mem_b.import_signed_car(&car, "inbox", &receipt.agent_id, &wrong_signature),
                Err(Error::Io(_))
            ),
            "imports with the wrong signature must be rejected"
        );

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
        let _ = std::fs::remove_dir_all(&wrong_key_dir);
    }

    #[test]
    fn latest_share_pointer_updates_for_new_shares() {
        let dir = temp_workdir("latest-pointer");
        let mem = MemCli::new(&dir);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem, &dir, 2);
        let agent_id = mem.agent_id().expect("agent id").0;

        let first = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"first","kind":"project"}"#.to_string(),
            })
            .expect("put first");
        mem.bind("latest", &first).expect("bind first");
        let receipt1 = publish(&mem, "latest").expect("publish first");
        let pointer_name = format!("latest-share-{agent_id}");
        let pointer1 = mem.resolve(&pointer_name).expect("pointer 1");

        let second = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"second","kind":"project"}"#.to_string(),
            })
            .expect("put second");
        mem.bind("latest", &second).expect("bind second");
        let receipt2 = publish(&mem, "latest").expect("publish second");
        let pointer2 = mem.resolve(&pointer_name).expect("pointer 2");

        assert_ne!(
            pointer1, pointer2,
            "the mutable latest pointer must advance"
        );
        let record = mem
            .get(&CidOrName::Cid(pointer2.clone()))
            .expect("pointer record");
        let Record::Live { body_json, .. } = record else {
            panic!("expected a live pointer record");
        };
        let (pointer_agent_id, root, signature) =
            parse_share_pointer(&body_json).expect("parse pointer");
        assert_eq!(pointer_agent_id, receipt2.agent_id);
        assert_eq!(root, second);
        assert_eq!(signature, receipt2.signature);
        assert_eq!(receipt1.agent_id, receipt2.agent_id);
        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn car_manifest_counts_blocks_and_bytes() {
        let dir = temp_workdir("car-manifest");
        let mem = MemCli::new(&dir);
        let m1 = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"sized","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem.checkpoint("snap", &m1, None).expect("checkpoint");

        let (cids, bytes) = mem.export_car_manifest(&cp).expect("manifest");
        assert_eq!(cids.len(), mem.walk(&cp).expect("walk").len());
        assert!(bytes > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_backends_includes_the_free_ipfs_backend() {
        let dir = temp_workdir("backends");
        let mem = MemCli::new(&dir);
        let backends = mem.list_backends().expect("list backends");
        assert!(
            backends.iter().any(|backend| backend.name == "ipfs"),
            "the free local Kubo backend should be compiled in: {backends:?}"
        );
        assert!(
            backends
                .iter()
                .any(|backend| backend.requirements_summary().contains("IPFS_API")),
            "backend requirements should be displayed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn share_without_a_configured_backend_is_a_typed_error() {
        let dir = temp_workdir("share-nobackend");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"unshared","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "bogus".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        assert!(
            matches!(publish(&mem, "latest"), Err(Error::BackendDown(_))),
            "publishing with an unconfigured backend must surface as BackendDown"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_share_requires_explicit_public_publication() {
        let dir = temp_workdir("legacy-share-refused");
        let mem = MemCli::new(&dir);
        assert!(matches!(
            mem.share("latest"),
            Err(Error::ExplicitPublicPublishRequired)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn personal_checkpoint_roots_are_locked_by_default() {
        let dir = temp_workdir("default-checkpoint-lock");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"private by default","kind":"project"}"#.to_string(),
            })
            .expect("put");
        let checkpoint = mem.checkpoint("session", &root, None).expect("checkpoint");
        let lock = mem
            .locks()
            .expect("locks")
            .into_iter()
            .find(|lock| lock.root == checkpoint.0)
            .expect("default checkpoint lock");
        assert_eq!(lock.reason, crate::egress::LockReason::DefaultPersonal);
        assert!(matches!(
            mem.write_reviewed_plaintext_car(
                &mem.build_egress_plan_for_target_and_backend(
                    &root.0,
                    crate::egress::EgressOperation::PlaintextCarExport,
                    "local-file",
                    &dir.join("blocked.car").display().to_string(),
                    "plaintext-portable",
                )
                .expect("plan"),
                &dir.join("blocked.car"),
            ),
            Err(Error::PublicationBlocked { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reviewed_plaintext_export_is_exact_destination_and_guarded() {
        let dir = temp_workdir("reviewed-plaintext-export");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"portable","kind":"project"}"#.to_string(),
            })
            .expect("put");
        // Decision 0026: fenced from egress by default — clear before exporting.
        mem.set_password("pw").expect("password");
        mem.clear_for_egress(&root, "test", "pw").expect("clear");
        let output = dir.join("reviewed.car");
        let plan = mem
            .build_egress_plan_for_target_and_backend(
                &root.0,
                crate::egress::EgressOperation::PlaintextCarExport,
                "local-file",
                &output.display().to_string(),
                "plaintext-portable",
            )
            .expect("plan");
        assert!(matches!(
            mem.write_reviewed_plaintext_car(&plan, &dir.join("changed.car")),
            Err(Error::EgressPlanChanged(_))
        ));
        let bytes = mem
            .write_reviewed_plaintext_car(&plan, &output)
            .expect("reviewed export");
        assert!(bytes > 0);
        assert_eq!(std::fs::metadata(&output).unwrap().len(), bytes);
        assert!(mem
            .security_events()
            .unwrap()
            .iter()
            .any(|event| event.action == "egress_approved" && event.root == root.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn share_with_a_down_node_is_a_backend_error() {
        let dir = temp_workdir("share-nodedown");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"will not reach a node","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = "http://127.0.0.1:5999/api/v0".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        let result = publish(&mem, "latest");
        assert!(
            matches!(result, Err(Error::BackendDown(_))),
            "a down node must surface as BackendDown, got {result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_publish_aborts_if_the_backend_target_changes_after_review() {
        let dir = temp_workdir("publish-target-change");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"reviewed","kind":"project"}"#.to_string(),
            })
            .expect("put");
        mem.bind("latest", &root).expect("bind");
        let plan = mem
            .build_egress_plan_for_target("latest", crate::egress::EgressOperation::PublicPublish)
            .expect("plan");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.ipfs_api = "http://127.0.0.1:5998/api/v0".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        assert!(matches!(
            mem.publish_public(&plan),
            Err(Error::EgressPlanChanged(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn share_writes_a_local_receipt_and_posts_car_to_the_node() {
        let dir = temp_workdir("share-node");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"publish me","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");

        let join = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).expect("read request");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(
                request_text.contains("POST /api/v0/dag/import?pin-roots=true HTTP/1.1"),
                "unexpected request: {request_text}"
            );
            let headers_end = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("headers end")
                + 4;
            let header_text = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(k, v)| {
                        if k.eq_ignore_ascii_case("content-length") {
                            v.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                })
                .expect("content length");
            let already = request.len().saturating_sub(headers_end);
            let remaining = content_length.saturating_sub(already);
            if remaining > 0 {
                let mut body = vec![0u8; remaining];
                stream.read_exact(&mut body).expect("read body");
            }
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
            stream.write_all(response).expect("write response");
        });

        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = api_url;
        cfg.save_to_project_root(&dir).expect("save config");

        let receipt = publish(&mem, "latest").expect("publish");
        assert_eq!(receipt.backend, "ipfs");
        assert_eq!(receipt.root, cid.0);
        assert!(receipt.gateway_url.contains(&cid.0));

        let receipt_trail = dir.join(".concierge").join("publish-receipts.jsonl");
        let trail = std::fs::read_to_string(&receipt_trail).expect("receipt trail");
        assert!(trail.contains(r#""backend":"ipfs""#));
        let receipts = mem.publish_receipts().expect("read receipts");
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].root, cid.0);
        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_id_is_stable_across_restart_via_binding() {
        let dir = temp_workdir("agentid");
        let first = MemCli::new(&dir).agent_id().expect("agent id");
        // A fresh binding over the same working dir = a restart; the key persists.
        let second = MemCli::new(&dir)
            .agent_id()
            .expect("agent id after restart");
        assert_eq!(
            first, second,
            "the AgentID must be the same node after a restart"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn follow_and_nickname_persist_via_binding() {
        let dir = temp_workdir("social");
        let mem = MemCli::new(&dir);
        mem.follow("agent-xyz").expect("follow");
        mem.set_nickname("agent-xyz", "Friend").expect("nickname");
        let book = mem.social_book().expect("book");
        assert!(book.is_following("agent-xyz"));
        assert_eq!(book.nickname_of("agent-xyz"), Some(&"Friend".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_and_read_message_roundtrips_and_verifies() {
        let dir = temp_workdir("msg-roundtrip");
        let mem = MemCli::new(&dir);
        let cid = mem
            .post_message("conservation", "protect the wetlands")
            .expect("post");
        let env = mem.read_message(&cid).expect("read + verify");
        assert_eq!(env.payload, "protect the wetlands");
        assert_eq!(env.id, "conservation");
        assert_eq!(env.clock, 1);
        assert_eq!(env.key, mem.agent_id().unwrap().0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn room_thread_assembles_in_chronological_order() {
        let dir = temp_workdir("msg-thread");
        let mem = MemCli::new(&dir);
        mem.post_message("r", "one").expect("1");
        mem.post_message("r", "two").expect("2");
        mem.post_message("r", "three").expect("3");
        let thread = mem.room_thread("r").expect("thread");
        let payloads: Vec<_> = thread.iter().map(|(_, e)| e.payload.clone()).collect();
        assert_eq!(payloads, ["one", "two", "three"]);
        assert_eq!(thread[0].1.clock, 1);
        assert_eq!(
            thread[2].1.clock, 3,
            "Lamport clock increments along the chain"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn message_thread_coheres_across_installs() {
        // Install A authors a 2-message chain; install B receives both envelopes
        // and must reassemble the same thread — even though `mem`'s `created_at`
        // gives the messages different *block* CIDs on each install. Linking is by
        // signature, which is identical everywhere.
        let dir_a = temp_workdir("xinstall-a");
        let a = MemCli::new(&dir_a);
        a.post_message("r", "first").expect("post 1");
        a.post_message("r", "second").expect("post 2");
        let envelopes = a.room_message_envelopes("r").expect("envelopes");
        assert_eq!(envelopes.len(), 2);

        let dir_b = temp_workdir("xinstall-b");
        let b = MemCli::new(&dir_b);
        for env_json in &envelopes {
            assert!(
                b.store_inbound_message(env_json, true)
                    .expect("store")
                    .is_some(),
                "B accepts A's signed message"
            );
        }
        let payloads: Vec<_> = b
            .room_thread("r")
            .expect("thread")
            .into_iter()
            .map(|(_, e)| e.payload)
            .collect();
        assert_eq!(
            payloads,
            ["first", "second"],
            "B reassembles the chain via signature links, not install-specific CIDs"
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn room_thread_traverses_forked_next_links() {
        let dir = temp_workdir("msg-fork");
        let mem = MemCli::new(&dir);
        let identity = mem.identity().expect("identity");
        let author = identity.agent_id().0;

        let base = MessageEnvelope {
            id: "r".to_string(),
            payload: "base".to_string(),
            next: Vec::new(),
            refs: Vec::new(),
            clock: 1,
            key: author.clone(),
            sig: String::new(),
        };
        let mut base = base;
        base.sig = identity.sign(&base.signing_bytes());
        let base_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&base).expect("serialize base"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put base");

        let mut left = MessageEnvelope {
            id: "r".to_string(),
            payload: "left".to_string(),
            next: vec![base_cid.0.clone()],
            refs: Vec::new(),
            clock: 2,
            key: author.clone(),
            sig: String::new(),
        };
        left.sig = identity.sign(&left.signing_bytes());
        let left_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&left).expect("serialize left"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put left");

        let mut right = MessageEnvelope {
            id: "r".to_string(),
            payload: "right".to_string(),
            next: vec![base_cid.0.clone()],
            refs: Vec::new(),
            clock: 3,
            key: author.clone(),
            sig: String::new(),
        };
        right.sig = identity.sign(&right.signing_bytes());
        let right_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&right).expect("serialize right"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put right");

        let mut merge = MessageEnvelope {
            id: "r".to_string(),
            payload: "merge".to_string(),
            next: vec![left_cid.0.clone(), right_cid.0.clone()],
            refs: Vec::new(),
            clock: 4,
            key: author,
            sig: String::new(),
        };
        merge.sig = identity.sign(&merge.signing_bytes());
        let merge_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&merge).expect("serialize merge"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put merge");
        mem.bind("room-latest-r", &merge_cid).expect("bind merge");

        let thread = mem.room_thread("r").expect("thread");
        let payloads: Vec<_> = thread.iter().map(|(_, e)| e.payload.clone()).collect();
        assert_eq!(payloads, ["base", "left", "right", "merge"]);
        assert_eq!(thread.last().unwrap().1.payload, "merge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_send_lever_blocks_ai_in_a_humans_only_room() {
        let dir = temp_workdir("msg-lever");
        // Mark this install as an AI.
        let cfg = Config {
            identity: crate::config::IdentityConfig {
                kind: "ai".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.save_to_project_root(&dir).expect("save cfg");
        let mem = MemCli::new(&dir);
        mem.set_room_ai_send("townhall", "off")
            .expect("humans-only");
        assert!(
            mem.post_message("townhall", "let me jump in").is_err(),
            "an AI cannot post to a Human-only room"
        );
        assert!(
            mem.post_message("open-room", "hello").is_ok(),
            "an open room still accepts the AI"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mention_gated_room_requires_an_at_mention_for_ai() {
        let dir = temp_workdir("msg-mention");
        let cfg = Config {
            identity: crate::config::IdentityConfig {
                kind: "ai".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.save_to_project_root(&dir).expect("save cfg");
        let mem = MemCli::new(&dir);
        mem.set_room_ai_send("brainstorm", "on_mention")
            .expect("mention mode");
        assert!(
            mem.post_message("brainstorm", "hello there").is_err(),
            "an AI without an @ mention must be blocked"
        );
        assert!(
            mem.post_message("brainstorm", "hello @brainstorm").is_ok(),
            "an AI with an @ mention can speak in mention-gated mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn muted_author_is_hidden_in_thread_but_still_in_the_dag() {
        let dir = temp_workdir("msg-mute");
        let mem = MemCli::new(&dir);
        let cid = mem.post_message("r", "from me").expect("post");
        let me = mem.agent_id().unwrap().0;
        mem.mute_in_room("r", &me).expect("mute");
        assert!(
            mem.room_thread("r").expect("thread").is_empty(),
            "muted author is hidden in the thread view"
        );
        assert_eq!(
            mem.read_message(&cid).expect("read by cid").payload,
            "from me",
            "but the message is still in the DAG (mute != deafen)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contact_and_naming_state_preserves_concurrent_and_fresh_updates() {
        let dir = temp_workdir("state-race");
        let mem = MemCli::new(&dir);
        let mut joins = Vec::new();
        for index in 0..16 {
            let mem = mem.clone();
            joins.push(std::thread::spawn(move || {
                mem.add_contact(&format!("contact-{index}")).unwrap();
            }));
        }
        for join in joins {
            join.join().unwrap();
        }
        assert_eq!(mem.approved_contacts().unwrap().len(), 16);

        let peer = Identity::generate();
        let mut newer = ContactCard::new(&peer.agent_id(), "new", 20).unwrap();
        newer.sign(&peer);
        mem.import_card(&serde_json::to_string(&newer).unwrap())
            .unwrap();
        let mut older = ContactCard::new(&peer.agent_id(), "old", 10).unwrap();
        older.sign(&peer);
        assert!(mem
            .import_card(&serde_json::to_string(&older).unwrap())
            .is_err());
        assert_eq!(
            mem.card_of(&peer.agent_id().0)
                .unwrap()
                .unwrap()
                .display_name,
            "new"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn naming_metadata_limits_fail_closed() {
        let dir = temp_workdir("naming-limits");
        let mem = MemCli::new(&dir);
        let peer = Identity::generate();
        let mut card =
            ContactCard::new(&peer.agent_id(), &"x".repeat(MAX_CONTACT_NAME_BYTES + 1), 1).unwrap();
        card.sign(&peer);
        assert!(mem
            .import_card(&serde_json::to_string(&card).unwrap())
            .is_err());
        assert!(mem
            .update_my_card(
                Some(&"x".repeat(MAX_CONTACT_NAME_BYTES + 1)),
                None,
                None,
                None
            )
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reviewed_external_site_deploy_rejects_folder_changes_before_network_egress() {
        let dir = temp_workdir("site-deploy-review");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        mem.set_deploy_credentials(
            "github",
            r#"{"token":"token","owner":"owner","repo":"repo","branch":"gh-pages"}"#,
        )
        .unwrap();
        let site = dir.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>reviewed</h1>").unwrap();
        let reviewed = mem
            .review_site_deploy("site", site.to_str().unwrap(), "site", "github")
            .unwrap();
        std::fs::write(site.join("index.html"), "<h1>changed</h1>").unwrap();
        let error = mem.publish_site(&reviewed, "pw").unwrap_err().to_string();
        assert!(error.contains("changed after review"), "{error}");
        assert!(mem.publish_receipts().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn deploy_credentials_reject_symlinked_vault_files() {
        use std::os::unix::fs::symlink;

        let dir = temp_workdir("deploy-credential-symlink");
        let mem = MemCli::new(&dir);
        mem.set_deploy_credentials(
            "github",
            r#"{"token":"token","owner":"owner","repo":"repo","branch":"gh-pages"}"#,
        )
        .unwrap();
        let path = mem.deploy_credentials_path().unwrap();
        std::fs::remove_file(&path).unwrap();
        let outside = dir.join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(mem.deploy_credentials().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_site_receipt_verification_binds_the_exact_reviewed_plan() {
        let dir = temp_workdir("site-deploy-receipt");
        let mem = MemCli::new(&dir);
        let identity = mem.identity().unwrap();
        let site = dir.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>site</h1>").unwrap();
        let files = crate::deploy::walk_files(&site).unwrap();
        let plan = crate::deploy::SiteDeployPlan::from_files(
            "site",
            &site,
            "site",
            "github",
            "https://api.github.com/repos/o/r/branches/gh-pages",
            &files,
        )
        .unwrap();
        let url = "https://o.github.io/r/";
        let signed = format!(
            "{}\n{}\n{}\n{}",
            plan.manifest_digest, plan.destination, plan.platform, url
        );
        let receipt = PublishReceipt {
            root: format!("external-manifest:{}", plan.manifest_digest),
            backend: plan.platform.clone(),
            unix_time: now_secs(),
            gateway_url: url.to_string(),
            agent_id: identity.agent_id().0,
            signature: identity.sign(signed.as_bytes()),
            ipns_name: None,
            site_name: Some(plan.name.clone()),
        };
        assert!(mem.verify_external_site_receipt(&receipt, &plan).unwrap());
        let mut changed = plan.clone();
        changed.destination.push_str("/other");
        assert!(!mem
            .verify_external_site_receipt(&receipt, &changed)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
