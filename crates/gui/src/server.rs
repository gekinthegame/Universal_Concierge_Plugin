use super::*;

#[cfg(unix)]
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Async-signal-safe handler: just flag the request. A poller thread does the real
/// (non-signal-safe) shutdown work — stopping the detached Kubo node, then exiting.
#[cfg(unix)]
extern "C" fn handle_term_signal(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Stop background processes and exit. The single exit seam for **every** shutdown path —
/// window-close (heartbeat watchdog), watched-PID death, and Ctrl-C/SIGTERM — so none of them
/// can leak the **detached** private Kubo daemon (it outlives this process unless told to stop).
/// `process::exit` tears down all in-process work (chat node, embedder, server threads).
fn shutdown(mem: &MemCli) -> ! {
    let _ = mem.stop_sidekick_node();
    remove_gui_lock(mem);
    std::process::exit(0);
}

/// Drop the reuse lock so a second mount doesn't try to reach this now-dead server.
fn remove_gui_lock(mem: &MemCli) {
    if let Some(path) = gui_lock_path(mem) {
        let _ = std::fs::remove_file(path);
    }
}

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
        let _ = open_app(&format!("http://{addr}"));
    }

    if let Some(pid) = options.watch_pid {
        let mem_watch = mem.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            #[cfg(unix)]
            let alive = unsafe {
                libc::kill(pid as i32, 0) == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            };
            #[cfg(not(unix))]
            let alive = true;
            if !alive {
                shutdown(&mem_watch); // stop the detached Kubo node too, not just this process
            }
        });
    }

    // Hitting the GUI window's X must close the Concierge AND its background processes. Each
    // window heartbeats `/api/heartbeat` while open and beacons `/api/closing` on unload; when
    // the last window is gone (after at least one connected), shut the whole server down. Only
    // the process that owns a window does this — a headless `--no-open` server is governed by
    // watch_pid instead. A generous STALE tolerates background-tab timer throttling (the beacon
    // catches real closes fast); GRACE outlives a page reload before deciding the window is gone.
    if options.open_browser {
        let mem_life = mem.clone();
        let clients = options.clients.clone();
        std::thread::spawn(move || {
            const STALE: std::time::Duration = std::time::Duration::from_secs(75);
            const GRACE: std::time::Duration = std::time::Duration::from_secs(6);
            let mut empty_since: Option<std::time::Instant> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let (seen_any, empty) = {
                    let Ok(mut presence) = clients.lock() else {
                        continue;
                    };
                    let now = std::time::Instant::now();
                    presence
                        .last_seen
                        .retain(|_, seen| now.duration_since(*seen) < STALE);
                    (presence.seen_any, presence.last_seen.is_empty())
                };
                if !seen_any {
                    continue; // no window has connected yet — don't exit during startup
                }
                if empty {
                    match empty_since {
                        Some(since) if since.elapsed() >= GRACE => shutdown(&mem_life),
                        Some(_) => {}
                        None => empty_since = Some(std::time::Instant::now()),
                    }
                } else {
                    empty_since = None; // a window is back (e.g. a reload) — cancel shutdown
                }
            }
        });
    }

    // Ctrl-C / SIGTERM in a terminal must also stop the detached Kubo node, not just this
    // process. The handler only flags; this poller does the real shutdown work.
    #[cfg(unix)]
    {
        let handler = handle_term_signal as *const () as libc::sighandler_t;
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
        let mem_sig = mem.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
                shutdown(&mem_sig);
            }
        });
    }

    // We stop the detached Kubo daemon on window-close, so if the Sidekick was left enabled,
    // relaunch its node here (idempotent — `enable_sidekick` only launches if not running).
    {
        let mem_node = mem.clone();
        std::thread::spawn(move || {
            if mem_node.sidekick_status().enabled {
                let _ = mem_node.enable_sidekick();
            }
        });
    }

    // Phase C: while the app is open, continuously capture Claude Code sessions —
    // but only once the user has explicitly attached (consent-gated, opt-in).
    spawn_claude_code_capture(mem.clone());

    // Maintenance: silently compact the store (GC superseded blocks) once a day — a
    // first pass two minutes after launch (so startup isn't slowed), then every 24h.
    // Safe by construction (only blocks no live name/checkpoint/Decision can reach),
    // and invisible to the user — no button, no notice.
    {
        let compact_mem = mem.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(120));
            let _ = compact_mem.gc(&concierge_core::GcPolicy {
                keep_checkpoints: None,
            });
            std::thread::sleep(std::time::Duration::from_secs(24 * 60 * 60 - 120));
        });
    }

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
    mem.store_dir()
        .ok()
        .map(|dir| dir.join("capture-claude-code"))
}

pub(super) fn claude_code_attached(mem: &MemCli) -> bool {
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
        atomic_local_write(&path, b"attached")
    } else if path.exists() {
        std::fs::remove_file(&path)
    } else {
        Ok(())
    }
}

/// Where per-file ingest offsets are persisted across relaunches, so a restart
/// resumes the tail instead of re-scanning every session from byte 0.
fn capture_offsets_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir()
        .ok()
        .map(|dir| dir.join("capture-offsets.json"))
}

pub(super) fn load_capture_offsets(
    mem: &MemCli,
) -> std::collections::HashMap<std::path::PathBuf, u64> {
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

pub(super) fn save_capture_offsets(
    mem: &MemCli,
    offsets: &std::collections::HashMap<std::path::PathBuf, u64>,
) {
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
        let _ = atomic_local_write(&path, text.as_bytes());
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
pub(super) fn sidekick_status_json(mem: &MemCli) -> CoreResult<String> {
    let status = mem.sidekick_status();
    serde_json::to_string(&status).map_err(|e| Error::Io(format!("serialize sidekick status: {e}")))
}

/// Enable/disable the Sidekick (launches/winds down the private Kubo node).
/// CSRF-gated like every mutation; the password is not involved (local-only).
pub(super) fn mutation_sidekick(mem: &MemCli, enable: bool) -> Response {
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
pub(super) fn claude_code_status_json(mem: &MemCli) -> CoreResult<String> {
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
pub(super) fn mutation_claude_code_attach(mem: &MemCli, attached: bool) -> Response {
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
    let body = serde_json::json!({ "pid": std::process::id(), "port": port }).to_string();
    let _ = atomic_local_write(&path, body.as_bytes());
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
/// Locate an installed Brave binary, or `None`. Brave is the Concierge's preferred
/// shell (Decision 0033): running the GUI inside Brave is what makes the wallet,
/// native `ipns://`, and bookmark memory available.
pub fn brave_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            std::path::PathBuf::from(
                "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            ),
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join("Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for name in ["brave-browser", "brave", "brave-browser-stable"] {
            if let Ok(out) = Command::new("which").arg(name).output() {
                if out.status.success() {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(path.into());
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        for env in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                let p = std::path::Path::new(&base)
                    .join("BraveSoftware/Brave-Browser/Application/brave.exe");
                if p.exists() {
                    return Some(p);
                }
            }
        }
        None
    }
}

/// Locate an installed Opera binary, or `None`. Opera is the other supported
/// Chromium wallet browser (Decision 0033) — built-in wallet, `--app` mode, CDP.
pub fn opera_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            std::path::PathBuf::from("/Applications/Opera.app/Contents/MacOS/Opera"),
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join("Applications/Opera.app/Contents/MacOS/Opera"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for name in ["opera", "opera-stable"] {
            if let Ok(out) = Command::new("which").arg(name).output() {
                if out.status.success() {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(path.into());
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        for env in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(env) {
                for sub in ["Programs/Opera/opera.exe", "Opera/opera.exe"] {
                    let p = std::path::Path::new(&base).join(sub);
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
        None
    }
}

/// A supported Chromium browser with a built-in crypto wallet (Decision 0033).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletBrowser {
    Brave,
    Opera,
}
impl WalletBrowser {
    pub fn label(self) -> &'static str {
        match self {
            WalletBrowser::Brave => "Brave",
            WalletBrowser::Opera => "Opera",
        }
    }
}

/// Which wallet browser to run the Concierge in. `CONCIERGE_BROWSER=brave|opera`
/// forces a preference; otherwise Brave is preferred (fuller native-IPFS), then
/// Opera. `None` if neither is installed.
pub fn wallet_browser() -> Option<(WalletBrowser, std::path::PathBuf)> {
    let brave = || brave_path().map(|p| (WalletBrowser::Brave, p));
    let opera = || opera_path().map(|p| (WalletBrowser::Opera, p));
    match std::env::var("CONCIERGE_BROWSER")
        .ok()
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("opera") => opera().or_else(brave),
        Some("brave") => brave().or_else(opera),
        _ => brave().or_else(opera),
    }
}

/// Open `url` as the Concierge **app window** — a chromeless `--app` window in the
/// user's wallet browser (Brave or Opera) when one is present (so it has the wallet
/// + the user's bookmarks + native IPFS, using their default profile), otherwise
///   the default browser. Set `CONCIERGE_NO_BRAVE=1` to always use the default browser.
pub fn open_app(url: &str) -> CoreResult<()> {
    if std::env::var_os("CONCIERGE_NO_BRAVE").is_none() {
        if let Some((_, exe)) = wallet_browser() {
            if Command::new(&exe)
                .arg(format!("--app={url}"))
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
    }
    open_browser(url)
}

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

pub(super) fn serve_connection(
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
pub(super) fn route_request(
    mem: &MemCli,
    options: &GuiOptions,
    request: &ParsedRequest,
) -> Response {
    if let Some(rejection) = loopback_gate(request, &options.csrf_token) {
        return rejection;
    }
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    // Window-lifecycle pings are CSRF-gated by loopback_gate above but are NOT privacy
    // mutations — exempt them from the mutation rate limiter (the page pings every few seconds).
    if request.method == "POST" && (path == "/api/heartbeat" || path == "/api/closing") {
        return handle_lifecycle(options, path, &request.body);
    }
    if request.method == "POST" && !options.allow_mutation() {
        return Response::too_many_requests();
    }
    match request.method.as_str() {
        "GET" => handle_with_options(mem, options, path, query),
        "POST" => handle_mutation(mem, options, path, &request.body),
        _ => Response::method_not_allowed(),
    }
}

/// Record a window heartbeat (`/api/heartbeat`) or close beacon (`/api/closing`). The
/// open-window watchdog reads this presence to decide when the last window has gone and the
/// server should fully shut down. Body is `{"id":"<window-id>"}`.
fn handle_lifecycle(options: &GuiOptions, path: &str, body: &str) -> Response {
    let id = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if id.is_empty() {
        return Response::bad_request("missing window id");
    }
    if let Ok(mut presence) = options.clients.lock() {
        presence.seen_any = true;
        if path == "/api/heartbeat" {
            presence.last_seen.insert(id, std::time::Instant::now());
        } else {
            presence.last_seen.remove(&id);
        }
    }
    Response::json("{\"ok\":true}".to_string())
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

    // Read the body (if any). Ingest and Site operations get a larger budget.
    let path = target.split('?').next().unwrap_or("/");
    let body_limit = if path == "/api/ingest"
        || path == "/api/canvas/snapshot"
        || path == "/api/canvas/write"
        || path == "/api/site/publish"
    {
        MAX_LARGE_BODY_BYTES
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
    let csp = response.csp.map(str::to_string).unwrap_or_else(|| {
        format!(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors {frame_ancestors}; object-src 'none'; base-uri 'none'; form-action 'self'"
        )
    });
    let mut header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: {}\r\nContent-Security-Policy: {}\r\n",
        response.status,
        reason_phrase(response.status),
        content_type,
        response.body.len(),
        frame_options,
        csp,
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
