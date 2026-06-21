//! `concierge-kernel` binary — the stable memory daemon.
//!
//! The shared protocol, transport, and client live in the crate's library
//! (`concierge_kernel`); this binary adds the daemon itself — the warm-index state
//! and the serve loop — plus a thin CLI for testing a running daemon.
//!
//! Usage:
//!   concierge-kernel serve            # run the daemon (default)
//!   concierge-kernel ping             # round-trip check against a running daemon
//!   concierge-kernel status           # is the shared index warm yet?
//!   concierge-kernel search <query…>  # query the shared warm index
//!
//! Phase 1 is Unix-only by design; the Windows named-pipe transport is Phase 6.
//! On non-Unix the binary compiles to a stub so a full-workspace build succeeds.

#[cfg(any(unix, windows))]
mod capture;
#[cfg(any(unix, windows))]
mod node;
#[cfg(any(unix, windows))]
mod state;

#[cfg(any(unix, windows))]
fn main() -> std::process::ExitCode {
    daemon::run()
}

#[cfg(not(any(unix, windows)))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "concierge-kernel: the daemon needs a local AF_UNIX socket (Unix, or Windows 10+ \
         via uds_windows); this platform is unsupported."
    );
    std::process::ExitCode::FAILURE
}

#[cfg(any(unix, windows))]
mod daemon {
    use crate::state::KernelState;
    use concierge_kernel::client;
    use concierge_kernel::protocol::{Request, Response};
    use concierge_kernel::transport::{read_frame, write_frame};
    use concierge_kernel::{lockfile_path, socket_path};
    use std::io;
    #[cfg(unix)]
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;
    #[cfg(windows)]
    use uds_windows::{UnixListener, UnixStream};

    pub fn run() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str).unwrap_or("serve") {
            "serve" => serve(),
            "ping" => run_client(Request {
                id: 1,
                path: "/__kernel/ping".into(),
                ..Default::default()
            }),
            "status" => run_client(Request {
                id: 1,
                path: "/__kernel/status".into(),
                ..Default::default()
            }),
            "search" => {
                let query = args[1..].join(" ");
                if query.trim().is_empty() {
                    eprintln!("usage: concierge-kernel search <query…>");
                    return ExitCode::from(2);
                }
                let mut params = url::form_urlencoded::Serializer::new(String::new());
                params.append_pair("q", &query);
                params.append_pair("budget", "4000");
                params.append_pair("depth", "summary");
                run_client(Request {
                    id: 1,
                    path: "/api/search".into(),
                    query: params.finish(),
                    body: None,
                })
            }
            "peers" => run_client(Request {
                id: 1,
                path: "/api/peers".into(),
                ..Default::default()
            }),
            "routes" => {
                println!("{}", crate::state::KERNEL_ROUTES.join("\n"));
                ExitCode::SUCCESS
            }
            other => {
                eprintln!(
                    "unknown command: {other}\nusage: concierge-kernel <serve|ping|status|search|peers|routes>"
                );
                ExitCode::from(2)
            }
        }
    }

    // ── Shutdown: persist the index snapshot on SIGINT/SIGTERM ──────────────────

    static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Async-signal-safe handler: only flag the request. The poller below does the
    /// real (signal-unsafe) work — saving the snapshot and exiting.
    #[cfg(unix)]
    extern "C" fn on_term_signal(_sig: libc::c_int) {
        SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Windows has no SIGINT/SIGTERM; Ctrl-C / Ctrl-Break / console-close / logoff /
    /// shutdown arrive as console control events instead. We only flag here; the poller
    /// does the snapshot+cleanup. Returning TRUE marks the event handled.
    #[cfg(windows)]
    unsafe extern "system" fn on_console_ctrl(_ctrl_type: u32) -> i32 {
        SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
        1
    }

    #[cfg(windows)]
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    /// Register the platform's "please stop" signal so it sets [`SHUTDOWN`].
    #[cfg(unix)]
    fn register_shutdown_signal() {
        unsafe {
            let handler = on_term_signal as *const () as libc::sighandler_t;
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }

    #[cfg(windows)]
    fn register_shutdown_signal() {
        unsafe {
            SetConsoleCtrlHandler(Some(on_console_ctrl), 1);
        }
    }

    /// Catch the platform stop signal, then on the next poll persist the warm index
    /// snapshot (so the next start LOADS instead of rebuilding), drop the socket and
    /// lockfile, and exit. The poller is platform-neutral; only the signal source differs.
    fn install_shutdown(state: Arc<KernelState>, sock: PathBuf, lockfile: PathBuf) {
        register_shutdown_signal();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
                state.save_index_snapshot();
                let _ = std::fs::remove_file(&sock);
                let _ = std::fs::remove_file(&lockfile);
                std::process::exit(0);
            }
        });
    }

    // ── Daemon ────────────────────────────────────────────────────────────────

    fn serve() -> ExitCode {
        let sock = socket_path();
        let listener = match bind_socket(&sock) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("kernel: cannot bind {}: {error}", sock.display());
                return ExitCode::FAILURE;
            }
        };
        let lock = match KernelLock::write(&sock) {
            Ok(lock) => lock,
            Err(error) => {
                let _ = std::fs::remove_file(&sock);
                eprintln!("kernel: cannot write lockfile: {error}");
                return ExitCode::FAILURE;
            }
        };
        let state = Arc::new(KernelState::new(concierge_kernel::workdir()));
        state.start_background();
        install_shutdown(Arc::clone(&state), sock.clone(), lock.path().to_path_buf());
        eprintln!("kernel: listening on {}", sock.display());
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve_conn(stream, &state));
                }
                Err(error) => eprintln!("kernel: accept error: {error}"),
            }
        }
        drop(lock);
        ExitCode::SUCCESS
    }

    struct KernelLock {
        path: PathBuf,
    }

    impl KernelLock {
        fn write(sock: &Path) -> io::Result<Self> {
            let path = lockfile_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
            let tmp = path.with_extension("lock.tmp");
            let body = serde_json::json!({
                "pid": std::process::id(),
                "socket": sock.display().to_string(),
            })
            .to_string();
            std::fs::write(&tmp, body)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&tmp, &path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for KernelLock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// One connection: read framed requests, dispatch, write framed responses,
    /// until the peer closes. A connection error never takes the daemon down —
    /// other clients keep being served.
    fn serve_conn(mut stream: UnixStream, state: &KernelState) {
        loop {
            let frame = match read_frame(&mut stream) {
                Ok(Some(frame)) => frame,
                Ok(None) => return, // peer closed cleanly
                Err(error) => {
                    eprintln!("kernel: read error: {error}");
                    return;
                }
            };
            let response = match serde_json::from_slice::<Request>(&frame) {
                Ok(req) => dispatch(state, &req),
                Err(error) => Response::err(0, format!("bad request: {error}")),
            };
            let bytes = match serde_json::to_vec(&response) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("kernel: encode error: {error}");
                    return;
                }
            };
            if let Err(error) = write_frame(&mut stream, &bytes) {
                eprintln!("kernel: write error: {error}");
                return;
            }
        }
    }

    fn dispatch(state: &KernelState, req: &Request) -> Response {
        if req.path.is_empty() {
            Response::bad_request(req.id, "kernel request requires a path")
        } else {
            state.handle(req)
        }
    }

    fn bind_socket(sock: &Path) -> io::Result<UnixListener> {
        if let Some(parent) = sock.parent() {
            std::fs::create_dir_all(parent)?;
            // Owner-only dir on Unix; on Windows the profile dir's NTFS ACL already
            // restricts the AF_UNIX socket to the user (no `from_mode` there).
            #[cfg(unix)]
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        if sock.exists() {
            if UnixStream::connect(sock).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "kernel daemon is already listening",
                ));
            }
            // Not a live socket → a stale file from a crash. On Unix, refuse to clobber
            // a non-socket path; then remove the stale socket and rebind (both platforms).
            #[cfg(unix)]
            {
                let meta = std::fs::symlink_metadata(sock)?;
                if !meta.file_type().is_socket() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "kernel socket path exists and is not a socket",
                    ));
                }
            }
            std::fs::remove_file(sock)?;
        }
        let listener = UnixListener::bind(sock)?;
        #[cfg(unix)]
        std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    // ── Built-in CLI client (talks to a running daemon via the shared client) ──

    fn run_client(req: Request) -> ExitCode {
        match client::send(&req) {
            Ok(resp) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".into())
                );
                if resp.ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                eprintln!(
                    "kernel client: {error} (is the daemon running? `concierge-kernel serve`)"
                );
                ExitCode::FAILURE
            }
        }
    }

    // These exercise Unix-specific socket permissions/file-type checks; the Windows
    // AF_UNIX path is verified by hand on a Windows build (see KERNEL_BUILD_LOG).
    #[cfg(all(test, unix))]
    mod tests {
        use super::{bind_socket, KernelLock};
        use std::io::ErrorKind;
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        use std::path::Path;
        use std::sync::Mutex;

        static ENV_LOCK: Mutex<()> = Mutex::new(());

        fn with_workdir<R>(workdir: &Path, f: impl FnOnce() -> R) -> R {
            let _guard = ENV_LOCK.lock().unwrap();
            let old = std::env::var_os("CONCIERGE_WORKDIR");
            std::env::set_var("CONCIERGE_WORKDIR", workdir);
            let result = f();
            match old {
                Some(value) => std::env::set_var("CONCIERGE_WORKDIR", value),
                None => std::env::remove_var("CONCIERGE_WORKDIR"),
            }
            result
        }

        #[test]
        fn bind_socket_refuses_live_duplicate() {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join(".concierge/kernel.sock");
            let _listener = bind_socket(&sock).unwrap();
            let err = bind_socket(&sock).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::AddrInUse);
        }

        #[test]
        fn bind_socket_replaces_stale_socket_and_hardens_permissions() {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join(".concierge/kernel.sock");
            {
                let parent = sock.parent().unwrap();
                std::fs::create_dir_all(parent).unwrap();
                let _stale = UnixListener::bind(&sock).unwrap();
            }
            let _listener = bind_socket(&sock).unwrap();
            let parent_mode = std::fs::metadata(sock.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let socket_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
            assert_eq!(parent_mode, 0o700);
            assert_eq!(socket_mode, 0o600);
        }

        #[test]
        fn kernel_lockfile_records_pid_socket_and_cleans_up() {
            let dir = tempfile::tempdir().unwrap();
            with_workdir(dir.path(), || {
                let sock = dir.path().join(".concierge/kernel.sock");
                let lock = KernelLock::write(&sock).unwrap();
                let path = concierge_kernel::lockfile_path();
                assert_eq!(lock.path(), path.as_path());
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
                let value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert_eq!(value["pid"].as_u64(), Some(std::process::id() as u64));
                assert_eq!(value["socket"].as_str(), Some(sock.to_str().unwrap()));
                drop(lock);
                assert!(!path.exists());
            });
        }
    }
}
