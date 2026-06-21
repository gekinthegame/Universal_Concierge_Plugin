//! `concierge-kernel-client` — the kernel's wire protocol, framing transport, the
//! client every consumer (GUI host, CLI, MCP) uses to talk to a running kernel, and
//! the workdir/socket/lockfile paths both the daemon and its clients resolve.
//!
//! This crate is deliberately dependency-light (std + serde/serde_json/url/libc, **no**
//! `concierge-core`), so lightweight consumers — the MCP server and the CLI — can link
//! the client without pulling in the daemon's heavy deps (libp2p, tokio, the adapters).
//! The `concierge-kernel` crate builds its daemon on top of this and re-exports it, so
//! existing `concierge_kernel::{protocol, transport, client, socket_path, …}` paths keep
//! resolving unchanged.

#[cfg(any(unix, windows))]
pub mod client;
pub mod protocol;
pub mod transport;

use std::path::PathBuf;

/// The store and socket live under `<workdir>/.concierge`. `workdir` mirrors the
/// CLI and kernel: `CONCIERGE_WORKDIR`, else `HOME`, else the current directory.
pub fn workdir() -> PathBuf {
    if let Ok(val) = std::env::var("CONCIERGE_WORKDIR") {
        return PathBuf::from(val);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The kernel's Unix domain socket path.
pub fn socket_path() -> PathBuf {
    workdir().join(".concierge").join("kernel.sock")
}

/// The kernel daemon lockfile path. The daemon writes JSON with `{pid, socket}`
/// here after binding its socket so supervisors have a stable lifecycle marker.
pub fn lockfile_path() -> PathBuf {
    workdir().join(".concierge").join("kernel.lock")
}
