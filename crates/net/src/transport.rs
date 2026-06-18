use super::sync::{noop_provider, Pending, MAX_PENDING_DIRECT_MESSAGES};
use super::*;

/// Topic prefix so Concierge rooms don't collide with other gossipsub apps.
pub const ROOM_PREFIX: &str = "concierge/room/";
pub const PRIVATE_NAMESPACE_PREFIX: &str = "concierge/private/";
pub(super) const RELAY_MAX_RESERVATIONS: usize = 32;
pub(super) const RELAY_MAX_RESERVATIONS_PER_PEER: usize = 2;
pub(super) const RELAY_MAX_CIRCUITS: usize = 32;
pub(super) const RELAY_MAX_CIRCUITS_PER_PEER: usize = 2;
pub(super) const RELAY_MAX_CIRCUIT_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const RELAY_MAX_CIRCUIT_DURATION: Duration = Duration::from_secs(30 * 60);

/// How often the Kademlia client re-runs its bootstrap query to refresh the
/// routing table (mirrors universal-connectivity's `rust-peer` ~300s cadence).
const KADEMLIA_BOOTSTRAP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The well-known DHT key every Concierge advertises *and* queries under, so nodes
/// discover each other on the **public IPFS DHT with no central rendezvous server**
/// (rendezvous-via-DHT). A node calls `start_providing(KEY)` to announce "I'm a
/// Concierge" and `get_providers(KEY)` to find the others; the only shared infra is
/// the public libp2p bootstrap nodes, which nobody here runs. Bump the version suffix
/// to fork the discovery namespace (e.g. for an incompatible protocol change).
const CONCIERGE_RENDEZVOUS_KEY: &[u8] = b"concierge/rendezvous/v1";

/// How often we re-query the DHT for fellow Concierge providers (and, until it lands,
/// retry our own provider announcement). Provider records expire on the order of a
/// day and rust-libp2p auto-republishes ours; this interval is just discovery polling.
const RENDEZVOUS_QUERY_INTERVAL: Duration = Duration::from_secs(120);

/// The libp2p identify `protocol_version` every Concierge node advertises. Any peer
/// that reports this in its identify info is a fellow Concierge (drawn as a brain on
/// the map); everyone else is a generic libp2p/IPFS node (a white star).
pub const CONCIERGE_PROTOCOL_VERSION: &str = "/concierge/1.0.0";

/// The Amino (public IPFS) DHT protocol id — the default `kad::Config` protocol and
/// the one the public bootstrap network speaks. A peer that advertises it (over
/// identify) is a DHT server whose listen addresses we feed into our routing table.
/// Without this wiring, Kademlia never learns peers' addresses and the table never
/// grows beyond the bootstrap nodes (see the libp2p identify/kad docs).
const IPFS_KAD_PROTOCOL: &str = "/ipfs/kad/1.0.0";

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
pub(super) struct Behaviour {
    gossipsub: gossipsub::Behaviour,
    /// Use a relay (dial peers through it, reserve a slot on it).
    relay_client: relay::client::Behaviour,
    /// Be a relay for others only when the operator explicitly opts in.
    pub(super) relay: Toggle<relay::Behaviour>,
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
        /// The remote multiaddr of this connection — a real public IP for direct
        /// internet connections (used to geo-locate the peer on the map).
        address: String,
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
    /// A generic libp2p/IPFS peer observed by passive discovery. Unlike
    /// [`NodeEvent::RendezvousDiscovered`], this does not imply the peer runs
    /// Concierge; the GUI renders these as anonymous network stars.
    PeerDiscovered {
        peer_id: String,
        source: &'static str,
        addresses: Vec<String>,
    },
    /// A peer completed the libp2p identify exchange, telling us its
    /// `protocol_version` (and `agent_version`). When `protocol_version` matches
    /// [`CONCIERGE_PROTOCOL_VERSION`] the peer is a fellow Concierge — the signal the
    /// GUI uses to draw it as a brain rather than a generic network star.
    PeerIdentified {
        peer_id: String,
        protocol_version: String,
        agent_version: String,
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

/// Recover the concierge AgentID ("username", hex Ed25519 public key) from a libp2p
/// PeerID — the inverse of [`peer_id_from_ed25519_hex`]. Ed25519 PeerIDs embed the
/// public key directly (identity multihash, code 0x00), so no network lookup is needed.
/// Returns `None` for non-Ed25519 / non-identity-hashed peers.
pub fn ed25519_hex_from_peer_id(peer: &PeerId) -> Option<String> {
    let mh = libp2p::multihash::Multihash::<64>::from_bytes(&peer.to_bytes()).ok()?;
    if mh.code() != 0 {
        return None; // not an identity multihash — the key isn't embedded
    }
    let public = identity::PublicKey::try_decode_protobuf(mh.digest()).ok()?;
    let ed = public.try_into_ed25519().ok()?;
    Some(encode_hex(&ed.to_bytes()))
}

/// Encode bytes as lowercase hex (no external dependency).
fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    out
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

pub(super) fn build_swarm(
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
        // Wrap the TCP+QUIC transports with DNS resolution. Without this, `/dnsaddr`
        // and `/dns*` multiaddrs can't be resolved — and every public IPFS bootstrap
        // node is a `/dnsaddr/bootstrap.libp2p.io/...` address. No DNS ⇒ the node never
        // reaches a bootstrap peer, the Kademlia routing table never fills from the
        // global network, and discovery collapses to mDNS LAN peers only. With it, the
        // DHT bootstraps and the map populates with peers from across the network.
        .with_dns()
        .map_err(|e| e.to_string())?
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
                    CONCIERGE_PROTOCOL_VERSION.to_string(),
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

pub(super) fn build_gossipsub_config(config: &NodeConfig) -> Result<gossipsub::Config, String> {
    gossipsub::ConfigBuilder::default()
        .validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(config.max_message_bytes)
        .message_id_fn(|message| gossipsub::MessageId::from(content_message_id(&message.data)))
        .published_message_ids_cache_time(Duration::from_secs(24 * 60 * 60))
        .duplicate_cache_time(Duration::from_secs(24 * 60 * 60))
        .build()
        .map_err(|error| error.to_string())
}

pub(super) fn relay_server_config(config: &NodeConfig) -> relay::Config {
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
    // Distinct addresses peers have observed us at (identify `observed_addr`) — i.e.
    // our own public IP as seen from the internet. Emitted once each so the GUI can
    // geo-locate *this* node on the map; deduped to avoid per-identify spam.
    let mut observed_addrs: HashSet<String> = HashSet::new();
    // Kick off the initial DHT bootstrap (when Kademlia is enabled); the periodic
    // interval keeps the routing table fresh thereafter.
    if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
        let _ = kad.bootstrap();
    }
    // Serverless wide-area discovery: advertise ourselves under the shared Concierge
    // key on the public DHT and poll it for fellow Concierges. `rdv_provided` flips
    // once our `start_providing` query lands (it can't until the routing table has a
    // peer to store the record on, so we retry on each tick until it succeeds).
    let rdv_key = kad::RecordKey::new(&CONCIERGE_RENDEZVOUS_KEY);
    let mut rdv_provided = false;
    let mut rdv_tick = tokio::time::interval(RENDEZVOUS_QUERY_INTERVAL);
    rdv_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = rdv_tick.tick() => {
                // Re-announce ourselves (until it lands) and re-query for peers. Both
                // are no-ops when the DHT is disabled on this node.
                if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                    if !rdv_provided {
                        let _ = kad.start_providing(rdv_key.clone());
                    }
                    kad.get_providers(rdv_key.clone());
                }
            }
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
                        address: endpoint.get_remote_address().to_string(),
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
                SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                    peer_id,
                    info,
                    ..
                })) => {
                    // Hook identify → Kademlia. Identify is the only way kad learns the
                    // listen addresses of peers it meets; without feeding them in, the
                    // routing table never grows past the bootstrap nodes and discovery
                    // stalls (libp2p docs: identify "must be manually hooked up to
                    // Kademlia through calls to add_address"). Only DHT servers (peers
                    // that advertise the kad protocol) belong in the table.
                    let kad_protocol = StreamProtocol::new(IPFS_KAD_PROTOCOL);
                    let speaks_kad = info.protocols.contains(&kad_protocol);
                    if speaks_kad {
                        if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                            for addr in &info.listen_addrs {
                                kad.add_address(&peer_id, addr.clone());
                            }
                        }
                    }
                    // The peer reports the address it observed us at — our own public
                    // IP as seen from the internet. Surface each distinct one once so the
                    // GUI can place THIS node at its true location (self-geo on the map).
                    let observed = info.observed_addr.to_string();
                    if observed_addrs.insert(observed.clone()) {
                        emit(&evt, NodeEvent::ExternalAddressAdded { address: observed });
                    }
                    // The peer also told us what application it runs. A Concierge
                    // advertises `CONCIERGE_PROTOCOL_VERSION`; the GUI uses this to draw
                    // it as a brain. Generic IPFS nodes report their own version string.
                    emit(&evt, NodeEvent::PeerIdentified {
                        peer_id: peer_id.to_string(),
                        protocol_version: info.protocol_version,
                        agent_version: info.agent_version,
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    // A peer appeared on the LAN: treat it as an explicit gossipsub
                    // peer, register its address so a later dial-by-id resolves, and
                    // dial it now (no manual address exchange on a local network).
                    for (peer, addr) in list {
                        emit(&evt, NodeEvent::PeerDiscovered {
                            peer_id: peer.to_string(),
                            source: "lan/mdns",
                            addresses: vec![addr.to_string()],
                        });
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
                SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::StartProviding(Ok(_)),
                    ..
                })) => {
                    // Our "I'm a Concierge" record is now stored on the DHT; stop retrying.
                    rdv_provided = true;
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::GetProviders(Ok(providers)),
                    ..
                })) => {
                    // Fellow Concierges that advertised under the shared key. Surface each
                    // for the map and resolve+dial it through the existing routing path
                    // (get_closest_peers → PeerRouted → dial), skipping ourselves.
                    let found = match providers {
                        kad::GetProvidersOk::FoundProviders { providers, .. } => providers,
                        kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. } => HashSet::new(),
                    };
                    for provider in found {
                        if provider == peer_id {
                            continue;
                        }
                        emit(&evt, NodeEvent::PeerDiscovered {
                            peer_id: provider.to_string(),
                            source: "rendezvous",
                            addresses: Vec::new(),
                        });
                        if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
                            routing.insert(provider);
                            kad.get_closest_peers(provider);
                        }
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad::Event::RoutingUpdated {
                    peer,
                    addresses,
                    ..
                })) => {
                    emit(&evt, NodeEvent::PeerDiscovered {
                        peer_id: peer.to_string(),
                        source: "dht",
                        addresses: addresses.iter().map(|a| a.to_string()).collect(),
                    });
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
