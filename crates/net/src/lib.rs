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

/// The Phase N · Phase D sync protocol, spoken over libp2p request-response.
pub const SYNC_PROTOCOL: &str = "/concierge/sync/1.0.0";

/// A request on the [`SYNC_PROTOCOL`]: fetch one block by CID, or a namespace's
/// signed head record. Bounded, authenticated, point-to-point — never a broad
/// graph traversal (plan §Request-Response Requirements).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncRequest {
    /// Fetch the raw bytes of one content-addressed block.
    GetBlock(String),
    /// Fetch the signed [`HeadRecord`](concierge_core::HeadRecord) JSON for a
    /// canonical namespace.
    GetHeads(String),
    /// **Store-and-forward push**: hand a content-addressed block to a relay so it
    /// can hold it for an offline peer. The relay CID-verifies before storing and
    /// stores only **inert ciphertext** (it cannot decrypt). `(cid, bytes)`.
    PutBlock(String, Vec<u8>),
    /// **Direct message**: hand an opaque, app-signed message-envelope to a peer
    /// over this concierge-only point-to-point protocol — *not* a public gossipsub
    /// topic anyone could read or inject into. The transport never inspects or
    /// verifies the payload; the receiving application verifies the envelope's
    /// signature. The bytes are an opaque envelope (e.g. signed JSON).
    Deliver(Vec<u8>),
}

/// The response to a [`SyncRequest`]. `None` means "I don't have it / I won't serve
/// it" — a deterministic answer that does not reveal what else exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncResponse {
    Block(Option<Vec<u8>>),
    Heads(Option<Vec<u8>>),
    /// Whether a pushed block was accepted (CID verified, within limits).
    Stored(bool),
    /// Whether the receiving application accepted or durably queued a direct
    /// message after authenticating and applying its consent policy.
    Delivered(bool),
}

/// The serve side: answers inbound [`SyncRequest`]s from the local store. The
/// application supplies this (it holds the `MemCli` + the capability checks); the
/// transport just moves the bytes. A node with no provider serves nothing.
pub type SyncProvider = Arc<dyn Fn(SyncRequest) -> SyncResponse + Send + Sync>;

/// The largest single block a store-and-forward relay will accept on a push.
pub const MAX_PUSH_BLOCK_BYTES: usize = 16 * 1024 * 1024;
const MAX_PENDING_DIRECT_MESSAGES: usize = 1024;

fn noop_provider() -> SyncProvider {
    Arc::new(|request| match request {
        SyncRequest::GetBlock(_) => SyncResponse::Block(None),
        SyncRequest::GetHeads(_) => SyncResponse::Heads(None),
        SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// Build a serve-side [`SyncProvider`] backed by a real local store: it answers
/// `GetBlock` from the content-addressed block store. (Head serving is left to the
/// application, which maintains the signed [`HeadRecord`](concierge_core::HeadRecord)
/// for each namespace it writes.) Sensitive content is inert ciphertext at rest, and
/// which CIDs a peer learns to ask for is governed by the Phase C/D head/manifest
/// gating — so serving a known CID's bytes is safe defense-in-depth.
pub fn store_provider(mem: Arc<concierge_core::MemCli>) -> SyncProvider {
    Arc::new(move |request| match request {
        SyncRequest::GetBlock(cid) => SyncResponse::Block(mem.block_bytes(&cid)),
        SyncRequest::GetHeads(namespace) => {
            // Serve the signed head record this device maintains for the namespace.
            let head = concierge_core::Namespace::parse(&namespace)
                .and_then(|ns| mem.stored_head(&ns.network_id, &namespace).ok().flatten())
                .and_then(|record| serde_json::to_vec(&record).ok());
            SyncResponse::Heads(head)
        }
        // A plain store does not accept pushes (use `relay_provider` for that).
        SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// A **store-and-forward relay** provider: like [`store_provider`] but it also
/// *accepts* pushed blocks, holding them for offline peers. A pushed block is
/// **CID-verified before storage** (a wrong-CID block is refused) and bounded by
/// [`MAX_PUSH_BLOCK_BYTES`]. The relay stores only inert ciphertext — it cannot
/// decrypt and is not a membership authority (plan §Offline and Store-and-Forward).
pub fn relay_provider(mem: Arc<concierge_core::MemCli>) -> SyncProvider {
    Arc::new(move |request| match request {
        SyncRequest::GetBlock(cid) => SyncResponse::Block(mem.block_bytes(&cid)),
        SyncRequest::GetHeads(namespace) => {
            let head = concierge_core::Namespace::parse(&namespace)
                .and_then(|ns| mem.stored_head(&ns.network_id, &namespace).ok().flatten())
                .and_then(|record| serde_json::to_vec(&record).ok());
            SyncResponse::Heads(head)
        }
        SyncRequest::PutBlock(cid, bytes) => {
            if bytes.len() > MAX_PUSH_BLOCK_BYTES {
                return SyncResponse::Stored(false);
            }
            // import_verified_block CID-verifies; a forged cid/bytes pair is rejected.
            SyncResponse::Stored(mem.import_verified_block(&cid, &bytes).is_ok())
        }
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// What a pending outbound request was asking for, so its response can be labeled.
#[derive(Debug)]
enum Pending {
    Block {
        cid: String,
        reply: Option<oneshot::Sender<Option<Vec<u8>>>>,
    },
    Heads {
        namespace: String,
        reply: Option<oneshot::Sender<Option<Vec<u8>>>>,
    },
    Push(String),
}

/// **The end-to-end sync driver (Phase F)**: pull a namespace from `peer` to
/// convergence over the live connection. It runs the full Phase D loop —
/// exchange-heads → verify → reconcile → fetch only the missing, CID-verified
/// blocks (walking the graph down as they arrive) → adopt the converged heads —
/// and returns a [`SyncReceipt`]. Bounded by `overall_timeout`. Requests are serial
/// (one in flight) for simple response correlation; a hostile/absent peer just
/// times out. The caller supplies the verified `descriptor` + the current
/// `revoked` set so a stale, unauthorized, or revoked head is rejected.
#[allow(clippy::too_many_arguments)]
pub async fn sync_from_peer(
    node: &ConciergeNode,
    _events: &mut mpsc::UnboundedReceiver<NodeEvent>,
    peer: PeerId,
    mem: &concierge_core::MemCli,
    descriptor: &concierge_core::NetworkDescriptor,
    namespace: &concierge_core::Namespace,
    revoked: &concierge_core::RevocationSet,
    overall_timeout: std::time::Duration,
) -> Result<concierge_core::SyncReceipt, String> {
    use concierge_core::{reconcile, verify_head_record, HeadRecord};
    use tokio::time::Instant;

    let deadline = Instant::now() + overall_timeout;
    let canonical = namespace.canonical();

    // 1. Exchange heads — fetch and verify the peer's signed advertisement.
    let head_bytes = fetch_head(node, peer, &canonical, deadline)
        .await
        .ok_or_else(|| "peer did not return a head record".to_string())?;
    let remote: HeadRecord =
        serde_json::from_slice(&head_bytes).map_err(|e| format!("malformed head record: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    verify_head_record(&remote, descriptor, now, revoked)
        .map_err(|e| format!("head record rejected: {e}"))?;

    // 2. Reconcile against our local view.
    let local = mem.local_heads(&descriptor.network_id, &canonical);
    let recon = reconcile(&local, &remote, |h| mem.has_block(h));

    // 3. Fetch only the missing blocks, walking the graph down as they arrive.
    let mut imported = 0usize;
    let mut bytes_total = 0u64;
    let mut queue = recon.missing_heads.clone();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(cid) = queue.pop() {
        if !seen.insert(cid.clone()) {
            continue;
        }
        if !mem.has_block(&cid) {
            let bytes = fetch_block(node, peer, &cid, deadline)
                .await
                .ok_or_else(|| format!("could not fetch block {cid}"))?;
            // CID-verified before durable import; a tampered block aborts the sync.
            mem.import_verified_block(&cid, &bytes)
                .map_err(|e| format!("import {cid}: {e}"))?;
            imported += 1;
            bytes_total += bytes.len() as u64;
        }
        // Follow the block's immediate links (present now that it is imported).
        if let Ok(links) = mem.block_links(&concierge_core::Cid(cid.clone())) {
            for link in links {
                if !seen.contains(&link.0) {
                    queue.push(link.0);
                }
            }
        }
    }

    // 4. Durable commit done — adopt the converged head set.
    mem.set_local_heads(&descriptor.network_id, &canonical, &recon.converged_heads)
        .map_err(|error| format!("commit converged heads: {error}"))?;
    Ok(concierge_core::SyncReceipt {
        network_id: descriptor.network_id.0.clone(),
        namespace: canonical,
        peer: peer.to_string(),
        blocks_imported: imported,
        bytes: bytes_total,
        heads: recon.converged_heads,
        at: now,
    })
}

/// Request a namespace's head record from `peer`, retrying until the connection is
/// up or the deadline passes.
async fn fetch_head(
    node: &ConciergeNode,
    peer: PeerId,
    namespace: &str,
    deadline: tokio::time::Instant,
) -> Option<Vec<u8>> {
    use tokio::time::Instant;
    while Instant::now() < deadline {
        let slice = (deadline - Instant::now()).min(std::time::Duration::from_millis(700));
        match tokio::time::timeout(slice, node.request_heads_response(peer, namespace)).await {
            Ok(Ok(Some(bytes))) => return Some(bytes),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await
            }
        }
    }
    None
}

/// Request one block from `peer`, retrying until served or the deadline passes.
async fn fetch_block(
    node: &ConciergeNode,
    peer: PeerId,
    cid: &str,
    deadline: tokio::time::Instant,
) -> Option<Vec<u8>> {
    use tokio::time::Instant;
    while Instant::now() < deadline {
        let slice = (deadline - Instant::now()).min(std::time::Duration::from_millis(700));
        match tokio::time::timeout(slice, node.request_block_response(peer, cid)).await {
            Ok(Ok(Some(bytes))) => return Some(bytes),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await
            }
        }
    }
    None
}

/// Topic prefix so Concierge rooms don't collide with other gossipsub apps.
pub const ROOM_PREFIX: &str = "concierge/room/";
pub const PRIVATE_NAMESPACE_PREFIX: &str = "concierge/private/";
const RELAY_MAX_RESERVATIONS: usize = 32;
const RELAY_MAX_RESERVATIONS_PER_PEER: usize = 2;
const RELAY_MAX_CIRCUITS: usize = 32;
const RELAY_MAX_CIRCUITS_PER_PEER: usize = 2;
const RELAY_MAX_CIRCUIT_BYTES: u64 = 16 * 1024 * 1024;
const RELAY_MAX_CIRCUIT_DURATION: Duration = Duration::from_secs(30 * 60);

/// How often the Kademlia client re-runs its bootstrap query to refresh the
/// routing table (mirrors universal-connectivity's `rust-peer` ~300s cadence).
const KADEMLIA_BOOTSTRAP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The public IPFS bootstrap nodes — the well-known DHT entry points that let a
/// fresh node populate its routing table and become reachable/route-able from
/// anywhere. Same `/dnsaddr/bootstrap.libp2p.io/p2p/Qm…` set the `rust-peer` uses.
const IPFS_BOOTSTRAP_NODES: [&str; 4] = [
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
];

/// Parse the [`IPFS_BOOTSTRAP_NODES`] strings into multiaddrs, skipping any that
/// fail to parse (the default for [`NodeConfig::bootstrap`]).
fn default_bootstrap() -> Vec<Multiaddr> {
    IPFS_BOOTSTRAP_NODES
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .collect()
}

fn topic(room: &str) -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(format!("{ROOM_PREFIX}{room}"))
}

fn private_topic(namespace: &str) -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(format!("{PRIVATE_NAMESPACE_PREFIX}{namespace}"))
}

/// Conservative defaults for ordinary nodes and opt-in community relays.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Explicitly enable Circuit Relay v2 server behaviour.
    pub host_relay: bool,
    /// Addresses confirmed by the operator as externally reachable.
    pub external_addresses: Vec<Multiaddr>,
    /// Maximum signed gossipsub message size.
    pub max_message_bytes: usize,
    /// Maximum total established libp2p connections.
    pub max_established_connections: u32,
    /// Maximum established connections from one peer.
    pub max_connections_per_peer: u32,
    /// Duration granted by an opt-in relay server before client renewal.
    pub relay_reservation_duration: Duration,
    /// Restrict this node to explicitly allowlisted authenticated peers and
    /// private namespace topics. There is no discovery or public DHT behaviour.
    pub private_swarm: bool,
    /// PeerIDs allowed to connect while `private_swarm` is enabled.
    pub allowed_private_peers: HashSet<PeerId>,
    /// Run a **rendezvous point** so members can register + discover each other
    /// without manual address exchange (opt-in, like relay hosting).
    pub rendezvous_point: bool,
    /// Enable the **Kademlia DHT client**: bootstrap to the public IPFS nodes and
    /// route to peers by PeerID from anywhere on the internet (see
    /// [`ConciergeNode::find_peer`]). Forced off under `private_swarm`.
    pub dht: bool,
    /// Enable **mDNS** local-network discovery: find + auto-dial peers on the same
    /// LAN with no manual address exchange. Forced off under `private_swarm`.
    pub mdns: bool,
    /// DHT bootstrap multiaddrs (each `…/p2p/<peer-id>`). Defaults to the public
    /// IPFS bootstrap nodes; unparsable entries are skipped.
    pub bootstrap: Vec<Multiaddr>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            host_relay: false,
            external_addresses: Vec::new(),
            max_message_bytes: 64 * 1024,
            max_established_connections: 128,
            max_connections_per_peer: 4,
            relay_reservation_duration: Duration::from_secs(60 * 60),
            private_swarm: false,
            allowed_private_peers: HashSet::new(),
            rendezvous_point: false,
            // Discovery (mDNS LAN + public DHT) is on in production. In this crate's
            // own test build it is OFF so concurrent test nodes don't mDNS-discover
            // and cross-connect on the host (which makes the multi-node integration
            // tests flaky); those tests dial explicitly.
            dht: !cfg!(test),
            mdns: !cfg!(test),
            bootstrap: default_bootstrap(),
        }
    }
}

/// Stable content-derived gossipsub message ID. Re-publishing the same signed
/// envelope is deduplicated even though gossipsub would otherwise assign a new
/// source/sequence ID on every publish.
pub fn content_message_id(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The composed behaviour: gossipsub for rooms + the NAT-traversal stack.
#[derive(NetworkBehaviour)]
struct Behaviour {
    gossipsub: gossipsub::Behaviour,
    /// Use a relay (dial peers through it, reserve a slot on it).
    relay_client: relay::client::Behaviour,
    /// Be a relay for others only when the operator explicitly opts in.
    relay: Toggle<relay::Behaviour>,
    /// Hole-punch a relayed connection up to a direct one.
    dcutr: dcutr::Behaviour,
    /// Exchange observed addresses (relay/DCUtR depend on this).
    identify: identify::Behaviour,
    /// Request-response block/head sync (Phase F): the wire for the Phase D
    /// reconciliation logic over direct and relayed paths.
    sync: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    /// Assess our own NAT reachability so we know when to use a relay.
    autonat: autonat::Behaviour,
    /// Register with / discover peers at a rendezvous point (no manual addresses).
    rendezvous_client: rendezvous::client::Behaviour,
    /// Be a rendezvous point only when the operator explicitly opts in.
    rendezvous_server: Toggle<rendezvous::server::Behaviour>,
    /// Kademlia DHT client: global peer routing + bootstrap into the public IPFS
    /// network so a node is reachable by PeerID from anywhere (disabled when off
    /// or in private-swarm mode).
    kademlia: Toggle<kad::Behaviour<kad::store::MemoryStore>>,
    /// mDNS local discovery: auto-find peers on the same LAN (disabled when off or
    /// in private-swarm mode).
    mdns: Toggle<mdns::tokio::Behaviour>,
    /// Bound connections before an internet-facing peer can exhaust the process.
    connection_limits: connection_limits::Behaviour,
}

/// Something the node observed. Operational success/failure is part of the API
/// so callers never need to infer NAT reachability from a queued command.
#[derive(Debug, Clone)]
pub enum NodeEvent {
    /// A dialable listen address (with `/p2p/<peer-id>` appended). For a relay
    /// reservation this is a `…/p2p-circuit/p2p/<peer-id>` address.
    Listening(String),
    /// A message received on a subscribed room.
    Message {
        room: String,
        data: Vec<u8>,
        source: Option<String>,
    },
    Subscribed {
        room: String,
    },
    Published {
        room: String,
        message_id: String,
        duplicate: bool,
    },
    ConnectionEstablished {
        peer_id: String,
        relayed: bool,
    },
    ExternalAddressAdded {
        address: String,
    },
    RelayReservationAccepted {
        relay_peer_id: String,
        renewed: bool,
    },
    RelayCircuitEstablished {
        peer_id: String,
        direction: &'static str,
    },
    DirectConnectionUpgrade {
        peer_id: String,
        succeeded: bool,
        error: Option<String>,
    },
    /// A peer's reply to our [`SyncRequest::GetBlock`] — `bytes` is `None` if the
    /// peer doesn't have it or won't serve it. The application CID-verifies before
    /// importing (the transport does not).
    BlockReceived {
        cid: String,
        bytes: Option<Vec<u8>>,
    },
    /// A peer's reply to our [`SyncRequest::GetHeads`] — serialized signed
    /// [`HeadRecord`](concierge_core::HeadRecord) JSON, which the application
    /// verifies before trusting.
    HeadsReceived {
        namespace: String,
        heads: Option<Vec<u8>>,
    },
    /// A store-and-forward relay's reply to our [`SyncRequest::PutBlock`] — whether
    /// it accepted (CID-verified, within limits) and now holds the block.
    BlockStored {
        cid: String,
        ok: bool,
    },
    /// A peer delivered an opaque, app-signed message-envelope to us over the
    /// concierge-only point-to-point protocol (a [`SyncRequest::Deliver`]) — *not*
    /// a public gossipsub topic. `from_peer` is the sending peer's PeerID;
    /// `data` is the opaque envelope. The application verifies the envelope's
    /// signature before trusting it; the transport does not.
    DirectMessage {
        from_peer: String,
        data: Vec<u8>,
        delivery_id: u64,
    },
    /// A direct message we sent was acknowledged as received by `to_peer`. The
    /// `message_id` is `content_message_id(data)` of the data we sent, so the
    /// sender can match it to its outbox entry and stop retrying.
    DirectMessageDelivered {
        to_peer: String,
        message_id: String,
    },
    /// We successfully registered under `namespace` at a rendezvous point.
    RendezvousRegistered {
        namespace: String,
    },
    /// A peer discovered via a rendezvous point (its id + dialable addresses).
    RendezvousDiscovered {
        peer_id: String,
        addresses: Vec<String>,
    },
    /// A peer we asked to route to (via [`ConciergeNode::find_peer`]) was located
    /// on the DHT — its currently-known dialable addresses. The node auto-dials
    /// them; this event is informational (UI "found peer / connecting").
    PeerRouted {
        peer_id: String,
        addresses: Vec<String>,
    },
    /// Our assessed NAT reachability changed (`public` / `private` / `unknown`).
    /// `private` is the signal to reserve a relay.
    NatStatus {
        reachability: &'static str,
    },
    OperationFailed {
        operation: &'static str,
        error: String,
    },
}

enum Command {
    Subscribe(String),
    Publish(String, Vec<u8>),
    SubscribePrivate(String),
    PublishPrivate(String, Vec<u8>),
    Dial(Multiaddr),
    Listen(Multiaddr),
    /// Reserve a slot on a relay and listen on its circuit (NAT traversal).
    Reserve(Multiaddr),
    AddExternalAddress(Multiaddr),
    /// Ask a peer for one block by CID (Phase F sync).
    RequestBlock(PeerId, String, Option<oneshot::Sender<Option<Vec<u8>>>>),
    /// Ask a peer for a namespace's signed head record (Phase F sync).
    RequestHeads(PeerId, String, Option<oneshot::Sender<Option<Vec<u8>>>>),
    /// Push a block to a store-and-forward relay (Phase F).
    PushBlock(PeerId, String, Vec<u8>),
    /// Register this node under a namespace at a rendezvous point.
    RegisterRendezvous(PeerId, String),
    /// Discover peers registered under a namespace at a rendezvous point.
    DiscoverRendezvous(PeerId, String),
    /// Look up a peer by id on the Kademlia DHT (global routing) and dial it.
    FindPeer(PeerId),
    /// Send an opaque, app-signed message-envelope directly to a peer over the
    /// concierge-only point-to-point protocol (not a public topic).
    SendDirect(PeerId, Vec<u8>),
    AcknowledgeDirect(u64, bool),
}

/// A handle to a running node.
#[derive(Clone)]
pub struct ConciergeNode {
    cmd: mpsc::UnboundedSender<Command>,
    pub peer_id: PeerId,
    private_swarm: bool,
}

impl ConciergeNode {
    /// Spawn a node from a 32-byte Ed25519 secret (the AgentID key). Returns the
    /// handle plus a stream of [`NodeEvent`]s.
    pub fn spawn(
        secret: [u8; 32],
    ) -> Result<(ConciergeNode, mpsc::UnboundedReceiver<NodeEvent>), String> {
        Self::spawn_with_config(secret, NodeConfig::default())
    }

    pub fn spawn_with_config(
        secret: [u8; 32],
        config: NodeConfig,
    ) -> Result<(ConciergeNode, mpsc::UnboundedReceiver<NodeEvent>), String> {
        Self::spawn_with_provider(secret, config, noop_provider())
    }

    /// Spawn with a **serve side**: `provider` answers inbound block/head requests
    /// from the local store (the application supplies the capability checks). The
    /// node still requests from peers via [`ConciergeNode::request_block`] etc.
    pub fn spawn_with_provider(
        secret: [u8; 32],
        config: NodeConfig,
        provider: SyncProvider,
    ) -> Result<(ConciergeNode, mpsc::UnboundedReceiver<NodeEvent>), String> {
        let mut secret = secret;
        let keypair =
            identity::Keypair::ed25519_from_bytes(&mut secret).map_err(|e| e.to_string())?;
        let peer_id = keypair.public().to_peer_id();
        let swarm = build_swarm(keypair, &config)?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(
            swarm,
            peer_id,
            cmd_rx,
            evt_tx,
            config.private_swarm,
            config.allowed_private_peers.clone(),
            provider,
        ));
        Ok((
            ConciergeNode {
                cmd: cmd_tx,
                peer_id,
                private_swarm: config.private_swarm,
            },
            evt_rx,
        ))
    }

    /// Ask `peer` for one block by CID. The reply arrives as
    /// [`NodeEvent::BlockReceived`]; the caller CID-verifies before importing.
    pub fn request_block(&self, peer: PeerId, cid: &str) -> Result<(), String> {
        self.send(Command::RequestBlock(peer, cid.to_string(), None))
    }

    /// Ask `peer` for a namespace's signed head record (arrives as
    /// [`NodeEvent::HeadsReceived`]).
    pub fn request_heads(&self, peer: PeerId, namespace: &str) -> Result<(), String> {
        self.send(Command::RequestHeads(peer, namespace.to_string(), None))
    }

    pub async fn request_block_response(
        &self,
        peer: PeerId,
        cid: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::RequestBlock(peer, cid.to_string(), Some(tx)))?;
        rx.await
            .map_err(|_| "network node stopped before returning the block".to_string())
    }

    pub async fn request_heads_response(
        &self,
        peer: PeerId,
        namespace: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::RequestHeads(peer, namespace.to_string(), Some(tx)))?;
        rx.await
            .map_err(|_| "network node stopped before returning heads".to_string())
    }

    /// Push a block to a store-and-forward `relay` so it can hold it for offline
    /// peers (reply: [`NodeEvent::BlockStored`]).
    pub fn push_block(&self, relay: PeerId, cid: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.send(Command::PushBlock(relay, cid.to_string(), bytes))
    }

    /// Register this node under `namespace` at a rendezvous `point` (so peers can
    /// find it without a manually-exchanged address). Reply:
    /// [`NodeEvent::RendezvousRegistered`].
    pub fn register_rendezvous(&self, point: PeerId, namespace: &str) -> Result<(), String> {
        self.send(Command::RegisterRendezvous(point, namespace.to_string()))
    }

    /// Discover peers registered under `namespace` at a rendezvous `point` (each
    /// arrives as [`NodeEvent::RendezvousDiscovered`]).
    pub fn discover_rendezvous(&self, point: PeerId, namespace: &str) -> Result<(), String> {
        self.send(Command::DiscoverRendezvous(point, namespace.to_string()))
    }

    /// Look up a peer by id on the Kademlia DHT (global routing). When the DHT
    /// returns addresses the node emits [`NodeEvent::PeerRouted`] and dials them.
    pub fn find_peer(&self, peer: PeerId) -> Result<(), String> {
        self.send(Command::FindPeer(peer))
    }

    /// Send an opaque, app-signed message-envelope directly to `peer` over the
    /// concierge-only point-to-point protocol — a private channel, *not* a public
    /// gossipsub topic anyone could read or inject into. The peer surfaces it as
    /// [`NodeEvent::DirectMessage`]; the receiving application verifies the
    /// envelope's signature (the transport moves opaque bytes and does not). The
    /// peer's address must already be known (it is, for an mDNS- or DHT-discovered
    /// peer — see the discovery handlers), otherwise the underlying request cannot
    /// dial and the send fails as an outbound failure.
    pub fn send_dm(&self, peer: PeerId, data: Vec<u8>) -> Result<(), String> {
        self.send(Command::SendDirect(peer, data))
    }

    pub fn acknowledge_dm(&self, delivery_id: u64, accepted: bool) -> Result<(), String> {
        self.send(Command::AcknowledgeDirect(delivery_id, accepted))
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.cmd
            .send(command)
            .map_err(|_| "network node is no longer running".to_string())
    }

    pub fn listen(&self, addr: Multiaddr) -> Result<(), String> {
        self.send(Command::Listen(addr))
    }
    pub fn dial(&self, addr: Multiaddr) -> Result<(), String> {
        self.send(Command::Dial(addr))
    }
    /// Reserve a slot on a relay (its plain `/p2p/<relay>` multiaddr) and listen
    /// on the circuit — so peers behind NAT become reachable through the relay.
    pub fn reserve(&self, relay_addr: Multiaddr) -> Result<(), String> {
        self.send(Command::Reserve(relay_addr))
    }
    pub fn add_external_address(&self, addr: Multiaddr) -> Result<(), String> {
        self.send(Command::AddExternalAddress(addr))
    }
    pub fn subscribe(&self, room: &str) -> Result<(), String> {
        if self.private_swarm {
            return Err("public room subscriptions are disabled in private-swarm mode".to_string());
        }
        self.send(Command::Subscribe(room.to_string()))
    }
    /// Publish raw bytes (e.g. a signed message envelope JSON) to a room.
    pub fn publish(&self, room: &str, data: Vec<u8>) -> Result<(), String> {
        if self.private_swarm {
            return Err("public room publishing is disabled in private-swarm mode".to_string());
        }
        self.send(Command::Publish(room.to_string(), data))
    }

    pub fn subscribe_private(&self, namespace: &str) -> Result<(), String> {
        if !self.private_swarm {
            return Err("private namespace subscriptions require private-swarm mode".to_string());
        }
        self.send(Command::SubscribePrivate(namespace.to_string()))
    }

    pub fn publish_private(&self, namespace: &str, ciphertext: Vec<u8>) -> Result<(), String> {
        if !self.private_swarm {
            return Err("private ciphertext publishing requires private-swarm mode".to_string());
        }
        self.send(Command::PublishPrivate(namespace.to_string(), ciphertext))
    }
}

/// This transport does not *announce* (provide records on) the public DHT — it
/// only uses Kademlia as a client for peer routing. There is no public-DHT
/// advertisement of what content this node holds.
pub const fn public_dht_announcements_enabled() -> bool {
    false
}

/// Derive the libp2p PeerID from a hex-encoded Ed25519 public key (the
/// concierge AgentID / "username"). The node's own key is the same Ed25519
/// key, so a username resolves to exactly that node's PeerID.
pub fn peer_id_from_ed25519_hex(hex: &str) -> Option<PeerId> {
    // Decode the 32-byte compressed Ed25519 public key.
    let bytes = decode_hex(hex)?;
    let public = identity::ed25519::PublicKey::try_from_bytes(&bytes).ok()?;
    let public: identity::PublicKey = public.into();
    Some(public.to_peer_id())
}

/// Decode an even-length hex string into bytes (no external dependency).
fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Extract the `/p2p/<peer-id>` component of a multiaddr, if present — so a caller
/// that dials `…/p2p/<id>` can name the peer it just connected to (for sync).
pub fn peer_id_in(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn build_swarm(
    keypair: identity::Keypair,
    config: &NodeConfig,
) -> Result<libp2p::Swarm<Behaviour>, String> {
    let behaviour_config = config.clone();
    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| e.to_string())?
        // Also run QUIC (UDP). Two concierge nodes on the *same host* discover each
        // other over mDNS but their TCP dials collide on "Address already in use
        // (os error 48)" — the libp2p single-host TCP port-reuse bug. Carrying QUIC
        // as a second transport gives those dials a UDP path that doesn't collide
        // (this is exactly why the chat / universal-connectivity examples run QUIC).
        // `with_quic()` is infallible and sits between `with_tcp` and
        // `with_relay_client` in the 0.56 builder type-state. The caller adds a
        // `/ip4/0.0.0.0/udp/0/quic-v1` listener via `ConciergeNode::listen`.
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|e| e.to_string())?
        .with_behaviour(move |key, relay_client| {
            let gossipsub_config = build_gossipsub_config(&behaviour_config)
                .map_err(|e| -> Box<dyn Error + Send + Sync> { e.into() })?;
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )
            .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() })?;
            let relay_server = behaviour_config.host_relay.then(|| {
                let relay_config = relay_server_config(&behaviour_config);
                relay::Behaviour::new(key.public().to_peer_id(), relay_config)
            });
            let limits = connection_limits::ConnectionLimits::default()
                .with_max_pending_incoming(Some(32))
                .with_max_pending_outgoing(Some(32))
                .with_max_established_incoming(Some(64))
                .with_max_established(Some(behaviour_config.max_established_connections))
                .with_max_established_per_peer(Some(behaviour_config.max_connections_per_peer));
            let local_peer_id = key.public().to_peer_id();
            // Discovery is global-internet-facing, so it is suppressed entirely in
            // private-swarm mode (allowlisted peers + manual addresses only).
            let discovery = !behaviour_config.private_swarm;
            // Kademlia DHT client: bootstrap into the public IPFS network so this
            // node can be routed to by PeerID from anywhere.
            let kademlia = (discovery && behaviour_config.dht).then(|| {
                let mut cfg = kad::Config::default();
                cfg.set_periodic_bootstrap_interval(Some(KADEMLIA_BOOTSTRAP_INTERVAL));
                let store = kad::store::MemoryStore::new(local_peer_id);
                let mut kademlia = kad::Behaviour::with_config(local_peer_id, store, cfg);
                // Seed the routing table with the bootstrap peers' addresses.
                for addr in &behaviour_config.bootstrap {
                    if let Some(peer) = peer_id_in(addr) {
                        kademlia.add_address(&peer, addr.clone());
                    }
                }
                kademlia
            });
            // mDNS local-network discovery.
            let mdns = (discovery && behaviour_config.mdns)
                .then(|| mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id))
                .transpose()
                .map_err(|e| -> Box<dyn Error + Send + Sync> { e.into() })?;
            Ok(Behaviour {
                gossipsub,
                relay_client,
                relay: relay_server.into(),
                dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/concierge/1.0.0".to_string(),
                    key.public(),
                )),
                sync: request_response::cbor::Behaviour::new(
                    [(StreamProtocol::new(SYNC_PROTOCOL), ProtocolSupport::Full)],
                    request_response::Config::default(),
                ),
                autonat: autonat::Behaviour::new(
                    key.public().to_peer_id(),
                    autonat::Config::default(),
                ),
                rendezvous_client: rendezvous::client::Behaviour::new(key.clone()),
                rendezvous_server: behaviour_config
                    .rendezvous_point
                    .then(|| {
                        rendezvous::server::Behaviour::new(rendezvous::server::Config::default())
                    })
                    .into(),
                kademlia: kademlia.into(),
                mdns: mdns.into(),
                connection_limits: connection_limits::Behaviour::new(limits),
            })
        })
        .map_err(|e| e.to_string())?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    for address in &config.external_addresses {
        swarm.add_external_address(address.clone());
    }
    Ok(swarm)
}

fn build_gossipsub_config(config: &NodeConfig) -> Result<gossipsub::Config, String> {
    gossipsub::ConfigBuilder::default()
        .validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(config.max_message_bytes)
        .message_id_fn(|message| gossipsub::MessageId::from(content_message_id(&message.data)))
        .published_message_ids_cache_time(Duration::from_secs(24 * 60 * 60))
        .duplicate_cache_time(Duration::from_secs(24 * 60 * 60))
        .build()
        .map_err(|error| error.to_string())
}

fn relay_server_config(config: &NodeConfig) -> relay::Config {
    relay::Config {
        max_reservations: RELAY_MAX_RESERVATIONS,
        max_reservations_per_peer: RELAY_MAX_RESERVATIONS_PER_PEER,
        reservation_duration: config.relay_reservation_duration,
        max_circuits: RELAY_MAX_CIRCUITS,
        max_circuits_per_peer: RELAY_MAX_CIRCUITS_PER_PEER,
        max_circuit_duration: RELAY_MAX_CIRCUIT_DURATION,
        max_circuit_bytes: RELAY_MAX_CIRCUIT_BYTES,
        ..relay::Config::default()
    }
}

fn emit(evt: &mpsc::UnboundedSender<NodeEvent>, event: NodeEvent) {
    let _ = evt.send(event);
}

fn failed(evt: &mpsc::UnboundedSender<NodeEvent>, operation: &'static str, error: impl ToString) {
    emit(
        evt,
        NodeEvent::OperationFailed {
            operation,
            error: error.to_string(),
        },
    );
}

fn is_relayed(addr: &Multiaddr) -> bool {
    addr.iter().any(|protocol| protocol == Protocol::P2pCircuit)
}

async fn run(
    mut swarm: libp2p::Swarm<Behaviour>,
    peer_id: PeerId,
    mut cmd: mpsc::UnboundedReceiver<Command>,
    evt: mpsc::UnboundedSender<NodeEvent>,
    private_swarm: bool,
    allowed_private_peers: HashSet<PeerId>,
    provider: SyncProvider,
) {
    // Correlate outbound sync requests with their responses (request-response
    // replies don't echo the request).
    let mut pending: HashMap<OutboundRequestId, Pending> = HashMap::new();
    // Outbound direct messages awaiting their `Delivered` ack, keyed by the
    // request-response id `send_request` hands back. The value is the destination
    // peer + the `content_message_id` of the data we sent, so the ack can be
    // matched back to the sender's outbox entry (see `DirectMessageDelivered`).
    let mut outbound_dms: std::collections::HashMap<
        request_response::OutboundRequestId,
        (PeerId, String),
    > = std::collections::HashMap::new();
    let mut inbound_dms: HashMap<u64, request_response::ResponseChannel<SyncResponse>> =
        HashMap::new();
    let mut next_delivery_id = 1u64;
    // PeerIDs we've been asked to route to (via `find_peer`), so a closest-peers
    // query result for one of them is recognised and dialed.
    let mut routing: HashSet<PeerId> = HashSet::new();
    // Kick off the initial DHT bootstrap (when Kademlia is enabled); the periodic
    // interval keeps the routing table fresh thereafter.
    if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
        let _ = kad.bootstrap();
    }
    loop {
        tokio::select! {
            command = cmd.recv() => match command {
                Some(Command::Listen(addr)) => {
                    if let Err(error) = swarm.listen_on(addr) {
                        failed(&evt, "listen", format!("{error:?}"));
                    }
                }
                Some(Command::Dial(addr)) => {
                    if let Err(error) = swarm.dial(addr) {
                        failed(&evt, "dial", error);
                    }
                }
                Some(Command::Reserve(relay_addr)) => {
                    if let Err(error) = swarm.listen_on(relay_addr.with(Protocol::P2pCircuit)) {
                        failed(&evt, "relay reservation", error);
                    }
                }
                Some(Command::AddExternalAddress(addr)) => {
                    swarm.add_external_address(addr.clone());
                    emit(&evt, NodeEvent::ExternalAddressAdded {
                        address: addr.to_string(),
                    });
                }
                Some(Command::RequestBlock(peer, cid, reply)) => {
                    let id = swarm.behaviour_mut().sync.send_request(&peer, SyncRequest::GetBlock(cid.clone()));
                    pending.insert(id, Pending::Block { cid, reply });
                }
                Some(Command::RequestHeads(peer, namespace, reply)) => {
                    let id = swarm.behaviour_mut().sync.send_request(&peer, SyncRequest::GetHeads(namespace.clone()));
                    pending.insert(id, Pending::Heads { namespace, reply });
                }
                Some(Command::PushBlock(peer, cid, bytes)) => {
                    let id = swarm.behaviour_mut().sync.send_request(&peer, SyncRequest::PutBlock(cid.clone(), bytes));
                    pending.insert(id, Pending::Push(cid));
                }
                Some(Command::SendDirect(peer, data)) => {
                    // Concierge-only point-to-point delivery over the sync
                    // request-response (NOT a public topic). request-response will
                    // dial `peer` using an address registered via
                    // `Swarm::add_peer_address` in the discovery handlers. The
                    // The `Delivered` ack is matched back in the response arm and
                    // surfaced as `DirectMessageDelivered` so the sender can stop
                    // retrying; we record the request id keyed to its content hash.
                    let message_id = content_message_id(&data);
                    let request_id = swarm.behaviour_mut().sync.send_request(&peer, SyncRequest::Deliver(data));
                    outbound_dms.insert(request_id, (peer, message_id));
                }
                Some(Command::AcknowledgeDirect(delivery_id, accepted)) => {
                    if let Some(channel) = inbound_dms.remove(&delivery_id) {
                        let _ = swarm
                            .behaviour_mut()
                            .sync
                            .send_response(channel, SyncResponse::Delivered(accepted));
                    }
                }
                Some(Command::RegisterRendezvous(point, namespace)) => {
                    match rendezvous::Namespace::new(namespace) {
                        Ok(ns) => {
                            if let Err(error) = swarm.behaviour_mut().rendezvous_client.register(ns, point, None) {
                                failed(&evt, "rendezvous register", format!("{error:?}"));
                            }
                        }
                        Err(error) => failed(&evt, "rendezvous register", error),
                    }
                }
                Some(Command::DiscoverRendezvous(point, namespace)) => {
                    match rendezvous::Namespace::new(namespace) {
                        Ok(ns) => swarm.behaviour_mut().rendezvous_client.discover(Some(ns), None, None, point),
                        Err(error) => failed(&evt, "rendezvous discover", error),
                    }
                }
                Some(Command::FindPeer(peer)) => {
                    // Route to `peer` over the DHT; the closest-peers result arrives
                    // as a Kademlia OutboundQueryProgressed event (handled below).
                    if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                        routing.insert(peer);
                        kad.get_closest_peers(peer);
                    } else {
                        failed(&evt, "find peer", "the Kademlia DHT is disabled on this node");
                    }
                }
                Some(Command::Subscribe(room)) => {
                    match swarm.behaviour_mut().gossipsub.subscribe(&topic(&room)) {
                        Ok(_) => emit(&evt, NodeEvent::Subscribed { room }),
                        Err(error) => failed(&evt, "subscribe", error),
                    }
                }
                Some(Command::Publish(room, data)) => {
                    let message_id = content_message_id(&data);
                    match swarm.behaviour_mut().gossipsub.publish(topic(&room), data) {
                        Ok(_) => emit(&evt, NodeEvent::Published {
                            room,
                            message_id,
                            duplicate: false,
                        }),
                        Err(gossipsub::PublishError::Duplicate) => emit(&evt, NodeEvent::Published {
                            room,
                            message_id,
                            duplicate: true,
                        }),
                        Err(error) => failed(&evt, "publish", error),
                    }
                }
                Some(Command::SubscribePrivate(namespace)) => {
                    match swarm.behaviour_mut().gossipsub.subscribe(&private_topic(&namespace)) {
                        Ok(_) => emit(&evt, NodeEvent::Subscribed { room: namespace }),
                        Err(error) => failed(&evt, "private subscribe", error),
                    }
                }
                Some(Command::PublishPrivate(namespace, data)) => {
                    let message_id = content_message_id(&data);
                    match swarm.behaviour_mut().gossipsub.publish(private_topic(&namespace), data) {
                        Ok(_) => emit(&evt, NodeEvent::Published {
                            room: namespace,
                            message_id,
                            duplicate: false,
                        }),
                        Err(gossipsub::PublishError::Duplicate) => emit(&evt, NodeEvent::Published {
                            room: namespace,
                            message_id,
                            duplicate: true,
                        }),
                        Err(error) => failed(&evt, "private publish", error),
                    }
                }
                None => break, // handle dropped — shut the node down
            },
            event = swarm.select_next_some() => {
              match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    let full = address.with_p2p(peer_id).unwrap_or_else(|a| a);
                    emit(&evt, NodeEvent::Listening(full.to_string()));
                }
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    if private_swarm && !allowed_private_peers.contains(&peer_id) {
                        let _ = swarm.disconnect_peer_id(peer_id);
                        failed(&evt, "private peer authorization", "peer is not allowlisted");
                        continue;
                    }
                    emit(&evt, NodeEvent::ConnectionEstablished {
                        peer_id: peer_id.to_string(),
                        relayed: is_relayed(endpoint.get_remote_address()),
                    });
                }
                SwarmEvent::OutgoingConnectionError { error, .. } => {
                    failed(&evt, "dial", error);
                }
                SwarmEvent::ListenerError { error, .. } => {
                    failed(&evt, "listen", error);
                }
                SwarmEvent::ListenerClosed { reason: Err(error), .. } => {
                    failed(&evt, "listen", error);
                }
                SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted {
                        relay_peer_id,
                        renewal,
                        ..
                    },
                )) => {
                    emit(&evt, NodeEvent::RelayReservationAccepted {
                        relay_peer_id: relay_peer_id.to_string(),
                        renewed: renewal,
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                    relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. },
                )) => {
                    emit(&evt, NodeEvent::RelayCircuitEstablished {
                        peer_id: relay_peer_id.to_string(),
                        direction: "outbound",
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                    relay::client::Event::InboundCircuitEstablished { src_peer_id, .. },
                )) => {
                    emit(&evt, NodeEvent::RelayCircuitEstablished {
                        peer_id: src_peer_id.to_string(),
                        direction: "inbound",
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                    let (succeeded, error) = match event.result {
                        Ok(_) => (true, None),
                        Err(error) => (false, Some(error.to_string())),
                    };
                    emit(&evt, NodeEvent::DirectConnectionUpgrade {
                        peer_id: event.remote_peer_id.to_string(),
                        succeeded,
                        error,
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Sync(request_response::Event::Message {
                    peer,
                    message,
                    ..
                })) => match message {
                    request_response::Message::Request {
                        request: SyncRequest::Deliver(data),
                        channel,
                        ..
                    } => {
                        // A direct message: this is concierge-only point-to-point
                        // traffic, NOT block/head sync — it must not reach the store
                        // `provider`. Surface the opaque envelope and wait for the
                        // receiving application to authenticate, apply consent, and
                        // explicitly acknowledge acceptance.
                        if inbound_dms.len() >= MAX_PENDING_DIRECT_MESSAGES {
                            let _ = swarm
                                .behaviour_mut()
                                .sync
                                .send_response(channel, SyncResponse::Delivered(false));
                            continue;
                        }
                        let delivery_id = next_delivery_id;
                        next_delivery_id = next_delivery_id.wrapping_add(1).max(1);
                        inbound_dms.insert(delivery_id, channel);
                        emit(&evt, NodeEvent::DirectMessage {
                            from_peer: peer.to_string(),
                            data,
                            delivery_id,
                        });
                    }
                    request_response::Message::Request { request, channel, .. } => {
                        // Serve from the local store via the application's provider
                        // (which enforces capability/encryption gating). The
                        // transport never reads the store itself.
                        let response = provider(request);
                        let _ = swarm.behaviour_mut().sync.send_response(channel, response);
                    }
                    request_response::Message::Response { request_id, response } => {
                        // A `Delivered` ack belongs to an outbound direct message, not
                        // a block/head request — match it against `outbound_dms` so the
                        // sender learns delivery succeeded and stops retrying.
                        if let SyncResponse::Delivered(accepted) = response {
                            if let Some((to_peer, message_id)) = outbound_dms.remove(&request_id) {
                                if accepted {
                                    emit(&evt, NodeEvent::DirectMessageDelivered {
                                        to_peer: to_peer.to_string(),
                                        message_id,
                                    });
                                }
                            }
                            continue;
                        }
                        match (pending.remove(&request_id), response) {
                            (Some(Pending::Block { cid, reply }), SyncResponse::Block(bytes)) => {
                                if let Some(reply) = reply {
                                    let _ = reply.send(bytes);
                                } else {
                                    emit(&evt, NodeEvent::BlockReceived { cid, bytes });
                                }
                            }
                            (Some(Pending::Heads { namespace, reply }), SyncResponse::Heads(heads)) => {
                                if let Some(reply) = reply {
                                    let _ = reply.send(heads);
                                } else {
                                    emit(&evt, NodeEvent::HeadsReceived { namespace, heads });
                                }
                            }
                            (Some(Pending::Push(cid)), SyncResponse::Stored(ok)) => {
                                emit(&evt, NodeEvent::BlockStored { cid, ok });
                            }
                            _ => {}
                        }
                    }
                },
                SwarmEvent::Behaviour(BehaviourEvent::Sync(request_response::Event::OutboundFailure {
                    request_id,
                    error,
                    ..
                })) => {
                    // A failed direct message just drops its outbox entry — no event;
                    // the sender's scheduled retry handles re-delivery.
                    outbound_dms.remove(&request_id);
                    // Surface the failure under the right shape so the caller can retry.
                    match pending.remove(&request_id) {
                        Some(Pending::Block { cid, reply }) => {
                            if let Some(reply) = reply {
                                let _ = reply.send(None);
                            } else {
                                emit(&evt, NodeEvent::BlockReceived { cid, bytes: None });
                            }
                        }
                        Some(Pending::Heads { namespace, reply }) => {
                            if let Some(reply) = reply {
                                let _ = reply.send(None);
                            } else {
                                emit(&evt, NodeEvent::HeadsReceived { namespace, heads: None });
                            }
                        }
                        Some(Pending::Push(cid)) => emit(&evt, NodeEvent::BlockStored { cid, ok: false }),
                        None => {}
                    }
                    failed(&evt, "sync request", error);
                }
                SwarmEvent::Behaviour(BehaviourEvent::RendezvousClient(
                    rendezvous::client::Event::Registered { namespace, .. },
                )) => {
                    emit(&evt, NodeEvent::RendezvousRegistered { namespace: namespace.to_string() });
                }
                SwarmEvent::Behaviour(BehaviourEvent::RendezvousClient(
                    rendezvous::client::Event::Discovered { registrations, .. },
                )) => {
                    for registration in registrations {
                        let peer = registration.record.peer_id();
                        let addresses = registration.record.addresses().iter().map(|a| a.to_string()).collect();
                        emit(&evt, NodeEvent::RendezvousDiscovered { peer_id: peer.to_string(), addresses });
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
                    new,
                    ..
                })) => {
                    let reachability = match new {
                        autonat::NatStatus::Public(_) => "public",
                        autonat::NatStatus::Private => "private",
                        autonat::NatStatus::Unknown => "unknown",
                    };
                    emit(&evt, NodeEvent::NatStatus { reachability });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    // A peer appeared on the LAN: treat it as an explicit gossipsub
                    // peer, register its address so a later dial-by-id resolves, and
                    // dial it now (no manual address exchange on a local network).
                    for (peer, addr) in list {
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                        swarm.add_peer_address(peer, addr.clone());
                        if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                            kad.add_address(&peer, addr);
                        }
                        let _ = swarm.dial(peer);
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer, _addr) in list {
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer);
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk { peers, .. })),
                    ..
                })) => {
                    // A routing query finished. For any peer we explicitly asked to
                    // route to, register its discovered addresses, announce them, and
                    // dial — completing find_peer's "found peer / connecting".
                    for info in peers {
                        if routing.remove(&info.peer_id) {
                            for addr in &info.addrs {
                                swarm.add_peer_address(info.peer_id, addr.clone());
                            }
                            emit(&evt, NodeEvent::PeerRouted {
                                peer_id: info.peer_id.to_string(),
                                addresses: info.addrs.iter().map(|a| a.to_string()).collect(),
                            });
                            let _ = swarm.dial(info.peer_id);
                        }
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    message,
                    ..
                })) => {
                    let topic_str = message.topic.as_str();
                    let room = if private_swarm {
                        let Some(namespace) = topic_str.strip_prefix(PRIVATE_NAMESPACE_PREFIX) else {
                            continue;
                        };
                        namespace.to_string()
                    } else {
                        let Some(room) = topic_str.strip_prefix(ROOM_PREFIX) else {
                            continue;
                        };
                        room.to_string()
                    };
                    emit(&evt, NodeEvent::Message {
                        room,
                        data: message.data,
                        source: message.source.map(|p| p.to_string()),
                    });
                }
                _ => {}
              }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    fn key(seed: u8) -> [u8; 32] {
        let mut k = [seed; 32];
        k[0] = seed; // distinct keys -> distinct PeerIDs
        k
    }

    fn peer_id(seed: u8) -> PeerId {
        let mut secret = key(seed);
        identity::Keypair::ed25519_from_bytes(&mut secret)
            .expect("test keypair")
            .public()
            .to_peer_id()
    }

    async fn next_listen(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::Listening(addr)) => return addr,
                Some(NodeEvent::OperationFailed {
                    operation: "listen",
                    error,
                }) => panic!("listen failed: {error}"),
                Some(_) => {}
                None => panic!("node event stream closed before a listen address arrived"),
            }
        }
    }

    async fn next_relayed_connection(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::ConnectionEstablished {
                    peer_id,
                    relayed: true,
                }) => return peer_id,
                Some(_) => {}
                None => panic!("node event stream closed before relayed connection arrived"),
            }
        }
    }

    async fn wait_for_reservation(
        rx: &mut mpsc::UnboundedReceiver<NodeEvent>,
        expected_relay: &str,
    ) -> String {
        let mut connected = false;
        let mut accepted = false;
        let mut circuit = None;
        loop {
            match rx.recv().await {
                Some(NodeEvent::ConnectionEstablished {
                    peer_id,
                    relayed: false,
                }) if peer_id == expected_relay => connected = true,
                Some(NodeEvent::RelayReservationAccepted { relay_peer_id, .. })
                    if relay_peer_id == expected_relay =>
                {
                    accepted = true;
                }
                Some(NodeEvent::Listening(address)) if address.contains("p2p-circuit") => {
                    circuit = Some(address);
                }
                Some(_) => {}
                None => panic!("node event stream closed before relay reservation completed"),
            }
            if connected && accepted {
                if let Some(circuit) = circuit {
                    return circuit;
                }
            }
        }
    }

    async fn next_external_address(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::ExternalAddressAdded { address }) => return address,
                Some(_) => {}
                None => panic!("node event stream closed before external address confirmation"),
            }
        }
    }

    async fn next_relay_renewal(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::RelayReservationAccepted {
                    relay_peer_id,
                    renewed: true,
                }) => return relay_peer_id,
                Some(_) => {}
                None => panic!("node event stream closed before relay renewal"),
            }
        }
    }

    async fn next_dcutr_success(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::DirectConnectionUpgrade {
                    peer_id,
                    succeeded: true,
                    ..
                }) => return peer_id,
                Some(_) => {}
                None => panic!("node event stream closed before DCUtR success arrived"),
            }
        }
    }

    async fn next_published(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> (String, bool) {
        loop {
            match rx.recv().await {
                Some(NodeEvent::Published {
                    message_id,
                    duplicate,
                    ..
                }) => return (message_id, duplicate),
                Some(_) => {}
                None => panic!("node event stream closed before publish result arrived"),
            }
        }
    }

    async fn next_failure(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> (&'static str, String) {
        loop {
            match rx.recv().await {
                Some(NodeEvent::OperationFailed { operation, error }) => {
                    return (operation, error);
                }
                Some(_) => {}
                None => panic!("node event stream closed before failure arrived"),
            }
        }
    }

    async fn next_failure_for(
        rx: &mut mpsc::UnboundedReceiver<NodeEvent>,
        expected_operation: &'static str,
    ) -> String {
        loop {
            let (operation, error) = next_failure(rx).await;
            if operation == expected_operation {
                return error;
            }
        }
    }

    async fn next_direct_message(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> (Vec<u8>, u64) {
        loop {
            match rx.recv().await {
                Some(NodeEvent::DirectMessage {
                    data, delivery_id, ..
                }) => return (data, delivery_id),
                Some(_) => {}
                None => panic!("node event stream closed before a direct message arrived"),
            }
        }
    }

    async fn next_direct_delivery(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> (String, String) {
        loop {
            match rx.recv().await {
                Some(NodeEvent::DirectMessageDelivered {
                    to_peer,
                    message_id,
                }) => return (to_peer, message_id),
                Some(_) => {}
                None => {
                    panic!("node event stream closed before a delivery acknowledgement arrived")
                }
            }
        }
    }

    /// Drive `a` to publish until `b` receives `payload` (or the timeout fires).
    async fn await_delivery(
        a: &ConciergeNode,
        b_rx: &mut mpsc::UnboundedReceiver<NodeEvent>,
        room: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        timeout(Duration::from_secs(20), async {
            loop {
                a.publish(room, payload.to_vec()).expect("queue publish");
                if let Ok(Some(NodeEvent::Message { data, .. })) =
                    timeout(Duration::from_millis(500), b_rx.recv()).await
                {
                    return data;
                }
            }
        })
        .await
        .expect("message should be delivered within the timeout")
    }

    async fn await_private_delivery(
        a: &ConciergeNode,
        b_rx: &mut mpsc::UnboundedReceiver<NodeEvent>,
        namespace: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        timeout(Duration::from_secs(20), async {
            loop {
                a.publish_private(namespace, payload.to_vec())
                    .expect("queue private publish");
                if let Ok(Some(NodeEvent::Message { data, .. })) =
                    timeout(Duration::from_millis(500), b_rx.recv()).await
                {
                    return data;
                }
            }
        })
        .await
        .expect("private ciphertext should be delivered within the timeout")
    }

    #[tokio::test]
    async fn two_peers_exchange_a_message_over_a_gossipsub_room() {
        let (a, mut a_rx) = ConciergeNode::spawn(key(1)).expect("node a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(2)).expect("node b");
        assert_ne!(a.peer_id, b.peer_id);

        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a should report a listen addr");

        b.dial(a_addr.parse().unwrap()).expect("queue dial");
        a.subscribe("conservation").expect("queue subscribe");
        b.subscribe("conservation").expect("queue subscribe");

        let payload = b"protect the wetlands".to_vec();
        let received = await_delivery(&a, &mut b_rx, "conservation", &payload).await;
        assert_eq!(received, payload, "B receives the exact bytes A published");
    }

    #[tokio::test]
    async fn sync_responses_do_not_consume_direct_messages_and_acceptance_controls_acknowledgement()
    {
        let provider: SyncProvider = Arc::new(|request| match request {
            SyncRequest::GetHeads(namespace) if namespace == "shared" => {
                SyncResponse::Heads(Some(b"signed-heads".to_vec()))
            }
            SyncRequest::GetBlock(_) => SyncResponse::Block(None),
            SyncRequest::GetHeads(_) => SyncResponse::Heads(None),
            SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
            SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
        });
        let (a, mut a_rx) =
            ConciergeNode::spawn_with_provider(key(3), NodeConfig::default(), provider)
                .expect("node a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(4)).expect("node b");

        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a should report a listen addr");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(NodeEvent::ConnectionEstablished { peer_id, .. }) = b_rx.recv().await {
                    if peer_id == a.peer_id.to_string() {
                        break;
                    }
                }
            }
        })
        .await
        .expect("b should connect to a");
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(NodeEvent::ConnectionEstablished { peer_id, .. }) = a_rx.recv().await {
                    if peer_id == b.peer_id.to_string() {
                        break;
                    }
                }
            }
        })
        .await
        .expect("a should observe b's connection");

        let payload = b"authenticated direct envelope".to_vec();
        a.send_dm(b.peer_id, payload.clone())
            .expect("queue direct message");
        assert_eq!(
            b.request_heads_response(a.peer_id, "shared")
                .await
                .expect("head request"),
            Some(b"signed-heads".to_vec())
        );
        let (received, rejected_delivery) =
            timeout(Duration::from_secs(5), next_direct_message(&mut b_rx))
                .await
                .expect("direct message remains available after sync");
        assert_eq!(received, payload);

        b.acknowledge_dm(rejected_delivery, false)
            .expect("reject direct message");
        assert!(
            timeout(Duration::from_millis(500), next_direct_delivery(&mut a_rx))
                .await
                .is_err(),
            "a rejected message must not be reported as delivered"
        );

        a.send_dm(b.peer_id, payload.clone())
            .expect("retry direct message");
        let (_, accepted_delivery) =
            timeout(Duration::from_secs(5), next_direct_message(&mut b_rx))
                .await
                .expect("retried direct message");
        b.acknowledge_dm(accepted_delivery, true)
            .expect("accept direct message");
        let (to_peer, message_id) =
            timeout(Duration::from_secs(5), next_direct_delivery(&mut a_rx))
                .await
                .expect("accepted direct message is acknowledged");
        assert_eq!(to_peer, b.peer_id.to_string());
        assert_eq!(message_id, content_message_id(&payload));
    }

    #[tokio::test]
    async fn relayed_peers_exchange_a_message_through_a_relay() {
        // R is a relay; A reserves a slot on R; B dials A *through* R (the path a
        // NAT'd peer needs). A publishes; B must receive it via the relayed link.
        let relay_config = NodeConfig {
            host_relay: true,
            ..NodeConfig::default()
        };
        let (_relay, mut r_rx) =
            ConciergeNode::spawn_with_config(key(10), relay_config).expect("relay");
        let (a, mut a_rx) = ConciergeNode::spawn(key(11)).expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(12)).expect("b");

        _relay
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue relay listen");
        let relay_addr = timeout(Duration::from_secs(5), next_listen(&mut r_rx))
            .await
            .expect("relay should report a listen addr");
        let mut relay_external: Multiaddr = relay_addr.parse().expect("relay multiaddr");
        assert!(matches!(relay_external.pop(), Some(Protocol::P2p(_))));
        _relay
            .add_external_address(relay_external)
            .expect("queue external address");
        let confirmed_external = timeout(Duration::from_secs(5), next_external_address(&mut r_rx))
            .await
            .expect("relay should confirm its operator-supplied external address");
        assert!(relay_addr.starts_with(&confirmed_external));

        // A requests a relay reservation. The relay transport establishes the
        // connection itself; no sleep or separate dial race is required.
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue a listen");
        a.reserve(relay_addr.parse().unwrap())
            .expect("queue reservation");
        let a_circuit = timeout(
            Duration::from_secs(15),
            wait_for_reservation(&mut a_rx, &_relay.peer_id.to_string()),
        )
        .await
        .expect("A should connect, reserve, and obtain a circuit address");

        // B dials A through the relay.
        b.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue b listen");
        b.dial(a_circuit.parse().unwrap())
            .expect("queue circuit dial");
        let relayed_peer = timeout(Duration::from_secs(15), next_relayed_connection(&mut b_rx))
            .await
            .expect("B should establish a relayed connection");
        assert_eq!(relayed_peer, a.peer_id.to_string());
        let upgraded_peer = timeout(Duration::from_secs(15), next_dcutr_success(&mut b_rx))
            .await
            .expect("DCUtR should upgrade the loopback relayed connection");
        assert_eq!(upgraded_peer, a.peer_id.to_string());
        a.subscribe("relayroom").expect("queue subscribe");
        b.subscribe("relayroom").expect("queue subscribe");

        let payload = b"relayed hello".to_vec();
        let received = await_delivery(&a, &mut b_rx, "relayroom", &payload).await;
        assert_eq!(
            received, payload,
            "B receives A's message through the relay"
        );
    }

    #[tokio::test]
    async fn relay_remains_a_working_fallback_when_dcutr_cannot_upgrade() {
        let relay_config = NodeConfig {
            host_relay: true,
            ..NodeConfig::default()
        };
        let (relay, mut relay_events) =
            ConciergeNode::spawn_with_config(key(50), relay_config).expect("relay");
        let (a, mut a_events) = ConciergeNode::spawn(key(51)).expect("a");
        let (b, mut b_events) = ConciergeNode::spawn(key(52)).expect("b");

        relay
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue relay listen");
        let relay_addr = timeout(Duration::from_secs(5), next_listen(&mut relay_events))
            .await
            .expect("relay listen");
        let mut relay_external: Multiaddr = relay_addr.parse().unwrap();
        relay_external.pop();
        relay
            .add_external_address(relay_external)
            .expect("queue relay external address");
        timeout(
            Duration::from_secs(5),
            next_external_address(&mut relay_events),
        )
        .await
        .expect("external address acknowledgement");

        // Neither peer opens a direct listener, so DCUtR has no usable direct
        // address. The relayed connection must remain functional.
        a.reserve(relay_addr.parse().unwrap())
            .expect("queue reservation");
        let a_circuit = timeout(
            Duration::from_secs(15),
            wait_for_reservation(&mut a_events, &relay.peer_id.to_string()),
        )
        .await
        .expect("reservation");
        b.dial(a_circuit.parse().unwrap())
            .expect("queue relay dial");
        timeout(
            Duration::from_secs(15),
            next_relayed_connection(&mut b_events),
        )
        .await
        .expect("relayed connection");
        a.subscribe("fallback").expect("subscribe");
        b.subscribe("fallback").expect("subscribe");

        let payload = b"relay fallback".to_vec();
        let received = await_delivery(&a, &mut b_events, "fallback", &payload).await;
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn relay_reservation_renews_automatically() {
        let relay_config = NodeConfig {
            host_relay: true,
            relay_reservation_duration: Duration::from_secs(2),
            ..NodeConfig::default()
        };
        let (relay, mut relay_events) =
            ConciergeNode::spawn_with_config(key(60), relay_config).expect("relay");
        let (client, mut client_events) = ConciergeNode::spawn(key(61)).expect("client");

        relay
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue relay listen");
        let relay_addr = timeout(Duration::from_secs(5), next_listen(&mut relay_events))
            .await
            .expect("relay listen");
        let mut relay_external: Multiaddr = relay_addr.parse().unwrap();
        relay_external.pop();
        relay
            .add_external_address(relay_external)
            .expect("queue external address");
        timeout(
            Duration::from_secs(5),
            next_external_address(&mut relay_events),
        )
        .await
        .expect("external address acknowledgement");

        client
            .reserve(relay_addr.parse().unwrap())
            .expect("queue reservation");
        timeout(
            Duration::from_secs(10),
            wait_for_reservation(&mut client_events, &relay.peer_id.to_string()),
        )
        .await
        .expect("initial reservation");
        let renewed_by = timeout(
            Duration::from_secs(10),
            next_relay_renewal(&mut client_events),
        )
        .await
        .expect("reservation should renew before expiry");
        assert_eq!(renewed_by, relay.peer_id.to_string());
    }

    #[tokio::test]
    async fn private_swarm_disables_public_topics_and_public_dht() {
        let config = NodeConfig {
            private_swarm: true,
            ..NodeConfig::default()
        };
        let (node, _events) = ConciergeNode::spawn_with_config(key(90), config).unwrap();
        assert!(node.subscribe("public-room").is_err());
        assert!(node.publish("public-room", b"no".to_vec()).is_err());
        assert!(node.subscribe_private("team:wetlands").is_ok());
        assert!(node
            .publish_private("team:wetlands", b"ciphertext".to_vec())
            .is_ok());
        assert!(!public_dht_announcements_enabled());
    }

    #[tokio::test]
    async fn allowlisted_private_peers_exchange_ciphertext_and_reject_other_peers() {
        let a_config = NodeConfig {
            private_swarm: true,
            allowed_private_peers: HashSet::from([peer_id(92)]),
            ..NodeConfig::default()
        };
        let b_config = NodeConfig {
            private_swarm: true,
            allowed_private_peers: HashSet::from([peer_id(91)]),
            ..NodeConfig::default()
        };
        let (a, mut a_events) = ConciergeNode::spawn_with_config(key(91), a_config).unwrap();
        let (b, mut b_events) = ConciergeNode::spawn_with_config(key(92), b_config).unwrap();
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_events))
            .await
            .expect("private peer should report a listen address");
        b.dial(a_addr.parse().unwrap()).unwrap();
        a.subscribe_private("team:wetlands").unwrap();
        b.subscribe_private("team:wetlands").unwrap();
        let ciphertext = b"opaque-ciphertext-block".to_vec();
        assert_eq!(
            await_private_delivery(&a, &mut b_events, "team:wetlands", &ciphertext).await,
            ciphertext
        );

        let (outsider, _outsider_events) =
            ConciergeNode::spawn_with_config(key(93), NodeConfig::default()).unwrap();
        outsider.dial(a_addr.parse().unwrap()).unwrap();
        let error = timeout(
            Duration::from_secs(5),
            next_failure_for(&mut a_events, "private peer authorization"),
        )
        .await
        .expect("private node should reject the outsider");
        assert!(error.contains("not allowlisted"));
    }

    #[test]
    fn relay_hosting_is_explicit_and_external_addresses_are_operator_supplied() {
        let keypair = |seed| {
            let mut secret = key(seed);
            identity::Keypair::ed25519_from_bytes(&mut secret).expect("keypair")
        };
        let client = build_swarm(keypair(20), &NodeConfig::default()).expect("client swarm");
        assert!(!client.behaviour().relay.is_enabled());
        assert_eq!(client.external_addresses().count(), 0);

        let external: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().unwrap();
        let relay_config = NodeConfig {
            host_relay: true,
            external_addresses: vec![external.clone()],
            ..NodeConfig::default()
        };
        let relay = build_swarm(keypair(21), &relay_config).expect("relay swarm");
        assert!(relay.behaviour().relay.is_enabled());
        assert_eq!(
            relay.external_addresses().collect::<Vec<_>>(),
            vec![&external]
        );
    }

    #[tokio::test]
    async fn content_addressed_publish_deduplicates_retries() {
        let (a, mut a_rx) = ConciergeNode::spawn(key(30)).expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(31)).expect("b");
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("listen");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");
        a.subscribe("dedup").expect("subscribe");
        b.subscribe("dedup").expect("subscribe");

        let payload = b"stable signed envelope".to_vec();
        let received = await_delivery(&a, &mut b_rx, "dedup", &payload).await;
        assert_eq!(received, payload);
        let (first_id, first_duplicate) =
            timeout(Duration::from_secs(5), next_published(&mut a_rx))
                .await
                .expect("first publish result");
        assert!(!first_duplicate);
        assert_eq!(first_id, content_message_id(&payload));

        a.publish("dedup", payload.clone())
            .expect("queue duplicate");
        let (second_id, second_duplicate) =
            timeout(Duration::from_secs(5), next_published(&mut a_rx))
                .await
                .expect("duplicate publish result");
        assert!(second_duplicate);
        assert_eq!(second_id, first_id);
        assert!(
            timeout(Duration::from_millis(500), b_rx.recv())
                .await
                .is_err(),
            "duplicate payload must not be forwarded again"
        );
    }

    #[tokio::test]
    async fn operational_errors_are_observable() {
        let (node, mut events) = ConciergeNode::spawn(key(40)).expect("node");
        node.listen("/memory/42".parse().unwrap())
            .expect("queue unsupported listen");
        let (operation, error) = timeout(Duration::from_secs(5), next_failure(&mut events))
            .await
            .expect("listen failure event");
        assert_eq!(operation, "listen");
        assert!(!error.is_empty());
    }

    #[test]
    fn message_ids_are_stable_and_content_derived() {
        assert_eq!(content_message_id(b"same"), content_message_id(b"same"));
        assert_ne!(
            content_message_id(b"same"),
            content_message_id(b"different")
        );
    }

    #[test]
    fn gossipsub_requires_signatures_and_bounds_message_size() {
        let config = NodeConfig {
            max_message_bytes: 4096,
            ..NodeConfig::default()
        };
        let gossipsub = build_gossipsub_config(&config).expect("gossipsub config");
        assert!(matches!(
            gossipsub.validation_mode(),
            gossipsub::ValidationMode::Strict
        ));
        assert_eq!(gossipsub.max_transmit_size(), 4096);
    }

    #[test]
    fn opt_in_relay_limits_are_bounded_but_support_sustained_rooms() {
        let config = NodeConfig::default();
        let relay = relay_server_config(&config);
        assert_eq!(relay.max_reservations, RELAY_MAX_RESERVATIONS);
        assert_eq!(
            relay.max_reservations_per_peer,
            RELAY_MAX_RESERVATIONS_PER_PEER
        );
        assert_eq!(relay.max_circuits, RELAY_MAX_CIRCUITS);
        assert_eq!(relay.max_circuits_per_peer, RELAY_MAX_CIRCUITS_PER_PEER);
        assert_eq!(relay.max_circuit_duration, RELAY_MAX_CIRCUIT_DURATION);
        assert_eq!(relay.max_circuit_bytes, RELAY_MAX_CIRCUIT_BYTES);
        assert!(relay.max_circuit_bytes >= (config.max_message_bytes as u64) * 100);
    }

    async fn next_block(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> (String, Option<Vec<u8>>) {
        loop {
            match rx.recv().await {
                Some(NodeEvent::BlockReceived { cid, bytes }) => return (cid, bytes),
                Some(_) => {}
                None => panic!("event stream closed before a block arrived"),
            }
        }
    }

    #[tokio::test]
    async fn a_peer_fetches_a_block_over_request_response() {
        // A serves a CID→bytes block from its store (via a provider); B fetches it
        // by CID over the sync protocol and receives the exact bytes. This is the
        // Phase D reconciliation moving over a real libp2p connection (Phase F).
        let cid = "bafyTESTBLOCK".to_string();
        let block = b"the verified bytes".to_vec();
        let served = (cid.clone(), block.clone());
        let provider: SyncProvider = Arc::new(move |req| match req {
            SyncRequest::GetBlock(c) if c == served.0 => {
                SyncResponse::Block(Some(served.1.clone()))
            }
            SyncRequest::GetBlock(_) => SyncResponse::Block(None),
            SyncRequest::GetHeads(_) => SyncResponse::Heads(None),
            SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
            SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
        });
        let (a, mut a_rx) =
            ConciergeNode::spawn_with_provider(key(21), NodeConfig::default(), provider)
                .expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(22)).expect("b");

        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a listen addr");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");

        // Fetch the served CID, retrying until the connection is up.
        let got = timeout(Duration::from_secs(20), async {
            loop {
                b.request_block(a.peer_id, &cid).expect("queue request");
                if let Ok(Some(NodeEvent::BlockReceived {
                    bytes: Some(bytes), ..
                })) = timeout(Duration::from_millis(500), async {
                    loop {
                        match b_rx.recv().await {
                            Some(e @ NodeEvent::BlockReceived { .. }) => return Some(e),
                            Some(_) => {}
                            None => return None,
                        }
                    }
                })
                .await
                {
                    return bytes;
                }
            }
        })
        .await
        .expect("block should arrive");
        assert_eq!(got, block, "B receives the exact served block bytes");
    }

    #[tokio::test]
    async fn two_real_stores_converge_over_libp2p_with_verified_import() {
        // End-to-end Phase F: A serves a real block from its store; B fetches it
        // over libp2p, CID-verifies, and imports it — convergence over the wire.
        use concierge_core::{CoreBinding, MemCli, Node, SyncLimits};

        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = Arc::new(MemCli::new(dir_a.path()));
        let cid = mem_a
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"over the wire","kind":"reference"}"#.into(),
            })
            .unwrap();

        let (a, mut a_rx) = ConciergeNode::spawn_with_provider(
            key(25),
            NodeConfig::default(),
            store_provider(mem_a.clone()),
        )
        .expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(26)).expect("b");
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a listen addr");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");

        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        assert!(!mem_b.has_block(&cid.0), "B starts without the block");

        // Fetch over the network, then import with CID verification.
        let bytes = timeout(Duration::from_secs(20), async {
            loop {
                b.request_block(a.peer_id, &cid.0).expect("queue request");
                if let Ok((_c, Some(bytes))) =
                    timeout(Duration::from_millis(500), next_block(&mut b_rx)).await
                {
                    return bytes;
                }
            }
        })
        .await
        .expect("the real block should arrive");

        // The application verifies + imports (the transport never touched the store).
        mem_b
            .pull_blocks(
                std::slice::from_ref(&cid.0),
                |_| Some(bytes.clone()),
                SyncLimits::default(),
            )
            .unwrap();
        assert!(
            mem_b.has_block(&cid.0),
            "B converged: the verified block is in its store"
        );
        assert_eq!(
            mem_b.block_bytes(&cid.0),
            Some(bytes),
            "exact bytes, CID-verified"
        );
    }

    async fn next_registered(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::RendezvousRegistered { namespace }) => return namespace,
                Some(_) => {}
                None => panic!("event stream closed before rendezvous registration"),
            }
        }
    }

    async fn next_discovered(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> String {
        loop {
            match rx.recv().await {
                Some(NodeEvent::RendezvousDiscovered { peer_id, .. }) => return peer_id,
                Some(_) => {}
                None => panic!("event stream closed before rendezvous discovery"),
            }
        }
    }

    #[tokio::test]
    async fn peers_find_each_other_through_a_rendezvous_point() {
        // Phase F discovery: A registers at a rendezvous point; B discovers A there —
        // no manually-exchanged address between A and B.
        let rdv_config = NodeConfig {
            rendezvous_point: true,
            ..NodeConfig::default()
        };
        let (rdv, mut rdv_rx) =
            ConciergeNode::spawn_with_config(key(50), rdv_config).expect("rendezvous point");
        rdv.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let rdv_addr = timeout(Duration::from_secs(5), next_listen(&mut rdv_rx))
            .await
            .expect("rdv addr");

        // A: advertise its own address, connect to the point, and register.
        let (a, mut a_rx) = ConciergeNode::spawn(key(51)).expect("a");
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a addr");
        let mut a_external: Multiaddr = a_addr.parse().unwrap();
        if matches!(a_external.iter().last(), Some(Protocol::P2p(_))) {
            a_external.pop();
        }
        a.add_external_address(a_external).expect("queue external");
        a.dial(rdv_addr.parse().unwrap()).expect("dial rdv");
        timeout(Duration::from_secs(25), async {
            loop {
                a.register_rendezvous(rdv.peer_id, "concierge").ok();
                if (timeout(Duration::from_millis(800), next_registered(&mut a_rx)).await).is_ok() {
                    return;
                }
            }
        })
        .await
        .expect("A registers at the rendezvous point");

        // B: connect to the point and discover — it learns A without A's address.
        let (b, mut b_rx) = ConciergeNode::spawn(key(52)).expect("b");
        b.dial(rdv_addr.parse().unwrap()).expect("dial rdv");
        let found = timeout(Duration::from_secs(25), async {
            loop {
                b.discover_rendezvous(rdv.peer_id, "concierge").ok();
                if let Ok(peer) =
                    timeout(Duration::from_millis(800), next_discovered(&mut b_rx)).await
                {
                    if peer == a.peer_id.to_string() {
                        return peer;
                    }
                }
            }
        })
        .await
        .expect("B discovers A through the rendezvous point");
        assert_eq!(found, a.peer_id.to_string());
    }

    async fn next_stored(rx: &mut mpsc::UnboundedReceiver<NodeEvent>) -> bool {
        loop {
            match rx.recv().await {
                Some(NodeEvent::BlockStored { ok, .. }) => return ok,
                Some(_) => {}
                None => panic!("event stream closed before a store result"),
            }
        }
    }

    #[tokio::test]
    async fn a_store_and_forward_relay_holds_a_block_for_an_offline_peer() {
        // A writer pushes a block to a relay and may then go offline; a third peer
        // pulls the block from the relay. Convergence without the writer online.
        use concierge_core::{CoreBinding, MemCli, Node};

        let dir_w = tempfile::tempdir().unwrap();
        let mem_w = MemCli::new(dir_w.path());
        let cid = mem_w
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"for offline peers","kind":"reference"}"#.into(),
            })
            .unwrap();
        let bytes = mem_w.block_bytes(&cid.0).unwrap();

        // The relay accepts pushes (store-and-forward).
        let dir_r = tempfile::tempdir().unwrap();
        let mem_r = Arc::new(MemCli::new(dir_r.path()));
        let (relay, mut relay_rx) = ConciergeNode::spawn_with_provider(
            key(40),
            NodeConfig::default(),
            relay_provider(mem_r.clone()),
        )
        .expect("relay");
        relay
            .listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let relay_addr = timeout(Duration::from_secs(5), next_listen(&mut relay_rx))
            .await
            .expect("relay addr");

        // Writer pushes the block to the relay.
        let (w, mut w_rx) = ConciergeNode::spawn(key(41)).expect("writer");
        w.dial(relay_addr.parse().unwrap()).expect("dial");
        let stored = timeout(Duration::from_secs(20), async {
            loop {
                w.push_block(relay.peer_id, &cid.0, bytes.clone())
                    .expect("queue push");
                if let Ok(ok) = timeout(Duration::from_millis(500), next_stored(&mut w_rx)).await {
                    if ok {
                        return true;
                    }
                }
            }
        })
        .await
        .expect("push should be accepted");
        assert!(stored);
        assert!(
            mem_r.has_block(&cid.0),
            "the relay now holds the (inert) block"
        );

        // A different peer pulls the block from the relay — the writer is irrelevant now.
        let (b, mut b_rx) = ConciergeNode::spawn(key(42)).expect("offline-returning peer");
        b.dial(relay_addr.parse().unwrap()).expect("dial");
        let got = timeout(Duration::from_secs(20), async {
            loop {
                b.request_block(relay.peer_id, &cid.0)
                    .expect("queue request");
                if let Ok((_c, Some(bytes))) =
                    timeout(Duration::from_millis(500), next_block(&mut b_rx)).await
                {
                    return bytes;
                }
            }
        })
        .await
        .expect("the relayed block should arrive");
        assert_eq!(got, bytes, "the peer fetched the block from the relay");
    }

    #[tokio::test]
    async fn the_sync_driver_pulls_a_namespace_to_convergence_over_libp2p() {
        // Phase F end-to-end: A publishes a signed head + serves its blocks; B runs
        // the whole sync loop (exchange heads → verify → reconcile → pull missing,
        // verified → adopt heads) over the live connection and converges.
        use concierge_core::{
            Capability, CoreBinding, MemCli, Namespace, NamespaceScope, NetworkDescriptor, Node,
            Operation, RevocationSet,
        };

        // --- A: found a network, build a graph, publish a signed head ---
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = Arc::new(MemCli::new(dir_a.path()));
        let descriptor: NetworkDescriptor = mem_a.create_network("research-team").unwrap();
        let ns = Namespace::new(
            descriptor.network_id.clone(),
            NamespaceScope::Project("atlas".into()),
        );

        let child = mem_a
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"shared fact","kind":"reference"}"#.into(),
            })
            .unwrap();
        let head = mem_a.checkpoint("latest", &child, None).unwrap();
        let graph_size = mem_a.walk(&head).unwrap().len();
        assert!(graph_size >= 2);

        // A is a writer: a root-signed sync_write capability for the namespace.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let a_cap = Capability::issue(
            &mem_a.user_identity().unwrap(),
            ns.clone(),
            &mem_a.identity().unwrap().agent_id().0,
            vec![Operation::SyncRead, Operation::SyncWrite],
            now,
            24 * 3600,
            descriptor.membership_epoch,
            false,
        );
        mem_a
            .publish_head(&descriptor, &ns, vec![head.0.clone()], a_cap)
            .unwrap();

        let (a, mut a_rx) = ConciergeNode::spawn_with_provider(
            key(30),
            NodeConfig::default(),
            store_provider(mem_a.clone()),
        )
        .expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(31)).expect("b");
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a listen addr");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");

        // --- B: drive the sync to convergence ---
        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        let receipt = sync_from_peer(
            &b,
            &mut b_rx,
            a.peer_id,
            &mem_b,
            &descriptor,
            &ns,
            &RevocationSet::new(),
            Duration::from_secs(25),
        )
        .await
        .expect("sync should converge");

        assert_eq!(
            receipt.blocks_imported, graph_size,
            "pulled exactly the missing graph"
        );
        assert_eq!(receipt.heads, vec![head.0.clone()], "converged on A's head");
        assert!(
            mem_b.has_block(&head.0) && mem_b.has_block(&child.0),
            "B has the full graph"
        );
        assert_eq!(
            mem_b.local_heads(&descriptor.network_id, &ns.canonical()),
            vec![head.0.clone()]
        );

        // A second sync is a no-op (already converged).
        let again = sync_from_peer(
            &b,
            &mut b_rx,
            a.peer_id,
            &mem_b,
            &descriptor,
            &ns,
            &RevocationSet::new(),
            Duration::from_secs(25),
        )
        .await
        .expect("second sync");
        assert_eq!(again.blocks_imported, 0, "nothing left to pull");
    }

    #[tokio::test]
    async fn an_unknown_cid_returns_a_negative_without_revealing_other_blocks() {
        // A serves nothing; B asks for a CID and gets a deterministic "not here".
        let (a, mut a_rx) = ConciergeNode::spawn(key(23)).expect("a");
        let (b, mut b_rx) = ConciergeNode::spawn(key(24)).expect("b");
        a.listen("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .expect("queue listen");
        let a_addr = timeout(Duration::from_secs(5), next_listen(&mut a_rx))
            .await
            .expect("a listen addr");
        b.dial(a_addr.parse().unwrap()).expect("queue dial");

        let (_cid, bytes) = timeout(Duration::from_secs(20), async {
            loop {
                b.request_block(a.peer_id, "bafyMISSING")
                    .expect("queue request");
                if let Ok(received) =
                    timeout(Duration::from_millis(500), next_block(&mut b_rx)).await
                {
                    return received;
                }
            }
        })
        .await
        .expect("a negative reply should arrive");
        assert!(
            bytes.is_none(),
            "an unserved CID yields None, not an error or another block"
        );
    }
}
