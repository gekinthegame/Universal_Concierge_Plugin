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
                    ..
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
                    ..
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
