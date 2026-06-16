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

mod deployment;
mod identity_naming;
mod messaging_binding;
mod pinning;
mod publication;
mod wallet_binding;

pub use pinning::{HotPin, PinReceipt, RecordPinReceipt};

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
include!("binding/tests.rs");
