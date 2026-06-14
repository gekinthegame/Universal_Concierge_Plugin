use super::*;

pub(super) fn mutation_save_checkpoint(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = match body_str(&value, "name") {
        Ok(name) => name.trim().to_string(),
        Err(response) => return response,
    };
    if !valid_site_name(&name) {
        return Response::bad_request("site name must be letters, digits, - _ . (max 64)");
    }
    // The draft HTML comes inline (Write tab) or is read from the open folder.
    let html = match value
        .get("html")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        Some(html) => html.to_string(),
        None => match value
            .get("folder")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(folder) => {
                match std::fs::read_to_string(std::path::Path::new(folder).join("index.html")) {
                    Ok(html) => html,
                    Err(error) => {
                        return Response::error(format!("read folder index.html: {error}"))
                    }
                }
            }
            None => return Response::bad_request("write some HTML or open a folder first"),
        },
    };

    // Store-side: a content-addressed snapshot + a real checkpoint node in Records.
    let cid = match mem.save_site_checkpoint(&name, &html) {
        Ok((cid, _ts)) => cid,
        Err(error) => return Response::error(error.to_string()),
    };
    // Sidecar: stage the HTML so the ⏱ Checkpoints list can reopen this version.
    let mut warning: Option<String> = None;
    if let Ok(store) = mem.store_dir() {
        let folder = store.join("canvas").join(safe_site(&name));
        if std::fs::create_dir_all(&folder).is_ok()
            && std::fs::write(folder.join("index.html"), &html).is_ok()
        {
            warning = record_site_checkpoint(mem, &name, &folder.to_string_lossy(), None, &cid, "")
                .err()
                .map(|error| error.to_string());
        }
    }
    Response::json(
        serde_json::json!({ "ok": true, "cid": cid, "checkpoint_warning": warning }).to_string(),
    )
}

// ── Studio publish checkpoints ──────────────────────────────────────────────
// Every successful publish snapshots the published index.html + its stable IPNS
// address + a timestamp. Because the IPNS address stays the same across updates,
// the user can re-open any past published version in the editor and re-publish to
// the SAME address. Stored under `<store>/canvas/.checkpoints/`.

/// Sanitize a site name to a safe single path segment (matches the publish name set).
fn safe_site(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(48)
        .collect();
    if s.is_empty() || s == "." || s == ".." {
        "draft".to_string()
    } else {
        s
    }
}

fn site_ckpt_dir(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir()
        .ok()
        .map(|d| d.join("canvas").join(".checkpoints"))
}

pub(super) fn record_site_checkpoint(
    mem: &MemCli,
    site: &str,
    folder: &str,
    ipns: Option<&str>,
    cid: &str,
    url: &str,
) -> CoreResult<()> {
    let base = site_ckpt_dir(mem).ok_or_else(|| Error::Io("store unavailable".to_string()))?;
    let html = std::fs::read_to_string(std::path::Path::new(folder).join("index.html"))
        .map_err(|error| Error::Io(format!("read checkpoint source: {error}")))?;
    let safe = safe_site(site);
    let mut ts = now_unix();
    let dir = base.join(&safe);
    std::fs::create_dir_all(&dir)
        .map_err(|error| Error::Io(format!("create checkpoint dir: {error}")))?;
    loop {
        let path = dir.join(format!("{ts}.html"));
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(html.as_bytes())
                    .map_err(|error| Error::Io(format!("write checkpoint: {error}")))?;
                file.sync_all()
                    .map_err(|error| Error::Io(format!("sync checkpoint: {error}")))?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ts += 1,
            Err(error) => return Err(Error::Io(format!("create checkpoint: {error}"))),
        }
    }
    let mpath = base.join("manifest.json");
    let mut manifest: serde_json::Value = std::fs::read_to_string(&mpath)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    let entry = serde_json::json!({ "ts": ts, "ipns": ipns, "cid": cid, "url": url });
    if let Some(obj) = manifest.as_object_mut() {
        let list = obj
            .entry(safe)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(arr) = list.as_array_mut() {
            arr.insert(0, entry); // newest first
            arr.truncate(40); // bound the history
        }
    }
    atomic_local_write(&mpath, manifest.to_string().as_bytes())
        .map_err(|error| Error::Io(format!("write checkpoint manifest: {error}")))
}

/// `GET /api/site/checkpoints` — every publish checkpoint across all sites, newest first.
pub(super) fn site_checkpoints_json(mem: &MemCli) -> CoreResult<String> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    if let Some(base) = site_ckpt_dir(mem) {
        if let Ok(text) = std::fs::read_to_string(base.join("manifest.json")) {
            if let Ok(map) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(obj) = map.as_object() {
                    for (site, list) in obj {
                        if let Some(arr) = list.as_array() {
                            for e in arr {
                                out.push(serde_json::json!({
                                    "site": site,
                                    "ts": e.get("ts"),
                                    "ipns": e.get("ipns"),
                                    "cid": e.get("cid"),
                                    "url": e.get("url"),
                                }));
                            }
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b.get("ts")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            .cmp(&a.get("ts").and_then(serde_json::Value::as_u64).unwrap_or(0))
    });
    Ok(serde_json::json!({ "checkpoints": out }).to_string())
}

/// `GET /api/site/checkpoint?site=&ts=` — the saved HTML of one checkpoint, to
/// reload into the editor and update.
pub(super) fn site_checkpoint_response(mem: &MemCli, query: &str) -> Response {
    let params = parse_query(query);
    let site = params.get("site").map(|s| safe_site(s)).unwrap_or_default();
    let ts = params.get("ts").cloned().unwrap_or_default();
    if site.is_empty() || ts.is_empty() || !ts.chars().all(|c| c.is_ascii_digit()) {
        return Response::bad_request("site and numeric ts are required");
    }
    let Some(base) = site_ckpt_dir(mem) else {
        return Response::error("store unavailable".to_string());
    };
    let path = base.join(&site).join(format!("{ts}.html"));
    match std::fs::read_to_string(&path) {
        Ok(html) => {
            Response::json(serde_json::json!({ "site": site, "ts": ts, "html": html }).to_string())
        }
        Err(e) => Response::error(format!("checkpoint not found: {e}")),
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
pub(super) fn parse_canvas_signal(json: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("type").and_then(|t| t.as_str()) == Some("canvas-signal") {
        value.get("signal").cloned()
    } else {
        None
    }
}

/// Recognise a `{"type":"contact-card","card":{…}}` DM envelope (Layer 2), returning
/// the inner card JSON to verify + import.
pub(super) fn parse_contact_card(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("type").and_then(|t| t.as_str()) == Some("contact-card") {
        value.get("card").map(|c| c.to_string())
    } else {
        None
    }
}

pub(super) fn approved_agent_matches_peer(mem: &MemCli, agent_id: &str, from_peer: &str) -> bool {
    mem.is_contact(agent_id)
        && peer_id_from_ed25519_hex(agent_id)
            .map(|peer| peer.to_string() == from_peer)
            .unwrap_or(false)
}

pub(super) fn approved_contact_card_author(
    mem: &MemCli,
    card_json: &str,
    from_peer: &str,
) -> Option<String> {
    let card: concierge_core::naming::ContactCard = serde_json::from_str(card_json).ok()?;
    if !card.verify() {
        return None;
    }
    let agent_id = card.agent_id().ok()?.0;
    approved_agent_matches_peer(mem, &agent_id, from_peer).then_some(agent_id)
}

pub(super) fn queue_canvas_signal(
    store: &mut HashMap<String, Vec<serde_json::Value>>,
    signal: serde_json::Value,
) -> bool {
    let Some(session) = signal
        .get("session")
        .and_then(|value| value.as_str())
        .map(str::trim)
    else {
        return false;
    };
    if session.is_empty()
        || session.len() > MAX_CANVAS_SESSION_LEN
        || serde_json::to_vec(&signal).map_or(true, |bytes| bytes.len() > MAX_CANVAS_SIGNAL_BYTES)
    {
        return false;
    }
    if !store.contains_key(session) && store.len() >= MAX_CANVAS_SESSIONS {
        return false;
    }
    let queue = store.entry(session.to_string()).or_default();
    queue.push(signal);
    if queue.len() > MAX_CANVAS_SIGNAL_QUEUE {
        let excess = queue.len() - MAX_CANVAS_SIGNAL_QUEUE;
        queue.drain(0..excess);
    }
    true
}

/// `GET /api/canvas/signal?session=&me=`: drain pending signaling messages
/// addressed to `me` (or broadcast `*`) for this session.
pub(super) fn canvas_signal_get(options: &GuiOptions, query: &str) -> Response {
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
pub(super) fn mutation_canvas_signal(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let to = value
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let queued = options
        .canvas
        .lock()
        .map(|mut store| queue_canvas_signal(&mut store, value.clone()))
        .unwrap_or(false);
    if !queued {
        return Response::bad_request("invalid signal or signaling capacity reached");
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
pub(super) fn mutation_canvas_snapshot(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let session = value
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("snapshot");
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
    let safe = if safe.is_empty() {
        "snapshot".to_string()
    } else {
        safe
    };
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
pub(super) fn preview_token(dir: &std::path::Path) -> String {
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
        let Ok(entries) = std::fs::read_dir(cur) else {
            return;
        };
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
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                walk(base, &path, out);
            } else if metadata.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
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
        let Ok(entries) = std::fs::read_dir(cur) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if _skip_dir(&name.to_string_lossy()) {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                walk(&path, max);
            } else if metadata.is_file() {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                        *max = (*max).max(dur.as_secs());
                    }
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
pub(super) fn canvas_draft_get(mem: &MemCli) -> Response {
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
pub(super) fn mutation_canvas_open(options: &GuiOptions, body: &str) -> Response {
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
        if !dirs.contains_key(&token) && dirs.len() >= MAX_PREVIEW_DIRS {
            return Response::too_many_requests();
        }
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
pub(super) fn canvas_files_get(options: &GuiOptions, query: &str) -> Response {
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
pub(super) fn canvas_mtime_get(options: &GuiOptions, query: &str) -> Response {
    match canvas_dir(options, query) {
        Some(dir) => Response::json(serde_json::json!({ "mtime": folder_mtime(&dir) }).to_string()),
        None => Response::bad_request("unknown or missing preview token"),
    }
}

/// Serve `/canvas-preview/<token>/<relpath>` from the registered folder, read-only
/// and fenced to that folder (no path traversal). This is what the preview iframe
/// loads, so a multi-file site renders with correct relative links.
pub(super) fn canvas_preview_serve(options: &GuiOptions, rest: &str) -> Response {
    let (token, relpath) = rest.split_once('/').unwrap_or((rest, ""));
    let relpath = percent_decode(relpath);
    let relpath = if relpath.trim_matches('/').is_empty() {
        "index.html".to_string()
    } else {
        relpath
    };
    let Some(dir) = options
        .preview_dirs
        .lock()
        .ok()
        .and_then(|d| d.get(token).cloned())
    else {
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
        Ok(bytes) => {
            let mut response = Response::new(200, site_media_type(&canon.to_string_lossy()), bytes);
            response.embeddable = true;
            response
        }
        Err(_) => Response::not_found(),
    }
}
