//! A thin client for talking to a running kernel daemon over a local AF_UNIX socket.
//! Every consumer — the GUI host, the CLI, the MCP server — uses this, so there is
//! one definition of how to reach the kernel. The socket is a Unix domain socket on
//! Unix and an AF_UNIX socket on Windows 10+ (via `uds_windows`), so one code path
//! serves both; only process spawn/stop differ per platform.

use crate::protocol::{Request, Response};
use crate::transport::{read_frame, write_frame};
use std::io;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

/// Is a kernel daemon reachable right now? A cheap connect probe used by
/// supervisors before deciding whether to spawn.
pub fn available() -> bool {
    UnixStream::connect(crate::socket_path()).is_ok()
}

/// Send one request to the kernel and await its response.
pub fn send(req: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(crate::socket_path())?;
    let bytes =
        serde_json::to_vec(req).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(&mut stream, &bytes)?;
    let frame = read_frame(&mut stream)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "kernel closed the connection")
    })?;
    serde_json::from_slice(&frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Route a read request through the kernel's GUI-compatible API contract.
pub fn get(path: impl Into<String>, query: impl Into<String>) -> io::Result<Response> {
    send(&Request {
        id: 1,
        path: path.into(),
        query: query.into(),
        body: None,
    })
}

/// Run a search against the kernel's shared warm index.
pub fn search(query: String, budget: usize, depth: String, hops: u8) -> io::Result<Response> {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("q", &query);
    params.append_pair("budget", &budget.to_string());
    params.append_pair("depth", &depth);
    params.append_pair("hops", &hops.to_string());
    get("/api/search", params.finish())
}

/// Read the kernel-owned network-discovery map.
pub fn peers() -> io::Result<Response> {
    get("/api/peers", "")
}

/// Ensure a kernel daemon is running; returns whether one is now reachable.
///
/// If a kernel is already listening, returns immediately. Otherwise it spawns
/// `concierge-kernel serve` **detached** — its own session via `setsid` on Unix, or
/// `DETACHED_PROCESS` on Windows, with null stdio — so the kernel outlives the
/// spawner: the GUI window can close, or a terminal Ctrl-C land, without taking the
/// kernel (and its warm index) down. Then it waits briefly for the socket to appear.
/// The spawn race is safe: the daemon's `bind_socket` refuses a second live listener,
/// so at most one kernel ever runs.
pub fn ensure_running() -> bool {
    if available() {
        return true;
    }
    let Some(bin) = kernel_binary_path() else {
        return available();
    };
    let mut command = std::process::Command::new(bin);
    command
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach so the spawner's lifecycle (window close / Ctrl-C) can't reach the kernel.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS (0x08) | CREATE_NEW_PROCESS_GROUP (0x200): no inherited
        // console and its own process group, so closing the GUI/terminal can't signal it.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    if command.spawn().is_err() {
        return available();
    }
    // Give the daemon up to ~2s to bind its socket.
    for _ in 0..40 {
        if available() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    available()
}

const KERNEL_BIN_ENV: &str = "CONCIERGE_KERNEL_BIN";

#[cfg(windows)]
const KERNEL_BIN_NAME: &str = "concierge-kernel.exe";
#[cfg(not(windows))]
const KERNEL_BIN_NAME: &str = "concierge-kernel";

/// The `concierge-kernel` binary to supervise. A packaged install should ship it
/// next to `concierge-plugin`; development builds also fall back from a release
/// binary to the sibling debug target when only the daemon was built in debug.
fn kernel_binary_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os(KERNEL_BIN_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
    {
        return Some(path);
    }
    let exe = std::env::current_exe().ok()?;
    kernel_binary_path_for_exe(&exe)
}

fn kernel_binary_path_for_exe(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    kernel_binary_candidates_for_exe(exe)
        .into_iter()
        .find(|path| path.exists())
}

fn kernel_binary_candidates_for_exe(exe: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };

    let mut candidates = vec![dir.join(KERNEL_BIN_NAME)];

    let profile = dir.file_name().and_then(|name| name.to_str());
    if profile == Some("release") {
        if let Some(target_dir) = dir.parent() {
            candidates.push(target_dir.join("debug").join(KERNEL_BIN_NAME));
        }
    }

    candidates
}

/// Lifecycle of a supervised request, so a caller can tell the user about the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLifecycle {
    /// A kernel was already running and answered — its index is already warm.
    Warm,
    /// No kernel was running; one was spawned. Its index warms on first use.
    Started,
    /// A kernel died mid-request; it was restarted and the request retried. Its
    /// in-memory index is rebuilding (from the embed-cache, so nothing re-embeds).
    Restarted,
    /// A running kernel answered but spoke a different protocol version — a stale
    /// daemon left over from before an app upgrade. It was stopped and replaced with
    /// the current binary, and the request retried on it.
    Upgraded,
}

impl KernelLifecycle {
    /// A one-line notice to show the user when the index is (re)warming, or `None`
    /// when the kernel was already warm (nothing to say).
    pub fn index_notice(self) -> Option<&'static str> {
        match self {
            KernelLifecycle::Warm => None,
            KernelLifecycle::Started => {
                Some("memory kernel starting — warming the index (first query may be slower)")
            }
            KernelLifecycle::Restarted => {
                Some("memory kernel restarted — rebuilding the index from cache (no re-embedding)")
            }
            KernelLifecycle::Upgraded => Some(
                "memory kernel upgraded — replaced a stale daemon, rebuilding the index from cache",
            ),
        }
    }
}

fn retry_lifecycle(had_socket_before_send: bool) -> KernelLifecycle {
    if had_socket_before_send {
        KernelLifecycle::Restarted
    } else {
        KernelLifecycle::Started
    }
}

/// Supervised search against the kernel's shared warm index. Ensures a kernel is
/// running, auto-restarts it if it crashed, builds the `/api/search` query, and
/// returns the response + [`KernelLifecycle`]. Used by the CLI and MCP so they
/// share the one warm index instead of each building their own.
pub fn search_supervised(
    query: &str,
    budget: usize,
    depth: &str,
    hops: u8,
    kinds: Option<&str>,
) -> io::Result<(Response, KernelLifecycle)> {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("q", query);
    params.append_pair("budget", &budget.to_string());
    params.append_pair("depth", depth);
    params.append_pair("hops", &hops.to_string());
    if let Some(kinds) = kinds {
        params.append_pair("kinds", kinds);
    }
    let req = Request {
        id: 1,
        path: "/api/search".to_string(),
        query: params.finish(),
        body: None,
    };
    send_supervised(&req)
}

/// Send `req`, ensuring a kernel is running and **auto-restarting it if it crashes**
/// (one retry). Returns the response plus a [`KernelLifecycle`] so the caller can
/// surface [`KernelLifecycle::index_notice`] when the index is (re)warming.
pub fn send_supervised(req: &Request) -> io::Result<(Response, KernelLifecycle)> {
    // Fast path: try the running kernel directly — one connection, no extra probe
    // (a separate `available()` connect would consume a single-accept listener and
    // race the real send).
    let had_socket_before_send = crate::socket_path().exists();
    if let Ok(resp) = send(req) {
        if resp.version == crate::protocol::PROTOCOL_VERSION {
            return Ok((resp, KernelLifecycle::Warm));
        }
        // The kernel answered but speaks a different protocol version — a stale daemon
        // left running across an app upgrade. Stop it (SIGTERM lets it snapshot its
        // index and clear its socket/lock), spawn the current binary, and retry once.
        stop_running_kernel();
        ensure_running();
        return send(req).map(|resp| (resp, KernelLifecycle::Upgraded));
    }
    // Couldn't reach/complete the request. If the socket existed before the
    // failed send, treat the retry as a restart; otherwise it is a cold start.
    // Avoid probing here: another connect can consume one-shot fake listeners in
    // tests and races real supervisors without improving the retry.
    ensure_running();
    let lifecycle = retry_lifecycle(had_socket_before_send);
    send(req).map(|resp| (resp, lifecycle))
}

/// Stop the kernel named in the lockfile and wait for it to release its socket.
/// Used to replace a daemon whose protocol version no longer matches ours (one left
/// running across an app upgrade). The SIGTERM lets it snapshot its index and remove
/// its own socket/lock before the replacement binary binds.
fn stop_running_kernel() {
    if let Some(pid) = lockfile_pid() {
        #[cfg(unix)]
        // SAFETY: kill(2) with a normal termination signal; a stale/invalid pid just
        // returns ESRCH, which we ignore. (`pid_t` is `i32` on our Unix targets.)
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            // No SIGTERM on Windows; ask the OS to terminate the stale daemon. This
            // skips its graceful snapshot, but the next start rebuilds cheaply from
            // the embed-cache, so a version-skew replacement stays safe.
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    // Wait (≤3s) for the old daemon to stop listening, so the replacement's
    // bind_socket doesn't see a live duplicate and refuse to start. This probe is
    // safe here (unlike the fast path) because it only runs on a real version skew,
    // never against the single-accept fake kernels used in tests.
    for _ in 0..60 {
        if !available() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The pid recorded in the kernel lockfile (`{pid, socket}`), if present and valid.
fn lockfile_pid() -> Option<i32> {
    let body = std::fs::read_to_string(crate::lockfile_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    i32::try_from(value.get("pid")?.as_i64()?).ok()
}

#[cfg(test)]
mod tests {
    use super::{kernel_binary_path_for_exe, retry_lifecycle, KernelLifecycle, KERNEL_BIN_NAME};
    use std::fs;

    #[test]
    fn retry_lifecycle_uses_pre_send_socket_presence() {
        assert_eq!(retry_lifecycle(false), KernelLifecycle::Started);
        assert_eq!(retry_lifecycle(true), KernelLifecycle::Restarted);
    }

    #[test]
    fn index_notice_present_when_warming_absent_when_warm() {
        assert!(KernelLifecycle::Warm.index_notice().is_none());
        assert!(KernelLifecycle::Started.index_notice().is_some());
        assert!(KernelLifecycle::Restarted.index_notice().is_some());
        assert!(KernelLifecycle::Upgraded.index_notice().is_some());
    }

    #[test]
    fn kernel_binary_path_prefers_adjacent_installed_daemon() {
        let root = tempfile::tempdir().unwrap();
        let bin_dir = root.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join(if cfg!(windows) {
            "concierge-plugin.exe"
        } else {
            "concierge-plugin"
        });
        let kernel = bin_dir.join(KERNEL_BIN_NAME);
        fs::write(&exe, "").unwrap();
        fs::write(&kernel, "").unwrap();

        assert_eq!(
            kernel_binary_path_for_exe(&exe).as_deref(),
            Some(kernel.as_path())
        );
    }

    #[test]
    fn kernel_binary_path_falls_back_from_release_to_debug_target() {
        let root = tempfile::tempdir().unwrap();
        let release_dir = root.path().join("target").join("release");
        let debug_dir = root.path().join("target").join("debug");
        fs::create_dir_all(&release_dir).unwrap();
        fs::create_dir_all(&debug_dir).unwrap();
        let exe = release_dir.join(if cfg!(windows) {
            "concierge-plugin.exe"
        } else {
            "concierge-plugin"
        });
        let kernel = debug_dir.join(KERNEL_BIN_NAME);
        fs::write(&exe, "").unwrap();
        fs::write(&kernel, "").unwrap();

        assert_eq!(
            kernel_binary_path_for_exe(&exe).as_deref(),
            Some(kernel.as_path())
        );
    }

    #[test]
    fn kernel_binary_path_does_not_escape_test_deps_dir() {
        let root = tempfile::tempdir().unwrap();
        let debug_dir = root.path().join("target").join("debug");
        let deps_dir = debug_dir.join("deps");
        fs::create_dir_all(&deps_dir).unwrap();
        let exe = deps_dir.join(if cfg!(windows) {
            "concierge_kernel_client_test.exe"
        } else {
            "concierge_kernel_client_test"
        });
        let kernel = debug_dir.join(KERNEL_BIN_NAME);
        fs::write(&exe, "").unwrap();
        fs::write(&kernel, "").unwrap();

        assert!(kernel_binary_path_for_exe(&exe).is_none());
    }
}
