use super::*;

pub(super) fn peers_response(mem: &MemCli, options: &GuiOptions) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/peers", "") {
            return response;
        }
    }

    let _ = ensure_chat_node(mem, options);
    let now = now_unix();
    let (online, self_peer, self_agent, self_addrs, mut peers) = match options.chat.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(chat) => {
                let list: Vec<PeerInfo> = chat
                    .peers
                    .lock()
                    .map(|m| m.values().cloned().collect())
                    .unwrap_or_default();
                let addrs = chat.addrs.lock().map(|a| a.clone()).unwrap_or_default();
                (
                    true,
                    chat.peer_id.clone(),
                    chat.agent_id.clone(),
                    addrs,
                    list,
                )
            }
            None => (false, String::new(), String::new(), Vec::new(), Vec::new()),
        },
        Err(_) => (false, String::new(), String::new(), Vec::new(), Vec::new()),
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
    // No render cap — every discovered peer goes on the map so the global pattern is
    // visible. The store is bounded by MAX_DISCOVERY_PEERS, which keeps this sane.
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
            // A fellow Concierge is any peer that identified itself with the Concierge
            // protocol version, plus our existing positives (approved contact, or found
            // via the Concierge rendezvous namespace). Everything else is a network star.
            let is_concierge =
                p.is_concierge || p.source == "rendezvous" || concierge_peers.contains(&p.peer_id);
            // A concierge node's AgentID ("username") for DMs is recoverable from its
            // Ed25519 PeerID — so clicking its brain can copy a sendable username.
            let username = is_concierge
                .then(|| {
                    p.peer_id
                        .parse()
                        .ok()
                        .and_then(|peer| ed25519_hex_from_peer_id(&peer))
                })
                .flatten();
            // Real geo-IP (bundled offline DB-IP City Lite): the peer's true lat/lon
            // from its public address, so the map plots actual locations instead of
            // a stylised region scatter. None ⇒ relay/LAN-only peer with no public IP.
            let geo = crate::geoip::locate_addrs(&p.addresses);
            serde_json::json!({
                "peer_id": p.peer_id,
                "status": p.status,
                "source": p.source,
                "relayed": p.relayed,
                "addresses": p.addresses,
                "last_seen": p.last_seen,
                "is_concierge": is_concierge,
                "username": username,
                "lat": geo.as_ref().map(|g| g.0),
                "lon": geo.as_ref().map(|g| g.1),
                "country": geo.as_ref().and_then(|g| g.2.clone()),
            })
        })
        .collect();
    // Our own node's real location, when one of its addresses is a public IP (e.g. an
    // AutoNAT-observed external address); None ⇒ the map uses the stylised fallback.
    let self_geo = crate::geoip::locate_addrs(&self_addrs);
    Response::json(
        serde_json::json!({
            "self": {
                "peer_id": self_peer,
                "agent_id": self_agent,
                "online": online,
                "lat": self_geo.as_ref().map(|g| g.0),
                "lon": self_geo.as_ref().map(|g| g.1),
            },
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

pub(super) fn meta_response(mem: &MemCli, options: &GuiOptions) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(mut response) = kernel_get("/api/meta", "") {
            if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&response.body) {
                if let Some(object) = value.as_object_mut() {
                    // These fields belong to the GUI process, not the kernel daemon.
                    object.insert(
                        "mounted_model".to_string(),
                        serde_json::Value::String(options.mounted_model.clone()),
                    );
                    object.insert(
                        "store".to_string(),
                        serde_json::Value::String(options.store_label.clone()),
                    );
                    object.insert(
                        "csrf_token".to_string(),
                        serde_json::Value::String(options.csrf_token.clone()),
                    );
                    response.body = value.to_string().into_bytes();
                    return response;
                }
            }
        }
    }
    to_response(concierge_routes::meta_json(
        mem,
        &options.mounted_model,
        &options.store_label,
        &options.csrf_token,
    ))
}

pub(super) fn resolve_response(mem: &MemCli, query: &str) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/resolve", query) {
            return response;
        }
    }
    to_response(concierge_routes::resolve_json_for_query(mem, query))
}

pub(super) fn names_response(mem: &MemCli) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/names", "") {
            return response;
        }
    }
    to_response(concierge_routes::names_json(mem))
}

pub(super) fn record_response(mem: &MemCli, query: &str) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/record", query) {
            return response;
        }
    }
    let params = concierge_routes::parse_query(query);
    let Some(key) = concierge_routes::query_key(&params) else {
        return Response::bad_request("need ?name= or ?cid=");
    };
    to_response(concierge_routes::record_json(mem, key))
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
    match concierge_routes::blob_bytes(mem, &Cid(cid.clone())) {
        Ok(Some((media_type, filename, bytes))) => {
            Response::asset(&media_type, bytes, filename.as_deref(), download)
        }
        Ok(None) => Response::forbidden(),
        Err(error) => Response::bad_request(&error.to_string()),
    }
}

pub(super) fn checkpoints_response(mem: &MemCli) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/checkpoints", "") {
            return response;
        }
    }
    to_response(concierge_routes::checkpoints_json(mem))
}

pub(super) fn graph_response(mem: &MemCli, query: &str) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/graph", query) {
            return response;
        }
    }
    to_response(concierge_routes::graph_json_for_query(mem, query))
}

pub(super) fn stats_response(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/stats", query) {
            return response;
        }
    }
    to_response(concierge_routes::stats_json_for_query(
        mem,
        query,
        &options.store_label,
    ))
}

pub(super) fn hot_pins_response(mem: &MemCli) -> Response {
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/hot-pins", "") {
            return response;
        }
    }
    to_response(concierge_routes::hot_pins_json(mem))
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
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/privacy", query) {
            return response;
        }
    }
    to_response(concierge_routes::privacy_json(mem, target))
}

/// Phase 5 keeps the retrieval embedder/index in the kernel. The GUI only reports
/// the configured backend here; it does not build or cache a local model.
fn embedder_status(mem: &MemCli) -> serde_json::Value {
    let config_detail = || -> (String, String) {
        let cfg = mem.config().map(|c| c.librarian).unwrap_or_default();
        let url = cfg.embedding_url.trim().to_string();
        let detail = match cfg.embedder.as_str() {
            "lexical" => "lexical-v1 · offline, zero-dependency".to_string(),
            "fastembed" => format!("fastembed · {} (in-process)", cfg.embedding_model),
            "http" if !url.is_empty() => format!("http · {} @ {}", cfg.embedding_model, url),
            "http" => "http · (no endpoint set → lexical fallback)".to_string(),
            _ if !url.is_empty() => format!("auto · http {} @ {}", cfg.embedding_model, url),
            _ => format!("auto · {} or lexical fallback", cfg.embedding_model),
        };
        (detail, cfg.embedder)
    };
    let (detail, backend) = config_detail();
    serde_json::json!({ "built": false, "owner": "kernel", "id": detail, "backend": backend })
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
            "embedder": embedder_status(mem),
            "next_seq": next_seq,
            "entries": entries,
        })
        .to_string(),
    )
}

/// Try the kernel route contract. Returns `None` only when no kernel can be
/// reached after supervision. A reachable kernel's HTTP-shaped error response is
/// preserved instead of being masked.
#[cfg(any(unix, windows))]
pub(super) fn kernel_get(path: &str, query: &str) -> Option<Response> {
    use concierge_kernel::client;
    // Phase 5: route through the kernel by default (it owns the warm shared index).
    // `send_supervised` ensures a kernel is running and auto-restarts it if it
    // crashed, so the GUI recovers transparently. `CONCIERGE_NO_KERNEL` is a hard
    // diagnostic opt-out: routes then use their explicit non-kernel paths, if any.
    if std::env::var_os("CONCIERGE_NO_KERNEL").is_some() {
        return None;
    }
    let req = concierge_kernel::protocol::Request {
        id: 1,
        path: path.to_string(),
        query: query.to_string(),
        body: None,
    };
    match client::send_supervised(&req) {
        Ok((resp, _lifecycle)) => {
            let status = if resp.status == 0 { 200 } else { resp.status };
            let mut response = Response::new(status, "application/json", resp.body.into_bytes());
            if !resp.content_type.is_empty() {
                response.content_type_owned = Some(resp.content_type);
            }
            Some(response)
        }
        Err(_) => None,
    }
}

fn log_kernel_search(options: &GuiOptions, q: &str, response: &Response) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return;
    };
    let hits = value
        .get("items")
        .and_then(|items| items.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let used = value
        .get("used_tokens")
        .and_then(|tokens| tokens.as_u64())
        .unwrap_or(0);
    let budget = value
        .get("budget_tokens")
        .and_then(|tokens| tokens.as_u64())
        .unwrap_or(0);
    options.log(
        "ev",
        format!(
            "retrieve “{}” → {} hit(s) · {}/{} tokens",
            q, hits, used, budget
        ),
    );
}

/// Search through the kernel's shared warm index. The GUI deliberately does not
/// build a local search index in Phase 5; if supervision fails, the route returns
/// an explicit service-unavailable response instead of creating a second cache.
pub(super) fn search_response(_mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let q = params
        .get("q")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if q.is_empty() {
        return Response::bad_request("search requires a non-empty ?q=");
    }
    #[cfg(any(unix, windows))]
    {
        if let Some(response) = kernel_get("/api/search", query) {
            log_kernel_search(options, &q, &response);
            return response;
        }
    }
    Response::new(
        503,
        "application/json",
        serde_json::json!({
            "error": "kernel unavailable; start concierge-kernel or clear CONCIERGE_NO_KERNEL"
        })
        .to_string()
        .into_bytes(),
    )
}
