//! Phase 5.7 transport — libp2p gossipsub rooms with NAT traversal.
//!
//! Moves the *same* signed `Message` nodes the local conversation plane already
//! builds (Decision 0008) between installs. A room is a gossipsub topic
//! (`concierge/room/<room>`); a node publishes a signed message envelope and
//! peers receive + verify it. The **PeerID is derived from the same Ed25519 key
//! as the AgentID** — identity, transport auth, and addressing collapse into one
//! key (Decision 0007).
//!
//! Every node carries the NAT-traversal client stack so peers behind home routers
//! can still connect: **gossipsub** + **relay client** (use a relay) + **DCUtR**
//! (hole-punch a relayed connection up to a direct one) + **identify** (exchange
//! observed addresses). Relay-server hosting is explicit opt-in because it
//! forwards third-party traffic. Mirrors universal-connectivity's `rust-peer`.
//!
//! **Deferred** (the rest of the transport): QUIC/WebRTC/WebTransport transports,
//! AutoNAT reachability detection, request-response fetch-on-demand for large
//! blocks, store-and-forward, and rendezvous discovery for global `topic:` rooms.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    autonat, connection_limits, dcutr, gossipsub, identify, identity, kad, mdns, noise, relay,
    rendezvous, tcp, yamux, PeerId, StreamProtocol, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

/// Re-exported so callers can parse/construct addresses without depending on
/// `libp2p` directly (e.g. the CLI's `--relay`/`--dial` flags).
pub use libp2p::Multiaddr;
/// Re-exported so callers can name the peer a request targets.
pub use libp2p::PeerId as Peer;

mod sync;
mod transport;

pub use sync::{
    relay_provider, store_provider, sync_from_peer, SyncProvider, SyncRequest, SyncResponse,
    SYNC_PROTOCOL,
};
pub use transport::{
    content_message_id, ed25519_hex_from_peer_id, peer_id_from_ed25519_hex, peer_id_in,
    public_dht_announcements_enabled, ConciergeNode, NodeConfig, NodeEvent,
    CONCIERGE_PROTOCOL_VERSION, PRIVATE_NAMESPACE_PREFIX, ROOM_PREFIX,
};

#[cfg(test)]
use transport::{
    build_gossipsub_config, build_swarm, relay_server_config, RELAY_MAX_CIRCUITS,
    RELAY_MAX_CIRCUITS_PER_PEER, RELAY_MAX_CIRCUIT_BYTES, RELAY_MAX_CIRCUIT_DURATION,
    RELAY_MAX_RESERVATIONS, RELAY_MAX_RESERVATIONS_PER_PEER,
};

#[cfg(test)]
include!("tests.rs");
