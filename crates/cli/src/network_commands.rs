use super::*;

pub(super) fn cmd_network(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("create") => {
            let Some(name) = args.get(2) else {
                eprintln!("usage: concierge-plugin network create <name>");
                return ExitCode::from(2);
            };
            match mem.create_network(name) {
                Ok(descriptor) => {
                    println!("created network `{}`", descriptor.name);
                    println!("  network-id: {}", descriptor.network_id.0);
                    println!("  root user:  {}", descriptor.root_user_ids[0].0);
                    println!("  epoch:      {}", descriptor.membership_epoch);
                    println!("this device joined as a member (sync_read, sync_write)");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("network create failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("list") | None => match mem.networks() {
            Ok(networks) => {
                if networks.is_empty() {
                    println!("no networks (found one with `network create <name>`)");
                } else {
                    for n in networks {
                        println!(
                            "{}\t{}\tepoch {}",
                            n.name, n.network_id.0, n.membership_epoch
                        );
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("network list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("show") => {
            let networks = match mem.networks() {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("network show failed: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let target = args.get(2).map(String::as_str);
            let descriptor = match target {
                Some(id) => networks.into_iter().find(|n| n.network_id.0 == id),
                None => networks.into_iter().next(),
            };
            let Some(descriptor) = descriptor else {
                eprintln!("no matching network");
                return ExitCode::FAILURE;
            };
            let descriptor_ok = descriptor.verify().is_ok();
            println!(
                "network `{}`  ({})",
                descriptor.name, descriptor.network_id.0
            );
            println!(
                "  descriptor signature: {}",
                if descriptor_ok { "valid" } else { "INVALID" }
            );
            println!(
                "  root users: {}",
                descriptor
                    .root_user_ids
                    .iter()
                    .map(|u| u.0.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("  epoch: {}", descriptor.membership_epoch);
            match mem.device_membership(&descriptor.network_id) {
                Ok(Some(cert)) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let valid = concierge_core::verify_membership(
                        &cert,
                        &descriptor,
                        now,
                        &concierge_core::RevocationSet::new(),
                    )
                    .is_ok();
                    println!(
                        "  this device: {} ({})",
                        cert.subject_id,
                        if valid {
                            "membership valid"
                        } else {
                            "membership INVALID"
                        }
                    );
                    println!("  capabilities: {}", cert.capabilities.join(", "));
                }
                Ok(None) => println!("  this device: no membership certificate"),
                Err(e) => println!("  this device: error reading cert: {e}"),
            }
            ExitCode::SUCCESS
        }
        Some("pair") => network_pair(&mem, args),
        Some("respond") => network_respond(args),
        Some("approve") => network_approve(&mem, args),
        Some("accept") => network_accept(&mem, args),
        Some("grant") => network_grant(&mem, args),
        Some("revoke") => network_revoke(&mem, args),
        Some(other) => {
            eprintln!("unknown network subcommand `{other}` — use create|list|show|pair|respond|approve|accept|grant|revoke");
            ExitCode::from(2)
        }
    }
}

/// Read a JSON value from a file path argument.
fn read_json_file<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))
}

/// Parse a namespace like `personal`, `project:atlas`, `room:x`, `agent:y`, `all`.
fn parse_namespace(
    network_id: concierge_core::NetworkId,
    s: &str,
) -> Option<concierge_core::Namespace> {
    use concierge_core::{Namespace, NamespaceScope};
    let scope = match s {
        "all" | "*" => NamespaceScope::All,
        "personal" => NamespaceScope::Personal,
        other => {
            let (kind, id) = other.split_once(':')?;
            match kind {
                "project" => NamespaceScope::Project(id.to_string()),
                "room" => NamespaceScope::Room(id.to_string()),
                "agent" => NamespaceScope::Agent(id.to_string()),
                _ => return None,
            }
        }
    };
    Some(Namespace::new(network_id, scope))
}

fn parse_operations(csv: &str) -> Result<Vec<concierge_core::Operation>, String> {
    csv.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|tok| {
            serde_json::from_value(serde_json::Value::String(tok.to_string()))
                .map_err(|_| format!("unknown operation `{tok}`"))
        })
        .collect()
}

/// `network pair [--rendezvous A] [--network ID]` — admin side: mint a one-use
/// pairing offer for a network and print it (share as a QR/code; carries no secrets).
fn network_pair(mem: &MemCli, args: &[String]) -> ExitCode {
    let networks = match mem.networks() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("network pair failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let target = flag_value(args, "--network");
    let descriptor = match target {
        Some(id) => networks.into_iter().find(|n| n.network_id.0 == id),
        None => networks.into_iter().next(),
    };
    let Some(descriptor) = descriptor else {
        eprintln!("no network to pair into — create one with `network create <name>`");
        return ExitCode::from(2);
    };
    let rendezvous =
        flag_value(args, "--rendezvous").unwrap_or_else(|| "/ip4/127.0.0.1/tcp/4001".to_string());
    match mem.create_pairing_offer(&descriptor.network_id, &rendezvous) {
        Ok(offer) => {
            eprintln!(
                "# pairing offer for `{}` (expires in 10 min, one-use)",
                descriptor.name
            );
            eprintln!(
                "# share this with the new device, then run `network respond <offer.json>` there"
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&offer).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("network pair failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `network respond <offer.json>` — new device: prove possession of this device's
/// key and print the response + the confirmation phrase to compare with the admin.
fn network_respond(args: &[String]) -> ExitCode {
    let Some(path) = args.get(2) else {
        eprintln!("usage: concierge-plugin network respond <offer.json>");
        return ExitCode::from(2);
    };
    let offer: concierge_core::PairingOffer = match read_json_file(path) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = offer.verify() {
        eprintln!("offer rejected: {e}");
        return ExitCode::FAILURE;
    }
    let mem = MemCli::new(workdir());
    let device = match mem.identity() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("identity error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let response = concierge_core::PairingResponse::create(&offer, &device);
    eprintln!(
        "# confirmation phrase (must match the admin's): {}",
        concierge_core::confirmation_phrase(&offer, &response)
    );
    eprintln!("# send this response back; the admin runs `network approve <response.json> --namespace … --ops …`");
    println!(
        "{}",
        serde_json::to_string_pretty(&response).unwrap_or_default()
    );
    ExitCode::SUCCESS
}

/// `network approve <response.json> --namespace NS --ops a,b` — admin: verify the
/// response, show the confirmation phrase to compare, and issue the scoped grant.
fn network_approve(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(path) = args.get(2) else {
        eprintln!(
            "usage: concierge-plugin network approve <response.json> --namespace <ns> --ops <a,b>"
        );
        return ExitCode::from(2);
    };
    let response: concierge_core::PairingResponse = match read_json_file(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(ns_arg) = flag_value(args, "--namespace") else {
        eprintln!("need --namespace <personal|project:ID|room:ID|agent:ID|all>");
        return ExitCode::from(2);
    };
    let ops = match parse_operations(&flag_value(args, "--ops").unwrap_or_default()) {
        Ok(ops) if !ops.is_empty() => ops,
        _ => {
            eprintln!("need --ops <comma-separated ops, e.g. sync_read,message_receive>");
            return ExitCode::from(2);
        }
    };
    // Find the offer's network (the response references it via the stored offer).
    let networks = mem.networks().unwrap_or_default();
    let Some(descriptor) = networks.into_iter().next() else {
        eprintln!("no network on this device");
        return ExitCode::from(2);
    };
    let Some(namespace) = parse_namespace(descriptor.network_id.clone(), &ns_arg) else {
        eprintln!("bad --namespace `{ns_arg}`");
        return ExitCode::from(2);
    };
    match mem.complete_pairing(
        &response,
        &[(namespace, ops)],
        concierge_core::DEFAULT_CERT_TTL_SECS,
    ) {
        Ok(grant) => {
            eprintln!(
                "# approved — send this grant to the new device: `network accept <grant.json>`"
            );
            eprintln!(
                "# {} capability(ies) granted to {}",
                grant.capabilities.len(),
                grant.membership.subject_id
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&grant).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("approve failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `network accept <grant.json>` — new device: verify and persist the grant. After
/// this the device is a member holding exactly the approved scopes.
fn network_accept(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(path) = args.get(2) else {
        eprintln!("usage: concierge-plugin network accept <grant.json>");
        return ExitCode::from(2);
    };
    let grant: concierge_core::PairingGrant = match read_json_file(path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    match mem.accept_pairing_grant(&grant) {
        Ok(()) => {
            println!(
                "joined network `{}` ({})",
                grant.descriptor.name, grant.descriptor.network_id.0
            );
            for cap in &grant.capabilities {
                println!(
                    "  {} → {}",
                    cap.namespace.canonical(),
                    cap.operations
                        .iter()
                        .map(|o| format!("{o:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("accept failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `actor <list | enroll <actor-id> --namespace NS --ops a,b>` — Phase N · Phase A/B.
/// Enroll a harness/agent hosted by this device with a device-signed certificate,
/// scoped to operations that are a subset of this device's own (least privilege).
pub(super) fn cmd_actor(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        eprintln!("no network on this device — create one with `network create <name>`");
        return ExitCode::from(2);
    };
    match args.get(1).map(String::as_str) {
        Some("list") | None => match mem.actor_certificates(&descriptor.network_id) {
            Ok(certs) => {
                if certs.is_empty() {
                    println!("no actors enrolled");
                } else {
                    for c in certs {
                        println!(
                            "{}  {}  ops: {}",
                            c.actor_id,
                            c.namespace,
                            c.operations
                                .iter()
                                .map(|o| format!("{o:?}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("actor list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("enroll") => {
            let Some(actor_id) = args.get(2) else {
                eprintln!(
                    "usage: concierge-plugin actor enroll <actor-id> --namespace <ns> [--ops a,b]"
                );
                return ExitCode::from(2);
            };
            let Some(ns_arg) = flag_value(args, "--namespace") else {
                eprintln!("need --namespace <personal|project:ID|room:ID|agent:ID|all>");
                return ExitCode::from(2);
            };
            let Some(namespace) = parse_namespace(descriptor.network_id.clone(), &ns_arg) else {
                eprintln!("bad --namespace `{ns_arg}`");
                return ExitCode::from(2);
            };
            let ops = match flag_value(args, "--ops") {
                Some(csv) => match parse_operations(&csv) {
                    Ok(ops) => ops,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(2);
                    }
                },
                None => Vec::new(), // least-privilege default
            };
            match mem.enroll_actor(&descriptor.network_id, actor_id, &namespace, ops) {
                Ok(cert) => {
                    println!("enrolled actor {actor_id}");
                    println!("  namespace: {}", cert.namespace);
                    println!(
                        "  granted (∩ this device's own): {}",
                        cert.operations
                            .iter()
                            .map(|o| format!("{o:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("actor enroll failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown actor subcommand `{other}` — use list|enroll");
            ExitCode::from(2)
        }
    }
}

/// `sync <status | now --peer <multiaddr> --namespace NS>` — Phase N · Phase F.
/// `status` shows this device's namespaces and the heads it has converged on;
/// `now` dials a peer and runs the full reconcile→pull→converge loop, printing a
/// receipt. Sync is capability-gated and transfers only verified blocks.
pub(super) fn cmd_sync(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("status") | None => {
            let networks = mem.networks().unwrap_or_default();
            if networks.is_empty() {
                println!("no networks (found one with `network create <name>`)");
                return ExitCode::SUCCESS;
            }
            for descriptor in networks {
                println!(
                    "network `{}` (epoch {})",
                    descriptor.name, descriptor.membership_epoch
                );
                // Surface the namespaces this device tracks heads for.
                for cap in mem
                    .device_capabilities(&descriptor.network_id)
                    .unwrap_or_default()
                {
                    let ns = cap.namespace.canonical();
                    let heads = mem.local_heads(&descriptor.network_id, &ns);
                    let head_str = if heads.is_empty() {
                        "(no heads yet)".to_string()
                    } else {
                        heads.join(", ")
                    };
                    println!("  {ns}  heads: {head_str}");
                }
            }
            ExitCode::SUCCESS
        }
        Some("now") => {
            let Some(peer_addr) = flag_value(args, "--peer") else {
                eprintln!("usage: concierge-plugin sync now --peer <multiaddr> --namespace <ns>");
                return ExitCode::from(2);
            };
            let Some(ns_arg) = flag_value(args, "--namespace") else {
                eprintln!("need --namespace <personal|project:ID|room:ID|agent:ID|all>");
                return ExitCode::from(2);
            };
            let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
                eprintln!("no network on this device");
                return ExitCode::from(2);
            };
            let Some(namespace) = parse_namespace(descriptor.network_id.clone(), &ns_arg) else {
                eprintln!("bad --namespace `{ns_arg}`");
                return ExitCode::from(2);
            };
            let addr: concierge_net::Multiaddr = match peer_addr.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("bad --peer multiaddr: {e}");
                    return ExitCode::from(2);
                }
            };
            let Some(peer) = concierge_net::peer_id_in(&addr) else {
                eprintln!("--peer must include the peer's /p2p/<id> component");
                return ExitCode::from(2);
            };
            let secret = match mem.identity() {
                Ok(id) => id.secret_bytes(),
                Err(e) => {
                    eprintln!("identity: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let revoked = mem
                .revocation_set(&descriptor.network_id)
                .unwrap_or_default();

            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("sync: runtime: {e}");
                    return ExitCode::FAILURE;
                }
            };
            rt.block_on(async move {
                let (node, mut events) = match concierge_net::ConciergeNode::spawn(secret) {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("sync: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                if let Err(e) = node.dial(addr) {
                    eprintln!("sync: dial: {e}");
                    return ExitCode::FAILURE;
                }
                match concierge_net::sync_from_peer(
                    &node,
                    &mut events,
                    peer,
                    &mem,
                    &descriptor,
                    &namespace,
                    &revoked,
                    std::time::Duration::from_secs(60),
                )
                .await
                {
                    Ok(receipt) => {
                        println!(
                            "converged on {} ({} block(s), {} bytes)",
                            namespace.canonical(),
                            receipt.blocks_imported,
                            receipt.bytes
                        );
                        for h in &receipt.heads {
                            println!("  head: {h}");
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("sync failed: {e}");
                        ExitCode::FAILURE
                    }
                }
            })
        }
        Some(other) => {
            eprintln!("unknown sync subcommand `{other}` — use status|now");
            ExitCode::from(2)
        }
    }
}

/// `network grant <subject-id> --namespace NS --ops a,b` — Phase N · Phase G. Issue
/// a remaining/new member a scoped grant at the current epoch (root-signed). Prints
/// a grant JSON the member installs with `network accept`.
fn network_grant(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(subject) = args.get(2) else {
        eprintln!(
            "usage: concierge-plugin network grant <subject-id> --namespace <ns> --ops <a,b>"
        );
        return ExitCode::from(2);
    };
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        eprintln!("no network on this device");
        return ExitCode::from(2);
    };
    let Some(ns_arg) = flag_value(args, "--namespace") else {
        eprintln!("need --namespace <personal|project:ID|room:ID|agent:ID|all>");
        return ExitCode::from(2);
    };
    let Some(namespace) = parse_namespace(descriptor.network_id.clone(), &ns_arg) else {
        eprintln!("bad --namespace `{ns_arg}`");
        return ExitCode::from(2);
    };
    let ops = match parse_operations(&flag_value(args, "--ops").unwrap_or_default()) {
        Ok(ops) if !ops.is_empty() => ops,
        _ => {
            eprintln!("need --ops <comma-separated ops, e.g. sync_read,sync_write>");
            return ExitCode::from(2);
        }
    };
    match mem.grant_capability(
        &descriptor.network_id,
        subject,
        concierge_core::SubjectKind::Device,
        &namespace,
        ops,
    ) {
        Ok((membership, capability)) => {
            let grant = concierge_core::PairingGrant {
                descriptor,
                membership,
                capabilities: vec![capability],
            };
            eprintln!("# grant for {subject} — send to the member: `network accept <grant.json>`");
            println!(
                "{}",
                serde_json::to_string_pretty(&grant).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("grant failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `network revoke <subject-id>` — Phase N · Phase G. Sign a revocation, advance the
/// membership epoch (every prior cert/cap becomes stale), and persist it. Remaining
/// members must be re-granted at the new epoch (`network grant`). Prospective only —
/// it cannot recall data a removed device already holds.
fn network_revoke(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(subject) = args.get(2) else {
        eprintln!("usage: concierge-plugin network revoke <subject-id>");
        return ExitCode::from(2);
    };
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        eprintln!("no network on this device");
        return ExitCode::from(2);
    };
    match mem.revoke(&descriptor.network_id, subject) {
        Ok(advanced) => {
            println!("revoked {subject}");
            println!(
                "  membership epoch advanced to {}",
                advanced.membership_epoch
            );
            println!("  re-grant remaining members at the new epoch with `network grant`");
            println!("  (prospective: this does not recall data the removed device already holds)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("revoke failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `sidekick <status | enable | disable>` — bring up (or down) the on-node
/// embedding-model Sidekick *and* its private Kubo node together. They are
/// coupled: the Sidekick needs the node, and the private node only runs as part
/// of the Sidekick.
pub(super) fn cmd_sidekick(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    let print_status = |label: &str| {
        let s = mem.sidekick_status();
        println!(
            "{label}: enabled={} · node_running={} · kubo_installed={} · operational={}",
            s.enabled, s.node_running, s.kubo_installed, s.operational
        );
    };
    match args.get(1).map(String::as_str) {
        Some("status") | None => {
            print_status("sidekick");
            ExitCode::SUCCESS
        }
        Some("enable") => {
            println!("{}", concierge_core::SIDEKICK_DISCLAIMER);
            match mem.enable_sidekick() {
                Ok(s) => {
                    println!(
                        "sidekick enabled — private node launching (running={}, operational={})",
                        s.node_running, s.operational
                    );
                    println!(
                        "(the node may take a few seconds to come up; re-run `sidekick status`)"
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("enable sidekick failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("disable") => match mem.disable_sidekick() {
            Ok(_) => {
                println!("sidekick disabled");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("disable sidekick failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("unknown sidekick subcommand `{other}` — use status|enable|disable");
            ExitCode::from(2)
        }
    }
}
