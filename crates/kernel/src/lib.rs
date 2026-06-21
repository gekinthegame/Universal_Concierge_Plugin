//! `concierge-kernel` library — re-exports the kernel client library so existing
//! `concierge_kernel::{protocol, transport, client, socket_path, workdir, lockfile_path}`
//! paths keep resolving. The wire protocol, framing, the client, and the path helpers
//! now live in the dependency-light `concierge-kernel-client` crate, so lightweight
//! consumers (MCP, CLI) can link the client without pulling in this crate's daemon
//! dependencies (libp2p, tokio, the capture adapters). The `concierge-kernel` *binary*
//! builds the daemon (state + serve loop) on top of these.

pub use concierge_kernel_client::{lockfile_path, protocol, socket_path, transport, workdir};

#[cfg(any(unix, windows))]
pub use concierge_kernel_client::client;
