use super::*;

pub(super) fn cmd_id() -> ExitCode {
    let mem = MemCli::new(workdir());
    match mem.agent_id() {
        Ok(id) => {
            println!("{}", id.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("id failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `follow <agentid>` — follow another install's AgentID (local allowlist).
pub(super) fn cmd_follow(args: &[String]) -> ExitCode {
    let Some(agent_id) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin follow <agentid>");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    match mem.follow(agent_id) {
        Ok(()) => {
            println!("following {agent_id}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("follow failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `nickname <agentid> <name>` — give an AgentID a local petname.
pub(super) fn cmd_nickname(args: &[String]) -> ExitCode {
    let (Some(agent_id), Some(nickname)) = (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) else {
        eprintln!("usage: concierge-plugin nickname <agentid> <name>");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    match mem.set_nickname(agent_id, nickname) {
        Ok(()) => {
            println!("{agent_id} → {nickname}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nickname failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `following` — list the AgentIDs you follow, with petnames where known.
pub(super) fn cmd_following() -> ExitCode {
    let mem = MemCli::new(workdir());
    match mem.social_book() {
        Ok(book) if book.following.is_empty() => {
            println!("not following anyone yet");
            ExitCode::SUCCESS
        }
        Ok(book) => {
            for agent_id in &book.following {
                match book.nickname_of(agent_id) {
                    Some(name) => println!("{agent_id}\t{name}"),
                    None => println!("{agent_id}"),
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("following failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `shared` — show verified shares from followed AgentIDs.
pub(super) fn cmd_shared() -> ExitCode {
    let mem = MemCli::new(workdir());
    match mem.shared_with_me() {
        Ok(items) if items.is_empty() => {
            println!("no verified shares from followed AgentIDs yet");
            ExitCode::SUCCESS
        }
        Ok(items) => {
            for item in items {
                match item.nickname.as_deref() {
                    Some(name) => println!(
                        "{}\t{}\t{}\t{}",
                        item.agent_id, name, item.root.0, item.pointer_cid.0
                    ),
                    None => println!("{}\t{}\t{}", item.agent_id, item.root.0, item.pointer_cid.0),
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("shared failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `msg <room> <text...>` — post a signed message to a room (AI-send lever applies).
pub(super) fn cmd_msg(args: &[String]) -> ExitCode {
    let Some(room) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin msg <room> <text...>");
        return ExitCode::from(2);
    };
    let text = args[2..].join(" ");
    if text.is_empty() {
        eprintln!("usage: concierge-plugin msg <room> <text...>");
        return ExitCode::from(2);
    }
    let mem = MemCli::new(workdir());
    match mem.post_message(room, &text) {
        Ok(cid) => {
            println!("posted to {room}: {}", cid.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("msg failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `thread <room>` — show the verified, chronological thread for a room.
pub(super) fn cmd_thread(args: &[String]) -> ExitCode {
    let Some(room) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin thread <room>");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    let book = mem.social_book().ok();
    match mem.room_thread(room) {
        Ok(thread) if thread.is_empty() => {
            println!("(no messages in {room})");
            ExitCode::SUCCESS
        }
        Ok(thread) => {
            for (_, env) in thread {
                let who = book
                    .as_ref()
                    .and_then(|b| b.nickname_of(&env.key).cloned())
                    .unwrap_or_else(|| format!("{}…", &env.key[..env.key.len().min(8)]));
                println!("[{}] {who}: {}", env.clock, env.payload);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("thread failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `room <humans-only|open|mute> <room> [agentid]` — set a room's AI-send lever
/// or mute an AgentID.
pub(super) fn cmd_room(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) {
        (Some("humans-only"), Some(room)) => result_line(
            mem.set_room_ai_send(room, "off"),
            format!("{room}: Human-only (AIs muted)"),
        ),
        (Some("open"), Some(room)) => result_line(
            mem.set_room_ai_send(room, "on"),
            format!("{room}: open brainstorm"),
        ),
        (Some("mention"), Some(room)) => result_line(
            mem.set_room_ai_send(room, "on_mention"),
            format!("{room}: mention-gated brainstorm"),
        ),
        (Some("mute"), Some(room)) => match args.get(3).map(String::as_str) {
            Some(agent) => result_line(
                mem.mute_in_room(room, agent),
                format!("muted {agent} in {room}"),
            ),
            None => {
                eprintln!("usage: concierge-plugin room mute <room> <agentid>");
                ExitCode::from(2)
            }
        },
        (Some("serve"), Some(room)) => cmd_room_serve(&mem, room, args),
        _ => {
            eprintln!(
                "usage: concierge-plugin room <humans-only|open|mention|mute|serve> <room> [args]"
            );
            ExitCode::from(2)
        }
    }
}

pub(super) fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub(super) fn envelope_declares_room(json: &str, expected_room: &str) -> bool {
    serde_json::from_str::<concierge_core::MessageEnvelope>(json)
        .is_ok_and(|envelope| envelope.room() == expected_room)
}

/// `room serve <room> [--listen ADDR] [--dial PEER] [--relay ADDR] [--trust-all]`
/// — join a gossipsub room over libp2p: republish this room's local messages so
/// peers catch up, and store inbound messages (verified, follow-gated) so they
/// appear in `thread`. `--relay` reserves a Circuit Relay v2 slot so peers behind
/// a NAT remain reachable (DCUtR then attempts a direct upgrade). Foreground;
/// Ctrl-C to stop. Hosting a relay for third-party traffic is explicit opt-in via
/// `--host-relay --external-address ADDR`. (Phase 5.7 transport.)
fn cmd_room_serve(mem: &MemCli, room: &str, args: &[String]) -> ExitCode {
    let listen = flag_value(args, "--listen").unwrap_or_else(|| "/ip4/0.0.0.0/tcp/0".to_string());
    let dial = flag_value(args, "--dial");
    let trust_all = args.iter().any(|a| a == "--trust-all");
    let host_relay = args.iter().any(|a| a == "--host-relay");
    let external_address = flag_value(args, "--external-address");
    if host_relay && external_address.is_none() {
        eprintln!("room serve: --host-relay requires --external-address ADDR");
        return ExitCode::from(2);
    }
    let secret = match mem.identity() {
        Ok(id) => id.secret_bytes(),
        Err(e) => {
            eprintln!("room serve: {e}");
            return ExitCode::FAILURE;
        }
    };
    let room = room.to_string();
    let external_addresses = match external_address {
        Some(address) => match address.parse::<concierge_net::Multiaddr>() {
            Ok(address) => vec![address],
            Err(e) => {
                eprintln!("room serve: bad --external-address: {e}");
                return ExitCode::from(2);
            }
        },
        None => Vec::new(),
    };

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("room serve: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(async move {
        let config = concierge_net::NodeConfig {
            host_relay,
            external_addresses,
            ..concierge_net::NodeConfig::default()
        };
        let (node, mut events) = match concierge_net::ConciergeNode::spawn_with_config(secret, config) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("room serve: {e}");
                return ExitCode::FAILURE;
            }
        };
        let listen_addr = match listen.parse() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("room serve: bad --listen: {e}");
                return ExitCode::from(2);
            }
        };
        if let Err(e) = node.listen(listen_addr) {
            eprintln!("room serve: listen: {e}");
            return ExitCode::FAILURE;
        }
        if let Err(e) = node.subscribe(&room) {
            eprintln!("room serve: subscribe: {e}");
            return ExitCode::FAILURE;
        }
        if let Some(d) = &dial {
            match d.parse() {
                Ok(a) => {
                    if let Err(e) = node.dial(a) {
                        eprintln!("room serve: dial: {e}");
                        return ExitCode::FAILURE;
                    }
                }
                Err(e) => {
                    eprintln!("room serve: bad --dial: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        if let Some(r) = flag_value(args, "--relay") {
            match r.parse::<concierge_net::Multiaddr>() {
                Ok(a) => {
                    if let Err(e) = node.reserve(a) {
                        eprintln!("room serve: relay request: {e}");
                        return ExitCode::FAILURE;
                    }
                    println!("requesting a relay reservation on {r}");
                }
                Err(e) => {
                    eprintln!("room serve: bad --relay: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        if host_relay {
            println!("hosting a bounded Circuit Relay v2 service");
        }
        println!("serving room `{room}` as {}  (Ctrl-C to stop)", node.peer_id);

        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
        let mut published = std::collections::BTreeSet::new();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(concierge_net::NodeEvent::Listening(addr)) => println!("listening: {addr}"),
                    Some(concierge_net::NodeEvent::Message { room: inbound_room, data, source }) => {
                        if inbound_room != room {
                            eprintln!("← rejected cross-room message for `{inbound_room}` on `{room}`");
                            continue;
                        }
                        let json = String::from_utf8_lossy(&data);
                        if !envelope_declares_room(&json, &room) {
                            eprintln!("← rejected invalid or cross-room envelope on `{room}`");
                            continue;
                        }
                        match mem.store_inbound_message(&json, trust_all) {
                            Ok(Some(cid)) => println!("← stored {} from {}", cid.0, source.unwrap_or_default()),
                            Ok(None) => println!("← rejected (bad signature or not followed)"),
                            Err(e) => eprintln!("inbound store error: {e}"),
                        }
                    }
                    Some(concierge_net::NodeEvent::Subscribed { room }) => println!("subscribed: {room}"),
                    Some(concierge_net::NodeEvent::Published { message_id, duplicate, .. }) => {
                        published.insert(message_id);
                        if duplicate && std::env::var("CONCIERGE_NET_DEBUG").is_ok() {
                            eprintln!("publish deduplicated");
                        }
                    }
                    Some(concierge_net::NodeEvent::ConnectionEstablished { peer_id, relayed }) => {
                        println!("connected: {peer_id} ({})", if relayed { "relayed" } else { "direct" });
                    }
                    Some(concierge_net::NodeEvent::ExternalAddressAdded { address }) => {
                        println!("external address configured: {address}");
                    }
                    Some(concierge_net::NodeEvent::RelayReservationAccepted { relay_peer_id, renewed }) => {
                        println!("relay reservation {}: {relay_peer_id}", if renewed { "renewed" } else { "accepted" });
                    }
                    Some(concierge_net::NodeEvent::RelayCircuitEstablished { peer_id, direction }) => {
                        println!("relay circuit established: {direction} {peer_id}");
                    }
                    Some(concierge_net::NodeEvent::DirectConnectionUpgrade { peer_id, succeeded, error }) => {
                        if succeeded {
                            println!("DCUtR direct upgrade succeeded: {peer_id}");
                        } else {
                            eprintln!("DCUtR direct upgrade failed for {peer_id}: {}", error.unwrap_or_default());
                        }
                    }
                    Some(concierge_net::NodeEvent::OperationFailed { operation, error }) => {
                        eprintln!("network {operation} failed: {error}");
                    }
                    // Phase F sync/NAT events are not surfaced by the room-serve loop.
                    Some(_) => {}
                    None => break,
                },
                _ = ticker.tick() => {
                    match mem.room_message_envelopes_with_cids(&room) {
                        Ok(envelopes) => for (cid, envelope) in envelopes {
                            let bytes = envelope.into_bytes();
                            if !published.contains(&concierge_net::content_message_id(&bytes)) {
                                let plan = match mem.build_egress_plan_for_backend(
                                    &cid,
                                    EgressOperation::PublicRoomAttach,
                                    &format!("gossipsub-room:{room}"),
                                    "public-room",
                                ) {
                                    Ok(plan) => plan,
                                    Err(e) => {
                                        eprintln!("public-room attach plan failed: {e}");
                                        continue;
                                    }
                                };
                                if let Err(e) = mem.execute_public_room_attach(&plan, |_| {
                                    node.publish(&room, bytes)
                                        .map_err(concierge_core::Error::Io)
                                }) {
                                    eprintln!("network publish queue failed: {e}");
                                }
                            }
                        },
                        Err(e) => eprintln!("room history read failed: {e}"),
                    }
                }
            }
        }
        ExitCode::SUCCESS
    })
}
