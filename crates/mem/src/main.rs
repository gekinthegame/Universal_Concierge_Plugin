//! `mem` CLI — the human surface over the memory layer. Data commands
//! (`put`/`get`/`bind`/`resolve`/`ls`/`cat`) drive the real store; `sync` and
//! `chat` are wired but their engines land in Phase 4 and Phase 3.2.
//!
//! Nodes are given/printed as JSON (the human surface); DAG-CBOR remains the
//! storage format. Every command emits `[mem] …` trace lines to stderr.

use anyhow::{Context, Result};
use atomic_write_file::AtomicWriteFile;
use clap::{Parser, Subcommand};
use mem::blockstore::LocalBlocks;
use mem::cid::Cid;
use mem::config::{CONFIG_PATH, Config};
use mem::gc::RetentionPolicy;
use mem::names::NameIndex;
use mem::node::{Node, Source};
use mem::repl::{Session, run};
use mem::store::{Lookup, Store};
use mem::tombstones::{Tombstone, iso8601};
use mem::{backend, model, trace};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "mem",
    version,
    about = "Unified memory layer — content-addressed agent state"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Store a node given as JSON; prints its CID.
    Put {
        /// e.g. '{"type":"memory","kind":"project","text":"hello"}'.
        /// Pass `-`, or omit entirely, to read the JSON from stdin (use this
        /// for large nodes such as blobs that would overflow the OS argv limit).
        json: Option<String>,
    },
    /// Fetch a record by CID; prints it as JSON.
    Get { cid: String },
    /// Fetch many records at once: read one CID per line from stdin and print
    /// one compact JSON line per CID (`{"cid":…,"record":…}` for a live record,
    /// `{"cid":…,"tombstone":…}` if pruned, `{"cid":…,"error":…}` on failure).
    /// Loads the block/name indexes once, so the GUI can build a whole-store
    /// graph in a single process instead of one `mem get` per node.
    GetMany,
    /// Print the deduplicated union of block CIDs reachable from a set of roots,
    /// one per line. Roots are read from stdin (one CID per line); if stdin is
    /// empty, every bound name is used as a root. Block-level (no body
    /// serialization), so the GUI can size "reachable / orphans" cheaply.
    Reachable,
    /// Point a name at a root CID.
    Bind { name: String, cid: String },
    /// Resolve a name to its current CID.
    Resolve { name: String },
    /// List all name → CID bindings.
    Ls,
    /// Print a record by name or CID (tombstone-aware).
    Cat { target: String },
    /// Ingest a local directory into the memory layer.
    Ingest {
        /// Path to the directory or file to ingest.
        path: String,
        /// Optional name to bind the root manifest CID to.
        #[arg(long)]
        name: Option<String>,
        /// Run one named ingest plugin. Repeat to run multiple plugins.
        #[arg(long = "plugin")]
        plugins: Vec<String>,
        /// Run every ingest plugin compiled into this build.
        #[arg(long, conflicts_with = "plugins")]
        all_plugins: bool,
    },
    /// Publish a chosen root (by name or CID) to a network backend.
    Share { target: String },
    /// Manage network backends (list compiled-in, configure one).
    Backend {
        #[command(subcommand)]
        action: BackendCmd,
    },
    /// Configure model roles (concierge, worker).
    Model {
        #[command(subcommand)]
        action: ModelCmd,
    },
    /// Interactive setup: choose a working directory and configure models.
    Init,
    /// Resume the chat from a checkpoint (defaults to `latest`).
    Resume {
        /// Checkpoint name or CID; omitted resumes `latest`.
        target: Option<String>,
    },
    /// Interactive chat (Phase 3.2).
    Chat,
    /// Retention/GC: trim the auto-checkpoint chain to the newest N and sweep
    /// orphan blocks. Every deletion leaves a tombstone receipt.
    Gc {
        /// Override how many newest auto-checkpoints to keep (default from config).
        #[arg(long)]
        keep: Option<u32>,
    },
}

#[derive(Subcommand)]
enum BackendCmd {
    /// List the network backends compiled into this build.
    List,
    /// Configure a compiled-in backend and show its required environment.
    Add { name: String },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// List configured model roles.
    List,
    /// Set or swap a role's model; omitted flags keep existing/default values.
    Set {
        role: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove an optional role (the concierge role can't be removed).
    Rm { role: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => init_flow(std::io::stdin().lock()),
        Cmd::Chat => {
            prepare_chat_workspace(std::io::stdin().lock())?;
            run_command(Cmd::Chat)
        }
        cmd => run_command(cmd),
    }
}

fn run_command(cmd: Cmd) -> Result<()> {
    let cfg = Config::load()?;
    trace::set_verbosity(cfg.trace.verbosity);

    let blocks = LocalBlocks::new(cfg.blocks_dir());
    let names = NameIndex::load(cfg.names_path())?;
    let mut store = Store::new(blocks, names);

    match cmd {
        Cmd::Put { json } => {
            let json = match json {
                Some(j) if j != "-" => j,
                _ => {
                    use std::io::Read as _;
                    let mut buf = String::new();
                    std::io::stdin()
                        .lock()
                        .read_to_string(&mut buf)
                        .context("reading node JSON from stdin")?;
                    buf
                }
            };
            let node: Node = serde_json::from_str(&json).context("invalid node JSON")?;
            let cid = store.put_node(node, Source::User)?;
            println!("{cid}");
        }
        Cmd::Get { cid } => {
            let cid = parse_cid(&cid)?;
            match store.lookup(&cid)? {
                Lookup::Present(record) => {
                    println!("{}", serde_json::to_string_pretty(&record)?)
                }
                Lookup::Pruned(t) => print_receipt(&cid, &t),
            }
        }
        Cmd::GetMany => {
            let stdout = std::io::stdout();
            let mut out = std::io::BufWriter::new(stdout.lock());
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                let raw = line.trim();
                if raw.is_empty() {
                    continue;
                }
                let entry = match parse_cid(raw) {
                    Ok(cid) => match store.lookup(&cid) {
                        Ok(Lookup::Present(record)) => {
                            serde_json::json!({ "cid": raw, "record": record })
                        }
                        Ok(Lookup::Pruned(t)) => {
                            let receipt = format!(
                                "{raw} was pruned\n  died:   {} ({})\n  reason: {}\n  see:    {}",
                                iso8601(t.pruned_at),
                                t.pruned_at,
                                t.reason,
                                t.superseded_by
                                    .map(|next| next.to_string())
                                    .unwrap_or_else(|| "(nothing — orphan)".to_string()),
                            );
                            serde_json::json!({ "cid": raw, "tombstone": receipt })
                        }
                        Err(e) => serde_json::json!({ "cid": raw, "error": e.to_string() }),
                    },
                    Err(e) => serde_json::json!({ "cid": raw, "error": e.to_string() }),
                };
                writeln!(out, "{}", serde_json::to_string(&entry)?)?;
            }
            out.flush()?;
        }
        Cmd::Reachable => {
            // Roots: stdin CIDs if provided, otherwise every bound name.
            let mut roots: Vec<Cid> = Vec::new();
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                let raw = line.trim();
                if raw.is_empty() {
                    continue;
                }
                roots.push(parse_cid(raw)?);
            }
            if roots.is_empty() {
                roots = store.names().map(|(_, cid)| *cid).collect();
            }
            let mut seen: std::collections::HashSet<Cid> = std::collections::HashSet::new();
            let stdout = std::io::stdout();
            let mut out = std::io::BufWriter::new(stdout.lock());
            for root in &roots {
                // A root may itself be pruned/missing; skip rather than abort the
                // whole count.
                if let Ok(reachable) = store.reachable(root) {
                    for cid in reachable {
                        if seen.insert(cid) {
                            writeln!(out, "{cid}")?;
                        }
                    }
                }
            }
            out.flush()?;
        }
        Cmd::Bind { name, cid } => {
            let cid = parse_cid(&cid)?;
            store.bind(&name, cid)?;
        }
        Cmd::Resolve { name } => {
            println!("{}", store.resolve(&name)?);
        }
        Cmd::Ls => {
            for (name, cid) in store.names() {
                println!("{name}\t{cid}");
            }
        }
        Cmd::Cat { target } => {
            let cid = match target.parse::<Cid>() {
                Ok(cid) => cid,
                Err(_) => store.resolve(&target)?,
            };
            match store.lookup(&cid)? {
                Lookup::Present(record) => {
                    println!("{}", serde_json::to_string_pretty(&record)?)
                }
                Lookup::Pruned(t) => print_receipt(&cid, &t),
            }
        }
        Cmd::Ingest {
            path,
            name,
            plugins,
            all_plugins,
        } => {
            let options = mem::ingest::IngestOptions {
                plugins,
                all_plugins,
            };
            let report = mem::ingest::ingest_with_options(&store, Path::new(&path), &options)?;
            if let Some(name) = name {
                store.bind(&name, report.root)?;
            }
            println!("root_manifest\t{}", report.root);
            println!("ingest_run\t{}", report.ingest_run);
            println!("file_count\t{}", report.stats.file_count);
            println!("byte_count\t{}", report.stats.byte_count);
            println!("ignored_count\t{}", report.stats.ignored_count);
            println!("plugin_records\t{}", report.stats.plugin_records);
            println!("plugin_failures\t{}", report.stats.plugin_failures);
            for (path, records) in &report.stats.per_file_plugin_records {
                let failures = report
                    .stats
                    .per_file_plugin_failures
                    .get(path)
                    .copied()
                    .unwrap_or(0);
                println!("plugin_file\t{path}\t{records}\t{failures}");
            }
            for (path, failures) in &report.stats.per_file_plugin_failures {
                if !report.stats.per_file_plugin_records.contains_key(path) {
                    println!("plugin_file\t{path}\t0\t{failures}");
                }
            }
            for failure in &report.plugin_failures {
                println!(
                    "plugin_failure\t{}\t{}\t{}",
                    failure.plugin, failure.path, failure.message
                );
            }
        }
        Cmd::Share { target } => {
            let root = match target.parse::<Cid>() {
                Ok(cid) => cid,
                Err(_) => store.resolve(&target)?,
            };
            let name = cfg.backend.name.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "no backend configured; set [backend].name in .concierge/config.toml \
                     (e.g. \"ipfs\") and build with that feature"
                )
            })?;
            let registry = backend::registry();
            let entry = registry.get(name).ok_or_else(|| {
                anyhow::anyhow!("backend {name:?} is not available; enable its Cargo feature")
            })?;
            let remote = (entry.factory)(&cfg)?;
            store.share(remote.as_ref(), &root)?;
            println!("shared {root} via {name}");
        }
        Cmd::Backend { action } => match action {
            BackendCmd::List => {
                let registry = backend::registry();
                if registry.is_empty() {
                    println!(
                        "no backends compiled in — build with a backend feature (e.g. --features ipfs)"
                    );
                }
                for (name, entry) in &registry {
                    println!("{name}\t{}", entry.manifest.label);
                    for key in &entry.manifest.requires {
                        let secret = if key.secret { " (secret)" } else { "" };
                        println!("    env {}{} — {}", key.key, secret, key.prompt);
                    }
                }
            }
            BackendCmd::Add { name } => {
                let registry = backend::registry();
                let entry = registry.get(name.as_str()).ok_or_else(|| {
                    anyhow::anyhow!("backend {name:?} is not available; enable its Cargo feature")
                })?;
                configure_backend_name(&name)?;
                println!("configured backend {name:?} in {CONFIG_PATH}");
                println!("required environment:");
                for key in &entry.manifest.requires {
                    match key.url {
                        Some(url) => {
                            println!("    export {}=<value>   # {} ({url})", key.key, key.prompt)
                        }
                        None => println!("    export {}=<value>   # {}", key.key, key.prompt),
                    }
                }
            }
        },
        Cmd::Model { action } => match action {
            ModelCmd::List => {
                for (role, m) in cfg.models.roles() {
                    println!("{role}\t{} {} ({})", m.provider, m.name, m.host);
                }
            }
            ModelCmd::Set {
                role,
                provider,
                host,
                name,
            } => {
                // Keep existing/default values for any flag not given, so
                // `model set concierge --name X` just swaps the model.
                let existing = cfg.models.role(&role).cloned().unwrap_or_default();
                let provider = provider.unwrap_or(existing.provider);
                let host = host.unwrap_or(existing.host);
                let name = name.unwrap_or(existing.name);
                configure_model(&role, &provider, &host, &name)?;
                println!("set {role} → {provider} {name} ({host}) in {CONFIG_PATH}");
            }
            ModelCmd::Rm { role } => {
                if role == "concierge" {
                    anyhow::bail!("the concierge role is always present and can't be removed");
                }
                remove_model(&role)?;
                println!("removed model role {role:?}");
            }
        },
        Cmd::Init => unreachable!("init is handled before config/store loading"),
        Cmd::Resume { target } => {
            let model = model::build(&cfg.models.concierge())?;
            let mut session = Session::new(store, model, "conversation")
                .with_checkpoints(cfg.checkpoint.clone())
                .with_verifier(cfg.verify.clone());
            if let Some(worker) = cfg.models.role("worker") {
                session = session.with_worker(model::build(worker)?);
            }
            if let Some(cleanup) = cfg.models.role("cleanup") {
                session = session.with_cleanup(model::build(cleanup)?);
            }
            let target = target.unwrap_or_else(|| session.checkpoint_name().to_string());
            let info = session.resume_from_checkpoint(&target)?;
            eprintln!(
                "resumed {target} ({} records, {} loaded context)",
                info.turns, info.loaded_context
            );
            run(session)?;
        }
        Cmd::Chat => {
            let model = model::build(&cfg.models.concierge())?;
            let mut session = Session::new(store, model, "conversation")
                .with_checkpoints(cfg.checkpoint.clone())
                .with_verifier(cfg.verify.clone());
            if let Some(worker) = cfg.models.role("worker") {
                session = session.with_worker(model::build(worker)?);
            }
            if let Some(cleanup) = cfg.models.role("cleanup") {
                session = session.with_cleanup(model::build(cleanup)?);
            }
            run(session)?;
        }
        Cmd::Gc { keep } => {
            let policy = RetentionPolicy {
                keep_checkpoints: keep.unwrap_or(cfg.checkpoint.keep_checkpoints) as usize,
                checkpoint_name: cfg.checkpoint.name.clone(),
            };
            let report = store.gc(&policy)?;
            println!(
                "gc: scanned {}, kept {}, pruned {} ({} checkpoints, {} orphans)",
                report.scanned,
                report.kept,
                report.pruned_total(),
                report.pruned_checkpoints.len(),
                report.pruned_orphans.len(),
            );
        }
    }
    Ok(())
}

fn prepare_chat_workspace(mut input: impl BufRead) -> Result<()> {
    enter_project_directory(&mut input)?;
    if Path::new(CONFIG_PATH).try_exists()? {
        return Ok(());
    }
    println!("No {CONFIG_PATH} found in this project. Starting first-run setup.");
    setup_current_project(&mut input)
}

/// Print a tombstone as a receipt of truth: a pruned CID isn't an error, it's a
/// death certificate with a time and a pointer to where the history continues.
fn print_receipt(cid: &Cid, t: &Tombstone) {
    println!("{cid} was pruned");
    println!("  died:   {} ({})", iso8601(t.pruned_at), t.pruned_at);
    println!("  reason: {}", t.reason);
    match t.superseded_by {
        Some(next) => println!("  see:    {next}"),
        None => println!("  see:    (nothing — orphan)"),
    }
}

fn parse_cid(s: &str) -> Result<Cid> {
    s.parse()
        .map_err(|e| anyhow::anyhow!("invalid CID {s:?}: {e}"))
}

fn load_config_doc() -> Result<toml::Value> {
    let path = Path::new(CONFIG_PATH);
    if path.try_exists()? {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if text.trim().is_empty() {
            Ok(empty_toml_table())
        } else {
            text.parse::<toml::Value>()
                .with_context(|| format!("parse {}", path.display()))
        }
    } else {
        Ok(empty_toml_table())
    }
}

fn write_config_doc(doc: &toml::Value) -> Result<()> {
    let path = Path::new(CONFIG_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(doc)?;
    let mut file = AtomicWriteFile::open(path)?;
    file.write_all(rendered.as_bytes())?;
    file.commit()?;
    Ok(())
}

fn config_table(doc: &mut toml::Value) -> Result<&mut toml::Table> {
    doc.as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("{CONFIG_PATH} must be a TOML table"))
}

fn configure_backend_name(name: &str) -> Result<()> {
    let mut doc = load_config_doc()?;
    let backend = config_table(&mut doc)?
        .entry("backend".to_string())
        .or_insert_with(empty_toml_table)
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[backend] must be a TOML table"))?;
    backend.insert("name".to_string(), toml::Value::String(name.to_string()));
    write_config_doc(&doc)
}

fn configure_model(role: &str, provider: &str, host: &str, name: &str) -> Result<()> {
    let mut doc = load_config_doc()?;
    let models = config_table(&mut doc)?
        .entry("models".to_string())
        .or_insert_with(empty_toml_table)
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[models] must be a TOML table"))?;
    let role_tbl = models
        .entry(role.to_string())
        .or_insert_with(empty_toml_table)
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[models.{role}] must be a TOML table"))?;
    role_tbl.insert(
        "provider".to_string(),
        toml::Value::String(provider.to_string()),
    );
    role_tbl.insert("host".to_string(), toml::Value::String(host.to_string()));
    role_tbl.insert("name".to_string(), toml::Value::String(name.to_string()));
    write_config_doc(&doc)
}

fn remove_model(role: &str) -> Result<()> {
    let mut doc = load_config_doc()?;
    if let Some(models) = config_table(&mut doc)?
        .get_mut("models")
        .and_then(|m| m.as_table_mut())
    {
        models.remove(role);
    }
    write_config_doc(&doc)
}

fn configure_store_root(dir: &str) -> Result<()> {
    let mut doc = load_config_doc()?;
    let store = config_table(&mut doc)?
        .entry("store".to_string())
        .or_insert_with(empty_toml_table)
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[store] must be a TOML table"))?;
    store.insert("root".to_string(), toml::Value::String(dir.to_string()));
    write_config_doc(&doc)
}

fn configure_verifier(
    enabled: bool,
    install: bool,
    test: bool,
    timeout_seconds: u64,
) -> Result<()> {
    let mut doc = load_config_doc()?;
    let verify = config_table(&mut doc)?
        .entry("verify".to_string())
        .or_insert_with(empty_toml_table)
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[verify] must be a TOML table"))?;
    verify.insert("enabled".to_string(), toml::Value::Boolean(enabled));
    verify.insert("install".to_string(), toml::Value::Boolean(install));
    verify.insert("test".to_string(), toml::Value::Boolean(test));
    verify.insert(
        "timeout_seconds".to_string(),
        toml::Value::Integer(timeout_seconds as i64),
    );
    write_config_doc(&doc)
}

fn empty_toml_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

/// Read one line; blank input keeps the default.
fn prompt(input: &mut impl BufRead, label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let value = line.trim();
    Ok(if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    })
}

/// Yes/no prompt; blank input takes the default.
fn confirm(input: &mut impl BufRead, label: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    print!("{label} [{hint}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Pick/create a working directory and enter it before loading config.
fn enter_project_directory(input: &mut impl BufRead) -> Result<PathBuf> {
    let dir = prompt(input, "Project directory", ".")?;
    let project_dir = PathBuf::from(&dir);
    std::fs::create_dir_all(&project_dir)
        .with_context(|| format!("create project directory {}", project_dir.display()))?;
    std::env::set_current_dir(&project_dir)
        .with_context(|| format!("enter project directory {}", project_dir.display()))?;
    let cwd = std::env::current_dir()?;
    println!("working in {}", cwd.display());
    Ok(cwd)
}

/// Interactive setup: pick/create a working directory, then optionally
/// configure the concierge, worker, and (after the worker) cleanup models.
/// Writes `config.toml`.
fn init_flow(mut input: impl BufRead) -> Result<()> {
    enter_project_directory(&mut input)?;
    setup_current_project(&mut input)
}

/// Write setup for the current project directory.
fn setup_current_project(input: &mut impl BufRead) -> Result<()> {
    configure_store_root(".concierge")?;
    let cwd = std::env::current_dir()?;
    println!("memory store: {}", cwd.join(".concierge").display());

    if confirm(input, "Configure the concierge model now?", true)? {
        let provider = prompt(input, "  concierge provider", "ollama")?;
        let host = prompt(input, "  concierge host", "http://localhost:11434")?;
        let name = prompt(input, "  concierge model", "llama3.2")?;
        maybe_pull_ollama_model(input, &provider, &name)?;
        configure_model("concierge", &provider, &host, &name)?;
        println!("  concierge → {name}");
    }
    if confirm(input, "Add a worker model?", false)? {
        let provider = prompt(input, "  worker provider", "ollama")?;
        let host = prompt(input, "  worker host", "http://localhost:11434")?;
        let name = prompt(input, "  worker model", "qwen2.5-coder:7b")?;
        maybe_pull_ollama_model(input, &provider, &name)?;
        configure_model("worker", &provider, &host, &name)?;
        println!("  worker → {name}");

        // The cleanup model reviews the deterministic audit of the worker's
        // output and rewrites only the flagged files. It belongs to the same
        // build pipeline as the worker, so it's offered right after it.
        if confirm(
            input,
            "Add a cleanup model? (reviews and fixes the worker's output)",
            false,
        )? {
            let provider = prompt(input, "  cleanup provider", "ollama")?;
            let host = prompt(input, "  cleanup host", "http://localhost:11434")?;
            let name = prompt(input, "  cleanup model", "rnj-1:8b")?;
            maybe_pull_ollama_model(input, &provider, &name)?;
            configure_model("cleanup", &provider, &host, &name)?;
            println!("  cleanup → {name}");
        }
    }
    if confirm(
        input,
        "Enable sandboxed build/test verifier for `/work`?",
        true,
    )? {
        let install = confirm(
            input,
            "  allow sandboxed dependency install? (`npm install --ignore-scripts`)",
            true,
        )?;
        let test = confirm(input, "  run detected test commands?", true)?;
        let timeout = prompt(input, "  verifier command timeout seconds", "120")?;
        let timeout_seconds = timeout.trim().parse::<u64>().unwrap_or(120);
        configure_verifier(true, install, test, timeout_seconds)?;
        println!("  verifier → enabled");
    } else {
        configure_verifier(false, false, false, 120)?;
        println!("  verifier → disabled");
    }
    println!("wrote {CONFIG_PATH}");
    Ok(())
}

fn maybe_pull_ollama_model(input: &mut impl BufRead, provider: &str, name: &str) -> Result<()> {
    if provider != "ollama" {
        return Ok(());
    }
    if confirm(
        input,
        &format!("  pull {name} with `ollama pull` now?"),
        false,
    )? {
        let status = Command::new("ollama")
            .arg("pull")
            .arg(name)
            .status()
            .with_context(|| format!("run `ollama pull {name}`"))?;
        if !status.success() {
            anyhow::bail!("`ollama pull {name}` failed with status {status}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
