//! The typed node taxonomy — the single source of truth. Mirrored by
//! `schema.ipldsch` (Phase 1.4). Adding a `Node` variant is a deliberate act:
//! bump `Record::schema_version` and stay additive-only (see the spec's
//! "Schema discipline").

use crate::cid::Cid;
use serde::{Deserialize, Serialize};

/// A CID link to another record. Under DAG-CBOR this serializes as a tag-42
/// IPLD link, so the DAG is walkable and recursively pinnable.
pub type Link = Cid;

/// The schema version stamped into every Record we write today. Bump on a
/// taxonomy change; evolution stays additive-only (new fields are `nullable`/
/// `optional`), so older records keep decoding. See "Schema discipline".
pub const CURRENT_SCHEMA_VERSION: u16 = 8;

/// Every block we write is a Record: a typed payload plus the metadata a
/// content-addressed, permanent store cannot retrofit later.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Record {
    pub schema_version: u16,
    pub created_at: u64,
    pub source: Source,
    pub edges: Vec<Edge>,
    pub body: Node,
}

/// A labeled, directed relationship to another record. Structural links that
/// code acts on stay typed fields on the node; associative edges live here.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Edge {
    pub rel: EdgeRel,
    pub to: Link,
}

/// Closed vocabulary for associative graph edges.
///
/// Unit variants serialize as snake_case strings in DAG-CBOR, but Rust callers
/// cannot mint arbitrary relationship labels. Adding a relationship is a schema
/// decision: add a variant, update `schema.ipldsch`, and bump the reader.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRel {
    Produced,
    PlannedAs,
    DerivedFrom,
    UsedAsContext,
    References,
    Contains,
    Summarizes,
    Supersedes,
}

/// Provenance: who/what produced a record.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    User,
    Model { name: String },
    System,
    Derived { from: Vec<Link> },
}

/// Every durable thing the agent knows is one of these.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Memory(Memory),
    UserPrefs(UserPrefs),
    Plan(Plan),
    Decision(Decision),
    Prompt(Prompt),
    Response(Response),
    ToolResult(ToolResult),
    Blob(Blob),
    FileRef(FileRef),
    Task(Task),
    Conversation(Conversation),
    Skill(Skill),
    Checkpoint(Checkpoint),
    DirectoryManifest(DirectoryManifest),
    IngestRun(IngestRun),
    Symbol(Symbol),
    ExtractedText(ExtractedText),
    // Phase A.5 calendar tier (DECISIONS.md 0014): index nodes, not new content.
    // Every event stays its own record; these only change how it is reached.
    DayIndex(DayIndex),
    MonthIndex(MonthIndex),
    YearIndex(YearIndex),
}

impl Node {
    /// The variant's wire tag (the `"type"` discriminant value). Single source
    /// for the human-facing kind label used in tracing; a test pins it to the
    /// serde-encoded tag so the two can never drift.
    pub fn kind(&self) -> &'static str {
        match self {
            Node::Memory(_) => "memory",
            Node::UserPrefs(_) => "user_prefs",
            Node::Plan(_) => "plan",
            Node::Decision(_) => "decision",
            Node::Prompt(_) => "prompt",
            Node::Response(_) => "response",
            Node::ToolResult(_) => "tool_result",
            Node::Blob(_) => "blob",
            Node::FileRef(_) => "file_ref",
            Node::Task(_) => "task",
            Node::Conversation(_) => "conversation",
            Node::Skill(_) => "skill",
            Node::Checkpoint(_) => "checkpoint",
            Node::DirectoryManifest(_) => "directory_manifest",
            Node::IngestRun(_) => "ingest_run",
            Node::Symbol(_) => "symbol",
            Node::ExtractedText(_) => "extracted_text",
            Node::DayIndex(_) => "day_index",
            Node::MonthIndex(_) => "month_index",
            Node::YearIndex(_) => "year_index",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Memory {
    pub text: String,
    pub kind: MemoryKind,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    User,
    Feedback,
    Project,
    Reference,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub body: String,
    pub supersedes: Option<Link>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Plan {
    pub title: String,
    pub prose: String,
    pub spec: Option<Link>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Conversation {
    pub turns: Vec<Link>,
    pub parent: Option<Link>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Checkpoint {
    pub label: String,
    pub root: Link,
    pub parent: Option<Link>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserPrefs {
    pub entries: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Decision {
    pub question: String,
    pub choice: String,
    pub rationale: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Prompt {
    pub text: String,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Response {
    pub text: String,
    pub model: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolResult {
    pub tool: String,
    pub input: String,
    pub output: String,
    pub ok: bool,
}

/// Opaque content addressed independently from any path that references it.
/// `FileRef.content` points here, so path metadata can change without copying
/// bytes and identical file bodies deduplicate to the same content record.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Blob {
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    pub media_type: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileRef {
    pub path: String,
    /// Size in bytes for the referenced file when ingest recorded it.
    #[serde(default)]
    pub size: Option<u64>,
    /// Best-known media type for the referenced file, if detected.
    #[serde(default)]
    pub media_type: Option<String>,
    /// Last modification time as Unix seconds, if the filesystem exposed it.
    #[serde(default)]
    pub mtime: Option<u64>,
    /// Must point to a `Node::Blob` record.
    pub content: Link,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Task {
    pub title: String,
    pub prose: String,
    pub parent: Option<Link>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DirectoryManifest {
    pub root_path: String,
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DirectoryEntry {
    pub path: String,
    pub file_ref: Link,
}

/// One UTC day's events, indexed by an IPLD HAMT (Phase A.5; DECISIONS.md 0014).
/// `events` is the HAMT root CID mapping an event key (e.g. `created_at`+id) to
/// the event record's Link. The day root is re-bound as events arrive (hot) and
/// sealed into an immutable CID once the day passes. The `Record::created_at`
/// wrapper carries the required UTC timestamp.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DayIndex {
    /// UTC calendar day, `YYYY-MM-DD`.
    pub date: String,
    /// HAMT root linking this day's event records.
    pub events: Link,
}

/// A calendar month: a manifest linking its day roots (Phase A.5). Small enough
/// (≤31 children) to stay a plain manifest rather than a HAMT.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MonthIndex {
    /// UTC month, `YYYY-MM`.
    pub month: String,
    pub days: Vec<CalendarEntry>,
}

/// A calendar year: a manifest linking its month roots (Phase A.5).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct YearIndex {
    /// UTC year, `YYYY`.
    pub year: String,
    pub months: Vec<CalendarEntry>,
}

/// One child link in a month/year manifest: the child's calendar label and its
/// index-node Link (a `DayIndex` under a month, a `MonthIndex` under a year).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CalendarEntry {
    /// The child's calendar label (`YYYY-MM-DD` for a day, `YYYY-MM` for a month).
    pub label: String,
    pub node: Link,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IngestRun {
    pub source_path: String,
    pub manifest: Link,
    pub file_count: u64,
    pub byte_count: u64,
    pub ignored_count: u64,
    pub plugin_records: u64,
    #[serde(default)]
    pub plugin_failures: u64,
    #[serde(default)]
    pub per_file_plugin_records: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub per_file_plugin_failures: std::collections::BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Symbol {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub language: String,
    pub signature: String,
    pub body: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtractedText {
    pub path: String,
    pub text: String,
    pub media_type: Option<String>,
}

/// Encode a Record to DAG-CBOR. CID links serialize as tag-42 IPLD links.
pub fn encode(record: &Record) -> anyhow::Result<Vec<u8>> {
    serde_ipld_dagcbor::to_vec(record).map_err(|e| anyhow::anyhow!("dag-cbor encode failed: {e}"))
}

/// Decode DAG-CBOR bytes into a Record. Evolution is additive-only, so newer
/// readers tolerate older records through explicit schema-version dispatch.
pub fn decode(bytes: &[u8]) -> anyhow::Result<Record> {
    let probe: VersionProbe = serde_ipld_dagcbor::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("dag-cbor version probe failed: {e}"))?;
    match probe.schema_version {
        0 => migrate_v0(bytes),
        1..=CURRENT_SCHEMA_VERSION => decode_v1(bytes),
        version => anyhow::bail!("unsupported schema_version {version}"),
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    schema_version: u16,
}

#[derive(Deserialize)]
struct RecordV0 {
    schema_version: u16,
    created_at: u64,
    source: Source,
    body: Node,
}

fn decode_v1(bytes: &[u8]) -> anyhow::Result<Record> {
    serde_ipld_dagcbor::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("dag-cbor v1 decode failed: {e}"))
}

fn migrate_v0(bytes: &[u8]) -> anyhow::Result<Record> {
    let old: RecordV0 = serde_ipld_dagcbor::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("dag-cbor v0 decode failed: {e}"))?;
    Ok(Record {
        schema_version: old.schema_version,
        created_at: old.created_at,
        source: old.source,
        edges: Vec::new(),
        body: old.body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipld_core::ipld::Ipld;
    use serde::Serialize;

    fn link() -> Link {
        crate::cid::compute(b"sample-link")
    }

    fn samples() -> Vec<Record> {
        let l = link();
        let bodies = vec![
            Node::Memory(Memory {
                text: "m".into(),
                kind: MemoryKind::Project,
            }),
            Node::UserPrefs(UserPrefs {
                entries: vec![("k".into(), "v".into())],
            }),
            Node::Plan(Plan {
                title: "t".into(),
                prose: "p".into(),
                spec: Some(l),
            }),
            Node::Decision(Decision {
                question: "q".into(),
                choice: "c".into(),
                rationale: "r".into(),
            }),
            Node::Prompt(Prompt {
                text: "t".into(),
                model: Some("opus".into()),
            }),
            Node::Response(Response {
                text: "t".into(),
                model: "opus".into(),
            }),
            Node::ToolResult(ToolResult {
                tool: "t".into(),
                input: "i".into(),
                output: "o".into(),
                ok: true,
            }),
            Node::Blob(Blob {
                bytes: vec![1, 2, 3],
                media_type: Some("text/plain".into()),
            }),
            Node::FileRef(FileRef {
                path: "/p".into(),
                size: Some(3),
                media_type: Some("text/plain".into()),
                mtime: Some(1234),
                content: l,
            }),
            Node::Task(Task {
                title: "t".into(),
                prose: "p".into(),
                parent: Some(l),
            }),
            Node::Conversation(Conversation {
                turns: vec![l],
                parent: None,
            }),
            Node::Skill(Skill {
                name: "n".into(),
                body: "b".into(),
                supersedes: Some(l),
            }),
            Node::Checkpoint(Checkpoint {
                label: "l".into(),
                root: l,
                parent: Some(l),
            }),
            Node::DirectoryManifest(DirectoryManifest {
                root_path: "/r".into(),
                entries: vec![DirectoryEntry {
                    path: "f".into(),
                    file_ref: l,
                }],
            }),
            Node::IngestRun(IngestRun {
                source_path: "/s".into(),
                manifest: l,
                file_count: 1,
                byte_count: 2,
                ignored_count: 3,
                plugin_records: 4,
                plugin_failures: 5,
                per_file_plugin_records: [("f".into(), 6)].into(),
                per_file_plugin_failures: [("f".into(), 7)].into(),
            }),
            Node::Symbol(Symbol {
                path: "/p".into(),
                name: "n".into(),
                kind: "k".into(),
                language: "l".into(),
                signature: "s".into(),
                body: "b".into(),
                start_line: 1,
                end_line: 2,
            }),
            Node::ExtractedText(ExtractedText {
                path: "/p".into(),
                text: "t".into(),
                media_type: Some("m".into()),
            }),
            Node::DayIndex(DayIndex {
                date: "2026-06-09".into(),
                events: l,
            }),
            Node::MonthIndex(MonthIndex {
                month: "2026-06".into(),
                days: vec![CalendarEntry {
                    label: "2026-06-09".into(),
                    node: l,
                }],
            }),
            Node::YearIndex(YearIndex {
                year: "2026".into(),
                months: vec![CalendarEntry {
                    label: "2026-06".into(),
                    node: l,
                }],
            }),
        ];
        let sources = [
            Source::User,
            Source::System,
            Source::Model {
                name: "opus".into(),
            },
            Source::Derived { from: vec![l] },
        ];
        let edge_rels = [
            EdgeRel::Produced,
            EdgeRel::PlannedAs,
            EdgeRel::DerivedFrom,
            EdgeRel::UsedAsContext,
            EdgeRel::References,
            EdgeRel::Contains,
            EdgeRel::Summarizes,
            EdgeRel::Supersedes,
        ];
        bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| Record {
                schema_version: CURRENT_SCHEMA_VERSION,
                created_at: 1000 + i as u64,
                source: sources[i % sources.len()].clone(),
                edges: vec![Edge {
                    rel: edge_rels[i % edge_rels.len()],
                    to: l,
                }],
                body,
            })
            .collect()
    }

    struct GoldenFixture {
        name: &'static str,
        hex: &'static str,
        cid: &'static str,
    }

    fn golden_fixtures() -> Vec<GoldenFixture> {
        vec![
            ("memory", "a564626f6479a3646b696e646770726f6a6563746474657874616d6474797065666d656d6f727965656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6870726f647563656466736f75726365a1646b696e6464757365726a637265617465645f61741903e86e736368656d615f76657273696f6e08", "bafyreihnvgoh6hq6wdndvv2gdmaken7aa65qjssi4cvfd3lrh6teigmzci"),
            ("user_prefs", "a564626f6479a264747970656a757365725f707265667367656e74726965738182616b617665656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a706c616e6e65645f617366736f75726365a1646b696e646673797374656d6a637265617465645f61741903e96e736368656d615f76657273696f6e08", "bafyreia7ghtzdyu2shi2grvjqdafikyhndejqbab5avgoftrlpac4gdpti"),
            ("plan", "a564626f6479a46473706563d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508647479706564706c616e6570726f73656170657469746c65617465656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6c646572697665645f66726f6d66736f75726365a2646b696e64656d6f64656c646e616d65646f7075736a637265617465645f61741903ea6e736368656d615f76657273696f6e08", "bafyreifdylc6as3busn7dtql4c3nottighxqui4bgax4ajymeehzkqyejq"),
            ("decision", "a564626f6479a46474797065686465636973696f6e6663686f6963656163687175657374696f6e617169726174696f6e616c65617265656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6f757365645f61735f636f6e7465787466736f75726365a26466726f6d81d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508646b696e6467646572697665646a637265617465645f61741903eb6e736368656d615f76657273696f6e08", "bafyreifgp7trovbevq2pnd445ouhrdbquyevulkdxnimsvlulrco2buppa"),
            ("prompt", "a564626f6479a36474657874617464747970656670726f6d7074656d6f64656c646f70757365656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a7265666572656e63657366736f75726365a1646b696e6464757365726a637265617465645f61741903ec6e736368656d615f76657273696f6e08", "bafyreih25id3qc3begwcsadaxskg532fe75ncgvkjepgcbecuzyryiwgoa"),
            ("response", "a564626f6479a364746578746174647479706568726573706f6e7365656d6f64656c646f70757365656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c68636f6e7461696e7366736f75726365a1646b696e646673797374656d6a637265617465645f61741903ed6e736368656d615f76657273696f6e08", "bafyreihjutsr35osocvo7n3c7zdm3qcrd22j2773k2tpmapdo5p6f5g7q4"),
            ("tool_result", "a564626f6479a5626f6bf564746f6f6c617464747970656b746f6f6c5f726573756c7465696e7075746169666f7574707574616f65656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a73756d6d6172697a657366736f75726365a2646b696e64656d6f64656c646e616d65646f7075736a637265617465645f61741903ee6e736368656d615f76657273696f6e08", "bafyreifluqdyxc5hbmw2unc3yeprve2kwjnwjzzpcz7uycfqxq6vf52n6e"),
            ("blob", "a564626f6479a3647479706564626c6f62656279746573430102036a6d656469615f747970656a746578742f706c61696e65656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a7375706572736564657366736f75726365a26466726f6d81d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508646b696e6467646572697665646a637265617465645f61741903ef6e736368656d615f76657273696f6e08", "bafyreicdxfjxmrmech425b4oxfbjjoj7notlq2mvnsbin7lmgssfqtbqsa"),
            ("file_ref", "a564626f6479a66470617468622f706473697a650364747970656866696c655f726566656d74696d651904d267636f6e74656e74d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086a6d656469615f747970656a746578742f706c61696e65656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6870726f647563656466736f75726365a1646b696e6464757365726a637265617465645f61741903f06e736368656d615f76657273696f6e08", "bafyreigjpvin7lo4uyyocfd4rwipeqzyaielhsabvcyrowkzkbtbwvaclm"),
            ("task", "a564626f6479a46474797065647461736b6570726f73656170657469746c65617466706172656e74d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650865656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a706c616e6e65645f617366736f75726365a1646b696e646673797374656d6a637265617465645f61741903f16e736368656d615f76657273696f6e08", "bafyreiehlberpxifuraslr7i4mj5tecty2wa6fwlpefuikxpo3ummyjyqq"),
            ("conversation", "a564626f6479a364747970656c636f6e766572736174696f6e657475726e7381d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650866706172656e74f665656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6c646572697665645f66726f6d66736f75726365a2646b696e64656d6f64656c646e616d65646f7075736a637265617465645f61741903f26e736368656d615f76657273696f6e08", "bafyreibt7d3vsnnu75omdxsz2jdjwhlcajhxjt5futw4fzz7fqo6mf5j2e"),
            ("skill", "a564626f6479a464626f64796162646e616d65616e647479706565736b696c6c6a73757065727365646573d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650865656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6f757365645f61735f636f6e7465787466736f75726365a26466726f6d81d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508646b696e6467646572697665646a637265617465645f61741903f36e736368656d615f76657273696f6e08", "bafyreigbx2qs2ugpuk2zccdhgcmcc3reyzyx5nndstyhekzrwhvx3dadii"),
            ("checkpoint", "a564626f6479a464726f6f74d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650864747970656a636865636b706f696e74656c6162656c616c66706172656e74d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650865656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a7265666572656e63657366736f75726365a1646b696e6464757365726a637265617465645f61741903f46e736368656d615f76657273696f6e08", "bafyreiekvvpnc36nypoqf5onxl2sctqem5q7qfzaepvschqmihzmww35be"),
            ("directory_manifest", "a564626f6479a36474797065726469726563746f72795f6d616e696665737467656e747269657381a2647061746861666866696c655f726566d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650869726f6f745f70617468622f7265656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c68636f6e7461696e7366736f75726365a1646b696e646673797374656d6a637265617465645f61741903f56e736368656d615f76657273696f6e08", "bafyreigo4s5uigavudl2lslw4skcvw7wspjzwp4khyowa3s3dewjwxmc3y"),
            ("ingest_run", "a564626f6479aa64747970656a696e676573745f72756e686d616e6966657374d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086a627974655f636f756e74026a66696c655f636f756e74016b736f757263655f70617468622f736d69676e6f7265645f636f756e74036e706c7567696e5f7265636f726473046f706c7567696e5f6661696c7572657305777065725f66696c655f706c7567696e5f7265636f726473a161660678187065725f66696c655f706c7567696e5f6661696c75726573a161660765656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a73756d6d6172697a657366736f75726365a2646b696e64656d6f64656c646e616d65646f7075736a637265617465645f61741903f66e736368656d615f76657273696f6e08", "bafyreieq4wynv6dxxl5qeysyeg2s7t2rcbxiihajnj2poooqjst4dwcw4e"),
            ("symbol", "a564626f6479a964626f64796162646b696e64616b646e616d65616e6470617468622f7064747970656673796d626f6c68656e645f6c696e6502686c616e6775616765616c697369676e617475726561736a73746172745f6c696e650165656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a7375706572736564657366736f75726365a26466726f6d81d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508646b696e6467646572697665646a637265617465645f61741903f76e736368656d615f76657273696f6e08", "bafyreidhwuo2f2mnlzzulks4supgeol7p3x2hm6or4moodpcwmbvq4ymli"),
            ("extracted_text", "a564626f6479a46470617468622f706474657874617464747970656e6578747261637465645f746578746a6d656469615f74797065616d65656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6870726f647563656466736f75726365a1646b696e6464757365726a637265617465645f61741903f86e736368656d615f76657273696f6e08", "bafyreihguszqqfsrs3tnp7ov6dkvmhq2dkaa6fmwjd26vsbtbohxshgcwu"),
            ("day_index", "a564626f6479a364646174656a323032362d30362d30396474797065696461795f696e646578666576656e7473d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef89870650865656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6a706c616e6e65645f617366736f75726365a1646b696e646673797374656d6a637265617465645f61741903f96e736368656d615f76657273696f6e08", "bafyreibtob6mqy5dggo6hkduotbw4bihiti6y45rpycdrbwi2nx56p3maa"),
            ("month_index", "a564626f6479a3646461797381a2646e6f6465d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508656c6162656c6a323032362d30362d303964747970656b6d6f6e74685f696e646578656d6f6e746867323032362d303665656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6c646572697665645f66726f6d66736f75726365a2646b696e64656d6f64656c646e616d65646f7075736a637265617465645f61741903fa6e736368656d615f76657273696f6e08", "bafyreibvmdgl2inb55clcgeyongyigcpgxgcto4idlltwtkuk6na2m4q7y"),
            ("year_index", "a564626f6479a364747970656a796561725f696e64657864796561726432303236666d6f6e74687381a2646e6f6465d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508656c6162656c67323032362d303665656467657381a262746fd82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef8987065086372656c6f757365645f61735f636f6e7465787466736f75726365a26466726f6d81d82a58250001711220d22895d40d270bd37b9393d52508f00183fa7cab5f86b32e254d3ef898706508646b696e6467646572697665646a637265617465645f61741903fb6e736368656d615f76657273696f6e08", "bafyreierq5pzaac2jbbnutnqwqhwssldkaip3a5eubg3mmaref5f3yzzoi"),
        ].into_iter().map(|(name, hex, cid)| GoldenFixture { name, hex, cid }).collect()
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        let compact: String = hex.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(compact.len() % 2, 0, "hex fixture must have even length");
        (0..compact.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&compact[i..i + 2], 16).expect("valid hex fixture byte"))
            .collect()
    }

    fn collect_keys(ipld: &Ipld, out: &mut std::collections::BTreeSet<String>) {
        match ipld {
            Ipld::Map(m) => {
                for (k, v) in m {
                    out.insert(k.clone());
                    collect_keys(v, out);
                }
            }
            Ipld::List(l) => {
                for v in l {
                    collect_keys(v, out);
                }
            }
            _ => {}
        }
    }

    fn schema_node_union_tags(schema: &str) -> std::collections::BTreeSet<String> {
        let mut tags = std::collections::BTreeSet::new();
        let mut in_node_union = false;
        for line in schema.lines() {
            let line = line.trim();
            if line == "type Node union {" {
                in_node_union = true;
                continue;
            }
            if in_node_union && line.starts_with('}') {
                break;
            }
            if !in_node_union || !line.starts_with('|') {
                continue;
            }
            let tag = line
                .split('"')
                .nth(1)
                .expect("Node union member must include a quoted wire tag");
            tags.insert(tag.to_string());
        }
        tags
    }

    #[test]
    fn golden_fixtures_pin_wire_bytes_and_cids_for_every_variant() {
        let records = samples();
        let fixtures = golden_fixtures();
        assert_eq!(
            records.len(),
            fixtures.len(),
            "one fixture per Node variant"
        );
        for (record, fixture) in records.iter().zip(fixtures.iter()) {
            let expected_bytes = decode_hex(fixture.hex);
            let encoded = encode(record).unwrap();
            assert_eq!(
                encoded, expected_bytes,
                "{} fixture bytes changed",
                fixture.name
            );
            assert_eq!(
                crate::cid::compute(&expected_bytes).to_string(),
                fixture.cid,
                "{} fixture CID changed",
                fixture.name
            );
            let decoded = decode(&expected_bytes).unwrap();
            assert_eq!(
                encode(&decoded).unwrap(),
                expected_bytes,
                "{} fixture must decode and re-encode byte-stably",
                fixture.name
            );
        }
    }

    #[test]
    fn every_variant_round_trips_byte_stably() {
        for rec in samples() {
            let b1 = encode(&rec).expect("encode");
            let back = decode(&b1).expect("decode");
            let b2 = encode(&back).expect("re-encode");
            assert_eq!(b1, b2, "round-trip must be byte-stable for {:?}", rec.body);
        }
    }

    #[test]
    fn node_kind_matches_serde_type_tag() {
        for rec in samples() {
            let expected = rec.body.kind();
            let bytes = encode(&rec).unwrap();
            let ipld: Ipld = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
            let tag = match &ipld {
                Ipld::Map(top) => match top.get("body") {
                    Some(Ipld::Map(body)) => match body.get("type") {
                        Some(Ipld::String(s)) => s.clone(),
                        _ => panic!("body missing \"type\" tag"),
                    },
                    _ => panic!("record body must be a map"),
                },
                _ => panic!("record must encode as a map"),
            };
            assert_eq!(
                tag.as_str(),
                expected,
                "kind() must equal the serde type tag"
            );
        }
    }

    #[test]
    fn cid_links_serialize_as_tag_42_not_strings() {
        let rec = &samples()[12];
        let bytes = encode(rec).unwrap();
        assert!(
            bytes.windows(2).any(|w| w == [0xd8, 0x2a]),
            "CID link must be encoded as CBOR tag-42"
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains("bafy"),
            "CID must not be stringified into the bytes"
        );
    }

    #[test]
    fn old_shape_record_decodes_into_additive_struct() {
        let old = samples().into_iter().next().unwrap();
        #[derive(Serialize)]
        struct OldRecord {
            schema_version: u16,
            created_at: u64,
            source: Source,
            body: Node,
        }
        let bytes = serde_ipld_dagcbor::to_vec(&OldRecord {
            schema_version: 0,
            created_at: old.created_at,
            source: old.source,
            body: old.body,
        })
        .unwrap();
        let migrated = decode(&bytes).unwrap();
        assert_eq!(migrated.schema_version, 0);
        assert!(migrated.edges.is_empty());
    }

    #[test]
    fn decode_rejects_unknown_schema_version() {
        let mut rec = samples().into_iter().next().unwrap();
        rec.schema_version = CURRENT_SCHEMA_VERSION + 1;
        let bytes = serde_ipld_dagcbor::to_vec(&rec).unwrap();
        let err = decode(&bytes).unwrap_err().to_string();
        assert!(err.contains("unsupported schema_version"));
    }

    #[test]
    fn schema_v1_records_still_decode_after_additive_schema_bumps() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 8);
        let mut rec = samples().into_iter().next().unwrap();
        rec.schema_version = 1;
        let bytes = serde_ipld_dagcbor::to_vec(&rec).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.schema_version, 1);
        assert!(matches!(decoded.body, Node::Memory(_)));
    }

    #[test]
    fn v2_file_refs_without_ingest_metadata_still_decode() {
        #[derive(Serialize)]
        struct OldFileRefBody {
            #[serde(rename = "type")]
            node_type: &'static str,
            path: String,
            content: Link,
        }
        #[derive(Serialize)]
        struct OldRecord {
            schema_version: u16,
            created_at: u64,
            source: Source,
            edges: Vec<Edge>,
            body: OldFileRefBody,
        }
        let bytes = serde_ipld_dagcbor::to_vec(&OldRecord {
            schema_version: 2,
            created_at: 1,
            source: Source::User,
            edges: Vec::new(),
            body: OldFileRefBody {
                node_type: "file_ref",
                path: "legacy.txt".to_string(),
                content: link(),
            },
        })
        .unwrap();
        let decoded = decode(&bytes).unwrap();
        let Node::FileRef(file_ref) = decoded.body else {
            panic!("not a file ref");
        };
        assert_eq!(file_ref.path, "legacy.txt");
        assert_eq!(file_ref.size, None);
    }

    #[test]
    fn v3_ingest_runs_without_plugin_failures_still_decode() {
        #[derive(Serialize)]
        struct OldIngestRunBody {
            #[serde(rename = "type")]
            node_type: &'static str,
            source_path: String,
            manifest: Link,
            file_count: u64,
            byte_count: u64,
            ignored_count: u64,
            plugin_records: u64,
        }
        #[derive(Serialize)]
        struct OldRecord {
            schema_version: u16,
            created_at: u64,
            source: Source,
            edges: Vec<Edge>,
            body: OldIngestRunBody,
        }
        let bytes = serde_ipld_dagcbor::to_vec(&OldRecord {
            schema_version: 3,
            created_at: 1,
            source: Source::User,
            edges: Vec::new(),
            body: OldIngestRunBody {
                node_type: "ingest_run",
                source_path: "/legacy".to_string(),
                manifest: link(),
                file_count: 1,
                byte_count: 2,
                ignored_count: 3,
                plugin_records: 4,
            },
        })
        .unwrap();
        let decoded = decode(&bytes).unwrap();
        let Node::IngestRun(run) = decoded.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.source_path, "/legacy");
        assert_eq!(run.plugin_failures, 0);
        assert!(run.per_file_plugin_records.is_empty());
        assert!(run.per_file_plugin_failures.is_empty());
    }

    #[test]
    fn v6_ingest_runs_without_per_file_failures_still_decode() {
        #[derive(Serialize)]
        struct OldIngestRunBody {
            #[serde(rename = "type")]
            node_type: &'static str,
            source_path: String,
            manifest: Link,
            file_count: u64,
            byte_count: u64,
            ignored_count: u64,
            plugin_records: u64,
            plugin_failures: u64,
            per_file_plugin_records: std::collections::BTreeMap<String, u64>,
        }
        #[derive(Serialize)]
        struct OldRecord {
            schema_version: u16,
            created_at: u64,
            source: Source,
            edges: Vec<Edge>,
            body: OldIngestRunBody,
        }
        let bytes = serde_ipld_dagcbor::to_vec(&OldRecord {
            schema_version: 6,
            created_at: 1,
            source: Source::User,
            edges: Vec::new(),
            body: OldIngestRunBody {
                node_type: "ingest_run",
                source_path: "/legacy".to_string(),
                manifest: link(),
                file_count: 1,
                byte_count: 2,
                ignored_count: 3,
                plugin_records: 4,
                plugin_failures: 5,
                per_file_plugin_records: [("legacy.py".to_string(), 6)].into(),
            },
        })
        .unwrap();
        let decoded = decode(&bytes).unwrap();
        let Node::IngestRun(run) = decoded.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.per_file_plugin_records["legacy.py"], 6);
        assert!(run.per_file_plugin_failures.is_empty());
    }

    #[test]
    fn schema_documents_every_node_wire_tag() {
        const SCHEMA: &str = include_str!("../schema.ipldsch");
        let rust_tags: std::collections::BTreeSet<String> = samples()
            .iter()
            .map(|rec| rec.body.kind().to_string())
            .collect();
        let schema_tags = schema_node_union_tags(SCHEMA);
        assert_eq!(schema_tags, rust_tags);
    }

    #[test]
    fn wire_field_names_are_documented_in_schema() {
        const SCHEMA: &str = include_str!("../schema.ipldsch");
        let mut keys = std::collections::BTreeSet::new();
        for rec in samples() {
            let bytes = encode(&rec).unwrap();
            let ipld: Ipld = serde_ipld_dagcbor::from_slice(&bytes).unwrap();
            collect_keys(&ipld, &mut keys);
        }
        let missing: Vec<&String> = keys
            .iter()
            .filter(|k| !SCHEMA.contains(k.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "schema.ipldsch is missing wire field names: {missing:?}"
        );
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(&[0xff, 0xff, 0xff]).is_err());
    }
}
