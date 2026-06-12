//! Universal Concierge Plugin CLI.
//!
//! Phase 0 wires the command surface from the plan's "Minimum CLI" so the tool
//! feels mountable, then dispatches each subcommand to a stub that names the
//! phase implementing it. No command does real work yet.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};

use concierge_adapter_hermes::ingest as ingest_hermes;
use concierge_adapter_jsonl::{
    ingest, ingest_envelopes, Importer, IngestReport, JsonlImporter, MarkdownImporter,
};
use concierge_core::{
    cid_from_link, default_embedder, BackendInfo, Cid, CidOrName, Config, CoreBinding, Depth,
    EgressOperation, EgressPlan, Librarian, MemCli, Record,
};
use serde_json::Value;

const HELP: &str = "\
concierge-plugin — mountable IPLD memory for AI agents

USAGE:
    concierge-plugin <COMMAND> [ARGS]

COMMANDS:
    init                 Create a local .concierge store and config   [Phase 1]
    attach --adapter X [--model M] [--no-gui]
                         Attach a harness adapter and open its explorer [Phase 2/6/7]
    claude-code <backfill|watch|inject> [projects-dir] [--peek]
                         Auto-attach to Claude Code: backfill ~/.claude/projects,
                         watch for new sessions, or `inject` drains the node→host
                         suggestion outbox into a context block               [Phase C/8]
    ingest <file.jsonl>  Ingest host-neutral events into IPLD memory  [Phase 2]
    checkpoint --name N  Write a checkpoint over the current root      [Phase 2]
    import <source> <path> Backfill an existing store (use --dry-run)  [Phase 2.5]
    recall <name>        Resolve a name and show its record            [Phase 1/2]
    retrieve <query…> [--budget N] [--depth brief|summary|full] [--hops N] [--kind a,b] [--external]
                         Librarian: semantic + graph-gravity + recency retrieval;
                         --hops pulls related context, --kind filters by node kind;
                         --external also federates to connected sources       [Phase 8]
    connect <list|add <http-url> <alias>|remove <alias>|search <query…> [--limit N]>
                         External knowledge connectors: federate a query to
                         decentralized indices, returning untrusted CID refs  [Phase 8]
    outbox <peek|drain|wake <query…> [--authority ID]>
                         Node→host write-back: inspect/consume suggestions, or
                         `wake` fires the gated proactive look-ahead          [Phase 8]
    user <init | recovery | rotate | recover | show>
                         Root UserID: create, establish recovery, rotate the
                         active key (device replacement), recover, or show     [Phase N/A,G]
    network <create <name>|list|show [id]>
                         Found a private network (signed descriptor + this
                         device's membership cert), list, or verify           [Phase N/A]
    network pair [--rendezvous A] | respond <offer.json>
            | approve <response.json> --namespace NS --ops a,b | accept <grant.json>
                         Secure pairing: mint a one-use offer, prove possession,
                         approve scoped capabilities, join (compare the phrase)  [Phase N/B]
    network grant <subject-id> --namespace NS --ops a,b | revoke <subject-id>
                         Grant a scoped capability, or revoke a subject (advances
                         the epoch; re-grant remaining members)               [Phase N/G]
    sync <status | now --peer <multiaddr> --namespace NS>
                         Show converged heads, or pull a namespace from a peer to
                         convergence (verified blocks only)                   [Phase N/F]
    actor <list | enroll <actor-id> --namespace NS [--ops a,b]>
                         Enroll a harness/agent on this device with a device-signed
                         cert, scoped ⊆ this device's own (least privilege)   [Phase N/A]
    sidekick <status|enable|disable>
                         Enable the on-node Sidekick (embedding model) + its
                         private Kubo node (they enable together)        [Phase 8]
    export-car <name|cid> [out.car] [--dry-run|--confirm-plaintext-export]
                         Review, then explicitly export plaintext CARv1       [Privacy]
    import-car <file.car> <name> [--agent-id ID --signature SIG]
                         Import a CAR and optionally verify a signed share [Phase 4/5.5]
    share <root>           Preview only; refuses ambiguous public sharing [Privacy]
    share-private <root> --namespace N --recipients A,B
                         Preview only; capability issuance requires Data Platter [Phase E]
    publish-public <root> --confirm-public
                         Irreversibly publish one reviewed root           [Privacy]
    lock <root|name> [--label \"...\"]  Mark a subgraph Locked / Local-only  [Privacy]
    locks                List Locked / Local-only roots                   [Privacy]
    quarantine <list|add <cid|name> [reason]|release <cid|name>>
                         Guardian bad-CID list: withhold a block (reversible) [Phase 8]
    synthesis <candidates [--threshold N]|assemble <room>|record <room> <summary>>
                         Memory synthesis: flag long threads, assemble for the
                         host to summarize, record the host's summary    [Phase 8]
    backend <list|show NAME|add NAME>  Inspect/configure backends       [Phase 5]
    id                   Show this install's stable AgentID            [Phase 5.5]
    follow <agentid>     Follow another install (local allowlist)      [Phase 5.5]
    nickname <agentid> <name>  Give an AgentID a local petname         [Phase 5.5]
    following            List who you follow (with petnames)           [Phase 5.5]
    shared               Show verified shares from followed AgentIDs   [Phase 5.5]
    msg <room> <text>    Post a signed message to a room               [Phase 5.7]
    thread <room>        Show a room's verified, chronological thread  [Phase 5.7]
    room <humans-only|open|mention|mute> <room> [agentid]  AI-send lever/mute  [Phase 5.7]
    room serve <room> [--listen A --dial PEER --relay R --trust-all]
                         Join a P2P room (--relay reserves a slot for NAT)  [Phase 5.7]
                         Relay hosting requires --host-relay --external-address A
    gui [--port N] [--model M] [--no-open]
                         Open the Data Platter privacy controls        [Phase 7/D]
    mcp serve            Serve memory over MCP        [Phase 3 — DEFERRED]
    help                 Show this help
";

fn todo(phase: &str, what: &str) -> ExitCode {
    eprintln!("not yet implemented — {what} lands in {phase}.");
    ExitCode::from(2)
}

/// The working directory `mem` keys its `.concierge` store off of.
///
/// By default this is the user's **home directory**, so every command — no
/// matter which project or directory the harness invoked it from — resolves to
/// the single central store at `~/.concierge`. That makes all CIDs ever
/// created, ingested, or encountered accumulate in one place and stay available
/// everywhere.
///
/// Resolution order:
/// 1. `CONCIERGE_WORKDIR` — explicit override (tests, or pinning a command to a
///    specific per-project store). Its `.concierge` child is the store.
/// 2. `$HOME` — the default, giving the central `~/.concierge` store.
/// 3. current directory — only if `$HOME` is somehow unset.
///
/// `mem` loads its config from `<workdir>/.concierge/config.toml` and resolves
/// the store relative to `workdir`, so pointing `workdir` at `$HOME` (the parent
/// of the central `.concierge`) keeps the plugin and `mem` in lockstep on the
/// same store.
fn workdir() -> PathBuf {
    if let Ok(val) = std::env::var("CONCIERGE_WORKDIR") {
        return PathBuf::from(val);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `init` — create the local store dir and a minimal plugin config. `mem`
/// auto-creates `.concierge/` on first write, so init just makes the directory
/// explicit and drops a config the rest of the plugin reads.
fn cmd_init() -> ExitCode {
    let dir = workdir();
    let store = dir.join(".concierge");
    if let Err(e) = std::fs::create_dir_all(&store) {
        eprintln!("init failed: could not create {}: {e}", store.display());
        return ExitCode::FAILURE;
    }
    let cfg_path = dir.join(".concierge/config.toml");
    if !cfg_path.exists() {
        let cfg = toml::to_string_pretty(&Config::default())
            .expect("phase 1 config should serialize cleanly");
        if let Err(e) = std::fs::write(&cfg_path, cfg) {
            eprintln!("init failed: could not write {}: {e}", cfg_path.display());
            return ExitCode::FAILURE;
        }
    }
    println!(
        "initialized store at {} (config: {})",
        store.display(),
        cfg_path.display()
    );
    ExitCode::SUCCESS
}

/// `recall <name>` — resolve a name through the core binding and print its
/// record. The end-to-end proof that the plugin reads real IPLD via `mem`.
fn cmd_recall(name: Option<&str>) -> ExitCode {
    let Some(name) = name else {
        eprintln!("usage: concierge-plugin recall <name>");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    match mem.get(&CidOrName::Name(name.to_string())) {
        Ok(Record::Live {
            cid,
            kind,
            body_json,
        }) => {
            println!("{cid} [{kind}]", cid = cid.0);
            println!("{body_json}");
            ExitCode::SUCCESS
        }
        Ok(Record::Tombstone { cid, receipt_json }) => {
            println!("{} [tombstoned]", cid.0);
            println!("{receipt_json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("recall failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `retrieve <query…> [--budget N] [--depth brief|summary|full]` — the Librarian
/// (Phase 8 §1, Decision 0022): semantic + graph-gravity retrieval over the local
/// store, packed into a token budget. Uses the zero-dependency lexical embedder;
/// a small semantic model is a feature-gated backend behind the same trait.
fn cmd_retrieve(args: &[String]) -> ExitCode {
    // The query is the positional args that aren't flags or flag values.
    let mut query_parts: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--budget" | "--depth" | "--hops" | "--kind" => i += 2, // skip flag + its value
            "--external" => i += 1,                                  // boolean flag
            other => {
                query_parts.push(other);
                i += 1;
            }
        }
    }
    let query = query_parts.join(" ");
    if query.trim().is_empty() {
        eprintln!("usage: concierge-plugin retrieve <query…> [--budget N] [--depth brief|summary|full]");
        return ExitCode::from(2);
    }
    let budget = flag_value(args, "--budget")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4000);
    let depth = match flag_value(args, "--depth").as_deref() {
        Some("brief") => Depth::Brief,
        Some("full") => Depth::Full,
        _ => Depth::Summary,
    };
    let hops = flag_value(args, "--hops")
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0)
        .min(3);
    let kinds: Option<Vec<String>> = flag_value(args, "--kind").map(|k| {
        k.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });

    let mem = MemCli::new(workdir());
    let embedder = default_embedder(&mem.config().unwrap_or_default().librarian);
    let librarian = match Librarian::index_all_persistent(&mem, embedder) {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("retrieve failed to index store: {e}");
            return ExitCode::FAILURE;
        }
    };
    if librarian.is_empty() {
        println!("nothing indexed yet — capture or ingest some sessions first");
        return ExitCode::SUCCESS;
    }
    let result = librarian.retrieve_multihop(&query, budget, &[], depth, hops, kinds.as_deref());
    println!(
        "retrieve: {} hit(s) over {} indexed node(s) · {}/{} tokens",
        result.items.len(),
        librarian.len(),
        result.used_tokens,
        result.budget_tokens
    );
    for hit in &result.items {
        let tag = if hit.hop > 0 {
            format!("  related (hop {})", hit.hop)
        } else {
            String::new()
        };
        println!(
            "\n[{score:.3}  sim {sim:.3}  gravity {grav:.3}]  {kind}  {cid}{tag}",
            score = hit.score,
            sim = hit.similarity,
            grav = hit.gravity,
            kind = hit.kind,
            cid = hit.cid,
        );
        println!("  {}", hit.preview.replace('\n', "\n  "));
    }
    // Opt-in federation (Phase 8 §6): query connected external indices and print
    // the External CID References in their OWN section — untrusted, never mixed
    // into local gravity ranking, attributed to their source. The host resolves
    // these CIDs from the global IPFS network only if it wants them.
    if args.iter().any(|a| a == "--external") {
        match mem.federate_search(&query, 10) {
            Ok(hits) if !hits.is_empty() => {
                println!("\n— external references ({} · untrusted, not auto-injected) —", hits.len());
                for hit in &hits {
                    println!(
                        "\n[ext {score:.3}  via {src}]  {cid}",
                        score = hit.score,
                        src = hit.source_alias,
                        cid = hit.cid,
                    );
                    if !hit.title.is_empty() {
                        println!("  {}", hit.title);
                    }
                    if !hit.snippet.is_empty() {
                        println!("  {}", hit.snippet.replace('\n', "\n  "));
                    }
                }
            }
            Ok(_) => println!("\n— external references: none (no connected source matched) —"),
            Err(e) => eprintln!("\nexternal federation skipped: {e}"),
        }
    }
    ExitCode::SUCCESS
}

/// `connect <list | add <url> <alias> | remove <alias> | search <query…> [--limit N]>`
/// — Phase 8 §6 External Knowledge Connectors. Register decentralized indices and
/// federate a query to them, returning **External CID References** (untrusted, the
/// host resolves them from the global IPFS network). Registering a source does not
/// query it; `search` (and `retrieve --external`) is the explicit egress step.
fn cmd_connect(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("list") | None => match mem.connector_registry() {
            Ok(reg) => {
                if reg.is_empty() {
                    println!("no external sources connected (opt-in)");
                } else {
                    for s in reg.list() {
                        println!("{}\t{}\t[{}]", s.alias, s.url, s.kind);
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("connect list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("add") => {
            let (Some(url), Some(alias)) = (args.get(2), args.get(3)) else {
                eprintln!("usage: concierge-plugin connect add <http-url> <alias>");
                return ExitCode::from(2);
            };
            match mem.connect_external(url, alias) {
                Ok(()) => {
                    println!("connected `{alias}` → {url} (querying it sends your query out — egress)");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("connect add failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("remove") => {
            let Some(alias) = args.get(2) else {
                eprintln!("usage: concierge-plugin connect remove <alias>");
                return ExitCode::from(2);
            };
            match mem.disconnect_external(alias) {
                Ok(true) => {
                    println!("disconnected `{alias}`");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    println!("`{alias}` was not connected");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("connect remove failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("search") => {
            let mut query_parts: Vec<&str> = Vec::new();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => i += 2,
                    other => {
                        query_parts.push(other);
                        i += 1;
                    }
                }
            }
            let query = query_parts.join(" ");
            if query.trim().is_empty() {
                eprintln!("usage: concierge-plugin connect search <query…> [--limit N]");
                return ExitCode::from(2);
            }
            let limit = flag_value(args, "--limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(10);
            match mem.federate_search(&query, limit) {
                Ok(hits) => {
                    println!("{} external reference(s) — untrusted, not auto-injected", hits.len());
                    for hit in &hits {
                        println!(
                            "\n[ext {score:.3}  via {src}]  {cid}",
                            score = hit.score,
                            src = hit.source_alias,
                            cid = hit.cid,
                        );
                        if !hit.title.is_empty() {
                            println!("  {}", hit.title);
                        }
                        if !hit.snippet.is_empty() {
                            println!("  {}", hit.snippet.replace('\n', "\n  "));
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("connect search failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown connect subcommand `{other}` — use list|add|remove|search");
            ExitCode::from(2)
        }
    }
}

/// `outbox <peek | drain | wake <query…> [--authority ID] [--event E]>` — the
/// node→host write-back seam (Phase 8 §2). `peek`/`drain` inspect/consume the raw
/// suggestion queue; `wake` fires the gated look-ahead for `query` (the live wake
/// trigger a capture loop calls) and enqueues a suggestion **only** if proactive
/// injection is enabled in config *and* a trusted authority is presented.
fn cmd_outbox(args: &[String]) -> ExitCode {
    use concierge_core::OutboundEvent;
    let mem = MemCli::new(workdir());
    let print_entries = |entries: &[concierge_core::OutboxEntry]| {
        for entry in entries {
            let OutboundEvent::ContextSuggested(s) = &entry.event;
            println!(
                "[{at}] context_suggested  authority={auth}  cids={n}  reason={reason}",
                at = entry.at,
                auth = s.authority,
                n = s.cids.len(),
                reason = s.reason,
            );
            for cid in &s.cids {
                println!("    {cid}");
            }
        }
    };
    match args.get(1).map(String::as_str) {
        Some("peek") | None => match mem.outbox_peek() {
            Ok(entries) => {
                if entries.is_empty() {
                    println!("outbox empty (the default node is tool-only)");
                } else {
                    print_entries(&entries);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("outbox peek failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("drain") => match mem.outbox_drain() {
            Ok(entries) => {
                print_entries(&entries);
                println!("— drained {} entr(ies) —", entries.len());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("outbox drain failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("wake") => {
            let mut query_parts: Vec<&str> = Vec::new();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--authority" | "--event" => i += 2,
                    other => {
                        query_parts.push(other);
                        i += 1;
                    }
                }
            }
            let query = query_parts.join(" ");
            if query.trim().is_empty() {
                eprintln!("usage: concierge-plugin outbox wake <query…> [--authority ID] [--event E]");
                return ExitCode::from(2);
            }
            let event = flag_value(args, "--event").unwrap_or_else(|| "user_prompt".to_string());
            let authority = flag_value(args, "--authority");
            match mem.proactive_wake(&event, &query, authority.as_deref()) {
                Ok(Some(s)) => {
                    println!(
                        "suggestion enqueued (authority={}, {} cid(s)) — drain via `claude-code inject`",
                        s.authority,
                        s.cids.len()
                    );
                    ExitCode::SUCCESS
                }
                Ok(None) => {
                    println!("no suggestion (proactive off, no authority, wake trigger not configured, or below confidence) — node stays tool-only");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("outbox wake failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown outbox subcommand `{other}` — use peek|drain|wake");
            ExitCode::from(2)
        }
    }
}

/// `user init` — create this install's root **UserID** (Phase N · Phase A). The
/// UserID is the long-lived ownership key that founds private networks; it is
/// distinct from the per-install DeviceID and is never copied between devices.
fn cmd_user(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("init") | None => match mem.user_identity() {
            Ok(user) => {
                println!("UserID: {}", user.agent_id().0);
                println!("(root ownership key — distinct from this device's identity; never copied)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("user init failed: {e}");
                ExitCode::FAILURE
            }
        },
        // Phase N · Phase G — UserID recovery / device replacement.
        Some("recovery") => match mem.establish_user_recovery() {
            Ok(r) => {
                println!("recovery established for UserID {}", r.user_id);
                println!("  recovery key: {}", r.recovery_key);
                println!("  BACK THIS UP OFFLINE — it is the only way to recover the identity if the active key is lost.");
                ExitCode::SUCCESS
            }
            Err(e) => { eprintln!("user recovery failed: {e}"); ExitCode::FAILURE }
        },
        Some("rotate") => match mem.rotate_user_key() {
            Ok(r) => {
                println!("rotated active key (UserID {} unchanged)", r.user_id);
                println!("  new active key: {}", r.active_key);
                ExitCode::SUCCESS
            }
            Err(e) => { eprintln!("user rotate failed: {e}"); ExitCode::FAILURE }
        },
        Some("recover") => match mem.recover_user_key() {
            Ok(r) => {
                println!("recovered UserID {} (identity preserved)", r.user_id);
                println!("  new active key: {}", r.active_key);
                ExitCode::SUCCESS
            }
            Err(e) => { eprintln!("user recover failed: {e}"); ExitCode::FAILURE }
        },
        Some("show") => match mem.user_identity_log() {
            Ok(Some(log)) => match concierge_core::verify_and_resolve(&log) {
                Ok(r) => {
                    println!("UserID:      {}", r.user_id);
                    println!("active key:  {}", r.active_key);
                    println!("recovery key:{}", r.recovery_key);
                    println!("log entries: {}", log.ops.len());
                    ExitCode::SUCCESS
                }
                Err(e) => { eprintln!("user log invalid: {e}"); ExitCode::FAILURE }
            },
            Ok(None) => { println!("no recovery log yet — run `user recovery` to establish one"); ExitCode::SUCCESS }
            Err(e) => { eprintln!("user show failed: {e}"); ExitCode::FAILURE }
        },
        Some(other) => {
            eprintln!("unknown user subcommand `{other}` — use init|recovery|rotate|recover|show");
            ExitCode::from(2)
        }
    }
}

/// `network <create <name> | list | show [network-id]>` — Phase N · Phase A. Found
/// a private network (signed descriptor + this device's membership certificate),
/// list the networks this install belongs to, or show one with verification.
fn cmd_network(args: &[String]) -> ExitCode {
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
                        println!("{}\t{}\tepoch {}", n.name, n.network_id.0, n.membership_epoch);
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
            println!("network `{}`  ({})", descriptor.name, descriptor.network_id.0);
            println!("  descriptor signature: {}", if descriptor_ok { "valid" } else { "INVALID" });
            println!("  root users: {}", descriptor.root_user_ids.iter().map(|u| u.0.clone()).collect::<Vec<_>>().join(", "));
            println!("  epoch: {}", descriptor.membership_epoch);
            match mem.device_membership(&descriptor.network_id) {
                Ok(Some(cert)) => {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let valid = concierge_core::verify_membership(&cert, &descriptor, now, &concierge_core::RevocationSet::new()).is_ok();
                    println!("  this device: {} ({})", cert.subject_id, if valid { "membership valid" } else { "membership INVALID" });
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
fn parse_namespace(network_id: concierge_core::NetworkId, s: &str) -> Option<concierge_core::Namespace> {
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
        Err(e) => { eprintln!("network pair failed: {e}"); return ExitCode::FAILURE; }
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
    let rendezvous = flag_value(args, "--rendezvous").unwrap_or_else(|| "/ip4/127.0.0.1/tcp/4001".to_string());
    match mem.create_pairing_offer(&descriptor.network_id, &rendezvous) {
        Ok(offer) => {
            eprintln!("# pairing offer for `{}` (expires in 10 min, one-use)", descriptor.name);
            eprintln!("# share this with the new device, then run `network respond <offer.json>` there");
            println!("{}", serde_json::to_string_pretty(&offer).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("network pair failed: {e}"); ExitCode::FAILURE }
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
        Err(e) => { eprintln!("{e}"); return ExitCode::FAILURE; }
    };
    if let Err(e) = offer.verify() {
        eprintln!("offer rejected: {e}");
        return ExitCode::FAILURE;
    }
    let mem = MemCli::new(workdir());
    let device = match mem.identity() {
        Ok(d) => d,
        Err(e) => { eprintln!("identity error: {e}"); return ExitCode::FAILURE; }
    };
    let response = concierge_core::PairingResponse::create(&offer, &device);
    eprintln!("# confirmation phrase (must match the admin's): {}", concierge_core::confirmation_phrase(&offer, &response));
    eprintln!("# send this response back; the admin runs `network approve <response.json> --namespace … --ops …`");
    println!("{}", serde_json::to_string_pretty(&response).unwrap_or_default());
    ExitCode::SUCCESS
}

/// `network approve <response.json> --namespace NS --ops a,b` — admin: verify the
/// response, show the confirmation phrase to compare, and issue the scoped grant.
fn network_approve(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(path) = args.get(2) else {
        eprintln!("usage: concierge-plugin network approve <response.json> --namespace <ns> --ops <a,b>");
        return ExitCode::from(2);
    };
    let response: concierge_core::PairingResponse = match read_json_file(path) {
        Ok(r) => r,
        Err(e) => { eprintln!("{e}"); return ExitCode::FAILURE; }
    };
    let Some(ns_arg) = flag_value(args, "--namespace") else {
        eprintln!("need --namespace <personal|project:ID|room:ID|agent:ID|all>");
        return ExitCode::from(2);
    };
    let ops = match parse_operations(&flag_value(args, "--ops").unwrap_or_default()) {
        Ok(ops) if !ops.is_empty() => ops,
        _ => { eprintln!("need --ops <comma-separated ops, e.g. sync_read,message_receive>"); return ExitCode::from(2); }
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
    match mem.complete_pairing(&response, &[(namespace, ops)], concierge_core::DEFAULT_CERT_TTL_SECS) {
        Ok(grant) => {
            eprintln!("# approved — send this grant to the new device: `network accept <grant.json>`");
            eprintln!("# {} capability(ies) granted to {}", grant.capabilities.len(), grant.membership.subject_id);
            println!("{}", serde_json::to_string_pretty(&grant).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("approve failed: {e}"); ExitCode::FAILURE }
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
        Err(e) => { eprintln!("{e}"); return ExitCode::FAILURE; }
    };
    match mem.accept_pairing_grant(&grant) {
        Ok(()) => {
            println!("joined network `{}` ({})", grant.descriptor.name, grant.descriptor.network_id.0);
            for cap in &grant.capabilities {
                println!("  {} → {}", cap.namespace.canonical(), cap.operations.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>().join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("accept failed: {e}"); ExitCode::FAILURE }
    }
}

/// `actor <list | enroll <actor-id> --namespace NS --ops a,b>` — Phase N · Phase A/B.
/// Enroll a harness/agent hosted by this device with a device-signed certificate,
/// scoped to operations that are a subset of this device's own (least privilege).
fn cmd_actor(args: &[String]) -> ExitCode {
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
                        println!("{}  {}  ops: {}", c.actor_id, c.namespace, c.operations.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>().join(", "));
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => { eprintln!("actor list failed: {e}"); ExitCode::FAILURE }
        },
        Some("enroll") => {
            let Some(actor_id) = args.get(2) else {
                eprintln!("usage: concierge-plugin actor enroll <actor-id> --namespace <ns> [--ops a,b]");
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
                Some(csv) => match parse_operations(&csv) { Ok(ops) => ops, Err(e) => { eprintln!("{e}"); return ExitCode::from(2); } },
                None => Vec::new(), // least-privilege default
            };
            match mem.enroll_actor(&descriptor.network_id, actor_id, &namespace, ops) {
                Ok(cert) => {
                    println!("enrolled actor {actor_id}");
                    println!("  namespace: {}", cert.namespace);
                    println!("  granted (∩ this device's own): {}", cert.operations.iter().map(|o| format!("{o:?}")).collect::<Vec<_>>().join(", "));
                    ExitCode::SUCCESS
                }
                Err(e) => { eprintln!("actor enroll failed: {e}"); ExitCode::FAILURE }
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
fn cmd_sync(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("status") | None => {
            let networks = mem.networks().unwrap_or_default();
            if networks.is_empty() {
                println!("no networks (found one with `network create <name>`)");
                return ExitCode::SUCCESS;
            }
            for descriptor in networks {
                println!("network `{}` (epoch {})", descriptor.name, descriptor.membership_epoch);
                // Surface the namespaces this device tracks heads for.
                for cap in mem.device_capabilities(&descriptor.network_id).unwrap_or_default() {
                    let ns = cap.namespace.canonical();
                    let heads = mem.local_heads(&descriptor.network_id, &ns);
                    let head_str = if heads.is_empty() { "(no heads yet)".to_string() } else { heads.join(", ") };
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
                Err(e) => { eprintln!("bad --peer multiaddr: {e}"); return ExitCode::from(2); }
            };
            let Some(peer) = concierge_net::peer_id_in(&addr) else {
                eprintln!("--peer must include the peer's /p2p/<id> component");
                return ExitCode::from(2);
            };
            let secret = match mem.identity() { Ok(id) => id.secret_bytes(), Err(e) => { eprintln!("identity: {e}"); return ExitCode::FAILURE; } };
            let revoked = mem.revocation_set(&descriptor.network_id).unwrap_or_default();

            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => { eprintln!("sync: runtime: {e}"); return ExitCode::FAILURE; }
            };
            rt.block_on(async move {
                let (node, mut events) = match concierge_net::ConciergeNode::spawn(secret) {
                    Ok(pair) => pair,
                    Err(e) => { eprintln!("sync: {e}"); return ExitCode::FAILURE; }
                };
                if let Err(e) = node.dial(addr) { eprintln!("sync: dial: {e}"); return ExitCode::FAILURE; }
                match concierge_net::sync_from_peer(&node, &mut events, peer, &mem, &descriptor, &namespace, &revoked, std::time::Duration::from_secs(60)).await {
                    Ok(receipt) => {
                        println!("converged on {} ({} block(s), {} bytes)", namespace.canonical(), receipt.blocks_imported, receipt.bytes);
                        for h in &receipt.heads { println!("  head: {h}"); }
                        ExitCode::SUCCESS
                    }
                    Err(e) => { eprintln!("sync failed: {e}"); ExitCode::FAILURE }
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
        eprintln!("usage: concierge-plugin network grant <subject-id> --namespace <ns> --ops <a,b>");
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
        _ => { eprintln!("need --ops <comma-separated ops, e.g. sync_read,sync_write>"); return ExitCode::from(2); }
    };
    match mem.grant_capability(&descriptor.network_id, subject, concierge_core::SubjectKind::Device, &namespace, ops) {
        Ok((membership, capability)) => {
            let grant = concierge_core::PairingGrant { descriptor, membership, capabilities: vec![capability] };
            eprintln!("# grant for {subject} — send to the member: `network accept <grant.json>`");
            println!("{}", serde_json::to_string_pretty(&grant).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("grant failed: {e}"); ExitCode::FAILURE }
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
            println!("  membership epoch advanced to {}", advanced.membership_epoch);
            println!("  re-grant remaining members at the new epoch with `network grant`");
            println!("  (prospective: this does not recall data the removed device already holds)");
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("revoke failed: {e}"); ExitCode::FAILURE }
    }
}

/// `sidekick <status | enable | disable>` — bring up (or down) the on-node
/// embedding-model Sidekick *and* its private Kubo node together. They are
/// coupled: the Sidekick needs the node, and the private node only runs as part
/// of the Sidekick.
fn cmd_sidekick(args: &[String]) -> ExitCode {
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
                    println!("(the node may take a few seconds to come up; re-run `sidekick status`)");
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

/// `quarantine <list | add <cid|name> [reason…] | release <cid|name>>` — the
/// Guardian's local, reversible, block-scoped bad-CID list (Phase 8 §3). A
/// quarantined CID is withheld from relay/surfacing; the user's local data is
/// untouched and `release` always lifts it.
fn cmd_quarantine(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    let resolve = |target: &str| -> Cid {
        mem.resolve(target).unwrap_or_else(|_| Cid(target.to_string()))
    };
    match args.get(1).map(String::as_str) {
        Some("list") | None => match mem.quarantine_registry() {
            Ok(reg) => {
                if reg.is_empty() {
                    println!("no quarantined CIDs");
                } else {
                    for (cid, record) in reg.list() {
                        println!("{cid}  [{}]  {}", record.at, record.reason);
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("quarantine list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("add") => {
            let Some(target) = args.get(2) else {
                eprintln!("usage: concierge-plugin quarantine add <cid|name> [reason…]");
                return ExitCode::from(2);
            };
            let reason = if args.len() > 3 { args[3..].join(" ") } else { "manual".to_string() };
            match mem.quarantine_cid(&resolve(target), &reason) {
                Ok(()) => {
                    println!("quarantined {target} — withheld from relay/surfacing (reversible)");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("quarantine add failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("release") => {
            let Some(target) = args.get(2) else {
                eprintln!("usage: concierge-plugin quarantine release <cid|name>");
                return ExitCode::from(2);
            };
            match mem.release_cid(&resolve(target)) {
                Ok(true) => {
                    println!("released {target}");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    println!("{target} was not quarantined");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("quarantine release failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown quarantine subcommand `{other}` — use list|add|release");
            ExitCode::from(2)
        }
    }
}

/// `synthesis <candidates [--threshold N] | assemble <room> | record <room> <summary…>>`
/// — Phase 8 §4 (Memory Synthesis). The node *detects* rooms worth synthesizing and
/// *assembles* their thread for the host to summarize, then *records* the host's
/// returned summary as a Decision node derived from the thread. **No on-node
/// generation** (Decision 0022): the summary text must come from the host.
fn cmd_synthesis(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("candidates") | None => {
            let threshold = flag_value(args, "--threshold")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(concierge_core::SYNTHESIS_THRESHOLD);
            match concierge_core::synthesis_candidates(&mem, threshold) {
                Ok(candidates) => {
                    if candidates.is_empty() {
                        println!("no rooms at or above {threshold} messages");
                    } else {
                        for c in candidates {
                            println!("{}\t{} messages", c.room, c.message_count);
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("synthesis candidates failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("assemble") => {
            let Some(room) = args.get(2) else {
                eprintln!("usage: concierge-plugin synthesis assemble <room>");
                return ExitCode::from(2);
            };
            match concierge_core::assemble_thread(&mem, room) {
                Ok((text, provenance)) => {
                    // The thread text is for the host model to summarize; the
                    // provenance CIDs are what `record` should link back to.
                    eprintln!("# {} messages — summarize on the host, then `synthesis record`", provenance.len());
                    println!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("synthesis assemble failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("record") => {
            let Some(room) = args.get(2) else {
                eprintln!("usage: concierge-plugin synthesis record <room> <host-summary…>");
                return ExitCode::from(2);
            };
            let summary = args.get(3..).map(|s| s.join(" ")).unwrap_or_default();
            if summary.trim().is_empty() {
                eprintln!("usage: concierge-plugin synthesis record <room> <host-summary…>");
                eprintln!("(the summary must come from the host model — the node does no generation)");
                return ExitCode::from(2);
            }
            let provenance = match concierge_core::assemble_thread(&mem, room) {
                Ok((_, cids)) => cids,
                Err(e) => {
                    eprintln!("synthesis record failed: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match concierge_core::record_synthesis(&mem, room, &summary, &provenance) {
                Ok(cid) => {
                    println!("{}", cid.0);
                    println!("recorded host synthesis of `{room}` ({} messages) as a Decision node", provenance.len());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("synthesis record failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown synthesis subcommand `{other}` — use candidates|assemble|record");
            ExitCode::from(2)
        }
    }
}

fn checkpoint_root_from_record(body_json: &str) -> Result<Cid, String> {
    let value: Value = serde_json::from_str(body_json)
        .map_err(|e| format!("could not parse checkpoint record JSON: {e}"))?;
    let body = value
        .get("body")
        .ok_or_else(|| "checkpoint record JSON missing body".to_string())?;
    let root = body
        .get("root")
        .ok_or_else(|| "checkpoint record JSON missing root".to_string())?;
    cid_from_link(root).map_err(|e| e.to_string())
}

fn cmd_checkpoint(name: Option<&str>) -> ExitCode {
    let Some(name) = name else {
        eprintln!("usage: concierge-plugin checkpoint --name <name>");
        return ExitCode::from(2);
    };

    let mem = MemCli::new(workdir());
    let current = match mem.resolve(name) {
        Ok(cid) => cid,
        Err(e) => {
            eprintln!("checkpoint failed: could not resolve `{name}`: {e}");
            return ExitCode::FAILURE;
        }
    };

    let record = match mem.get(&CidOrName::Cid(current.clone())) {
        Ok(record) => record,
        Err(e) => {
            eprintln!("checkpoint failed: could not read `{name}`: {e}");
            return ExitCode::FAILURE;
        }
    };

    let Record::Live { body_json, .. } = record else {
        eprintln!("checkpoint failed: `{name}` resolves to a tombstone");
        return ExitCode::FAILURE;
    };

    let root = match checkpoint_root_from_record(&body_json) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("checkpoint failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let checkpoint = match mem.checkpoint(name, &root, Some(&current)) {
        Ok(cid) => cid,
        Err(e) => {
            eprintln!("checkpoint failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = mem.bind(name, &checkpoint) {
        eprintln!("checkpoint failed: could not bind `{name}`: {e}");
        return ExitCode::FAILURE;
    }

    println!("{name} -> {}", checkpoint.0);
    ExitCode::SUCCESS
}

fn cmd_import(args: &[String]) -> ExitCode {
    let Some(source) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin import <source> <path> [--dry-run]");
        return ExitCode::from(2);
    };
    let Some(path) = args.get(2).map(String::as_str) else {
        eprintln!("usage: concierge-plugin import <source> <path> [--dry-run]");
        return ExitCode::from(2);
    };
    let dry_run = args.iter().any(|arg| arg == "--dry-run");

    match source {
        "jsonl" => run_import(JsonlImporter, path, dry_run),
        "markdown" => run_import(MarkdownImporter, path, dry_run),
        other => {
            eprintln!("unknown importer `{other}` — available: jsonl, markdown");
            ExitCode::from(2)
        }
    }
}

/// Shared backfill path for any [`Importer`]: read the source read-only, then
/// either report counts (`--dry-run`) or run the envelopes through the same
/// ingest path the live adapter uses.
fn run_import<I: Importer>(importer: I, path: &str, dry_run: bool) -> ExitCode {
    let source_path = PathBuf::from(path);
    let envelopes = match importer.read(&source_path) {
        Ok(envelopes) => envelopes,
        Err(e) => {
            eprintln!("import failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if dry_run {
        println!(
            "import dry-run: {} envelopes from {} ({})",
            envelopes.len(),
            source_path.display(),
            importer.source_system(),
        );
        return ExitCode::SUCCESS;
    }

    let mem = MemCli::new(workdir());
    let items = envelopes
        .into_iter()
        .enumerate()
        .map(|(idx, env)| (idx + 1, env));
    let report = ingest_envelopes(items, &mem, &workdir(), IngestReport::default());
    finish_ingest(&report)
}

/// `ingest <file.jsonl>` — read a JSONL event stream from a file into IPLD memory.
fn cmd_ingest(path: Option<&str>) -> ExitCode {
    let Some(path) = path else {
        eprintln!("usage: concierge-plugin ingest <file.jsonl>");
        return ExitCode::from(2);
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("ingest failed: cannot open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mem = MemCli::new(workdir());
    let report = ingest(BufReader::new(file), &mem, &workdir());
    finish_ingest(&report)
}

/// `attach --adapter jsonl` — stream JSONL events from stdin into IPLD memory.
/// The pipe path: `my-harness | concierge-plugin attach --adapter jsonl`.
fn cmd_attach(args: &[String]) -> ExitCode {
    let adapter = args
        .iter()
        .position(|a| a == "--adapter")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    if matches!(adapter, Some("jsonl" | "hermes")) && !args.iter().any(|arg| arg == "--no-gui") {
        spawn_mount_gui(
            flag_value(args, "--model")
                .as_deref()
                .or(adapter)
                .unwrap_or("mounted harness"),
        );
    }
    match adapter {
        Some("jsonl") => {
            let mem = MemCli::new(workdir());
            let report = ingest(std::io::stdin().lock(), &mem, &workdir());
            finish_ingest(&report)
        }
        Some("hermes") => {
            let mem = MemCli::new(workdir());
            match ingest_hermes(std::io::stdin().lock(), &mem, &workdir()) {
                Ok(report) => finish_ingest(&report.inner),
                Err(e) => {
                    eprintln!("hermes attach failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown adapter `{other}` — available: jsonl, hermes");
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "usage: concierge-plugin attach --adapter jsonl|hermes   (reads JSONL on stdin)"
            );
            ExitCode::from(2)
        }
    }
}

/// `claude-code <backfill|watch> [projects-dir]` — auto-attach to Claude Code
/// (Phase C, Decision 0013). Resolves `~/.claude/projects` by default; `backfill`
/// ingests every existing session once, `watch` backfills then keeps capture
/// continuous via a file-watcher. Ingest is content-addressed, so re-reading the
/// same events deduplicates by CID — capture is idempotent.
fn cmd_claude_code(args: &[String]) -> ExitCode {
    use concierge_adapter_claude_code::discovery;
    let projects_dir = args
        .get(2)
        .map(PathBuf::from)
        .or_else(discovery::claude_projects_dir);
    let Some(projects_dir) = projects_dir else {
        eprintln!("could not resolve ~/.claude/projects (is HOME set?)");
        return ExitCode::FAILURE;
    };
    match args.get(1).map(String::as_str) {
        Some("backfill") => claude_code_backfill(&projects_dir),
        Some("watch") => claude_code_watch(&projects_dir),
        Some("inject") => claude_code_inject(&args),
        _ => {
            eprintln!("usage: concierge-plugin claude-code <backfill|watch|inject> [projects-dir]");
            ExitCode::from(2)
        }
    }
}

/// `claude-code inject [--peek]` — the node→host **write-back** demo (Phase 8 §2/§5):
/// drain the Librarian's outbox of gated `ContextSuggested` events and print the
/// context block a Claude Code harness would prepend, attributed to the trusting
/// authority. `--peek` shows pending suggestions without consuming them. On the
/// default (tool-only) node the outbox is empty and this prints nothing.
fn claude_code_inject(args: &[String]) -> ExitCode {
    use concierge_adapter_claude_code::render_injection;
    use concierge_core::OutboundEvent;
    let mem = MemCli::new(workdir());
    let peek = args.iter().any(|a| a == "--peek");
    let entries = match if peek { mem.outbox_peek() } else { mem.outbox_drain() } {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("inject failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    if entries.is_empty() {
        println!("no suggestions pending (the node is tool-only unless proactive injection is enabled + granted)");
        return ExitCode::SUCCESS;
    }
    for entry in &entries {
        let OutboundEvent::ContextSuggested(suggestion) = &entry.event;
        // Resolve each suggested CID to a short preview for the injected block.
        let previews: Vec<(String, String)> = suggestion
            .cids
            .iter()
            .map(|cid| {
                let preview = match mem.get(&CidOrName::Cid(Cid(cid.clone()))) {
                    Ok(Record::Live { body_json, .. }) => preview_of(&body_json),
                    _ => "(content unavailable)".to_string(),
                };
                (cid.clone(), preview)
            })
            .collect();
        println!("{}", render_injection(suggestion, &previews));
    }
    ExitCode::SUCCESS
}

/// A short, single-line preview of a record body for the injected context block.
fn preview_of(body_json: &str) -> String {
    let text = serde_json::from_str::<Value>(body_json)
        .ok()
        .and_then(|v| {
            // Prefer a `body.text`/`body.choice` style field; fall back to the raw body.
            let body = v.get("body").cloned().unwrap_or(v);
            ["text", "choice", "payload", "label"]
                .iter()
                .find_map(|k| body.get(*k).and_then(|f| f.as_str()).map(String::from))
                .or_else(|| Some(body.to_string()))
        })
        .unwrap_or_else(|| body_json.to_string());
    // A room message stores its MessageEnvelope JSON inside `body.text`; unwrap one
    // more level to the human-readable `payload` when that's what we have.
    let text = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("payload").and_then(|p| p.as_str()).map(String::from))
        .unwrap_or(text);
    let flat = text.replace('\n', " ");
    if flat.chars().count() > 200 {
        format!("{}…", flat.chars().take(200).collect::<String>())
    } else {
        flat
    }
}

/// Ingest every existing Claude Code session under `projects_dir` once.
fn claude_code_backfill(projects_dir: &std::path::Path) -> ExitCode {
    use concierge_adapter_claude_code::{discovery, ingest_file_from_offset};
    let mem = MemCli::new(workdir());
    let base = workdir();
    let sessions = discovery::enumerate_sessions(projects_dir);
    if sessions.is_empty() {
        println!("no Claude Code sessions found under {}", projects_dir.display());
        return ExitCode::SUCCESS;
    }
    let mut total_events = 0usize;
    for session in &sessions {
        match ingest_file_from_offset(&session.session_file, 0, &mem, &base) {
            Ok((report, _)) => total_events += report.events,
            Err(e) => eprintln!("skip {}: {e}", session.session_file.display()),
        }
    }
    println!(
        "backfilled {} session(s), {} events from {}",
        sessions.len(),
        total_events,
        projects_dir.display()
    );
    ExitCode::SUCCESS
}

/// Backfill, then watch `projects_dir` and incrementally ingest appended lines.
fn claude_code_watch(projects_dir: &std::path::Path) -> ExitCode {
    use concierge_adapter_claude_code::{discovery, ingest_file_from_offset};
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::collections::HashMap;
    use std::sync::mpsc::channel;

    let mem = MemCli::new(workdir());
    let base = workdir();
    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();

    // Backfill everything first, recording how far each file is ingested.
    let sessions = discovery::enumerate_sessions(projects_dir);
    let mut backfilled = 0usize;
    for session in &sessions {
        if let Ok((_, offset)) = ingest_file_from_offset(&session.session_file, 0, &mem, &base) {
            offsets.insert(session.session_file.clone(), offset);
            backfilled += 1;
        }
    }
    println!(
        "attached: backfilled {backfilled} session(s); watching {} (Ctrl-C to stop)",
        projects_dir.display()
    );

    let (tx, rx) = channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(watcher) => watcher,
        Err(e) => {
            eprintln!("watch init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = watcher.watch(projects_dir, RecursiveMode::Recursive) {
        eprintln!("cannot watch {}: {e}", projects_dir.display());
        return ExitCode::FAILURE;
    }

    for res in rx {
        let Ok(event) = res else { continue };
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            continue;
        }
        for path in event.paths {
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let offset = offsets.get(&path).copied().unwrap_or(0);
            match ingest_file_from_offset(&path, offset, &mem, &base) {
                Ok((report, new_offset)) => {
                    offsets.insert(path.clone(), new_offset);
                    if report.events > 0 {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("session");
                        println!("+{} events  {name}", report.events);
                    }
                }
                Err(e) => eprintln!("ingest {}: {e}", path.display()),
            }
        }
    }
    ExitCode::SUCCESS
}

/// Launch the standalone explorer as a sibling process when a harness mounts.
/// Failure is non-fatal: the adapter remains observe-only and still records.
fn spawn_mount_gui(model: &str) {
    let Ok(binary) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(binary)
        .args(["gui", "--model", model])
        .current_dir(workdir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Print an ingest summary and pick an exit code. Skipped lines are reported but
/// do not fail the run (one bad line must not abort the stream); a run where
/// *nothing* parsed is a failure.
fn finish_ingest(report: &IngestReport) -> ExitCode {
    println!(
        "ingest: {} events → {} nodes, {} checkpoints, {} names bound ({} skipped)",
        report.events,
        report.nodes_written,
        report.checkpoints,
        report.names_bound,
        report.skipped.len(),
    );
    for skip in &report.skipped {
        eprintln!("  skipped line {}: {}", skip.line_no, skip.reason);
    }
    if report.lines > 0 && report.events == 0 {
        eprintln!("ingest failed: no valid events in stream");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Resolve a CLI argument to a CID: a bound name if it resolves, otherwise the
/// argument used directly as a CID (validated downstream by the export walk).
fn resolve_or_cid(mem: &MemCli, arg: &str) -> Cid {
    mem.resolve(arg).unwrap_or_else(|_| Cid(arg.to_string()))
}

/// Make a name safe to use as a default `.car` filename.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `export-car <name|cid> [out.car] [--dry-run]` — export the subgraph reachable
/// from a root as a CARv1 (or preview its manifest with `--dry-run`).
fn cmd_export_car(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin export-car <name|cid> [out.car] [--dry-run]");
        return ExitCode::from(2);
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let outfile = args
        .get(2)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| format!("{}.car", sanitize(target)));

    let mem = MemCli::new(workdir());
    let root = resolve_or_cid(&mem, target);

    if dry_run {
        match mem.build_egress_plan_for_target(target, EgressOperation::PlaintextCarExport) {
            Ok(plan) => {
                print_egress_preview(&plan);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("export-car failed: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        let plan = match mem.build_egress_plan_for_target_and_backend(
            target,
            EgressOperation::PlaintextCarExport,
            "local-file",
            &outfile,
            "plaintext-portable",
        ) {
            Ok(plan) => plan,
            Err(e) => {
                eprintln!("export-car failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        print_egress_preview(&plan);
        println!("Warning: this writes a portable plaintext CAR outside the store.");
        if !args.iter().any(|arg| arg == "--confirm-plaintext-export") {
            eprintln!("export-car refused: review the manifest and add --confirm-plaintext-export");
            return ExitCode::from(2);
        }
        match mem.write_reviewed_plaintext_car(&plan, std::path::Path::new(&outfile)) {
            Ok(bytes) => {
                println!("exported root {} → {outfile} ({bytes} bytes)", root.0);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("export-car failed: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// `lock <root|name> [--label L]` — mark a root and its entire reachable
/// subgraph Locked / Local-only. Any later public publish or plaintext export
/// whose manifest reaches a locked node is refused by the core egress guard.
fn cmd_lock(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin lock <root|name> [--label \"...\"]");
        return ExitCode::from(2);
    };
    let label = flag_value(args, "--label").unwrap_or_default();
    let mem = MemCli::new(workdir());
    let root = resolve_or_cid(&mem, target);
    match mem.lock_subgraph(&root, &label) {
        Ok(()) => {
            let n = mem
                .export_car_manifest(&root)
                .map(|(c, _)| c.len())
                .unwrap_or(0);
            println!("locked {} (+{n} reachable nodes) — local-only", root.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lock failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `locks` — list the current Locked / Local-only roots.
fn cmd_locks() -> ExitCode {
    let mem = MemCli::new(workdir());
    match mem.locks() {
        Ok(locks) if locks.is_empty() => {
            println!("no locked roots");
            ExitCode::SUCCESS
        }
        Ok(locks) => {
            for l in locks {
                let label = if l.label.is_empty() { "-" } else { &l.label };
                println!("{}\t{label}", l.root);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("locks failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `import-car <file.car> <name>` — import a CARv1, verifying every block's CID,
/// then bind the root under `name`.
fn cmd_import_car(args: &[String]) -> ExitCode {
    let (Some(file), Some(name)) = (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) else {
        eprintln!("usage: concierge-plugin import-car <file.car> <name>");
        return ExitCode::from(2);
    };
    let car = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("import-car failed: cannot read {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mem = MemCli::new(workdir());
    let agent_id = args
        .iter()
        .position(|a| a == "--agent-id")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    let signature = args
        .iter()
        .position(|a| a == "--signature")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);

    let result = match (agent_id, signature) {
        (Some(agent_id), Some(signature)) => mem.import_signed_car(&car, name, agent_id, signature),
        (None, None) => mem.import_car(&car, name),
        _ => {
            eprintln!("import-car failed: use both --agent-id and --signature together");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(root) => {
            println!("imported root {} bound to `{name}`", root.0);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import-car failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_egress_preview(plan: &EgressPlan) {
    println!(
        "egress preview: {} root {} -> {} blocks, {} bytes via {} to {} ({})",
        plan.operation.label(),
        plan.root.0,
        plan.block_count,
        plan.byte_size,
        plan.backend,
        plan.backend_target,
        plan.network_posture,
    );
    if !plan.file_paths.is_empty() {
        println!("files: {}", plan.file_paths.join(", "));
    }
    for warning in &plan.sensitivity_warnings {
        println!("warning: {warning}");
    }
    if plan.known_public_receipts > 0 {
        println!(
            "warning: {} prior public publication receipt(s) exist",
            plan.known_public_receipts
        );
    }
    if plan.is_blocked() {
        println!("blocked: {}", plan.blocker_summary());
    }
}

/// The ambiguous legacy word `share` is preview-only and always refuses public
/// egress. Phase A requires the explicit `publish-public` operation.
fn cmd_share(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin share <root|name> [--dry-run]");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());

    match mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish) {
        Ok(plan) => {
            print_egress_preview(&plan);
            eprintln!(
                "share refused: public publication must use `publish-public <root> --confirm-public`"
            );
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("share failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `share-private` never accepts a password or emits a capability in a terminal.
/// It previews the exact source and directs authorization to the Data Platter.
fn cmd_share_private(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!(
            "usage: concierge-plugin share-private <root|name> --namespace N --recipients A,B"
        );
        return ExitCode::from(2);
    };
    let Some(namespace) = flag_value(args, "--namespace") else {
        eprintln!("share-private failed: --namespace is required");
        return ExitCode::from(2);
    };
    let Some(recipients) = flag_value(args, "--recipients") else {
        eprintln!("share-private failed: --recipients is required");
        return ExitCode::from(2);
    };
    let recipients = recipients
        .split(',')
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mem = MemCli::new(workdir());
    match mem.build_encrypt_and_share_plan(target, &namespace, &recipients) {
        Ok(plan) => {
            print_egress_preview(&plan.source);
            println!("private namespace: {}", plan.destination_namespace);
            println!("recipients: {}", plan.recipients.join(", "));
            if plan.source.is_blocked() {
                eprintln!(
                    "Private sharing blocked: this root is locked local plaintext.\nOpen the Data Platter and choose Convert to encrypted private and share."
                );
            } else {
                eprintln!(
                    "Private sharing requires password authorization from the Data Platter. Open it and choose Convert to encrypted private and share."
                );
            }
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("share-private failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_publish_public(args: &[String]) -> ExitCode {
    let Some(target) = args.get(1).map(String::as_str) else {
        eprintln!("usage: concierge-plugin publish-public <root|name> --confirm-public");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    let plan = match mem.build_egress_plan_for_target(target, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("publish-public failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_egress_preview(&plan);
    println!(
        "IRREVERSIBLE PUBLICATION WARNING: standard Kubo is public-networked unless explicitly isolated."
    );
    if !args.iter().any(|arg| arg == "--confirm-public") {
        eprintln!("publish-public refused: review the manifest and add --confirm-public");
        return ExitCode::from(2);
    }
    match mem.publish_public(&plan) {
        Ok(receipt) => {
            println!(
                "published {} via {} - fetch: {}",
                receipt.root, receipt.backend, receipt.gateway_url
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("publish-public failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_backend_info(info: &BackendInfo) {
    println!(
        "{}\t{}\t{}",
        info.name,
        info.blurb,
        info.requirements_summary()
    );
}

/// `backend list` / `backend show <name>` / `backend add <name>` — inspect and
/// configure the publishing backends compiled into the plugin.
fn cmd_backend(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("list") => match mem.list_backends() {
            Ok(backends) if backends.is_empty() => {
                println!("no backends compiled in");
                ExitCode::SUCCESS
            }
            Ok(backends) => {
                for backend in backends {
                    print_backend_info(&backend);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("backend list failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("show") => {
            let Some(name) = args.get(2).map(String::as_str) else {
                eprintln!("usage: concierge-plugin backend show <name>");
                return ExitCode::from(2);
            };
            match mem.list_backends() {
                Ok(backends) => match backends.into_iter().find(|b| b.name == name) {
                    Some(backend) => {
                        print_backend_info(&backend);
                        ExitCode::SUCCESS
                    }
                    None => {
                        eprintln!("backend show failed: unknown backend `{name}`");
                        ExitCode::from(2)
                    }
                },
                Err(e) => {
                    eprintln!("backend show failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("add") => {
            let Some(name) = args.get(2).map(String::as_str) else {
                eprintln!("usage: concierge-plugin backend add <name>");
                return ExitCode::from(2);
            };
            match mem.add_backend(name) {
                Ok(()) => {
                    println!("configured backend `{name}`");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("backend add failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: concierge-plugin backend <list | show <name> | add <name>>");
            ExitCode::from(2)
        }
    }
}

/// `id` — print this install's stable AgentID (generating it on first use).
fn cmd_id() -> ExitCode {
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
fn cmd_follow(args: &[String]) -> ExitCode {
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
fn cmd_nickname(args: &[String]) -> ExitCode {
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
fn cmd_following() -> ExitCode {
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
fn cmd_shared() -> ExitCode {
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
fn cmd_msg(args: &[String]) -> ExitCode {
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
fn cmd_thread(args: &[String]) -> ExitCode {
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
fn cmd_room(args: &[String]) -> ExitCode {
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

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn envelope_declares_room(json: &str, expected_room: &str) -> bool {
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

fn result_line(r: Result<(), concierge_core::Error>, ok: String) -> ExitCode {
    match r {
        Ok(()) => {
            println!("{ok}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("room failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn forbidden_security_flag(args: &[String]) -> Option<&str> {
    args.iter()
        .find(|arg| arg.starts_with("--force") || arg.starts_with("--password"))
        .map(String::as_str)
}

/// `gui [--port N]` — open the Data Platter explorer and privacy controls.
/// `mcp serve [--write]` — speak MCP (JSON-RPC 2.0 / stdio) so a host AI can use the
/// Concierge's tools. Read-only by default; `--write` enables the write tools
/// (which still never publish — publishing stays the user's GUI act). A harness
/// registers this with e.g. `claude mcp add --transport stdio concierge -- \
/// concierge-plugin mcp serve --write`.
fn cmd_mcp(args: &[String]) -> ExitCode {
    let sub = args.get(1).map(String::as_str);
    if !matches!(sub, Some("serve") | Some("--write") | None) {
        eprintln!("usage: concierge-plugin mcp serve [--write]");
        return ExitCode::FAILURE;
    }
    let write_enabled = args.iter().any(|arg| arg == "--write");
    let mem = MemCli::new(workdir());
    match concierge_mcp::serve_stdio(mem, write_enabled) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_gui(args: &[String]) -> ExitCode {
    let preferred = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(4173);
    let model = flag_value(args, "--model").unwrap_or_else(|| "manual mount".to_string());
    let open_browser = !args.iter().any(|arg| arg == "--no-open");
    let reuse = !args.iter().any(|arg| arg == "--no-reuse");
    let watch_pid = args
        .iter()
        .position(|a| a == "--watch-pid")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u32>().ok());
    let mem = MemCli::new(workdir());

    // Idempotent mount: if a Data Platter is already serving this store, reuse it
    // (open the browser to it) instead of spawning a duplicate. This is what lets
    // every harness call `gui` on session start without leaking servers.
    if reuse {
        if let Some(port) = concierge_gui::running_gui_port(&mem) {
            let url = format!("http://127.0.0.1:{port}");
            println!("Data Platter already running → {url} (reusing)");
            if open_browser {
                let _ = concierge_gui::open_browser(&url);
            }
            return ExitCode::SUCCESS;
        }
    }

    let port = concierge_gui::pick_free_port(preferred);
    let addr = format!("127.0.0.1:{port}");
    let store_label = workdir().display().to_string();
    println!("Concierge Data Platter → http://{addr}  (loopback-only; Ctrl-C to stop)");
    match concierge_gui::serve_with_options(
        mem,
        &addr,
        concierge_gui::GuiOptions::new(model, store_label, open_browser, watch_pid),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gui failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(flag) = forbidden_security_flag(&args) {
        eprintln!(
            "refused security bypass flag `{flag}`; authorize locked actions from the Data Platter"
        );
        return ExitCode::from(2);
    }
    match args.first().map(String::as_str) {
        Some("init") => cmd_init(),
        Some("attach") => cmd_attach(&args),
        Some("claude-code") => cmd_claude_code(&args),
        Some("ingest") => cmd_ingest(args.get(1).map(String::as_str)),
        Some("checkpoint") => cmd_checkpoint(
            args.iter()
                .position(|a| a == "--name")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
                .or_else(|| args.get(1).map(String::as_str)),
        ),
        Some("import") => cmd_import(&args),
        Some("recall") => cmd_recall(args.get(1).map(String::as_str)),
        Some("retrieve") => cmd_retrieve(&args),
        Some("export-car") => cmd_export_car(&args),
        Some("import-car") => cmd_import_car(&args),
        Some("share") => cmd_share(&args),
        Some("share-private") => cmd_share_private(&args),
        Some("publish-public") => cmd_publish_public(&args),
        Some("lock") => cmd_lock(&args),
        Some("locks") => cmd_locks(),
        Some("quarantine") => cmd_quarantine(&args),
        Some("synthesis") => cmd_synthesis(&args),
        Some("connect") => cmd_connect(&args),
        Some("outbox") => cmd_outbox(&args),
        Some("user") => cmd_user(&args),
        Some("network") => cmd_network(&args),
        Some("sync") => cmd_sync(&args),
        Some("actor") => cmd_actor(&args),
        Some("sidekick") => cmd_sidekick(&args),
        Some("backend") => cmd_backend(&args),
        Some("id") => cmd_id(),
        Some("follow") => cmd_follow(&args),
        Some("nickname") => cmd_nickname(&args),
        Some("following") => cmd_following(),
        Some("shared") => cmd_shared(),
        Some("msg") => cmd_msg(&args),
        Some("thread") => cmd_thread(&args),
        Some("room") => cmd_room(&args),
        Some("gui") => cmd_gui(&args),
        Some("mcp") => cmd_mcp(&args),
        Some("help") | Some("--help") | Some("-h") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print!("{HELP}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{envelope_declares_room, forbidden_security_flag, HELP};

    #[test]
    fn inbound_envelope_must_declare_the_subscribed_room() {
        let envelope = r#"{
            "id":"room-a",
            "payload":"hello",
            "next":[],
            "refs":[],
            "clock":1,
            "key":"author",
            "sig":"signature"
        }"#;
        assert!(envelope_declares_room(envelope, "room-a"));
        assert!(!envelope_declares_room(envelope, "room-b"));
        assert!(!envelope_declares_room("not-json", "room-a"));
    }

    #[test]
    fn cli_exposes_no_password_or_force_bypass() {
        assert!(HELP.contains("share-private"));
        assert!(!HELP.contains("--password"));
        assert!(!HELP.contains("--force"));
        assert!(!HELP.contains("unlock"));
        assert_eq!(
            forbidden_security_flag(&["publish-public".into(), "--password=secret".into()]),
            Some("--password=secret")
        );
        assert_eq!(
            forbidden_security_flag(&["export-car".into(), "--force".into()]),
            Some("--force")
        );
    }
}
