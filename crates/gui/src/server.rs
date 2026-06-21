use super::*;

#[cfg(unix)]
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Async-signal-safe handler: just flag the request. A poller thread does the real
/// (non-signal-safe) shutdown work, then exits.
#[cfg(unix)]
extern "C" fn handle_term_signal(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

// ── Windows console-control shutdown (parity with the Unix signal path) ──────
//
// Windows has no SIGINT/SIGTERM. Closing the console, Ctrl-C, Ctrl-Break, logoff
// and system shutdown all arrive as *console control events* instead, delivered
// by the OS on a dedicated thread. Without this handler those events kill only
// the foreground process and orphan the **detached** Kubo daemon — the exact
// "background processes stay open on Windows" leak. Unlike a Unix signal context,
// this callback runs on a normal thread, so it can safely do the real shutdown
// work directly (no flag-and-poll dance needed). We stash a `MemCli` here so the
// handler can reach the same `shutdown()` seam every other path uses.
#[cfg(windows)]
static SHUTDOWN_MEM: std::sync::OnceLock<MemCli> = std::sync::OnceLock::new();

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> isize;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
    fn CloseHandle(handle: isize) -> i32;
    fn GetLastError() -> u32;
}

/// OS-invoked on Ctrl-C / Ctrl-Break / console-close / logoff / shutdown. Returning
/// is moot — we drop the GUI lock and exit from here.
#[cfg(windows)]
unsafe extern "system" fn handle_console_ctrl(_ctrl_type: u32) -> i32 {
    if let Some(mem) = SHUTDOWN_MEM.get() {
        shutdown(mem); // never returns
    }
    std::process::exit(0);
}

/// Is `pid` still running? Windows counterpart to the Unix `kill(pid, 0)` probe.
/// On any ambiguous error (e.g. access-denied) we assume *alive* so a transient
/// failure can never trigger a spurious shutdown; only an explicitly invalid PID
/// counts as dead.
#[cfg(windows)]
fn pid_is_alive_windows(pid: u32) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle == 0 {
            // No handle: dead only if the OS says the PID is invalid; otherwise
            // (e.g. ERROR_ACCESS_DENIED) the process exists, so treat it as alive.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
        CloseHandle(handle);
        alive
    }
}

/// Stop the GUI process. The single exit seam for **every** shutdown path —
/// window-close (heartbeat watchdog), watched-PID death, and Ctrl-C/SIGTERM.
/// Phase 5 moves the shared kernel and private Kubo lifecycle out of the GUI;
/// `process::exit` only tears down in-process GUI work (chat node, server threads).
fn shutdown(mem: &MemCli) -> ! {
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
    // Launch the app window in a wallet browser we *own* (a dedicated, persistent
    // `--user-data-dir`), then watch that child process. Closing the window — or a crash
    // or force-quit — exits the child, which is the reliable "the app is gone" signal that
    // shuts the server down. A backgrounded or minimized window keeps the process alive, so
    // it no longer triggers a spurious shutdown (the bug the old heartbeat watchdog had).
    // With no wallet browser we fall back to the default browser (nothing to watch) and
    // rely on Ctrl-C / the harness `watch_pid`.
    if options.open_browser {
        let url = format!("http://{addr}");
        match mem.store_dir().ok().map(|dir| dir.join("brave-profile")) {
            Some(profile) => {
                if let Some(child) = open_app_window(&url, &profile) {
                    spawn_browser_watch(mem.clone(), child);
                }
            }
            None => {
                let _ = open_app(&url);
            }
        }
    }

    // A harness that launched us headless (`--no-open`) passes its own PID; when that
    // parent dies we exit too. This watches a PID that is *not* our child, so a liveness
    // probe (not `try_wait`) is the right tool.
    if let Some(pid) = options.watch_pid {
        let mem_watch = mem.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            #[cfg(unix)]
            let alive = unsafe {
                libc::kill(pid as i32, 0) == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            };
            #[cfg(windows)]
            let alive = pid_is_alive_windows(pid);
            #[cfg(not(any(unix, windows)))]
            let alive = true;
            if !alive {
                shutdown(&mem_watch);
            }
        });
    }

    // Ctrl-C / SIGTERM in a terminal exits through the same shutdown seam. The
    // handler only flags; this poller does the real shutdown work.
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

    // Windows equivalent: register a console-control handler so Ctrl-C / Ctrl-Break /
    // console-close / logoff / shutdown all reach the same `shutdown()` seam.
    #[cfg(windows)]
    {
        let _ = SHUTDOWN_MEM.set(mem.clone());
        unsafe {
            SetConsoleCtrlHandler(Some(handle_console_ctrl), 1);
        }
    }

    // Phase 5: the memory kernel owns capture + the warm shared index. Ensure it
    // is running off the startup path so a slow kernel launch never delays the
    // window or the first HTTP response. AF_UNIX socket on Unix and Windows 10+.
    #[cfg(any(unix, windows))]
    {
        std::thread::spawn(move || {
            if std::env::var_os("CONCIERGE_NO_KERNEL").is_none() {
                let _ = concierge_kernel::client::ensure_running();
            }
        });
    }

    // The search index is kernel-owned in Phase 5. The GUI does not build or cache
    // its own retrieval index.

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

// ── Aider harness capture (same shape as Claude Code, different on-disk format) ──
// Aider writes a Markdown transcript per repo (`.aider.chat.history.md`); the
// adapter discovers them and re-ingests on change (CID-dedup makes that safe).
// Capture is consent-gated by its own sentinel, exactly like Claude Code.

fn aider_flag_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir().ok().map(|dir| dir.join("capture-aider"))
}

pub(super) fn aider_attached(mem: &MemCli) -> bool {
    aider_flag_path(mem)
        .map(|path| path.exists())
        .unwrap_or(false)
}

pub(super) fn set_aider_attached(mem: &MemCli, attached: bool) -> std::io::Result<()> {
    let Some(path) = aider_flag_path(mem) else {
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

/// `GET /api/aider/status`: which Aider transcripts the Concierge can see and
/// whether capture is attached — so the Host card can show "Aider · N sessions"
/// the same way it shows Claude Code.
pub(super) fn aider_status_json(mem: &MemCli) -> CoreResult<String> {
    use concierge_adapter_aider::discovery;
    let transcripts = discovery::discover();
    let session_count = discovery::total_sessions(&transcripts);
    serde_json::to_string(&serde_json::json!({
        "available": !transcripts.is_empty(),
        "transcript_count": transcripts.len(),
        "session_count": session_count,
        "attached": aider_attached(mem),
    }))
    .map_err(|e| Error::Io(format!("serialize aider status: {e}")))
}

/// Manual full backfill of Aider transcripts (the "Ingest" button) — runs in the
/// background so the request returns immediately.
pub(super) fn aider_ingest(mem: &MemCli) -> Response {
    let mem = mem.clone();
    let base = mem.working_dir().to_path_buf();
    std::thread::spawn(move || {
        let _ = concierge_adapter_aider::ingest_all(&mem, &base);
    });
    Response::json(serde_json::json!({ "ok": true, "started": true }).to_string())
}

// ── Additional file-based harnesses (Codex, Gemini, Continue) ────────────────
// The GUI owns status, attach consent, and manual historical ingest buttons.
// Continuous changed-file capture is kernel-owned in Phase 5. One macro generates
// the attach sentinel helpers, status JSON, and manual ingest endpoint.
macro_rules! harness_capture {
    ($flag:literal, $attached:ident, $set:ident, $status:ident,
     $disc:path, $label:literal, $ingest:ident, $ingest_all:path) => {
        pub(super) fn $attached(mem: &MemCli) -> bool {
            mem.store_dir()
                .ok()
                .map(|dir| dir.join($flag).exists())
                .unwrap_or(false)
        }

        pub(super) fn $set(mem: &MemCli, attached: bool) -> std::io::Result<()> {
            let Ok(dir) = mem.store_dir() else {
                return Ok(());
            };
            let path = dir.join($flag);
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

        pub(super) fn $status(mem: &MemCli) -> CoreResult<String> {
            use $disc as discovery;
            let sessions = discovery::discover();
            let session_count = discovery::total_sessions(&sessions);
            serde_json::to_string(&serde_json::json!({
                "available": !sessions.is_empty(),
                "transcript_count": sessions.len(),
                "session_count": session_count,
                "attached": $attached(mem),
            }))
            .map_err(|e| Error::Io(format!("serialize {} status: {}", $label, e)))
        }

        /// Manual full backfill (the "Ingest" button): runs the historical ingest in
        /// a background thread so the request returns immediately and the UI stays
        /// responsive while the System Console fills with progress.
        pub(super) fn $ingest(mem: &MemCli) -> Response {
            let mem = mem.clone();
            let base = mem.working_dir().to_path_buf();
            std::thread::spawn(move || {
                let _ = $ingest_all(&mem, &base);
            });
            Response::json(serde_json::json!({ "ok": true, "started": true }).to_string())
        }
    };
}

harness_capture!(
    "capture-codex",
    codex_attached,
    set_codex_attached,
    codex_status_json,
    concierge_adapter_codex::discovery,
    "codex",
    codex_ingest,
    concierge_adapter_codex::ingest_all
);
harness_capture!(
    "capture-gemini",
    gemini_attached,
    set_gemini_attached,
    gemini_status_json,
    concierge_adapter_gemini::discovery,
    "gemini",
    gemini_ingest,
    concierge_adapter_gemini::ingest_all
);
harness_capture!(
    "capture-continue",
    continue_attached,
    set_continue_attached,
    continue_status_json,
    concierge_adapter_continue::discovery,
    "continue",
    continue_ingest,
    concierge_adapter_continue::ingest_all
);
harness_capture!(
    "capture-antigravity",
    antigravity_attached,
    set_antigravity_attached,
    antigravity_status_json,
    concierge_adapter_antigravity::discovery,
    "antigravity",
    antigravity_ingest,
    concierge_adapter_antigravity::ingest_all
);
harness_capture!(
    "capture-openclaw",
    openclaw_attached,
    set_openclaw_attached,
    openclaw_status_json,
    concierge_adapter_openclaw::discovery,
    "openclaw",
    openclaw_ingest,
    concierge_adapter_openclaw::ingest_all
);
harness_capture!(
    "capture-cline",
    cline_attached,
    set_cline_attached,
    cline_status_json,
    concierge_adapter_cline::discovery,
    "cline",
    cline_ingest,
    concierge_adapter_cline::ingest_all
);
harness_capture!(
    "capture-cursor",
    cursor_attached,
    set_cursor_attached,
    cursor_status_json,
    concierge_adapter_cursor::discovery,
    "cursor",
    cursor_ingest,
    concierge_adapter_cursor::ingest_all
);
harness_capture!(
    "capture-opendevin",
    opendevin_attached,
    set_opendevin_attached,
    opendevin_status_json,
    concierge_adapter_opendevin::discovery,
    "opendevin",
    opendevin_ingest,
    concierge_adapter_opendevin::ingest_all
);
harness_capture!(
    "capture-copilot",
    copilot_attached,
    set_copilot_attached,
    copilot_status_json,
    concierge_adapter_copilot::discovery,
    "copilot",
    copilot_ingest,
    concierge_adapter_copilot::ingest_all
);

/// Where per-file ingest offsets are persisted across relaunches, so a restart
/// resumes the tail instead of re-scanning every session from byte 0.
#[cfg(test)]
fn capture_offsets_path(mem: &MemCli) -> Option<std::path::PathBuf> {
    mem.store_dir()
        .ok()
        .map(|dir| dir.join("capture-offsets.json"))
}

#[cfg(test)]
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

#[cfg(test)]
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

/// Manual full backfill of Claude Code sessions (the "Ingest" button) — re-reads
/// every session from byte 0 in a background thread so the request returns at once.
pub(super) fn claude_code_ingest(mem: &MemCli) -> Response {
    use concierge_adapter_claude_code::{discovery, ingest_all};
    let Some(projects_dir) = discovery::claude_projects_dir() else {
        return Response::error("Claude Code projects directory not found".to_string());
    };
    let mem = mem.clone();
    let base = mem.working_dir().to_path_buf();
    std::thread::spawn(move || {
        let _ = ingest_all(&projects_dir, &mem, &base);
    });
    Response::json(serde_json::json!({ "ok": true, "started": true }).to_string())
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
/// The 2-harness limit is enforced in the UI (the Attach buttons grey out); no
/// server-side cap check is needed.
pub(super) fn mutation_claude_code_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_claude_code_attached(mem, attached) {
        return Response::error(format!("could not update capture state: {error}"));
    }
    match claude_code_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Attach/detach Aider capture (the consent sentinel), same shape as Claude Code.
pub(super) fn mutation_aider_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_aider_attached(mem, attached) {
        return Response::error(format!("could not update aider capture state: {error}"));
    }
    match aider_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_codex_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_codex_attached(mem, attached) {
        return Response::error(format!("could not update codex capture state: {error}"));
    }
    match codex_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_gemini_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_gemini_attached(mem, attached) {
        return Response::error(format!("could not update gemini capture state: {error}"));
    }
    match gemini_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_continue_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_continue_attached(mem, attached) {
        return Response::error(format!("could not update continue capture state: {error}"));
    }
    match continue_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_antigravity_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_antigravity_attached(mem, attached) {
        return Response::error(format!(
            "could not update antigravity capture state: {error}"
        ));
    }
    match antigravity_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_openclaw_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_openclaw_attached(mem, attached) {
        return Response::error(format!("could not update openclaw capture state: {error}"));
    }
    match openclaw_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_cline_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_cline_attached(mem, attached) {
        return Response::error(format!("could not update cline capture state: {error}"));
    }
    match cline_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_cursor_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_cursor_attached(mem, attached) {
        return Response::error(format!("could not update cursor capture state: {error}"));
    }
    match cursor_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_opendevin_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_opendevin_attached(mem, attached) {
        return Response::error(format!("could not update opendevin capture state: {error}"));
    }
    match opendevin_status_json(mem) {
        Ok(body) => Response::json(body),
        Err(error) => Response::error(error.to_string()),
    }
}

pub(super) fn mutation_copilot_attach(mem: &MemCli, attached: bool) -> Response {
    if let Err(error) = set_copilot_attached(mem, attached) {
        return Response::error(format!("could not update copilot capture state: {error}"));
    }
    match copilot_status_json(mem) {
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

/// Launch the Concierge **app window** in a wallet browser using a dedicated, persistent
/// profile we own (`--user-data-dir`), so the spawned process *is* the browser instance —
/// no Chromium singleton hand-off to an already-running Brave — and we can watch it as a
/// child to know when the window closes. Returns the owned child on success. Falls back to
/// the default browser (returns `None`, nothing watchable) when no wallet browser is
/// installed or `CONCIERGE_NO_BRAVE` is set. The profile is created on first use and
/// persists, so the in-browser wallet and bookmarks survive across runs.
pub fn open_app_window(url: &str, profile_dir: &std::path::Path) -> Option<std::process::Child> {
    if std::env::var_os("CONCIERGE_NO_BRAVE").is_none() {
        if let Some((_, exe)) = wallet_browser() {
            let _ = std::fs::create_dir_all(profile_dir);
            if let Ok(child) = Command::new(&exe)
                .arg(format!("--app={url}"))
                .arg(format!("--user-data-dir={}", profile_dir.display()))
                .arg("--no-first-run")
                .arg("--no-default-browser-check")
                .spawn()
            {
                return Some(child);
            }
        }
    }
    let _ = open_browser(url);
    None
}

/// Watch the app-window browser child and shut the GUI down when it exits. `try_wait`
/// observes the child's death directly (no zombie or PID-reuse races), so closing the
/// window, a crash, or a force-quit all reap the server reliably — while a merely
/// backgrounded or minimized window, whose process stays alive, never does. A child that
/// dies within the first few seconds is treated as a singleton hand-off / failed launch
/// rather than a real close: we stop watching and leave the server up for Ctrl-C.
fn spawn_browser_watch(mem: MemCli, mut child: std::process::Child) {
    let started = std::time::Instant::now();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        match child.try_wait() {
            Ok(Some(_)) => {
                if started.elapsed() < std::time::Duration::from_secs(3) {
                    return; // hand-off / failed launch — not a user close
                }
                shutdown(&mem); // never returns
            }
            Ok(None) => continue,    // window still open
            Err(_) => return,        // can't observe the child — don't risk a spurious exit
        }
    });
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
    if request.method == "POST" && !options.allow_mutation() {
        return Response::too_many_requests();
    }
    match request.method.as_str() {
        "GET" => handle_with_options(mem, options, path, query),
        "POST" => handle_mutation(mem, options, path, &request.body),
        _ => Response::method_not_allowed(),
    }
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
