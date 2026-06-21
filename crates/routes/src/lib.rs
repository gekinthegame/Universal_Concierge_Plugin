//! Shared data-route handlers: store reads rendered to JSON, used by **both** the
//! GUI host and the kernel so there is exactly one implementation and no drift.
//!
//! Everything here returns plain data (`CoreResult<String>` JSON, bytes, or values)
//! — never an HTTP response type. Each caller wraps the result in its own response,
//! which is what lets the kernel use these without depending on the GUI.
//!
//! Phase 3 group 1: `names`, `record`, `blob`, `checkpoints` + `PrivacyOverlay` +
//! the shared query/link/preview helpers. The `graph` cluster lands in group 2.

use concierge_core::{
    cid_from_link, selected_backend_reachable, utc_date, Cid, CidOrName, CoreBinding,
    EgressOperation, Error, MemCli, Record, Result as CoreResult,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A rooted subgraph can fan out across the whole store via shared blob CIDs; the
/// view only ever renders the first this-many nodes, so BFS stops there.
const GRAPH_NODE_LIMIT: usize = 500;

/// Parse a `&` / `=` query string into a map (percent-decoded).
pub fn parse_query(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

/// Pick a record key from `?cid=` (preferred) or `?name=`.
pub fn query_key(params: &HashMap<String, String>) -> Option<CidOrName> {
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

// ── /api/meta ────────────────────────────────────────────────────────────────

pub fn meta_json(
    mem: &MemCli,
    mounted_model: &str,
    store_label: &str,
    csrf_token: &str,
) -> CoreResult<String> {
    let config = mem.config()?;
    Ok(serde_json::json!({
        "mounted_model": mounted_model,
        "store": store_label,
        "host": config.host.id,
        "adapter": config.host.adapter,
        "identity_kind": config.identity.kind,
        "read_only": true,
        "csrf_token": csrf_token,
        "password_set": mem.password_is_set().unwrap_or(false),
    })
    .to_string())
}

// ── /api/resolve ─────────────────────────────────────────────────────────────

pub fn resolve_json_for_query(mem: &MemCli, query: &str) -> CoreResult<String> {
    let params = parse_query(query);
    resolve_json(mem, params.get("q").map(String::as_str).unwrap_or(""))
}

pub fn resolve_json(mem: &MemCli, q: &str) -> CoreResult<String> {
    let matches: Vec<serde_json::Value> = mem
        .resolve_name(q)
        .into_iter()
        .map(|(agent_id, name)| {
            serde_json::json!({
                "agent_id": agent_id,
                "name": name.text,
                "source": name.source,
                "verified": name.verified,
            })
        })
        .collect();
    Ok(serde_json::json!({ "matches": matches }).to_string())
}

// ── Privacy overlay ──────────────────────────────────────────────────────────

/// Device-local privacy overlay used by every preview route. The map may still
/// show CIDs and topology while locked bodies/previews remain redacted.
pub struct PrivacyOverlay {
    // Decision 0026: everything is fenced from egress by default. The overlay
    // tracks the *exceptions* — roots explicitly cleared for export, and roots
    // already known-public — not what is "locked" (that is the default).
    pub cleared_roots: BTreeSet<String>,
    pub cleared_cids: BTreeSet<String>,
    pub known_public: BTreeSet<String>,
    /// Guardian-quarantined CIDs (§3). Surfaced as a badge locally; excluded from
    /// retrieval/relay. Local view stays transparent — you can see + release them.
    pub quarantined: BTreeSet<String>,
}

impl PrivacyOverlay {
    pub fn load(mem: &MemCli) -> CoreResult<Self> {
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

    pub fn is_quarantined(&self, cid: &str) -> bool {
        self.quarantined.contains(cid)
    }

    /// A CID is fenced from egress unless it has been explicitly cleared or is
    /// already known-public. Fenced is the default — this is what badges read.
    pub fn is_fenced(&self, cid: &Cid) -> bool {
        !self.cleared_cids.contains(&cid.0) && !self.known_public.contains(&cid.0)
    }

    pub fn is_cleared(&self, cid: &Cid) -> bool {
        self.cleared_cids.contains(&cid.0)
    }
}

// ── /api/names ───────────────────────────────────────────────────────────────

pub fn names_json(mem: &MemCli) -> CoreResult<String> {
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
/// seconds, used only to place the entry on a date), the node `kind`, and a short
/// description. Locked nodes keep their timestamp but redact the preview body.
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
            let created_at = value
                .get("created_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Does this record point at other records (a Merkle edge)? Computed from
            // the JSON already in hand — no extra store round-trip.
            let linked = mem
                .links_from_record_json(&body_json)
                .map(|l| !l.is_empty())
                .unwrap_or(false);
            Ok(serde_json::json!({
                "kind": kind,
                "created_at": created_at,
                "preview": record_preview(&value),
                "locked": privacy.is_fenced(cid),
                "linked": linked,
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

// ── /api/record ──────────────────────────────────────────────────────────────

pub fn record_json(mem: &MemCli, key: CidOrName) -> CoreResult<String> {
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
            // Blob nodes carry the file bytes as a giant JSON array — replace it with
            // a byte count so the record payload stays small (preview via /api/blob).
            if kind == "blob" {
                if let Some(object) = value
                    .get_mut("body")
                    .and_then(serde_json::Value::as_object_mut)
                {
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
        Record::Tombstone { cid, receipt_json } => Ok(serde_json::json!({
            "cid": cid.0,
            "live": false,
            "locked": privacy.is_fenced(&cid),
            "tombstone": receipt_json,
        })
        .to_string()),
    }
}

// ── /api/blob ────────────────────────────────────────────────────────────────

/// `(media_type, filename, bytes)`. A lock guards egress, not local viewing, so
/// blob bytes are always served to the local Data Platter.
pub type BlobAsset = (String, Option<String>, Vec<u8>);

/// Resolve a CID to a [`BlobAsset`]. Follows one `file_ref` → `content` hop.
pub fn blob_bytes(mem: &MemCli, cid: &Cid) -> CoreResult<Option<BlobAsset>> {
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
                .or_else(|| {
                    blob_fields
                        .get("media_type")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
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

// ── /api/checkpoints ─────────────────────────────────────────────────────────

pub fn checkpoints_json(mem: &MemCli) -> CoreResult<String> {
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

// ── /api/stats ───────────────────────────────────────────────────────────────

pub fn stats_json_for_query(mem: &MemCli, query: &str, store_label: &str) -> CoreResult<String> {
    let params = parse_query(query);
    stats_json(mem, &params, store_label)
}

fn stats_json(
    mem: &MemCli,
    params: &HashMap<String, String>,
    store_label: &str,
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
    let publishing_ready = selected_backend_reachable(&config);
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
        "store": store_label,
    })
    .to_string())
}

// ── /api/hot-pins ────────────────────────────────────────────────────────────

pub fn hot_pins_json(mem: &MemCli) -> CoreResult<String> {
    Ok(serde_json::json!({ "pins": mem.hot_pins()? }).to_string())
}

// ── Link / preview helpers (shared with the graph cluster in group 2) ────────

/// Typed + structural outbound links for a record, as `(relation, target)`.
pub fn link_details(record: &serde_json::Value, outbound_links: Vec<Cid>) -> Vec<(String, Cid)> {
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

/// Decode an IPLD link object (`{"/": "bafy…"}`) to a CID; `None` if not a link.
pub fn decode_link(value: &serde_json::Value) -> Option<Cid> {
    if value.is_null() {
        return None;
    }
    cid_from_link(value).ok()
}

/// A short, human-readable one-liner for a record (first prose-ish field).
pub fn record_preview(record: &serde_json::Value) -> String {
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

/// Truncate to `max_chars` characters, appending `...` when clipped.
pub fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

// ── /api/graph (group 2) ─────────────────────────────────────────────────────

/// `/api/graph` body: the whole-store **forest** when no target is given, else the
/// **rooted subgraph**. Returns JSON; callers wrap it in their own response. (A
/// resolve failure surfaces as an error here — callers may render it as 400/500.)
pub fn graph_json_for_query(mem: &MemCli, query: &str) -> CoreResult<String> {
    let params = parse_query(query);
    let has_target = params
        .get("cid")
        .or_else(|| params.get("root"))
        .or_else(|| params.get("name"))
        .is_some_and(|value| !value.is_empty());
    if has_target {
        let root = resolve_target(mem, &params)?;
        graph_json(mem, root)
    } else {
        forest_graph_json(mem)
    }
}

// ── /api/privacy ─────────────────────────────────────────────────────────────

pub fn privacy_json(mem: &MemCli, target: &str) -> CoreResult<String> {
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

/// Resolve `?cid=`/`?root=`/`?name=` to a root CID, falling back to `latest` then
/// the first named root.
pub fn resolve_target(mem: &MemCli, params: &HashMap<String, String>) -> CoreResult<Cid> {
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

/// Parse the session id out of a harness event name of the shape
/// `host:<host>:session:<id>:event:<hash>`. `None` for names without a session.
pub fn session_of(name: &str) -> Option<String> {
    let rest = name.split(":session:").nth(1)?;
    let id = rest.split(':').next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Civil date `YYYY-MM-DD` → Unix seconds (UTC midnight). `None` for a malformed date.
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

/// Friendly leaf label for a session bucket. `"loose"` collects events with no session.
fn session_label(session: &str) -> String {
    if session == "loose" {
        return "loose records".to_string();
    }
    let short = session.get(0..8).unwrap_or(session);
    format!("session {short}")
}

/// Node JSON + outbound `(relation, target)` links for a single CID.
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

/// Same as [`node_and_links`] but from a record already in hand (forest batch-fetch).
pub fn node_and_links_from_record(
    mem: &MemCli,
    privacy: &PrivacyOverlay,
    encrypted_plaintext_roots: &BTreeSet<String>,
    cid: &Cid,
    record: &Record,
) -> (serde_json::Value, Vec<(String, Cid)>) {
    let fenced = privacy.is_fenced(cid);
    let cleared = privacy.is_cleared(cid);
    let known_public = privacy.known_public.contains(&cid.0);
    let quarantined = privacy.is_quarantined(&cid.0);
    match record {
        Record::Live {
            kind, body_json, ..
        } => {
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

/// A store-wide forest: `store → year → month → day → session`, stopping at the
/// session tier (no per-record leaves, so no node cap). Imports are their own roots.
fn forest_graph_json(mem: &MemCli) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let encrypted_plaintext_roots = mem
        .private_conversions()?
        .into_iter()
        .map(|conversion| conversion.plaintext_root)
        .collect::<BTreeSet<_>>();

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
        if name.starts_with("day-") || name.starts_with("month-") || name.starts_with("year-") {
            continue;
        }
        if let Some(session) = session_of(&name) {
            cid_session.entry(cid.0.clone()).or_insert(session);
        }
    }

    let mut batch: Vec<Cid> = cid_session.keys().map(|c| Cid(c.clone())).collect();
    batch.extend(imports.iter().map(|(_, cid)| cid.clone()));
    let records = mem.get_many(&batch).unwrap_or_default();
    let created_day = |cid: &str| -> String {
        match records.get(cid) {
            Some(Record::Live { body_json, .. }) => {
                serde_json::from_str::<serde_json::Value>(body_json)
                    .ok()
                    .and_then(|v| v.get("created_at").and_then(serde_json::Value::as_u64))
                    .map(utc_date)
                    .unwrap_or_else(|| "undated".to_string())
            }
            _ => "undated".to_string(),
        }
    };

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
            nodes.push(tier_node(
                format!("month:{month}"),
                "month",
                &month,
                0,
                started,
            ));
            edges.push(serde_json::json!({ "from": format!("year:{year}"), "to": format!("month:{month}"), "relation": "month" }));
        }
        let day_cid = format!("day:{date}");
        if emitted_days.insert(date.clone()) {
            let dcount = *day_total.get(date).unwrap_or(&0);
            nodes.push(tier_node(day_cid.clone(), "day", date, dcount, started));
            edges.push(serde_json::json!({ "from": format!("month:{month}"), "to": day_cid.clone(), "relation": "day" }));
        }
        let session_cid = format!("session:{date}:{session}");
        nodes.push(tier_node(
            session_cid.clone(),
            "session",
            &session_label(session),
            *count,
            started,
        ));
        edges
            .push(serde_json::json!({ "from": day_cid, "to": session_cid, "relation": "session" }));
    }

    for (label, cid) in &imports {
        let (mut node, _) = match records.get(&cid.0) {
            Some(record) => {
                node_and_links_from_record(mem, &privacy, &encrypted_plaintext_roots, cid, record)
            }
            None => node_and_links(mem, &privacy, &encrypted_plaintext_roots, cid)?,
        };
        let display = label
            .split_once('-')
            .map(|(_, display)| display)
            .unwrap_or(label);
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

/// The rooted subgraph from `root`, BFS-bounded at [`GRAPH_NODE_LIMIT`] nodes.
fn graph_json(mem: &MemCli, root: Cid) -> CoreResult<String> {
    let privacy = PrivacyOverlay::load(mem)?;
    let encrypted_plaintext_roots = mem
        .private_conversions()?
        .into_iter()
        .map(|conversion| conversion.plaintext_root)
        .collect::<BTreeSet<_>>();
    let mut visited: BTreeSet<Cid> = BTreeSet::new();
    let mut included: Vec<Cid> = Vec::new();
    let mut records: HashMap<String, Record> = HashMap::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{CoreBinding, Node};

    fn store() -> (tempfile::TempDir, MemCli) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = MemCli::new(dir.path());
        (dir, mem)
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

    #[test]
    fn shared_phase3_handlers_render_store_data() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "root", "kernel route parity");
        let checkpoint = mem.checkpoint("head", &root, None).expect("checkpoint");
        mem.bind("latest", &checkpoint).expect("latest");

        let meta: serde_json::Value =
            serde_json::from_str(&meta_json(&mem, "model", "store", "csrf").expect("meta"))
                .expect("meta json");
        assert_eq!(meta["mounted_model"], "model");
        assert_eq!(meta["store"], "store");
        assert_eq!(meta["csrf_token"], "csrf");

        let names = names_json(&mem).expect("names");
        assert!(names.contains("latest"));
        assert!(names.contains(&checkpoint.0));

        let record = record_json(&mem, CidOrName::Name("latest".to_string())).expect("record");
        assert!(record.contains(&checkpoint.0));
        assert!(!record.contains("\"bytes\""));

        let checkpoints = checkpoints_json(&mem).expect("checkpoints");
        assert!(checkpoints.contains("\"label\":\"head\""));

        let graph = graph_json_for_query(&mem, "name=latest").expect("graph");
        assert!(graph.contains(&checkpoint.0));
        assert!(graph.contains(&root.0));

        let stats = stats_json_for_query(&mem, "name=latest", "store").expect("stats");
        assert!(stats.contains("\"store\":\"store\""));
        assert!(stats.contains("\"car_size\":"));

        let hot_pins = hot_pins_json(&mem).expect("hot pins");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&hot_pins).expect("hot pins json")["pins"]
                .as_array()
                .expect("pins array")
                .len(),
            0
        );

        let privacy = privacy_json(&mem, "latest").expect("privacy");
        assert!(privacy.contains("\"root\":"));

        let resolve = resolve_json_for_query(&mem, "q=alice").expect("resolve");
        assert!(resolve.contains("\"matches\""));
    }
}
