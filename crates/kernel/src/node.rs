//! Kernel-owned libp2p discovery node and `/api/peers` projection.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use concierge_core::MemCli;
use concierge_net::{
    ed25519_hex_from_peer_id, peer_id_from_ed25519_hex, peer_id_in, store_provider, ConciergeNode,
    Multiaddr, NodeConfig, NodeEvent, Peer, CONCIERGE_PROTOCOL_VERSION,
};

const MAX_DISCOVERY_PEERS: usize = 20_000;
const DISCOVERY_PEER_TTL_SECS: u64 = 600;

#[derive(Debug, Clone)]
struct PeerInfo {
    peer_id: String,
    status: &'static str,
    source: String,
    relayed: bool,
    addresses: Vec<String>,
    last_seen: u64,
    is_concierge: bool,
}

pub struct KernelNode {
    _runtime: tokio::runtime::Runtime,
    _node: ConciergeNode,
    agent_id: String,
    peer_id: String,
    addrs: Arc<Mutex<Vec<String>>>,
    peers: Arc<Mutex<BTreeMap<String, PeerInfo>>>,
}

impl std::fmt::Debug for KernelNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelNode")
            .field("peer_id", &self.peer_id)
            .finish()
    }
}

impl KernelNode {
    pub fn spawn(mem: MemCli) -> Result<Self, String> {
        let identity = mem.identity().map_err(|e| e.to_string())?;
        let secret = identity.secret_bytes();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("chat runtime: {e}"))?;
        let provider = store_provider(Arc::new(mem.clone()));
        let net_cfg = mem.config().map(|c| c.network).unwrap_or_default();
        let mut node_config = NodeConfig::default();
        if net_cfg.rendezvous_server || std::env::var("CONCIERGE_RENDEZVOUS_SERVER").is_ok() {
            node_config.rendezvous_point = true;
        }
        let (node, mut events) = {
            let _enter = runtime.enter();
            ConciergeNode::spawn_with_provider(secret, node_config, provider)?
        };

        let listen_addrs: Vec<String> = match std::env::var("CONCIERGE_LISTEN_PORT")
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
            .or(Some(net_cfg.listen_port).filter(|p| *p != 0))
        {
            Some(port) => vec![
                format!("/ip4/0.0.0.0/tcp/{port}"),
                format!("/ip4/0.0.0.0/udp/{port}/quic-v1"),
            ],
            None => vec![
                "/ip4/0.0.0.0/tcp/0".to_string(),
                "/ip4/0.0.0.0/udp/0/quic-v1".to_string(),
            ],
        };
        for listen in &listen_addrs {
            if let Ok(addr) = listen.parse::<Multiaddr>() {
                let _ = node.listen(addr);
            }
        }

        if let Some(addr) = std::env::var("CONCIERGE_RENDEZVOUS")
            .ok()
            .or_else(|| Some(net_cfg.rendezvous.clone()).filter(|s| !s.trim().is_empty()))
            .and_then(|s| s.trim().parse::<Multiaddr>().ok())
        {
            if let Some(point) = peer_id_in(&addr) {
                let rdv_node = node.clone();
                runtime.spawn(async move {
                    let _ = rdv_node.dial(addr.clone());
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    loop {
                        let _ = rdv_node.dial(addr.clone());
                        let _ = rdv_node.register_rendezvous(point, "concierge");
                        let _ = rdv_node.discover_rendezvous(point, "concierge");
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                });
            }
        }

        for room in known_rooms(&mem) {
            let _ = node.subscribe(&room);
        }

        let agent_id = identity.agent_id().0;
        let peer_id = node.peer_id.to_string();
        let addrs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let peers: Arc<Mutex<BTreeMap<String, PeerInfo>>> = Arc::new(Mutex::new(BTreeMap::new()));

        let drain_mem = mem.clone();
        let drain_addrs = addrs.clone();
        let drain_peers = peers.clone();
        let drain_node = node.clone();
        runtime.spawn(async move {
            let touch_peer = |id: String,
                              status: &'static str,
                              source: &str,
                              relayed: bool,
                              addresses: Vec<String>| {
                if let Ok(mut map) = drain_peers.lock() {
                    let now = now_unix();
                    let entry = map.entry(id.clone()).or_insert_with(|| PeerInfo {
                        peer_id: id,
                        status: "discovered",
                        source: source.to_string(),
                        relayed: true,
                        addresses: Vec::new(),
                        last_seen: now,
                        is_concierge: false,
                    });
                    if status == "connected" || entry.status != "connected" {
                        entry.status = status;
                    }
                    if !source.is_empty() {
                        entry.source = source.to_string();
                    }
                    if status == "connected" {
                        entry.relayed = relayed;
                    }
                    if !addresses.is_empty() {
                        entry.addresses = addresses;
                    }
                    entry.last_seen = now;
                    prune_discovery_peers(&mut map, now);
                }
            };

            while let Some(event) = events.recv().await {
                match event {
                    NodeEvent::DirectMessage {
                        from_peer,
                        data,
                        delivery_id,
                    } => {
                        let json = String::from_utf8_lossy(&data).into_owned();
                        let accepted = if let Some(card_json) = parse_contact_card(&json) {
                            approved_contact_card_author(&drain_mem, &card_json, &from_peer)
                                .and_then(|_| drain_mem.import_card(&card_json).ok())
                                .is_some()
                        } else {
                            drain_mem.receive_message(&json).is_ok()
                        };
                        let _ = drain_node.acknowledge_dm(delivery_id, accepted);
                    }
                    NodeEvent::Message { data, .. } => {
                        let json = String::from_utf8_lossy(&data).into_owned();
                        let _ = drain_mem.receive_message(&json);
                    }
                    NodeEvent::DirectMessageDelivered { message_id, .. } => {
                        let _ = drain_mem.mark_outbound_delivered(&message_id);
                    }
                    NodeEvent::Listening(addr)
                    | NodeEvent::ExternalAddressAdded { address: addr } => {
                        if let Ok(mut list) = drain_addrs.lock() {
                            if !list.contains(&addr) {
                                list.push(addr);
                            }
                        }
                    }
                    NodeEvent::ConnectionEstablished {
                        peer_id,
                        relayed,
                        address,
                    } => {
                        let source = if relayed { "relay" } else { "lan/direct" };
                        touch_peer(peer_id, "connected", source, relayed, vec![address]);
                    }
                    NodeEvent::DirectConnectionUpgrade {
                        peer_id,
                        succeeded: true,
                        ..
                    } => touch_peer(peer_id, "connected", "lan/direct", false, Vec::new()),
                    NodeEvent::DirectConnectionUpgrade { .. } => {}
                    NodeEvent::RendezvousDiscovered { peer_id, addresses } => {
                        touch_peer(peer_id, "discovered", "rendezvous", true, addresses);
                    }
                    NodeEvent::PeerRouted { peer_id, addresses } => {
                        touch_peer(peer_id, "discovered", "dht", true, addresses);
                    }
                    NodeEvent::PeerDiscovered {
                        peer_id,
                        source,
                        addresses,
                    } => {
                        touch_peer(peer_id, "discovered", source, false, addresses);
                    }
                    NodeEvent::PeerIdentified {
                        peer_id,
                        protocol_version,
                        ..
                    } => {
                        let is_concierge = protocol_version == CONCIERGE_PROTOCOL_VERSION;
                        if let Ok(mut map) = drain_peers.lock() {
                            let now = now_unix();
                            let entry = map.entry(peer_id.clone()).or_insert_with(|| PeerInfo {
                                peer_id,
                                status: "discovered",
                                source: "identify".to_string(),
                                relayed: true,
                                addresses: Vec::new(),
                                last_seen: now,
                                is_concierge: false,
                            });
                            entry.last_seen = now;
                            if is_concierge {
                                entry.is_concierge = true;
                            }
                            prune_discovery_peers(&mut map, now);
                        }
                    }
                    _ => {}
                }
            }
        });

        let retry_mem = mem.clone();
        let retry_node = node.clone();
        runtime.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tick.tick().await;
                let pending = retry_mem.pending_outbound().unwrap_or_default();
                let card_env = retry_mem.my_card().ok().map(|card| {
                    serde_json::json!({ "type": "contact-card", "card": card }).to_string()
                });
                let contacts = retry_mem.approved_contacts().unwrap_or_default();
                for (_id, recipient, envelope) in pending {
                    if let Some(peer) = peer_id_from_ed25519_hex(&recipient) {
                        let _ = retry_node.find_peer(peer);
                        let _ = retry_node.send_dm(peer, envelope.into_bytes());
                    }
                }
                if let Some(env) = &card_env {
                    for recipient in &contacts {
                        if let Some(peer) = peer_id_from_ed25519_hex(recipient) {
                            let _ = retry_node.find_peer(peer);
                            let _ = retry_node.send_dm(peer, env.clone().into_bytes());
                        }
                    }
                }
            }
        });

        Ok(Self {
            _runtime: runtime,
            _node: node,
            agent_id,
            peer_id,
            addrs,
            peers,
        })
    }

    pub fn peers_json(&self, mem: &MemCli) -> serde_json::Value {
        let now = now_unix();
        let self_addrs = self.addrs.lock().map(|a| a.clone()).unwrap_or_default();
        let mut peers: Vec<PeerInfo> = self
            .peers
            .lock()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default();
        peers.retain(|p| {
            p.status == "connected" || now.saturating_sub(p.last_seen) < DISCOVERY_PEER_TTL_SECS
        });
        peers.sort_by(|a, b| {
            (b.status == "connected")
                .cmp(&(a.status == "connected"))
                .then(b.last_seen.cmp(&a.last_seen))
        });
        let total = peers.len();
        let connected = peers.iter().filter(|p| p.status == "connected").count();
        let concierge_peers: HashSet<String> = mem
            .approved_contacts()
            .unwrap_or_default()
            .iter()
            .filter_map(|agent_id| peer_id_from_ed25519_hex(agent_id).map(|p| p.to_string()))
            .collect();
        let peers_json: Vec<serde_json::Value> = peers
            .iter()
            .map(|p| {
                let is_concierge = p.is_concierge
                    || p.source == "rendezvous"
                    || concierge_peers.contains(&p.peer_id);
                let username = is_concierge
                    .then(|| {
                        p.peer_id
                            .parse::<Peer>()
                            .ok()
                            .and_then(|peer| ed25519_hex_from_peer_id(&peer))
                    })
                    .flatten();
                serde_json::json!({
                    "peer_id": p.peer_id,
                    "status": p.status,
                    "source": p.source,
                    "relayed": p.relayed,
                    "addresses": p.addresses,
                    "last_seen": p.last_seen,
                    "is_concierge": is_concierge,
                    "username": username,
                    "lat": Option::<f64>::None,
                    "lon": Option::<f64>::None,
                    "country": Option::<String>::None,
                })
            })
            .collect();
        serde_json::json!({
            "self": {
                "peer_id": self.peer_id,
                "agent_id": self.agent_id,
                "online": true,
                "addresses": self_addrs,
                "lat": Option::<f64>::None,
                "lon": Option::<f64>::None,
            },
            "peers": peers_json,
            "total": total,
            "connected": connected,
        })
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn prune_discovery_peers(map: &mut BTreeMap<String, PeerInfo>, now: u64) {
    map.retain(|_, peer| {
        peer.status == "connected" || now.saturating_sub(peer.last_seen) < DISCOVERY_PEER_TTL_SECS
    });
    if map.len() <= MAX_DISCOVERY_PEERS {
        return;
    }
    let mut removable: Vec<(String, u64)> = map
        .iter()
        .filter(|(_, peer)| peer.status != "connected")
        .map(|(id, peer)| (id.clone(), peer.last_seen))
        .collect();
    removable.sort_by_key(|(_, last_seen)| *last_seen);
    for (id, _) in removable {
        if map.len() <= MAX_DISCOVERY_PEERS {
            break;
        }
        map.remove(&id);
    }
}

fn known_rooms(mem: &MemCli) -> Vec<String> {
    let mut rooms: BTreeSet<String> = BTreeSet::new();
    if let Ok(book) = mem.room_book() {
        rooms.extend(book.rooms.keys().cloned());
    }
    if let Ok(names) = mem.names() {
        for (name, _) in names {
            if let Some(room) = name.strip_prefix("room-latest-") {
                rooms.insert(room.to_string());
            }
        }
    }
    rooms.into_iter().collect()
}

fn parse_contact_card(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("type").and_then(|t| t.as_str()) == Some("contact-card") {
        value.get("card").map(|c| c.to_string())
    } else {
        None
    }
}

fn approved_contact_card_author(mem: &MemCli, card_json: &str, from_peer: &str) -> Option<String> {
    let card: concierge_core::naming::ContactCard = serde_json::from_str(card_json).ok()?;
    if !card.verify() {
        return None;
    }
    let agent_id = card.agent_id().ok()?.0;
    let from_matches = peer_id_from_ed25519_hex(&agent_id)
        .map(|peer| peer.to_string() == from_peer)
        .unwrap_or(false);
    (from_matches && mem.is_contact(&agent_id)).then_some(agent_id)
}
