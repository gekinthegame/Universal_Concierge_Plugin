//! Universal Concierge Plugin CLI.
//!
//! Command-line entry point for the local Concierge store, GUI, publishing,
//! messaging, and host-integration workflows.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};

use concierge_adapter_hermes::ingest as ingest_hermes;
use concierge_adapter_jsonl::{
    ingest, ingest_envelopes, Importer, IngestReport, JsonlImporter, MarkdownImporter,
};
use concierge_core::{
    cid_from_link, default_embedder, Cid, CidOrName, Config, CoreBinding, Depth, EgressOperation,
    Librarian, MemCli, Record,
};
use serde_json::Value;

mod messaging_commands;
mod network_commands;
mod publishing_commands;

#[cfg(test)]
use messaging_commands::envelope_declares_room;
use messaging_commands::{
    cmd_follow, cmd_following, cmd_id, cmd_msg, cmd_nickname, cmd_room, cmd_shared, cmd_thread,
    flag_value,
};
use network_commands::{cmd_actor, cmd_network, cmd_sidekick, cmd_sync};
use publishing_commands::{
    cmd_backend, cmd_export_car, cmd_import_car, cmd_lock, cmd_locks, cmd_publish_public,
    cmd_share, cmd_share_private,
};

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
    ingest <file|->      Ingest host-neutral events into IPLD memory (- = stdin)  [Phase 2]
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
    update <check|apply> Binary self-update: check the release channel, or download +
                         verify + stage the latest (applies on next launch) [Autoupdate]
    rules <status|refresh|source <ipns>|pin <key>|pause|resume>
                         YARA-X rules channel: show epoch/freshness/publisher, force a
                         signed refresh over IPNS, set the IPNS source, pin a key, or
                         pause/resume automatic updates (kill switch)    [Autoupdate]
    brain <status|model <id>>
                         Brain tab: show the connected local LLM (oMLX/OpenAI-compatible)
                         + embedder status, or set the model to route to     [Brain]
    youtube <status|connect|disconnect|receipts|upload <file> [flags]>
                         Upload a rendered canvas video to YouTube (official API).
                         upload flags: --title --description --tags a,b --privacy
                         private|unlisted|public --category N --made-for-kids
                         --synthetic --notify --playlist ID --thumbnail F
                         --caption F --caption-lang L                    [YouTube]
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
    mcp serve [--write]  Serve memory over MCP (stdio) for a host AI
    setup                Connect the Concierge to Claude Code as an MCP server
    blender-setup        Connect Blender (BlenderMCP) to the host AI for Movie/Animation
    help                 Show this help
";

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
            "--external" => i += 1,                                 // boolean flag
            other => {
                query_parts.push(other);
                i += 1;
            }
        }
    }
    let query = query_parts.join(" ");
    if query.trim().is_empty() {
        eprintln!(
            "usage: concierge-plugin retrieve <query…> [--budget N] [--depth brief|summary|full]"
        );
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
                println!(
                    "\n— external references ({} · untrusted, not auto-injected) —",
                    hits.len()
                );
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
                    println!(
                        "connected `{alias}` → {url} (querying it sends your query out — egress)"
                    );
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
            let limit = flag_value(args, "--limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(10);
            match mem.federate_search(&query, limit) {
                Ok(hits) => {
                    println!(
                        "{} external reference(s) — untrusted, not auto-injected",
                        hits.len()
                    );
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
                eprintln!(
                    "usage: concierge-plugin outbox wake <query…> [--authority ID] [--event E]"
                );
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
                println!(
                    "(root ownership key — distinct from this device's identity; never copied)"
                );
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
            Err(e) => {
                eprintln!("user recovery failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("rotate") => match mem.rotate_user_key() {
            Ok(r) => {
                println!("rotated active key (UserID {} unchanged)", r.user_id);
                println!("  new active key: {}", r.active_key);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("user rotate failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("recover") => match mem.recover_user_key() {
            Ok(r) => {
                println!("recovered UserID {} (identity preserved)", r.user_id);
                println!("  new active key: {}", r.active_key);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("user recover failed: {e}");
                ExitCode::FAILURE
            }
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
                Err(e) => {
                    eprintln!("user log invalid: {e}");
                    ExitCode::FAILURE
                }
            },
            Ok(None) => {
                println!("no recovery log yet — run `user recovery` to establish one");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("user show failed: {e}");
                ExitCode::FAILURE
            }
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
fn cmd_quarantine(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    let resolve = |target: &str| -> Cid {
        mem.resolve(target)
            .unwrap_or_else(|_| Cid(target.to_string()))
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
            let reason = if args.len() > 3 {
                args[3..].join(" ")
            } else {
                "manual".to_string()
            };
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

/// `concierge-plugin update <check|apply>` — the app (binary) self-update channel.
fn cmd_update(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("check") | None => match mem.update_check() {
            Ok(Some(release)) => {
                println!(
                    "update available: {} → {}",
                    env!("CARGO_PKG_VERSION"),
                    release.version
                );
                match release.asset_name {
                    Some(name) => println!("  asset: {name}"),
                    None => println!("  (no prebuilt asset for this platform)"),
                }
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("up to date (running {})", env!("CARGO_PKG_VERSION"));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("update check failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("apply") => match mem.update_apply() {
            Ok(Some(staged)) => {
                println!(
                    "staged {} — verified and ready; it will apply on next launch.",
                    staged.version
                );
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("already up to date — nothing to apply");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("update apply failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("unknown update subcommand `{other}` — use check|apply");
            ExitCode::from(2)
        }
    }
}

/// `concierge-plugin rules <status|refresh|source|pin|pause|resume>` — the YARA-X rules channel.
fn cmd_rules(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("status") | None => match mem.rules_status() {
            Ok(s) => {
                let age = if s.age_secs == 0 {
                    "fresh (baked baseline)".to_string()
                } else {
                    format!("{} day(s) old", s.age_secs / 86_400)
                };
                println!(
                    "rules epoch {} — {} ({} rules)",
                    s.epoch, s.version, s.rule_count
                );
                println!("  cid:        {}", s.cid);
                println!("  publisher:  {}", s.publisher_fpr);
                println!(
                    "  freshness:  {age}{}",
                    if s.fresh {
                        ""
                    } else {
                        " — may be stale; enable your node to refresh"
                    }
                );
                println!(
                    "  auto-rules: {}",
                    if s.paused {
                        "PAUSED (kill switch on)"
                    } else {
                        "on"
                    }
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rules status failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("refresh") => match mem.rules_refresh() {
            Ok(out) => {
                if out.updated {
                    println!("updated to epoch {} ({})", out.epoch, out.version);
                    if out.stale_publisher {
                        println!(
                            "  note: publisher has been quiet for {} day(s)",
                            out.age_secs / 86_400
                        );
                    }
                } else {
                    println!("already current at epoch {}", out.epoch);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rules refresh failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("source") => {
            let Some(ipns) = args.get(2) else {
                eprintln!("usage: concierge-plugin rules source <ipns-name>");
                return ExitCode::from(2);
            };
            match mem.rules_set_source(ipns) {
                Ok(()) => {
                    println!("rules IPNS source set to {ipns}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rules source failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("pin") => {
            let Some(key) = args.get(2) else {
                eprintln!("usage: concierge-plugin rules pin <ed25519-pubkey-hex>");
                return ExitCode::from(2);
            };
            match mem.rules_pin(key) {
                Ok(()) => {
                    println!("pinned publisher key {key}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rules pin failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(state @ ("pause" | "resume")) => {
            let paused = state == "pause";
            match mem.rules_set_paused(paused) {
                Ok(()) => {
                    println!(
                        "automatic rules updates {}",
                        if paused { "paused" } else { "resumed" }
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rules {state} failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!(
                "unknown rules subcommand `{other}` — use status|refresh|source|pin|pause|resume"
            );
            ExitCode::from(2)
        }
    }
}

/// `concierge-plugin youtube <status|connect|disconnect|receipts|upload …>`.
/// `concierge-plugin brain <status|model <id>>` — the connected sovereign LLM + embedder.
fn cmd_brain(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("status") | None => match mem.brain_metrics() {
            Ok(m) => {
                if m.baseline.up {
                    println!(
                        "engine: {} @ {} ({} model(s){})",
                        m.baseline.engine,
                        m.baseline.base_url,
                        m.baseline.models.len(),
                        m.baseline
                            .active_model
                            .map(|a| format!(", active: {a}"))
                            .unwrap_or_default()
                    );
                    if !m.baseline.models.is_empty() {
                        println!("  models: {}", m.baseline.models.join(", "));
                    }
                    match &m.rich {
                        Some(_) => println!("  rich metrics: available (oMLX)"),
                        None => println!("  rich metrics: not available for this engine"),
                    }
                } else {
                    println!(
                        "engine: not detected at {} — start a local engine (e.g. oMLX)",
                        m.baseline.base_url
                    );
                }
                println!(
                    "embedder: {} / {}{}",
                    m.embedder.backend,
                    m.embedder.model,
                    if m.embedder.shares_engine {
                        " (shares the connected engine)"
                    } else {
                        ""
                    }
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("brain status failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("model") => {
            let Some(id) = args.get(2) else {
                eprintln!("usage: concierge-plugin brain model <model-id>");
                return ExitCode::from(2);
            };
            match mem.brain_set_model(id) {
                Ok(()) => {
                    println!("routing to model: {id}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("brain model failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("unknown brain subcommand `{other}` — use status|model");
            ExitCode::from(2)
        }
    }
}

fn cmd_youtube(args: &[String]) -> ExitCode {
    let mem = MemCli::new(workdir());
    match args.get(1).map(String::as_str) {
        Some("status") | None => match mem.youtube_status() {
            Ok(s) => {
                if s.connected {
                    println!(
                        "YouTube: connected{}",
                        s.channel.map(|c| format!(" ({c})")).unwrap_or_default()
                    );
                } else {
                    println!("YouTube: not connected — run `concierge-plugin youtube connect`");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("youtube status failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("connect") => yt_connect(&mem),
        Some("disconnect") => match mem.youtube_disconnect() {
            Ok(()) => {
                println!("YouTube disconnected — tokens removed");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("youtube disconnect failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("receipts") => match mem.youtube_receipts() {
            Ok(rs) => {
                if rs.is_empty() {
                    println!("no uploads yet");
                } else {
                    for r in rs {
                        println!(
                            "{}  {}  [{}]  {}",
                            r.video_url, r.title, r.privacy_status, r.source_rel
                        );
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("youtube receipts failed: {e}");
                ExitCode::FAILURE
            }
        },
        Some("upload") => yt_upload(&mem, args),
        Some(other) => {
            eprintln!(
                "unknown youtube subcommand `{other}` — use status|connect|disconnect|receipts|upload"
            );
            ExitCode::from(2)
        }
    }
}

/// Drive a Google Desktop OAuth login over an ephemeral loopback redirect.
fn yt_connect(mem: &MemCli) -> ExitCode {
    use std::io::{Read, Write};
    if !concierge_core::youtube::oauth_configured() {
        eprintln!(
            "this build has no Google OAuth client — rebuild with UCP_YT_CLIENT_ID / \
             UCP_YT_CLIENT_SECRET (see crates/core/src/youtube/SETUP.md)"
        );
        return ExitCode::FAILURE;
    }
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind loopback for OAuth: {e}");
            return ExitCode::FAILURE;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let redirect = format!("http://127.0.0.1:{port}");
    let start = mem.youtube_oauth_start(&redirect);
    println!(
        "Authorize YouTube in your browser:\n{}",
        start.authorize_url
    );
    let _ = concierge_gui::open_app(&start.authorize_url);

    let (mut stream, _) = match listener.accept() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("OAuth redirect not received: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);
    let target = request
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = String::new();
    let mut state = String::new();
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("code=") {
            code = urldecode(v);
        } else if let Some(v) = pair.strip_prefix("state=") {
            state = urldecode(v);
        }
    }
    let body = "<html><body>YouTube connected — you can close this tab.</body></html>";
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    if state != start.state || code.is_empty() {
        eprintln!("OAuth state mismatch or missing code — aborting");
        return ExitCode::FAILURE;
    }
    let token = match concierge_core::youtube::oauth_exchange(&code, &start.verifier, &redirect) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token exchange failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match mem.youtube_save_oauth(token, None) {
        Ok(()) => {
            println!("YouTube connected.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not save credentials: {e}");
            ExitCode::FAILURE
        }
    }
}

fn yt_upload(mem: &MemCli, args: &[String]) -> ExitCode {
    let Some(file) = args.get(2) else {
        eprintln!(
            "usage: concierge-plugin youtube upload <file> [--title T] [--privacy private|unlisted|public] …"
        );
        return ExitCode::from(2);
    };
    let meta = concierge_core::VideoMetadata {
        title: flag_value(args, "--title").unwrap_or_else(|| "Untitled".to_string()),
        description: flag_value(args, "--description").unwrap_or_default(),
        tags: flag_value(args, "--tags")
            .map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        category_id: flag_value(args, "--category").unwrap_or_else(|| "22".to_string()),
        privacy_status: flag_value(args, "--privacy").unwrap_or_else(|| "private".to_string()),
        made_for_kids: args.iter().any(|a| a == "--made-for-kids"),
        contains_synthetic_media: args.iter().any(|a| a == "--synthetic"),
        publish_at: flag_value(args, "--publish-at"),
        notify_subscribers: args.iter().any(|a| a == "--notify"),
    };
    let req = concierge_core::UploadRequest {
        file_path: file.clone(),
        metadata: meta,
        thumbnail_path: flag_value(args, "--thumbnail"),
        caption_path: flag_value(args, "--caption"),
        caption_language: flag_value(args, "--caption-lang").unwrap_or_default(),
        playlist_id: flag_value(args, "--playlist"),
    };
    let mut last_pct = u64::MAX;
    let mut progress = |sent: u64, total: u64| {
        let pct = sent.saturating_mul(100).checked_div(total).unwrap_or(0);
        if pct != last_pct {
            last_pct = pct;
            eprint!("\ruploading… {pct}%");
        }
    };
    match mem.youtube_upload(&req, &mut progress) {
        Ok(receipt) => {
            eprintln!();
            println!("uploaded: {}", receipt.video_url);
            for x in &receipt.extras {
                println!("  {x}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\nyoutube upload failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Minimal percent-decoding for OAuth redirect query values.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
                    eprintln!(
                        "# {} messages — summarize on the host, then `synthesis record`",
                        provenance.len()
                    );
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
                eprintln!(
                    "(the summary must come from the host model — the node does no generation)"
                );
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
                    println!(
                        "recorded host synthesis of `{room}` ({} messages) as a Decision node",
                        provenance.len()
                    );
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

/// `ingest <file.jsonl|->` — read a JSONL event stream into IPLD memory.
/// `-` reads stdin (the pipe path every adapter documents: `adapter | ingest -`).
fn cmd_ingest(path: Option<&str>) -> ExitCode {
    let Some(path) = path else {
        eprintln!("usage: concierge-plugin ingest <file.jsonl|->   (- = stdin)");
        return ExitCode::from(2);
    };
    let mem = MemCli::new(workdir());
    if path == "-" {
        let report = ingest(std::io::stdin().lock(), &mem, &workdir());
        return finish_ingest(&report);
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("ingest failed: cannot open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
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
        Some("inject") => claude_code_inject(args),
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
    let entries = match if peek {
        mem.outbox_peek()
    } else {
        mem.outbox_drain()
    } {
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
        println!(
            "no Claude Code sessions found under {}",
            projects_dir.display()
        );
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
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("session");
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

/// `setup` — connect the Concierge to Claude Code as an MCP server so its tools
/// (recall / retrieve / browse / write_site / scaffold_engine / design …) are
/// available in any Claude Code session. Run by the installer and on first GUI
/// launch; safe to run again.
fn cmd_setup() -> ExitCode {
    println!("{}", connect_claude_mcp());
    ExitCode::SUCCESS
}

/// `blender-setup` — register **BlenderMCP** with Claude Code so the host AI can drive
/// Blender for Movie/Animation projects. Best-effort; reports missing prerequisites.
fn cmd_blender_setup() -> ExitCode {
    use std::process::Command;
    let listed = match Command::new("claude").args(["mcp", "list"]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => {
            println!("Claude Code (the `claude` CLI) was not found. Install Claude Code, then re-run this.");
            return ExitCode::FAILURE;
        }
    };
    if listed
        .lines()
        .any(|l| l.trim_start().starts_with("blender:"))
    {
        println!("Blender (BlenderMCP) is already connected to Claude Code.");
    } else {
        let has_uvx = Command::new("uvx")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_uvx {
            println!("Install uv (the Python runner BlenderMCP uses): https://docs.astral.sh/uv/ — then re-run this.");
            return ExitCode::FAILURE;
        }
        let ok = Command::new("claude")
            .args([
                "mcp",
                "add",
                "-s",
                "user",
                "blender",
                "--",
                "uvx",
                "blender-mcp",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("Connected Blender (BlenderMCP) to Claude Code. Restart Claude Code to use its tools.");
        } else {
            println!(
                "Auto-register failed. Run: claude mcp add -s user blender -- uvx blender-mcp"
            );
            return ExitCode::FAILURE;
        }
    }
    println!(
        "\nNext: install the Blender add-on:\n  Blender → Edit → Preferences → Add-ons → Install… → choose addon.py\n  (Concierge repo: vendor/blender-mcp/addon.py, or download from https://blendermcp.org)\n  Enable “Interface: Blender MCP”, then in the 3D viewport press N → BlenderMCP → Connect to MCP server."
    );
    ExitCode::SUCCESS
}

/// Register this binary as a Claude Code MCP server (`concierge`, write tools on).
/// Idempotent, and **upgrades a stale registration** (wrong path / missing `--write`)
/// so the Studio/canvas write tool is always available. Best-effort: a missing
/// `claude` CLI is reported, not an error.
fn connect_claude_mcp() -> String {
    use std::process::Command;
    let self_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return "MCP auto-connect: could not resolve own path.".to_string(),
    };
    let want = self_path.to_string_lossy().into_owned();

    // Probe: is the `claude` CLI present, and what's already registered?
    let listed = match Command::new("claude")
        .args(["mcp", "list"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => {
            return "Claude Code (the `claude` CLI) was not found — skipped MCP auto-connect. \
                Install Claude Code, then run:  concierge-plugin setup"
                .to_string()
        }
    };

    // Already correct (this binary + write tools)? Leave it.
    if listed.lines().any(|line| {
        line.trim_start().starts_with("concierge:")
            && line.contains(&want)
            && line.contains("--write")
    }) {
        return "Claude Code: the Concierge MCP server is already connected.".to_string();
    }

    // Otherwise (absent or stale): clear any old entry, then add the correct one.
    for scope in ["user", "local"] {
        let _ = Command::new("claude")
            .args(["mcp", "remove", "concierge", "-s", scope])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let status = Command::new("claude")
        .args([
            "mcp",
            "add",
            "-s",
            "user",
            "concierge",
            "--",
            &want,
            "mcp",
            "serve",
            "--write",
        ])
        .stdin(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => "Claude Code: connected the Concierge as an MCP server \
            ('concierge', write tools on). Restart Claude Code to use its tools."
            .to_string(),
        _ => "Claude Code found, but auto-registering failed. Run manually:\n  \
            claude mcp add -s user concierge -- concierge-plugin mcp serve --write"
            .to_string(),
    }
}

/// Connect to Claude Code once per install (sentinel-gated), on GUI launch — covers
/// the .dmg/.exe installs where no install script runs. Idempotent + non-fatal.
fn maybe_connect_claude(mem: &MemCli) {
    let sentinel = mem.store_dir().ok().map(|dir| dir.join(".mcp-connected"));
    if let Some(path) = &sentinel {
        if path.exists() {
            return;
        }
    }
    println!("{}", connect_claude_mcp());
    if let Some(path) = &sentinel {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, b"1");
    }
}

fn spawn_rules_autoupdater(mem: MemCli) {
    std::thread::spawn(move || loop {
        match mem.rules_auto_refresh_if_due() {
            Ok(Some(out)) if out.updated => {
                println!(
                    "auto-rules updated to epoch {} ({})",
                    out.epoch, out.version
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("note: auto-rules refresh skipped: {e}"),
        }
        let sleep_secs = mem
            .config()
            .map(|cfg| (cfg.update.poll_interval_secs / 4).clamp(60, 3_600))
            .unwrap_or(15 * 60);
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
    });
}

fn spawn_app_update_checker(mem: MemCli) {
    std::thread::spawn(move || loop {
        match mem.update_poll_cache() {
            Ok(status) => {
                if let Some(release) = status.release {
                    println!("app update available: {}", release.version);
                }
            }
            Err(e) => eprintln!("note: app update check skipped: {e}"),
        }
        let sleep_secs = mem
            .config()
            .map(|cfg| cfg.update.poll_interval_secs.clamp(60, 21_600))
            .unwrap_or(6 * 60 * 60);
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
    });
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

    // Apply-on-relaunch (autoupdater §4): if a verified binary update was staged on a
    // previous run, swap it in now, before serving. Best-effort — a failed swap must
    // never block the app from starting, so we only surface it.
    match mem.update_apply_on_launch() {
        Ok(Some(v)) => println!("applied staged update → {v} (restart to run it)"),
        Ok(None) => {}
        Err(e) => eprintln!("note: could not apply staged update: {e}"),
    }

    // First launch (any install path): connect to Claude Code as an MCP server.
    maybe_connect_claude(&mem);

    // Idempotent mount: if a Data Platter is already serving this store, reuse it
    // (open the browser to it) instead of spawning a duplicate. This is what lets
    // every harness call `gui` on session start without leaking servers.
    if reuse {
        if let Some(port) = concierge_gui::running_gui_port(&mem) {
            let url = format!("http://127.0.0.1:{port}");
            println!("Data Platter already running → {url} (reusing)");
            if open_browser {
                let _ = concierge_gui::open_app(&url);
            }
            return ExitCode::SUCCESS;
        }
    }

    let port = concierge_gui::pick_free_port(preferred);
    let addr = format!("127.0.0.1:{port}");
    let store_label = workdir().display().to_string();
    spawn_rules_autoupdater(mem.clone());
    spawn_app_update_checker(mem.clone());
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
        Some("update") => cmd_update(&args),
        Some("rules") => cmd_rules(&args),
        Some("youtube") => cmd_youtube(&args),
        Some("brain") => cmd_brain(&args),
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
        Some("setup") => cmd_setup(),
        Some("blender-setup") => cmd_blender_setup(),
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
include!("tests.rs");
