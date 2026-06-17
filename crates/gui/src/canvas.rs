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
                let root = match canvas_root(mem) {
                    Ok(root) => root,
                    Err(response) => return response,
                };
                let dir = match require_canvas_dir(mem, std::path::Path::new(folder)) {
                    Ok(dir) => dir,
                    Err(response) => return response,
                };
                match read_canvas_text(&root, &dir, std::path::Path::new("index.html")) {
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
    if let Ok(root) = canvas_root(mem) {
        let folder = root.join(safe_site(&name));
        if std::fs::create_dir_all(&folder).is_ok()
            && write_canvas_file(
                &root,
                &folder,
                std::path::Path::new("index.html"),
                html.as_bytes(),
            )
            .is_ok()
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
    let root = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
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
    let folder = root.join(&safe);
    if let Err(error) = std::fs::create_dir_all(&folder) {
        return Response::error(format!("create snapshot dir: {error}"));
    }
    if let Err(error) = write_canvas_file(
        &root,
        &folder,
        std::path::Path::new("index.html"),
        html.as_bytes(),
    ) {
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

const PREVIEW_CSP: &str = "sandbox allow-scripts; default-src 'self' data: blob:; style-src 'self' 'unsafe-inline' data: blob:; script-src 'self' 'unsafe-inline' data: blob:; img-src 'self' data: blob:; media-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' blob:; frame-ancestors 'self'; object-src 'none'; base-uri 'none'; form-action 'none'";

fn canvas_root(mem: &MemCli) -> Result<std::path::PathBuf, Response> {
    let root = mem
        .store_dir()
        .map_err(|error| Response::error(error.to_string()))?
        .join("canvas");
    std::fs::create_dir_all(&root)
        .map_err(|error| Response::error(format!("create canvas root: {error}")))?;
    let metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| Response::error(format!("inspect canvas root: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Response::forbidden());
    }
    root.canonicalize()
        .map_err(|error| Response::error(format!("resolve canvas root: {error}")))
}

fn require_canvas_dir(
    mem: &MemCli,
    folder: &std::path::Path,
) -> Result<std::path::PathBuf, Response> {
    let root = canvas_root(mem)?;
    let canon = folder
        .canonicalize()
        .map_err(|_| Response::error(format!("not a folder: {}", folder.display())))?;
    if !canon.is_dir() {
        return Err(Response::error(format!(
            "not a folder: {}",
            folder.display()
        )));
    }
    if !canon.starts_with(&root) {
        return Err(Response::forbidden());
    }
    Ok(canon)
}

fn registered_canvas_dir(
    mem: &MemCli,
    options: &GuiOptions,
    token: &str,
) -> Result<std::path::PathBuf, Response> {
    let Some(dir) = options
        .preview_dirs
        .lock()
        .ok()
        .and_then(|dirs| dirs.get(token).cloned())
    else {
        return Err(Response::bad_request("unknown or missing preview token"));
    };
    require_canvas_dir(mem, &dir)
}

fn reject_symlink_components(root: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| "canvas path escaped the canvas root".to_string())?;
    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component.as_os_str());
        match std::fs::symlink_metadata(&cur) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("refusing to follow symlink: {}", cur.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect {}: {error}", cur.display())),
        }
    }
    Ok(())
}

fn prepare_canvas_write(
    root: &std::path::Path,
    dir: &std::path::Path,
    rel: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let target = dir.join(rel);
    let parent = target
        .parent()
        .ok_or_else(|| "invalid canvas target".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create dir: {error}"))?;
    let parent_canon = parent
        .canonicalize()
        .map_err(|error| format!("resolve parent dir: {error}"))?;
    if !parent_canon.starts_with(dir) || !parent_canon.starts_with(root) {
        return Err("canvas path escaped the open project folder".to_string());
    }
    reject_symlink_components(root, &target)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if metadata.is_dir() {
            return Err(format!("target is a directory: {}", target.display()));
        }
    }
    Ok(target)
}

fn write_canvas_file(
    root: &std::path::Path,
    dir: &std::path::Path,
    rel: &std::path::Path,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    let target = prepare_canvas_write(root, dir, rel)?;
    std::fs::write(&target, bytes).map_err(|error| format!("write file: {error}"))?;
    Ok(target)
}

fn read_canvas_text(
    root: &std::path::Path,
    dir: &std::path::Path,
    rel: &std::path::Path,
) -> Result<String, String> {
    let target = dir.join(rel);
    reject_symlink_components(root, &target)?;
    let canon = target
        .canonicalize()
        .map_err(|error| format!("read file: {error}"))?;
    if !canon.starts_with(dir) || !canon.starts_with(root) || !canon.is_file() {
        return Err("canvas file escaped the open project folder".to_string());
    }
    std::fs::read_to_string(&canon).map_err(|error| format!("read file: {error}"))
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

/// `GET /api/canvas/draft`: the site the AI staged via the MCP write tools
/// (`concierge.write_site` / `concierge.write_asset`, which default to
/// `<store>/canvas/draft/`), so the Studio can auto-prefill the site-folder field and
/// open the AI's folder live. Returns `{folder, mtime, html}` (all null/0 if none yet);
/// `mtime` is the whole-folder modified time so multi-file AI writes register too.
pub(super) fn canvas_draft_get(mem: &MemCli) -> Response {
    let none = || {
        Response::json(
            serde_json::json!({
                "folder": serde_json::Value::Null,
                "mtime": 0,
                "html": serde_json::Value::Null,
            })
            .to_string(),
        )
    };
    let Some(folder) = mem
        .store_dir()
        .ok()
        .map(|dir| dir.join("canvas").join("draft"))
    else {
        return none();
    };
    let root = match canvas_root(mem) {
        Ok(root) => root,
        Err(_) => return none(),
    };
    let folder = match require_canvas_dir(mem, &folder) {
        Ok(folder) => folder,
        Err(_) => return none(),
    };
    match read_canvas_text(&root, &folder, std::path::Path::new("index.html")) {
        Ok(html) => Response::json(
            serde_json::json!({
                "folder": folder.to_string_lossy(),
                "mtime": folder_mtime(&folder),
                "html": html,
            })
            .to_string(),
        ),
        Err(_) => none(),
    }
}

/// Reject path traversal: split a client-supplied relative path into safe components,
/// rejecting absolute paths, `.` / `..`, backslashes, and empty segments. The returned
/// PathBuf is always relative and stays inside the canvas folder it is joined onto.
fn safe_rel_path(relpath: &str) -> Option<std::path::PathBuf> {
    let relpath = relpath.trim().trim_start_matches('/');
    if relpath.is_empty() {
        return None;
    }
    let mut out = std::path::PathBuf::new();
    for seg in relpath.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

/// `GET /api/canvas/file?token=&path=`: the text of one file in the registered folder
/// (defaults to index.html), so the Studio editor can load a file to edit. Fenced to
/// the folder (no traversal).
pub(super) fn canvas_file_get(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let params = parse_query(query);
    let token = params.get("token").cloned().unwrap_or_default();
    let dir = match registered_canvas_dir(mem, options, &token) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let root = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let rel = params
        .get("path")
        .map(|p| percent_decode(p))
        .unwrap_or_else(|| "index.html".to_string());
    let Some(rel) = safe_rel_path(&rel) else {
        return Response::bad_request("invalid path");
    };
    match read_canvas_text(&root, &dir, &rel) {
        Ok(content) => Response::json(
            serde_json::json!({
                "path": rel.to_string_lossy().replace('\\', "/"),
                "content": content,
            })
            .to_string(),
        ),
        Err(error) => Response::error(format!("read file: {error}")),
    }
}

/// `POST /api/canvas/write` `{token, path, content}`: write one file into the registered
/// canvas folder (path defaults to index.html). This is the unified writeable-canvas
/// seam — the Studio editor saves the file you are editing straight into the open
/// folder, which the live preview then hot-reloads. Fenced to the folder (no traversal).
pub(super) fn mutation_canvas_write(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let token = match body_str(&value, "token") {
        Ok(token) => token.trim().to_string(),
        Err(response) => return response,
    };
    let dir = match registered_canvas_dir(mem, options, &token) {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let root = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let rel = value
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("index.html");
    let Some(rel) = safe_rel_path(rel) else {
        return Response::bad_request("invalid path");
    };
    let content = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
    // Binary assets (e.g. an auto-captured animation video) arrive base64-encoded.
    let is_b64 = value
        .get("base64")
        .map(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"))
        .unwrap_or(false);
    let bytes: Vec<u8> = if is_b64 {
        match b64_decode(content) {
            Some(bytes) => bytes,
            None => return Response::bad_request("content is not valid base64"),
        }
    } else {
        content.as_bytes().to_vec()
    };
    // Streaming write: `pos` is a byte offset, so a long animation video can be written to
    // disk chunk-by-chunk (bounded memory, no whole-file buffer). pos == 0 starts fresh
    // (truncate); later chunks seek + write in place.
    if let Some(pos) = value.get("pos").and_then(|v| v.as_u64()) {
        use std::io::{Seek, SeekFrom, Write};
        let target = match prepare_canvas_write(&root, &dir, &rel) {
            Ok(target) => target,
            Err(error) => return Response::forbidden_with_message(error),
        };
        let open = if pos == 0 {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&target)
        } else {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&target)
        };
        let result = open.and_then(|mut file| {
            file.seek(SeekFrom::Start(pos))?;
            file.write_all(&bytes)
        });
        if let Err(error) = result {
            return Response::error(format!("write file: {error}"));
        }
        return Response::json(serde_json::json!({ "ok": true }).to_string());
    }
    if let Err(error) = write_canvas_file(&root, &dir, &rel, &bytes) {
        return Response::forbidden_with_message(error);
    }
    Response::json(serde_json::json!({ "ok": true, "mtime": folder_mtime(&dir) }).to_string())
}

/// `POST /api/canvas/pwa`: turn the open project into an installable **Progressive Web
/// App**. Writes a `manifest.json`, a `service-worker.js`, and icons into the folder, and
/// injects the PWA tags into `index.html`. After this, publishing the folder to *any*
/// host yields a URL that installs to the phone home screen (no app store, no fees, no
/// native build). Local file edits only — no egress, no code execution. Idempotent.
pub(super) fn mutation_canvas_pwa(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let token = match body_str(&value, "token") {
        Ok(token) => token.trim().to_string(),
        Err(response) => return response,
    };
    let dir = match registered_canvas_dir(mem, options, &token) {
        Ok(dir) => dir,
        Err(response) if response.status == 400 => {
            return Response::bad_request(
                "unknown or missing preview token — open the project first",
            )
        }
        Err(response) => return response,
    };
    let root = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
    };
    match apply_pwa(&root, &dir) {
        Ok((name, injected)) => Response::json(
            serde_json::json!({
                "ok": true,
                "name": name,
                "already": !injected,
                "files": ["manifest.json", "service-worker.js", "icon-192.png", "icon-512.png", "icon.svg"],
                "mtime": folder_mtime(&dir),
            })
            .to_string(),
        ),
        Err(error) => Response::error(error),
    }
}

/// Make a folder an installable PWA: write the manifest, service worker, and icons, and
/// inject the PWA tags into its index.html. Returns the app name + whether anything was
/// newly injected. Idempotent — reused by the "New → Mobile App/Game" scaffold.
fn apply_pwa(root: &std::path::Path, dir: &std::path::Path) -> Result<(String, bool), String> {
    let index_rel = std::path::Path::new("index.html");
    let mut html = read_canvas_text(root, dir, index_rel)
        .map_err(|_| "a PWA needs a web entry point — add an index.html first".to_string())?;

    let name = extract_title(&html)
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "App".to_string());
    let name = name.trim().to_string();
    let short: String = name
        .split_whitespace()
        .next()
        .unwrap_or(&name)
        .chars()
        .take(18)
        .collect();

    write_canvas_file(
        root,
        dir,
        std::path::Path::new("icon-512.png"),
        &pwa_png_icon(512),
    )
    .map_err(|e| format!("write icon: {e}"))?;
    let _ = write_canvas_file(
        root,
        dir,
        std::path::Path::new("icon-192.png"),
        &pwa_png_icon(192),
    );
    let _ = write_canvas_file(
        root,
        dir,
        std::path::Path::new("icon.svg"),
        PWA_ICON_SVG.as_bytes(),
    );

    let manifest = serde_json::json!({
        "name": name,
        "short_name": short,
        "start_url": ".",
        "scope": ".",
        "display": "standalone",
        "orientation": "any",
        "background_color": "#0a0b1a",
        "theme_color": "#d122e3",
        "icons": [
            { "src": "icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any maskable" },
            { "src": "icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable" },
            { "src": "icon.svg", "sizes": "any", "type": "image/svg+xml" }
        ]
    });
    write_canvas_file(
        root,
        dir,
        std::path::Path::new("manifest.json"),
        &serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| format!("write manifest: {e}"))?;
    write_canvas_file(
        root,
        dir,
        std::path::Path::new("service-worker.js"),
        PWA_SERVICE_WORKER.as_bytes(),
    )
    .map_err(|e| format!("write service worker: {e}"))?;

    let mut injected = false;
    if !html.contains("rel=\"manifest\"") {
        let head_tags = format!(
            "\n<link rel=\"manifest\" href=\"manifest.json\">\n<meta name=\"theme-color\" content=\"#d122e3\">\n<meta name=\"apple-mobile-web-app-capable\" content=\"yes\">\n<meta name=\"apple-mobile-web-app-status-bar-style\" content=\"black-translucent\">\n<meta name=\"apple-mobile-web-app-title\" content=\"{name}\">\n<link rel=\"apple-touch-icon\" href=\"icon-192.png\">\n"
        );
        html = inject_before(&html, "</head>", &head_tags);
        injected = true;
    }
    if !html.contains("width=device-width") {
        html = inject_before(
            &html,
            "</head>",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n",
        );
        injected = true;
    }
    if !html.contains("serviceWorker") {
        html = inject_before(&html, "</body>", PWA_SW_REGISTER);
        injected = true;
    }
    write_canvas_file(root, dir, index_rel, html.as_bytes())
        .map_err(|e| format!("update index.html: {e}"))?;
    Ok((name, injected))
}

/// `POST /api/canvas/new`: scaffold a fresh project under `<store>/canvas/<name>` and
/// stage starter files for the chosen kind — **"website"** (a site / web app) or **"app"**
/// (a mobile app / game, staged as an installable PWA from the start). Returns the new
/// folder path; the Studio then opens it. Local file writes only.
pub(super) fn mutation_canvas_new(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let display = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("website")
        .trim();
    let safe = sanitize_project_name(display);

    let canvas = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let dir = canvas.join(&safe);
    if std::fs::symlink_metadata(&dir).is_ok() {
        return Response::bad_request(
            "a project with that name already exists — pick another name",
        );
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return Response::error(format!("create project: {error}"));
    }

    let title = if display.is_empty() {
        safe.clone()
    } else {
        display.to_string()
    };
    let (index, css) = match kind {
        "app" => app_starter(&title),
        "movie" => movie_starter(&title),
        _ => website_starter(&title),
    };
    if let Err(error) = write_canvas_file(
        &canvas,
        &dir,
        std::path::Path::new("index.html"),
        index.as_bytes(),
    ) {
        return Response::error(format!("write index.html: {error}"));
    }
    let _ = write_canvas_file(
        &canvas,
        &dir,
        std::path::Path::new("style.css"),
        css.as_bytes(),
    );
    match kind {
        "app" => {
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("app.js"),
                APP_JS_STARTER.as_bytes(),
            );
            if let Err(error) = apply_pwa(&canvas, &dir) {
                return Response::error(error);
            }
        }
        "movie" => {
            // Self-contained animation skill: GSAP + Lottie are bundled into the project
            // (no CDN, no installs — works offline + on IPFS). The AI builds the motion in
            // animation.js; it plays in the preview and records to video in the browser
            // (MediaRecorder — no ffmpeg). For 3D the README points to scaffold_engine('three')
            // (in-browser, cinematic, seekable); Blender stays the optional heavy-offline path.
            let _ = write_canvas_file(&canvas, &dir, std::path::Path::new("gsap.min.js"), GSAP_JS);
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("lottie.min.js"),
                LOTTIE_JS,
            );
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("webm-muxer.js"),
                WEBM_MUXER_JS,
            );
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("animation.js"),
                MOVIE_ANIMATION_JS.as_bytes(),
            );
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("capture.js"),
                MOVIE_CAPTURE_JS.as_bytes(),
            );
            let _ = write_canvas_file(
                &canvas,
                &dir,
                std::path::Path::new("README.md"),
                movie_readme(&title).as_bytes(),
            );
        }
        _ => {}
    }

    // Tell the MCP server (a separate process) which project the user is now in, so its
    // write tools edit THESE staged files instead of defaulting to a stray "draft" folder.
    set_active_project(mem, &safe);

    Response::json(
        serde_json::json!({
            "ok": true,
            "name": safe,
            "kind": kind,
            "path": dir.to_string_lossy(),
        })
        .to_string(),
    )
}

/// Record the project the Studio currently has open in `<store>/canvas/.active` (one line, the
/// bare folder name). The MCP server — a *separate process* that only shares the filesystem —
/// reads this so its write/read tools default to the folder the user is looking at, instead of a
/// stray "draft" the user never sees. Best-effort: a write failure here only loses the default.
pub(super) fn set_active_project(mem: &MemCli, name: &str) {
    if let Ok(root) = canvas_root(mem) {
        let _ = std::fs::write(root.join(".active"), name.as_bytes());
    }
}

/// `POST /api/canvas/delete`: permanently remove a saved project folder under
/// `<store>/canvas/<name>`. Path-guarded to a *direct child* of the canvas folder — never
/// `.checkpoints`, never traversal. Local file delete only, irreversible.
pub(super) fn mutation_canvas_delete(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = match body_str(&value, "name") {
        Ok(name) => name.trim().to_string(),
        Err(response) => return response,
    };
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Response::bad_request("invalid project name");
    }
    let canvas = match canvas_root(mem) {
        Ok(root) => root,
        Err(response) => return response,
    };
    // The realised target must be an existing direct child of the canvas folder.
    let canon_dir = match canvas.join(&name).canonicalize() {
        Ok(dir) => dir,
        _ => return Response::bad_request("no such project"),
    };
    if !canon_dir.is_dir() || canon_dir.parent() != Some(canvas.as_path()) {
        return Response::forbidden();
    }
    if let Err(error) = std::fs::remove_dir_all(&canon_dir) {
        return Response::error(format!("delete project: {error}"));
    }
    Response::json(serde_json::json!({ "ok": true }).to_string())
}

/// Folder-safe project slug: letters/digits/`-`/`_`, spaces → `-`, everything else dropped;
/// trimmed to 64 chars; never empty.
fn sanitize_project_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            out.push(c);
        } else if c == ' ' && !out.ends_with('-') {
            out.push('-');
        }
        // Cap at 48 to match the MCP server's safe_site(), so the `.active` folder name
        // round-trips exactly and its write tools target the same folder.
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "new-project".to_string()
    } else {
        trimmed
    }
}

fn website_starter(title: &str) -> (String, String) {
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <main>\n    <span class=\"eyebrow\">New site</span>\n    <h1>{title}</h1>\n    <p>Your new website. Tell the Concierge what to build &mdash; it writes the files right here, and they render live beside you.</p>\n    <a class=\"cta\" href=\"#\">Get started</a>\n  </main>\n</body>\n</html>\n"
    );
    (html, STARTER_SITE_CSS.to_string())
}

fn app_starter(title: &str) -> (String, String) {
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n<title>{title}</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <div id=\"app\">\n    <h1>{title}</h1>\n    <p>Your new app. Describe the app or game you want &mdash; the Concierge builds it here, already set up to install on a phone.</p>\n    <button id=\"start\">Tap to start</button>\n  </div>\n  <script src=\"app.js\"></script>\n</body>\n</html>\n"
    );
    (html, STARTER_APP_CSS.to_string())
}

const STARTER_SITE_CSS: &str = r#":root{--bg:#0a0b1a;--ink:#e0e6ff;--muted:#8a90b8;--grad:linear-gradient(120deg,#f085fa,#d122e3 40%,#00e5ff)}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--ink);font:16px/1.6 -apple-system,system-ui,sans-serif;text-align:center;padding:24px}
main{max-width:640px}
.eyebrow{font:12px/1 ui-monospace,monospace;letter-spacing:.2em;text-transform:uppercase;color:#a855f7}
h1{font-size:clamp(36px,8vw,72px);margin:16px 0 10px;background:var(--grad);-webkit-background-clip:text;background-clip:text;color:transparent}
p{color:var(--muted);font-size:18px}
.cta{display:inline-block;margin-top:22px;padding:12px 24px;border-radius:10px;background:var(--grad);color:#0a0b1a;font-weight:700;text-decoration:none}
"#;

// GSAP + Lottie, vendored into the GUI binary so a Movie/Animation project ships them
// inline — no CDN, no npm, works offline and on IPFS. (Same recipe as the bundled
// Three.js/Phaser engines.) GSAP © GreenSock (no-charge license); Lottie © Airbnb, MIT.
const GSAP_JS: &[u8] = include_bytes!("engines/gsap.min.js");
const LOTTIE_JS: &[u8] = include_bytes!("engines/lottie.min.js");
// webm-muxer (© Vanilla, MIT) — turns WebCodecs frames into a real .webm container in the
// browser, so movies render frame-by-frame (any length) with no ffmpeg.
const WEBM_MUXER_JS: &[u8] = include_bytes!("engines/webm-muxer.js");

/// A Movie/Animation project draws to a `<canvas>` from a **seekable** timeline. The
/// Concierge renders it **frame-by-frame, offline** (any length) to a real video on
/// Save/Publish — no Record button, no screen capture, no time limit. `capture.js` is
/// the deterministic renderer (WebCodecs + the bundled WebM muxer); `animation.js` is
/// the motion (it declares `window.__duration`).
fn movie_starter(title: &str) -> (String, String) {
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n<script src=\"gsap.min.js\"></script>\n<script src=\"lottie.min.js\"></script>\n<script src=\"webm-muxer.js\"></script>\n</head>\n<body>\n  <canvas id=\"stage\"></canvas>\n  <script src=\"animation.js\"></script>\n  <script src=\"capture.js\"></script>\n</body>\n</html>\n"
    );
    (html, STARTER_MOVIE_CSS.to_string())
}

fn movie_readme(title: &str) -> String {
    format!(
        "# {title} — Movie / Animation\n\nA self-contained browser animation, built with **GSAP** + **Lottie** and rendered to real video **frame-by-frame, offline** — all bundled, no CDN, no npm, no ffmpeg, no installs. Works offline and on IPFS.\n\n> **Building this with an AI assistant?** This project is **already staged** — `index.html`, `animation.js`, `capture.js`, `gsap.min.js`, `lottie.min.js`, `webm-muxer.js`, and `style.css` all exist. **Build the movie by EDITING `animation.js`.** Do NOT scaffold a new project, do NOT re-add the engine files, and do NOT create a parallel folder — the renderer (`capture.js`) is already wired in and produces the video automatically on Save. (Via MCP: call `concierge.list_site` to see these files, then `concierge.write_asset` with `path='animation.js'`.)\n\n## How it works\n- The animation draws to a `<canvas id=\"stage\">` from a **paused, seekable GSAP timeline**. `animation.js` exposes three things the renderer uses:\n  - `window.__duration` — the movie length in **seconds** (defaults to the timeline length; set it to anything — 30, 900, 1800…).\n  - `window.__fps` — frames per second (default 30).\n  - `window.__seek(t)` — deterministically draws the frame at time `t`.\n- **Video is automatic and full-length.** On **Save** or **Publish**, the Concierge renders every frame (`__duration × __fps`) with **WebCodecs** + the bundled WebM muxer and writes `animation.webm` into the folder. It renders **as fast as your machine allows** — not in real time — so a 15- or 30-minute movie is just more frames, not a 30-minute wait while a tab records.\n- Because it's deterministic, build with a **seekable** timeline (no `Math.random()` / wall-clock); seeking to time `t` must always draw the same frame. Lottie works too: `renderer:'canvas'` + `anim.goToAndStop(t*1000, false)`.\n\n## 3D in your movie\nFor **in-browser 3D** — self-contained, cinematic, no install — add Three.js via `concierge.scaffold_engine(engine='three')`. It drops a PBR scene (environment-map reflections, soft shadows, ACES tone mapping) that renders into the same `<canvas id=\"stage\">` and stays **seekable**, so it exports video exactly like the 2D path. Keep `window.__seek(t)` deterministic (a function of `t`, never a clock/`getDelta`).\nFor heavy *offline* 3D, drive **Blender** instead (must be installed): `concierge-plugin blender-setup`, install `vendor/blender-mcp/addon.py`, connect it.\n"
    )
}

const STARTER_MOVIE_CSS: &str = r#"html,body{height:100%;margin:0;overflow:hidden;background:#0a0b1a}
#stage{display:block;width:100vw;height:100vh}
"#;

const MOVIE_ANIMATION_JS: &str = r#"// Build your movie by EXTENDING this timeline — its length sets the video length, and the
// Concierge renders every frame deterministically on Save/Publish (seconds or 30 minutes).
// Keep it seekable: no Math.random() or Date.now() — seeking to time t must always draw
// the same frame. Lottie also works (renderer:'canvas' + goToAndStop).
const canvas = document.getElementById('stage');
const ctx = canvas.getContext('2d');
const DPR = Math.min(window.devicePixelRatio || 1, 2);
function fit() { canvas.width = innerWidth * DPR; canvas.height = innerHeight * DPR; }
fit();
addEventListener('resize', fit);

const state = { y: 60, opacity: 0, scale: 1 };
function draw() {
  const w = canvas.width, h = canvas.height;
  ctx.fillStyle = '#0a0b1a';
  ctx.fillRect(0, 0, w, h);
  ctx.save();
  ctx.translate(w / 2, h / 2 + state.y * DPR);
  ctx.scale(state.scale, state.scale);
  ctx.globalAlpha = Math.max(0, Math.min(1, state.opacity));
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.font = '700 ' + Math.min(w / 8, 120 * DPR) + 'px -apple-system, system-ui, sans-serif';
  const g = ctx.createLinearGradient(-w / 3, 0, w / 3, 0);
  g.addColorStop(0, '#f085fa'); g.addColorStop(0.5, '#d122e3'); g.addColorStop(1, '#00e5ff');
  ctx.fillStyle = g;
  ctx.fillText(document.title || 'Animation', 0, 0);
  ctx.restore();
}

// A PAUSED, seekable timeline — the renderer seeks to each frame and draws it.
const tl = gsap.timeline({ paused: true });
tl.to(state, { y: 0, opacity: 1, duration: 1, ease: 'power3.out' })
  .to(state, { scale: 1.08, duration: 1.4, ease: 'sine.inOut' })
  .to(state, { scale: 1, duration: 1, ease: 'sine.inOut' });

window.__fps = 30;
window.__duration = tl.duration();          // ← set this to make a longer movie
window.__seek = function (t) { tl.time(Math.max(0, Math.min(t, tl.duration()))); draw(); };

// Live preview: loop the timeline so the canvas shows motion while you edit (the renderer
// pauses + seeks during capture, so this never affects the output).
let preview = true;
window.__beginRender = function () { preview = false; };
window.__endRender = function () { preview = true; tl.play(0); requestAnimationFrame(loop); };
function loop() { if (!preview) return; draw(); requestAnimationFrame(loop); }
tl.repeat(-1).yoyo(true).play(0);
requestAnimationFrame(loop);
"#;

const MOVIE_CAPTURE_JS: &str = r#"// Concierge deterministic STREAMING renderer. On Save/Publish the Studio asks this frame
// to render the FULL animation (window.__duration seconds) to a real video, frame-by-frame,
// offline. WebCodecs + the bundled WebM muxer in streaming mode emit encoded chunks as
// they're produced; each is streamed to disk (bounded memory, no whole-file buffer) — so a
// 30-minute movie has no time or size limit. No ffmpeg, no Record button. Falls back to a
// short real-time capture if WebCodecs is unavailable.
(function () {
  function send(extra, transfer) { try { parent.postMessage(Object.assign({ concierge: 'capture' }, extra), '*', transfer || []); } catch (e) {} }
  var ackResolve = null;
  window.addEventListener('message', function (e) {
    if (e.data && e.data.concierge === 'chunk-ack' && ackResolve) { var r = ackResolve; ackResolve = null; r(); }
  });
  function pushChunk(buf, position) {
    var ack = new Promise(function (r) { ackResolve = r; });
    send({ phase: 'chunk', position: position, buf: buf }, [buf]);
    return ack;   // backpressure: wait until the Studio has written this chunk to disk
  }

  async function renderStreaming(canvas, durationSec, fps) {
    if (typeof VideoEncoder === 'undefined' || !window.WebMMuxer) return false;
    var w = canvas.width, h = canvas.height;
    // Blit each frame onto a 2D scratch canvas before encoding. A WebGL/Three.js canvas
    // (without preserveDrawingBuffer) is cleared after compositing, so new VideoFrame(canvas)
    // captures BLANK 3D. Drawing the source onto a 2D canvas synchronously — right after
    // __seek renders, before the browser clears the buffer — captures it correctly for BOTH
    // WebGL and 2D (the 2D-source case is just a cheap copy).
    var scratch = document.createElement('canvas');
    scratch.width = w; scratch.height = h;
    var sctx = scratch.getContext('2d');
    var queue = [];
    var muxer = new WebMMuxer.Muxer({
      target: new WebMMuxer.StreamTarget({
        chunked: true,
        chunkSize: 4 * 1024 * 1024,
        onData: function (data, position) { queue.push({ buf: data.slice().buffer, position: position }); }
      }),
      video: { codec: 'V_VP9', width: w, height: h, frameRate: fps },
      streaming: true
    });
    var encoder = new VideoEncoder({ output: function (c, m) { muxer.addVideoChunk(c, m); }, error: function (err) { console.error(err); } });
    encoder.configure({ codec: 'vp09.00.10.08', width: w, height: h, framerate: fps, bitrate: 8000000 });
    async function drain() { while (queue.length) { var c = queue.shift(); await pushChunk(c.buf, c.position); } }

    if (typeof window.__beginRender === 'function') window.__beginRender();
    var total = Math.max(1, Math.round(durationSec * fps));
    for (var f = 0; f < total; f++) {
      var t = f / fps;
      if (typeof window.__seek === 'function') window.__seek(t);
      sctx.drawImage(canvas, 0, 0, w, h);
      var vf = new VideoFrame(scratch, { timestamp: Math.round(t * 1e6), duration: Math.round(1e6 / fps) });
      encoder.encode(vf, { keyFrame: f % (fps * 2) === 0 });
      vf.close();
      if (encoder.encodeQueueSize > 8) await new Promise(function (r) { setTimeout(r, 0); });
      if (queue.length) await drain();
      if (f % fps === 0) send({ phase: 'render', frame: f, total: total });
    }
    await encoder.flush();
    muxer.finalize();
    await drain();
    if (typeof window.__endRender === 'function') window.__endRender();
    return true;
  }

  async function renderRealtime(canvas, durationSec) {
    if (!canvas.captureStream || typeof MediaRecorder === 'undefined') return false;
    var chunks = [];
    var rec = new MediaRecorder(canvas.captureStream(30), { mimeType: 'video/webm' });
    rec.ondataavailable = function (ev) { if (ev.data && ev.data.size) chunks.push(ev.data); };
    var stopped = new Promise(function (r) { rec.onstop = r; });
    rec.start();
    await new Promise(function (r) { setTimeout(r, Math.min(durationSec, 120) * 1000); });
    rec.stop();
    await stopped;
    var buf = await new Blob(chunks, { type: 'video/webm' }).arrayBuffer();
    await pushChunk(buf, 0);
    return true;
  }

  window.addEventListener('message', async function (e) {
    var msg = e.data || {};
    if (msg.concierge !== 'record') return;
    // Prefer an explicit author hint, then the scaffold's #stage, then any canvas — so a
    // Three.js scene that renders into #stage is captured, not an empty 2D canvas.
    var canvas = window.__canvas || document.querySelector('#stage') || document.querySelector('canvas');
    if (!canvas) { send({ phase: 'done', ok: false }); return; }
    var fps = window.__fps || 30;
    var dur = Math.max(0.2, window.__duration || (msg.ms ? msg.ms / 1000 : 4));
    var ok = false;
    try { ok = await renderStreaming(canvas, dur, fps); } catch (err) { console.error(err); }
    if (!ok) { try { ok = await renderRealtime(canvas, dur); } catch (e2) {} }
    send({ phase: 'done', ok: ok });
  });
})();
"#;

const STARTER_APP_CSS: &str = r#":root{--bg:#0a0b1a;--ink:#e0e6ff;--muted:#8a90b8;--grad:linear-gradient(120deg,#f085fa,#d122e3 40%,#00e5ff)}
*{box-sizing:border-box}
html,body{height:100%;margin:0;overflow:hidden}
body{background:var(--bg);color:var(--ink);font:16px/1.5 -apple-system,system-ui,sans-serif}
#app{height:100%;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;text-align:center;padding:env(safe-area-inset-top,24px) 24px env(safe-area-inset-bottom,24px)}
h1{font-size:clamp(32px,9vw,64px);margin:0;background:var(--grad);-webkit-background-clip:text;background-clip:text;color:transparent}
p{color:var(--muted);max-width:520px;margin:0}
#start{margin-top:10px;padding:16px 30px;border:0;border-radius:14px;background:var(--grad);color:#0a0b1a;font:700 18px/1 system-ui;cursor:pointer;touch-action:manipulation}
"#;

const APP_JS_STARTER: &str = r#"// Starter — describe your app or game and the Concierge will build it here.
const start = document.getElementById('start');
if (start) start.addEventListener('click', () => {
  start.textContent = 'Building… ask the Concierge to make this real.';
});
"#;

/// Minimal standard-alphabet base64 decoder (no padding required) so binary assets
/// (e.g. an auto-captured animation video) can be staged without pulling in a crate.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = Vec::new();
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c)?;
        bits = (bits << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

/// Pull the text of the first `<title>…</title>` from an HTML document.
fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>")? + start;
    let title = html.get(start..end)?.trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// Insert `insert` just before the last `marker` (case-insensitive); append if absent.
fn inject_before(html: &str, marker: &str, insert: &str) -> String {
    match html.to_lowercase().rfind(&marker.to_lowercase()) {
        Some(pos) => {
            let mut out = String::with_capacity(html.len() + insert.len());
            out.push_str(&html[..pos]);
            out.push_str(insert);
            out.push_str(&html[pos..]);
            out
        }
        None => format!("{html}{insert}"),
    }
}

/// A simple, on-brand PWA icon: a glowing radial orb (full-bleed → maskable-safe).
fn pwa_png_icon(size: u32) -> Vec<u8> {
    let s = size as f32;
    let (cx, cy) = (s / 2.0, s * 0.46);
    let maxd = (cx).hypot(cy).max(1.0);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let n = size as usize;
    let mut data = vec![0u8; n * n * 4];
    for y in 0..size {
        for x in 0..size {
            let t = ((x as f32 - cx).hypot(y as f32 - cy) / maxd)
                .min(1.0)
                .powf(0.9);
            let i = (y as usize * n + x as usize) * 4;
            data[i] = lerp(209.0, 10.0, t) as u8;
            data[i + 1] = lerp(34.0, 11.0, t) as u8;
            data[i + 2] = lerp(227.0, 26.0, t) as u8;
            data[i + 3] = 255;
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = enc.write_header() {
            let _ = writer.write_image_data(&data);
        }
    }
    out
}

const PWA_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
<defs><radialGradient id="o" cx="50%" cy="46%" r="62%">
<stop offset="0%" stop-color="#d122e3"/><stop offset="70%" stop-color="#8a1299"/><stop offset="100%" stop-color="#0a0b1a"/>
</radialGradient></defs>
<rect width="512" height="512" fill="#0a0b1a"/>
<rect width="512" height="512" fill="url(#o)"/>
</svg>
"##;

const PWA_SERVICE_WORKER: &str = r#"const CACHE = 'concierge-pwa-v1';
self.addEventListener('install', function (e) {
  self.skipWaiting();
  e.waitUntil(caches.open(CACHE).then(function (c) {
    return c.addAll(['.', 'index.html', 'manifest.json']).catch(function () {});
  }));
});
self.addEventListener('activate', function (e) { e.waitUntil(self.clients.claim()); });
self.addEventListener('fetch', function (e) {
  if (e.request.method !== 'GET') return;
  e.respondWith(caches.match(e.request).then(function (hit) {
    return hit || fetch(e.request).then(function (res) {
      var copy = res.clone();
      caches.open(CACHE).then(function (c) { c.put(e.request, copy); }).catch(function () {});
      return res;
    }).catch(function () { return caches.match('index.html'); });
  }));
});
"#;

const PWA_SW_REGISTER: &str = r#"<script>if('serviceWorker' in navigator){window.addEventListener('load',function(){navigator.serviceWorker.register('service-worker.js').catch(function(){})});}</script>
"#;

/// `GET /api/canvas/projects`: the saved site projects under `<store>/canvas/` (each
/// immediate subfolder), so the Studio's Open button can show a "pick a project"
/// explorer instead of making the user type a path. Internal dirs (`.checkpoints`,
/// dotfolders) are skipped; newest-modified first.
pub(super) fn canvas_projects_get(mem: &MemCli) -> Response {
    let empty = || serde_json::json!({ "root": "", "projects": [] }).to_string();
    let canvas = match canvas_root(mem) {
        Ok(canvas) => canvas,
        Err(_) => return Response::json(empty()),
    };
    let mut projects: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&canvas) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            projects.push(serde_json::json!({
                "name": name,
                "path": path.to_string_lossy(),
                "files": folder_files(&path).len(),
                "has_index": path.join("index.html").is_file(),
                "mtime": folder_mtime(&path),
            }));
        }
    }
    projects.sort_by(|a, b| {
        b.get("mtime")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            .cmp(
                &a.get("mtime")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
    });
    Response::json(
        serde_json::json!({ "root": canvas.to_string_lossy(), "projects": projects }).to_string(),
    )
}

/// `POST /api/canvas/open`: register a site source folder for live preview. Returns
/// a token (used in `/canvas-preview/<token>/…`), the file list, and the mtime.
pub(super) fn mutation_canvas_open(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
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
    let canon = match require_canvas_dir(mem, std::path::Path::new(folder)) {
        Ok(canon) => canon,
        Err(response) => return response,
    };
    let token = preview_token(&canon);
    if let Ok(mut dirs) = options.preview_dirs.lock() {
        if !dirs.contains_key(&token) && dirs.len() >= MAX_PREVIEW_DIRS {
            return Response::too_many_requests();
        }
        dirs.insert(token.clone(), canon.clone());
    }
    // Mark this as the active project so MCP write tools target it (see set_active_project).
    if let Some(name) = canon.file_name().and_then(|n| n.to_str()) {
        set_active_project(mem, name);
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
pub(super) fn canvas_files_get(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let token = parse_query(query).get("token").cloned().unwrap_or_default();
    match registered_canvas_dir(mem, options, &token) {
        Ok(dir) => Response::json(
            serde_json::json!({
                "files": folder_files(&dir),
                "mtime": folder_mtime(&dir),
                "has_index": dir.join("index.html").is_file(),
            })
            .to_string(),
        ),
        Err(response) => response,
    }
}

/// `GET /api/canvas/mtime?token=`: the folder's newest mtime, for hot-reload polling.
pub(super) fn canvas_mtime_get(mem: &MemCli, options: &GuiOptions, query: &str) -> Response {
    let token = parse_query(query).get("token").cloned().unwrap_or_default();
    match registered_canvas_dir(mem, options, &token) {
        Ok(dir) => Response::json(serde_json::json!({ "mtime": folder_mtime(&dir) }).to_string()),
        Err(response) => response,
    }
}

/// Serve `/canvas-preview/<token>/<relpath>` from the registered folder, read-only
/// and fenced to that folder (no path traversal). This is what the preview iframe
/// loads, so a multi-file site renders with correct relative links.
pub(super) fn canvas_preview_serve(mem: &MemCli, options: &GuiOptions, rest: &str) -> Response {
    let (token, relpath) = rest.split_once('/').unwrap_or((rest, ""));
    let relpath = percent_decode(relpath);
    let relpath = if relpath.trim_matches('/').is_empty() {
        "index.html".to_string()
    } else {
        relpath
    };
    let Some(relpath) = safe_rel_path(&relpath) else {
        return Response::forbidden();
    };
    let Some(registered) = options
        .preview_dirs
        .lock()
        .ok()
        .and_then(|d| d.get(token).cloned())
    else {
        return Response::not_found();
    };
    let dir = match require_canvas_dir(mem, &registered) {
        Ok(dir) => dir,
        Err(response) if response.status == 400 => return Response::not_found(),
        Err(response) => return response,
    };
    let candidate = dir.join(&relpath);
    if reject_symlink_components(
        &match canvas_root(mem) {
            Ok(root) => root,
            Err(response) => return response,
        },
        &candidate,
    )
    .is_err()
    {
        return Response::forbidden();
    }
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
            response.csp = Some(PREVIEW_CSP);
            response
        }
        Err(_) => Response::not_found(),
    }
}
