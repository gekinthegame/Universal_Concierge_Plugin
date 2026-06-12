//! Ingest layer — turns local files and directories into IPLD memory roots.
//! See `INGEST_PLUGIN_PLAN.md` for the full spec.

use crate::blockstore::Blockstore;
use crate::cid::Cid;
use crate::node::{DirectoryEntry, DirectoryManifest, Edge, EdgeRel, IngestRun, Node, Source};
use crate::store::Store;
use crate::trace::trace;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

/// Stats for an ingest operation.
#[derive(Debug, Default, Clone)]
pub struct IngestStats {
    pub file_count: u64,
    pub byte_count: u64,
    pub ignored_count: u64,
    pub plugin_records: u64,
    pub plugin_failures: u64,
    pub per_file_plugin_records: BTreeMap<String, u64>,
    pub per_file_plugin_failures: BTreeMap<String, u64>,
}

/// Result of a lossless local ingest. The root manifest CID is the shareable
/// directory root; the run CID carries ingest stats/provenance for audit.
#[derive(Debug, Clone)]
pub struct IngestReport {
    pub root: Cid,
    pub ingest_run: Cid,
    pub stats: IngestStats,
    pub plugin_failures: Vec<PluginFailure>,
}

/// A plugin warning/failure that did not stop lossless core ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFailure {
    pub plugin: String,
    pub path: String,
    pub message: String,
}

/// Optional ingest behavior. Default ingest is lossless only; plugins run only
/// when explicitly selected, matching the plan's "basic ingest" boundary.
#[derive(Debug, Clone, Default)]
pub struct IngestOptions {
    pub plugins: Vec<String>,
    pub all_plugins: bool,
}

impl IngestOptions {
    pub fn plugins<I, S>(plugins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            plugins: plugins.into_iter().map(Into::into).collect(),
            all_plugins: false,
        }
    }

    pub fn all_plugins() -> Self {
        Self {
            plugins: Vec::new(),
            all_plugins: true,
        }
    }
}

/// Built-in ignore rules (Phase 1.2).
const IGNORE_DIRS: &[&str] = &[
    ".git",
    ".concierge",
    ".cc",
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
];

/// Walk a directory and collect files to ingest, respecting built-in ignore rules.
pub fn walk_directory(root: &Path) -> Result<(Vec<PathBuf>, IngestStats)> {
    let mut selected = Vec::new();
    let mut stats = IngestStats::default();

    let mut it = WalkDir::new(root).into_iter();
    loop {
        let entry = match it.next() {
            None => break,
            Some(Ok(e)) => e,
            Some(Err(e)) => return Err(e.into()),
        };

        let name = entry.file_name().to_string_lossy();
        if IGNORE_DIRS.contains(&name.as_ref()) {
            stats.ignored_count += 1;
            if entry.file_type().is_dir() {
                it.skip_current_dir();
            }
            continue;
        }

        if entry.file_type().is_file() {
            selected.push(entry.path().to_path_buf());
            stats.file_count += 1;
            stats.byte_count += entry.metadata()?.len();
        }
    }

    trace(
        "ingest walk",
        &format!(
            "{} files selected, {} ignored",
            stats.file_count, stats.ignored_count
        ),
    );

    selected.sort();

    Ok((selected, stats))
}

/// A pluggable processor that derives records from ingested files.
pub trait IngestPlugin {
    fn manifest(&self) -> IngestPluginManifest;

    /// Cheap filter. No file read should be required here.
    fn accepts(&self, file: &FileMeta) -> bool;

    /// Derive records from a file whose bytes are already stored.
    fn derive(&self, input: &IngestInput) -> Result<Vec<DerivedRecord>>;
}

#[derive(Clone, Debug)]
pub struct IngestPluginManifest {
    pub name: &'static str,
    pub label: &'static str,
    pub media_types: &'static [&'static str],
    pub extensions: &'static [&'static str],
}

#[derive(Clone, Debug)]
pub struct FileMeta {
    pub path: String,
    pub size: u64,
    pub media_type: Option<String>,
    pub mtime: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct IngestInput {
    pub file: FileMeta,
    pub file_ref: Cid,
    pub blob: Cid,

    /// Optional because some plugins can work from metadata only, and large
    /// files should not be loaded twice unless a plugin asks for bytes.
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct DerivedRecord {
    pub node: Node,
    pub edges: Vec<Edge>,
}

/// Build an ingest plugin as a trait object.
pub type IngestPluginFactory = fn() -> Box<dyn IngestPlugin>;

/// A registry entry for an ingest plugin.
#[derive(Clone)]
pub struct IngestPluginEntry {
    pub manifest: IngestPluginManifest,
    pub factory: IngestPluginFactory,
}

/// A registry entry for a concrete plugin type.
#[allow(dead_code)] // referenced by feature-gated registry lines
fn plugin_entry<P: IngestPlugin + Default + 'static>() -> IngestPluginEntry {
    let p = P::default();
    IngestPluginEntry {
        manifest: p.manifest(),
        factory: || Box::new(P::default()) as Box<dyn IngestPlugin>,
    }
}

/// name -> entry. Compile-time registry for ingest plugins.
pub fn registry() -> BTreeMap<&'static str, IngestPluginEntry> {
    #[allow(unused_mut)]
    let mut m = BTreeMap::new();
    // plugins added here (Phase III+)
    m.insert(
        "source-index",
        plugin_entry::<crate::ingestors::source::SourceIndexPlugin>(),
    );

    #[cfg(test)]
    {
        m.insert("mock", plugin_entry::<tests::MockPlugin>());
        m.insert("fail", plugin_entry::<tests::FailingPlugin>());
    }

    m
}

/// Ingest a local file or directory into the memory layer (Phase I).
pub fn ingest<B: Blockstore>(store: &Store<B>, path: &Path) -> Result<IngestReport> {
    ingest_with_options(store, path, &IngestOptions::default())
}

/// Ingest with explicit plugin selection (Phase II).
pub fn ingest_with_options<B: Blockstore>(
    store: &Store<B>,
    path: &Path,
    options: &IngestOptions,
) -> Result<IngestReport> {
    let mut stats = IngestStats::default();
    let (files, walk_stats) = walk_directory(path)?;
    stats.file_count = walk_stats.file_count;
    stats.byte_count = walk_stats.byte_count;
    stats.ignored_count = walk_stats.ignored_count;

    let base = manifest_base(path);
    let mut files: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|file_path| Ok((manifest_path(&base, &file_path)?, file_path)))
        .collect::<Result<_>>()?;
    files.sort_by(|(left, _), (right, _)| left.cmp(right));

    let plugins = selected_plugins(options)?;

    let mut entries = Vec::new();
    let mut derived_cids = Vec::new();
    let mut plugin_failures = Vec::new();

    for (relative_path, file_path) in files {
        let mut accepted_plugins = 0;
        let mut file_plugin_records = 0;
        let mut file_plugin_failures = 0;
        let bytes = std::fs::read(&file_path)
            .with_context(|| format!("failed to read file {:?}", file_path))?;
        let metadata = std::fs::metadata(&file_path)
            .with_context(|| format!("failed to stat file {:?}", file_path))?;
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        // 1.3 Store as Blob
        let blob_cid = store.put_node(
            Node::Blob(crate::node::Blob {
                bytes: bytes.clone(),
                media_type: None,
            }),
            Source::User,
        )?;

        // 1.4 Store as FileRef
        let file_ref_cid = store.put_node(
            Node::FileRef(crate::node::FileRef {
                path: relative_path.clone(),
                size: Some(metadata.len()),
                media_type: None,
                mtime,
                content: blob_cid,
            }),
            Source::User,
        )?;

        entries.push(DirectoryEntry {
            path: relative_path.clone(),
            file_ref: file_ref_cid,
        });

        // Phase II: Plugins
        let file_meta = FileMeta {
            path: relative_path,
            size: metadata.len(),
            media_type: None,
            mtime,
        };

        for plugin in &plugins {
            if plugin.accepts(&file_meta) {
                accepted_plugins += 1;
                let plugin_name = plugin.manifest().name;
                let input = IngestInput {
                    file: file_meta.clone(),
                    file_ref: file_ref_cid,
                    blob: blob_cid,
                    bytes: Some(bytes.clone()),
                };

                match plugin.derive(&input) {
                    Ok(derived_records) => {
                        for mut record in derived_records {
                            // 2.4 Link derived records to their FileRef
                            ensure_edge(&mut record.edges, EdgeRel::DerivedFrom, file_ref_cid);
                            match store.put_node_with_edges(
                                record.node,
                                Source::Derived {
                                    from: vec![file_ref_cid],
                                },
                                record.edges,
                            ) {
                                Ok(cid) => {
                                    stats.plugin_records += 1;
                                    file_plugin_records += 1;
                                    derived_cids.push(cid);
                                }
                                Err(e) => record_plugin_failure(
                                    &mut stats,
                                    &mut plugin_failures,
                                    &mut file_plugin_failures,
                                    plugin_name,
                                    &file_meta.path,
                                    format_args!("failed to store derived node: {e}"),
                                ),
                            }
                        }
                    }
                    Err(e) => {
                        // 2.3 Plugin failure doesn't corrupt core ingest
                        record_plugin_failure(
                            &mut stats,
                            &mut plugin_failures,
                            &mut file_plugin_failures,
                            plugin_name,
                            &file_meta.path,
                            format_args!("{e}"),
                        );
                    }
                }
            }
        }

        if accepted_plugins > 0 {
            stats
                .per_file_plugin_records
                .insert(file_meta.path.clone(), file_plugin_records);
            stats
                .per_file_plugin_failures
                .insert(file_meta.path, file_plugin_failures);
        }
    }

    // 1.5 Store root DirectoryManifest
    let manifest_edges = derived_cids
        .iter()
        .map(|cid| Edge {
            rel: EdgeRel::Contains,
            to: *cid,
        })
        .collect();
    let manifest_cid = store.put_node_with_edges(
        Node::DirectoryManifest(DirectoryManifest {
            root_path: path.to_string_lossy().to_string(),
            entries,
        }),
        Source::User,
        manifest_edges,
    )?;

    // 1.6 Store IngestRun
    let ingest_run_cid = store.put_node(
        Node::IngestRun(IngestRun {
            source_path: path.to_string_lossy().to_string(),
            manifest: manifest_cid,
            file_count: stats.file_count,
            byte_count: stats.byte_count,
            ignored_count: stats.ignored_count,
            plugin_records: stats.plugin_records,
            plugin_failures: stats.plugin_failures,
            per_file_plugin_records: stats.per_file_plugin_records.clone(),
            per_file_plugin_failures: stats.per_file_plugin_failures.clone(),
        }),
        Source::User,
    )?;

    // 1.7 Return root CID plus run metadata.
    Ok(IngestReport {
        root: manifest_cid,
        ingest_run: ingest_run_cid,
        stats,
        plugin_failures,
    })
}

fn selected_plugins(options: &IngestOptions) -> Result<Vec<Box<dyn IngestPlugin>>> {
    if options.all_plugins && !options.plugins.is_empty() {
        anyhow::bail!("--plugin and --all-plugins are mutually exclusive");
    }

    let registry = registry();
    let entries: Vec<IngestPluginEntry> = if options.all_plugins {
        registry.values().cloned().collect()
    } else {
        let requested: BTreeSet<&str> = options.plugins.iter().map(String::as_str).collect();
        for name in &requested {
            if !registry.contains_key(name) {
                anyhow::bail!("ingest plugin {name:?} is not available");
            }
        }
        registry
            .iter()
            .filter(|(name, _)| requested.contains(**name))
            .map(|(_, entry)| entry.clone())
            .collect()
    };

    Ok(entries.into_iter().map(|entry| (entry.factory)()).collect())
}

fn ensure_edge(edges: &mut Vec<Edge>, rel: EdgeRel, to: Cid) {
    if !edges.iter().any(|edge| edge.rel == rel && edge.to == to) {
        edges.push(Edge { rel, to });
    }
}

fn record_plugin_failure(
    stats: &mut IngestStats,
    failures: &mut Vec<PluginFailure>,
    file_failures: &mut u64,
    plugin: &str,
    path: &str,
    message: fmt::Arguments<'_>,
) {
    let message = message.to_string();
    stats.plugin_failures += 1;
    *file_failures += 1;
    trace(
        "ingest plugin",
        &format!("plugin {plugin} failed for {path}: {message}"),
    );
    failures.push(PluginFailure {
        plugin: plugin.to_string(),
        path: path.to_string(),
        message,
    });
}

fn manifest_base(root: &Path) -> PathBuf {
    if root.is_file() {
        root.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        root.to_path_buf()
    }
}

fn manifest_path(base: &Path, file_path: &Path) -> Result<String> {
    let relative = file_path.strip_prefix(base).unwrap_or(file_path);
    let path = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if !path.is_empty() {
        return Ok(path);
    }

    let name = file_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("ingest file path has no file name: {:?}", file_path))?;
    Ok(name.to_string_lossy().to_string())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::blockstore::LocalBlocks;
    use crate::names::NameIndex;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn restore<B: Blockstore>(store: &Store<B>, root: &Cid) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut restored = BTreeMap::new();
        let manifest_rec = store.get_node(root)?;
        let Node::DirectoryManifest(manifest) = manifest_rec.body else {
            panic!("not a manifest");
        };

        for entry in manifest.entries {
            let file_ref_rec = store.get_node(&entry.file_ref)?;
            let Node::FileRef(file_ref) = file_ref_rec.body else {
                panic!("not a file ref");
            };
            assert_eq!(entry.path, file_ref.path);
            let blob_rec = store.get_node(&file_ref.content)?;
            let Node::Blob(blob) = blob_rec.body else {
                panic!("not a blob");
            };
            restored.insert(file_ref.path, blob.bytes);
        }

        Ok(restored)
    }

    #[test]
    fn test_walk_directory_skips_ignored() -> Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        // Create some files
        fs::write(root.join("file1.txt"), "hello")?;
        fs::create_dir(root.join("subdir"))?;
        fs::write(root.join("subdir/file2.txt"), "world")?;

        // Create ignored directory and file
        fs::create_dir(root.join(".git"))?;
        fs::write(root.join(".git/config"), "secret")?;
        fs::create_dir(root.join("target"))?;
        fs::write(root.join("target/debug"), "binary")?;
        fs::write(root.join("node_modules"), "not a dir but ignored name")?;

        let (selected, stats) = walk_directory(root)?;

        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|p| p.ends_with("file1.txt")));
        assert!(selected.iter().any(|p| p.ends_with("file2.txt")));

        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.byte_count, 10); // "hello" (5) + "world" (5)
        assert_eq!(stats.ignored_count, 3); // .git, target, node_modules

        Ok(())
    }

    #[test]
    fn test_ingest_round_trip() -> Result<()> {
        let data_dir = tempdir()?;
        let data_root = data_dir.path();
        fs::write(data_root.join("z.txt"), "z")?;
        fs::create_dir(data_root.join("nested"))?;
        fs::write(data_root.join("nested/a.txt"), "a")?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report = ingest(&store, data_root)?;
        assert_eq!(report.stats.file_count, 2);
        assert_eq!(report.stats.byte_count, 2);
        assert_eq!(report.stats.ignored_count, 0);
        assert_eq!(report.stats.plugin_records, 0);
        assert_eq!(report.stats.plugin_failures, 0);
        assert!(report.plugin_failures.is_empty());

        let manifest_rec = store.get_node(&report.root)?;
        assert!(
            manifest_rec.edges.is_empty(),
            "plain ingest must not run plugins or link derived records"
        );
        let Node::DirectoryManifest(manifest) = manifest_rec.body else {
            panic!("not a manifest");
        };
        let paths: Vec<&str> = manifest
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, vec!["nested/a.txt", "z.txt"]);

        let run_rec = store.get_node(&report.ingest_run)?;
        let Node::IngestRun(run) = run_rec.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.manifest, report.root);
        assert_eq!(run.file_count, 2);
        assert_eq!(run.byte_count, 2);
        assert_eq!(run.ignored_count, 0);
        assert_eq!(run.plugin_records, 0);
        assert_eq!(run.plugin_failures, 0);

        let restored = restore(&store, &report.root)?;
        assert_eq!(restored.get("nested/a.txt").unwrap(), b"a");
        assert_eq!(restored.get("z.txt").unwrap(), b"z");

        Ok(())
    }

    #[test]
    fn test_ingest_single_file_keeps_file_name() -> Result<()> {
        let data_dir = tempdir()?;
        let file = data_dir.path().join("one.txt");
        fs::write(&file, "hi")?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report = ingest(&store, &file)?;
        let restored = restore(&store, &report.root)?;

        assert_eq!(restored.len(), 1);
        assert_eq!(restored.get("one.txt").unwrap(), b"hi");

        Ok(())
    }

    #[test]
    fn test_registry_is_not_empty_in_tests() {
        assert!(!registry().is_empty());
        assert!(registry().contains_key("mock"));
    }

    #[test]
    fn test_ingest_calls_plugins_and_links_edges() -> Result<()> {
        let data_dir = tempdir()?;
        fs::write(data_dir.path().join("test.txt"), "content")?;

        let mem_dir = tempdir()?;
        let block_path = mem_dir.path().join("blocks");
        let blocks = LocalBlocks::new(block_path.clone());
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report =
            ingest_with_options(&store, data_dir.path(), &IngestOptions::plugins(["mock"]))?;

        // MockPlugin returns 1 record per file it accepts.
        assert_eq!(report.stats.file_count, 1);
        assert_eq!(report.stats.plugin_records, 1);
        assert_eq!(report.stats.plugin_failures, 0);

        let manifest_rec = store.get_node(&report.root)?;
        let Node::DirectoryManifest(manifest) = &manifest_rec.body else {
            panic!("not a manifest");
        };
        let file_ref = manifest.entries[0].file_ref;
        let contains_edges: Vec<&Edge> = manifest_rec
            .edges
            .iter()
            .filter(|edge| edge.rel == EdgeRel::Contains)
            .collect();
        assert_eq!(contains_edges.len(), 1);
        let derived_cid = contains_edges[0].to;

        let derived_rec = store.get_node(&derived_cid)?;
        assert!(
            matches!(derived_rec.source, Source::Derived { ref from } if from == &vec![file_ref])
        );
        assert!(
            derived_rec
                .edges
                .iter()
                .any(|edge| edge.rel == EdgeRel::DerivedFrom && edge.to == file_ref),
            "derived record must point back to the FileRef"
        );
        let Node::Memory(memory) = derived_rec.body else {
            panic!("mock plugin should emit a memory node");
        };
        assert_eq!(memory.text, "derived from test.txt");

        let reachable = crate::dag::reachable_from(&LocalBlocks::new(block_path), &report.root)?;
        assert!(
            reachable.contains(&derived_cid),
            "derived plugin records must be reachable from the root manifest"
        );

        Ok(())
    }

    #[test]
    fn test_plugin_failure_is_reported_without_corrupting_core_ingest() -> Result<()> {
        let data_dir = tempdir()?;
        fs::write(data_dir.path().join("test.txt"), "content")?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report =
            ingest_with_options(&store, data_dir.path(), &IngestOptions::plugins(["fail"]))?;

        assert_eq!(report.stats.file_count, 1);
        assert_eq!(report.stats.plugin_records, 0);
        assert_eq!(report.stats.plugin_failures, 1);
        assert_eq!(report.stats.per_file_plugin_records["test.txt"], 0);
        assert_eq!(report.stats.per_file_plugin_failures["test.txt"], 1);
        assert_eq!(report.plugin_failures.len(), 1);
        assert_eq!(report.plugin_failures[0].plugin, "fail");
        assert_eq!(report.plugin_failures[0].path, "test.txt");
        assert!(
            report.plugin_failures[0]
                .message
                .contains("intentional failure")
        );

        let run_rec = store.get_node(&report.ingest_run)?;
        let Node::IngestRun(run) = run_rec.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.plugin_records, 0);
        assert_eq!(run.plugin_failures, 1);
        assert_eq!(run.per_file_plugin_records["test.txt"], 0);
        assert_eq!(run.per_file_plugin_failures["test.txt"], 1);

        let restored = restore(&store, &report.root)?;
        assert_eq!(restored.get("test.txt").unwrap(), b"content");

        Ok(())
    }

    #[test]
    fn test_source_index_records_symbols_and_keeps_them_reachable() -> Result<()> {
        let data_dir = tempdir()?;
        fs::write(
            data_dir.path().join("auth.py"),
            "def authenticate(token):\n    return verify(token)\n\nclass Authenticator:\n    def login(self, user):\n        return user.token\n",
        )?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report = ingest_with_options(
            &store,
            data_dir.path(),
            &IngestOptions::plugins(["source-index"]),
        )?;

        assert_eq!(report.stats.file_count, 1);
        assert_eq!(report.stats.plugin_records, 3);
        assert_eq!(report.stats.per_file_plugin_records["auth.py"], 3);
        assert_eq!(report.stats.per_file_plugin_failures["auth.py"], 0);
        assert_eq!(report.stats.plugin_failures, 0);
        assert!(report.plugin_failures.is_empty());

        let manifest_rec = store.get_node(&report.root)?;
        let Node::DirectoryManifest(manifest) = &manifest_rec.body else {
            panic!("not a manifest");
        };
        let file_ref = manifest.entries[0].file_ref;
        let derived_cids: Vec<Cid> = manifest_rec
            .edges
            .iter()
            .filter(|edge| edge.rel == EdgeRel::Contains)
            .map(|edge| edge.to)
            .collect();
        assert_eq!(derived_cids.len(), 3);

        let mut symbols = BTreeMap::new();
        for cid in &derived_cids {
            let record = store.get_node(cid)?;
            assert!(
                matches!(record.source, Source::Derived { ref from } if from == &vec![file_ref])
            );
            assert!(
                record
                    .edges
                    .iter()
                    .any(|edge| edge.rel == EdgeRel::DerivedFrom && edge.to == file_ref),
                "source-index symbol must point back to its FileRef"
            );
            let Node::Symbol(symbol) = record.body else {
                panic!("source-index must emit symbols");
            };
            symbols.insert(symbol.name.clone(), symbol);
        }

        assert_eq!(symbols["authenticate"].kind, "function");
        assert_eq!(symbols["Authenticator"].kind, "class");
        assert_eq!(symbols["login"].kind, "method");
        assert!(symbols.values().all(|symbol| symbol.language == "python"));

        let reachable = store.reachable(&report.root)?;
        for cid in &derived_cids {
            assert!(
                reachable.contains(cid),
                "source-index symbols must be reachable from root manifest"
            );
        }

        let run_rec = store.get_node(&report.ingest_run)?;
        let Node::IngestRun(run) = run_rec.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.per_file_plugin_records["auth.py"], 3);
        assert_eq!(run.per_file_plugin_failures["auth.py"], 0);

        Ok(())
    }

    #[test]
    fn test_source_index_per_file_stats_track_empty_and_failed_files() -> Result<()> {
        let data_dir = tempdir()?;
        fs::write(
            data_dir.path().join("has_symbol.py"),
            "def has_symbol():\n    return 1\n",
        )?;
        fs::write(data_dir.path().join("no_symbols.py"), "x = 1\n")?;
        fs::write(
            data_dir.path().join("broken.py"),
            "def broken(:\n    return 2\n",
        )?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report = ingest_with_options(
            &store,
            data_dir.path(),
            &IngestOptions::plugins(["source-index"]),
        )?;

        assert_eq!(report.stats.file_count, 3);
        assert_eq!(report.stats.plugin_records, 1);
        assert_eq!(report.stats.plugin_failures, 1);
        assert_eq!(report.stats.per_file_plugin_records["has_symbol.py"], 1);
        assert_eq!(report.stats.per_file_plugin_records["no_symbols.py"], 0);
        assert_eq!(report.stats.per_file_plugin_records["broken.py"], 0);
        assert_eq!(report.stats.per_file_plugin_failures["has_symbol.py"], 0);
        assert_eq!(report.stats.per_file_plugin_failures["no_symbols.py"], 0);
        assert_eq!(report.stats.per_file_plugin_failures["broken.py"], 1);

        let run_rec = store.get_node(&report.ingest_run)?;
        let Node::IngestRun(run) = run_rec.body else {
            panic!("not an ingest run");
        };
        assert_eq!(
            &run.per_file_plugin_records,
            &report.stats.per_file_plugin_records
        );
        assert_eq!(
            &run.per_file_plugin_failures,
            &report.stats.per_file_plugin_failures
        );

        let manifest_rec = store.get_node(&report.root)?;
        let derived_cids: Vec<Cid> = manifest_rec
            .edges
            .iter()
            .filter(|edge| edge.rel == EdgeRel::Contains)
            .map(|edge| edge.to)
            .collect();
        assert_eq!(derived_cids.len(), 1);

        let restored = restore(&store, &report.root)?;
        assert_eq!(restored.len(), 3);
        assert_eq!(
            restored.get("broken.py").unwrap(),
            b"def broken(:\n    return 2\n"
        );

        Ok(())
    }

    #[test]
    fn test_source_index_parse_failure_is_a_warning_not_core_ingest_failure() -> Result<()> {
        let data_dir = tempdir()?;
        fs::write(
            data_dir.path().join("broken.py"),
            "def stable(:\n    return 2\n",
        )?;

        let mem_dir = tempdir()?;
        let blocks = LocalBlocks::new(mem_dir.path().join("blocks"));
        let names = NameIndex::load(mem_dir.path().join("names.json"))?;
        let store = Store::new(blocks, names);

        let report = ingest_with_options(
            &store,
            data_dir.path(),
            &IngestOptions::plugins(["source-index"]),
        )?;

        assert_eq!(report.stats.file_count, 1);
        assert_eq!(report.stats.plugin_records, 0);
        assert_eq!(report.stats.plugin_failures, 1);
        assert_eq!(report.stats.per_file_plugin_records["broken.py"], 0);
        assert_eq!(report.stats.per_file_plugin_failures["broken.py"], 1);
        assert_eq!(report.plugin_failures.len(), 1);
        assert_eq!(report.plugin_failures[0].plugin, "source-index");
        assert_eq!(report.plugin_failures[0].path, "broken.py");
        assert!(
            report.plugin_failures[0]
                .message
                .contains("tree-sitter reported syntax errors in broken.py")
        );

        let manifest_rec = store.get_node(&report.root)?;
        assert!(
            manifest_rec.edges.is_empty(),
            "failed plugin records must not leave partial derived nodes"
        );
        let restored = restore(&store, &report.root)?;
        assert_eq!(
            restored.get("broken.py").unwrap(),
            b"def stable(:\n    return 2\n"
        );

        let run_rec = store.get_node(&report.ingest_run)?;
        let Node::IngestRun(run) = run_rec.body else {
            panic!("not an ingest run");
        };
        assert_eq!(run.plugin_records, 0);
        assert_eq!(run.plugin_failures, 1);
        assert_eq!(run.per_file_plugin_records["broken.py"], 0);
        assert_eq!(run.per_file_plugin_failures["broken.py"], 1);

        Ok(())
    }

    #[test]
    fn test_unknown_plugin_selection_errors() {
        let Err(err) = selected_plugins(&IngestOptions::plugins(["missing"])) else {
            panic!("missing plugin should error");
        };
        assert!(
            err.to_string()
                .contains("ingest plugin \"missing\" is not available")
        );
    }

    pub struct MockPlugin;
    impl Default for MockPlugin {
        fn default() -> Self {
            Self
        }
    }

    pub struct FailingPlugin;
    impl Default for FailingPlugin {
        fn default() -> Self {
            Self
        }
    }
    impl IngestPlugin for FailingPlugin {
        fn manifest(&self) -> IngestPluginManifest {
            IngestPluginManifest {
                name: "fail",
                label: "Failing plugin",
                media_types: &["text/plain"],
                extensions: &[".txt"],
            }
        }

        fn accepts(&self, _file: &FileMeta) -> bool {
            true
        }

        fn derive(&self, _input: &IngestInput) -> Result<Vec<DerivedRecord>> {
            anyhow::bail!("intentional failure")
        }
    }
    impl IngestPlugin for MockPlugin {
        fn manifest(&self) -> IngestPluginManifest {
            IngestPluginManifest {
                name: "mock",
                label: "Mock plugin",
                media_types: &["text/plain"],
                extensions: &[".txt"],
            }
        }
        fn accepts(&self, _file: &FileMeta) -> bool {
            true
        }
        fn derive(&self, input: &IngestInput) -> Result<Vec<DerivedRecord>> {
            Ok(vec![DerivedRecord {
                node: Node::Memory(crate::node::Memory {
                    text: format!("derived from {}", input.file.path),
                    kind: crate::node::MemoryKind::Project,
                }),
                edges: vec![],
            }])
        }
    }

    #[test]
    fn test_plugin_entry_exposes_manifest() {
        let e = plugin_entry::<MockPlugin>();
        assert_eq!(e.manifest.name, "mock");
        assert_eq!(e.manifest.label, "Mock plugin");
    }

    #[test]
    fn test_plugin_factory_builds_trait_object() {
        let e = plugin_entry::<MockPlugin>();
        let p = (e.factory)();
        assert_eq!(p.manifest().name, "mock");
    }
}
