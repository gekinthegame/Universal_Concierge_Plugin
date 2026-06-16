use super::*;

pub(super) fn peers_response(mem: &MemCli, options: &GuiOptions) -> Response {
    let _ = ensure_chat_node(mem, options);
    let now = now_unix();
    let (online, self_peer, self_agent, mut peers) = match options.chat.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(chat) => {
                let list: Vec<PeerInfo> = chat
                    .peers
                    .lock()
                    .map(|m| m.values().cloned().collect())
                    .unwrap_or_default();
                (true, chat.peer_id.clone(), chat.agent_id.clone(), list)
            }
            None => (false, String::new(), String::new(), Vec::new()),
        },
        Err(_) => (false, String::new(), String::new(), Vec::new()),
    };
    // Drop discovered-only peers we haven't actually reached in a while; keep all
    // connected ones. Show the freshest first, capped so the map stays legible.
    peers.retain(|p| {
        p.status == "connected" || now.saturating_sub(p.last_seen) < DISCOVERY_PEER_TTL_SECS
    });
    peers.sort_by(|a, b| {
        (b.status == "connected")
            .cmp(&(a.status == "connected"))
            .then(b.last_seen.cmp(&a.last_seen))
    });
    let total = peers.len();
    let connected = peers.iter().filter(|p| p.status == "connected").count();
    peers.truncate(200);
    // Which peers are fellow Concierges (drawn as brains, vs generic network nodes drawn
    // as stars): an approved contact, or a peer found via our concierge rendezvous
    // namespace. Everything else is just an anonymous IPFS/libp2p node in the swarm.
    let concierge_peers: std::collections::HashSet<String> = mem
        .approved_contacts()
        .unwrap_or_default()
        .iter()
        .filter_map(|agent_id| peer_id_from_ed25519_hex(agent_id).map(|p| p.to_string()))
        .collect();
    let peers_json: Vec<serde_json::Value> = peers
        .iter()
        .map(|p| {
            let is_concierge = p.source == "rendezvous" || concierge_peers.contains(&p.peer_id);
            // A concierge node's AgentID ("username") for DMs is recoverable from its
            // Ed25519 PeerID — so clicking its brain can copy a sendable username.
            let username = is_concierge
                .then(|| p.peer_id.parse().ok().and_then(|peer| ed25519_hex_from_peer_id(&peer)))
                .flatten();
            serde_json::json!({
                "peer_id": p.peer_id,
                "status": p.status,
                "source": p.source,
                "relayed": p.relayed,
                "addresses": p.addresses,
                "last_seen": p.last_seen,
                "is_concierge": is_concierge,
                "username": username,
            })
        })
        .collect();
    Response::json(
        serde_json::json!({
            "self": { "peer_id": self_peer, "agent_id": self_agent, "online": online },
            "peers": peers_json,
            "total": total,
            "connected": connected,
        })
        .to_string(),
    )
}

/// `/api/me`: the local username (shareable AgentID), its derived PeerID, and
/// the chat node's online state + listen addresses. Computing the username does
/// not require the node to be running, so this is safe to poll before any send.
pub(super) fn me_response(mem: &MemCli, options: &GuiOptions) -> Response {
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
            // The wallet browser (Brave or Opera) is the preferred shell (Decision
            // 0033): when present the GUI runs in its `--app` window and the wallet /
            // native-IPFS / bookmark features light up. Reports which one is installed
            // (browser-side checks stay authoritative for "am I *in* it"), or null.
            "wallet_browser": wallet_browser().map(|(kind, _)| kind.label()),
        })
        .to_string(),
    )
}

pub(super) fn meta_json(mem: &MemCli, options: &GuiOptions) -> CoreResult<String> {
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

pub(super) fn names_json(mem: &MemCli) -> CoreResult<String> {
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
            let created_at = value
                .get("created_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
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
pub(super) struct PrivacyOverlay {
    // Decision 0026: everything is fenced from egress by default. The overlay
    // tracks the *exceptions* — roots explicitly cleared for export, and roots
    // already known-public — not what is "locked" (that is the default).
    pub(super) cleared_roots: BTreeSet<String>,
    pub(super) cleared_cids: BTreeSet<String>,
    pub(super) known_public: BTreeSet<String>,
    /// Guardian-quarantined CIDs (§3). Surfaced as a badge locally; excluded from
    /// retrieval/relay. Local view stays transparent — you can see + release them.
    pub(super) quarantined: BTreeSet<String>,
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

pub(super) fn record_response(mem: &MemCli, query: &str) -> Response {
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
pub(super) fn blob_response(mem: &MemCli, query: &str) -> Response {
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
type BlobAsset = (String, Option<String>, Vec<u8>);

fn blob_bytes(mem: &MemCli, cid: &Cid) -> CoreResult<Option<BlobAsset>> {
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

pub(super) fn checkpoints_json(mem: &MemCli) -> CoreResult<String> {
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

pub(super) fn graph_response(mem: &MemCli, query: &str) -> Response {
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
pub(super) fn session_of(name: &str) -> Option<String> {
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
pub(super) fn node_and_links_from_record(
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

pub(super) fn stats_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
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

pub(super) fn rooms_json(mem: &MemCli) -> CoreResult<String> {
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
pub(super) fn network_json(mem: &MemCli) -> CoreResult<String> {
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

pub(super) fn thread_response(mem: &MemCli, query: &str) -> Response {
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
    let this_agent = mem
        .identity()
        .ok()
        .map(|id| id.agent_id().0)
        .unwrap_or_default();
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

pub(super) fn egress_plan_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
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
pub(super) fn privacy_response(mem: &MemCli, query: &str) -> Response {
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
/// Describe the embedder for the System Console — honest about whether a model is
/// actually loaded (`built`) or merely configured. Never *builds* a model just to
/// render status (that could download/load weights on a routine poll); it reports
/// the real `id()`/`dims()` once retrieval has built it, else what the config will
/// load on the first search.
fn embedder_status(mem: &MemCli, options: &GuiOptions) -> serde_json::Value {
    if let Ok(guard) = options.librarian.lock() {
        if let Some(embedder) = guard.embedder.as_ref() {
            return serde_json::json!({
                "built": true,
                "id": embedder.id(),
                "dims": embedder.dims(),
            });
        }
    }
    let cfg = mem.config().map(|c| c.librarian).unwrap_or_default();
    let url = cfg.embedding_url.trim();
    let detail = match cfg.embedder.as_str() {
        "lexical" => "lexical-v1 · offline, zero-dependency".to_string(),
        "fastembed" => format!("fastembed · {} (in-process)", cfg.embedding_model),
        "http" if !url.is_empty() => format!("http · {} @ {}", cfg.embedding_model, url),
        "http" => "http · (no endpoint set → lexical fallback)".to_string(),
        _ if !url.is_empty() => format!("auto · http {} @ {}", cfg.embedding_model, url),
        _ => format!("auto · {} or lexical fallback", cfg.embedding_model),
    };
    serde_json::json!({ "built": false, "id": detail, "backend": cfg.embedder })
}

/// `GET /api/activity?since=<seq>` — the incremental System Console feed. Returns
/// the embedder status (always) plus every entry with `seq >= since`, and the
/// `next_seq` the client should send next poll.
pub(super) fn activity_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let since = params.get("since").and_then(|s| s.parse::<u64>().ok());
    let (entries, next_seq): (Vec<serde_json::Value>, u64) = match options.activity.lock() {
        Ok(feed) => (
            feed.entries
                .iter()
                .filter(|e| since.is_none_or(|s| e.seq >= s))
                .map(|e| {
                    serde_json::json!({
                        "seq": e.seq,
                        "ts": e.ts_unix,
                        "level": e.level,
                        "text": e.text,
                    })
                })
                .collect(),
            feed.next_seq,
        ),
        Err(_) => (Vec::new(), 0),
    };
    Response::json(
        serde_json::json!({
            "embedder": embedder_status(mem, options),
            "next_seq": next_seq,
            "entries": entries,
        })
        .to_string(),
    )
}

/// Default embedder is the zero-dependency lexical one; a semantic backend swaps
/// in behind the same trait when its feature is enabled.
pub(super) fn search_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let q = params
        .get("q")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
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
        let built = default_embedder(&librarian_config);
        options.log(
            "ok",
            format!("embedder ready · {} ({}d)", built.id(), built.dims()),
        );
        guard.embedder = Some(built);
    }
    let embedder = guard.embedder.clone().expect("embedder built above");
    let stale = guard
        .cache
        .as_ref()
        .map(|c| c.built_at.elapsed() >= LIBRARIAN_TTL)
        .unwrap_or(true);
    if stale {
        options.log("ev", "indexing memory for retrieval…");
        match Librarian::index_all_persistent(mem, embedder) {
            Ok(librarian) => {
                options.log(
                    "ev",
                    format!("indexed {} node(s) for retrieval", librarian.len()),
                );
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
    options.log(
        "ev",
        format!(
            "retrieve “{}” → {} hit(s) · {}/{} tokens",
            q,
            items.len(),
            result.used_tokens,
            result.budget_tokens
        ),
    );
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
