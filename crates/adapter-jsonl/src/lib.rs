//! Layer 3 — generic JSONL event adapter.
//!
//! This is the low-friction path the plan calls for early: any harness that can
//! emit one JSON object per line can write a session into Concierge IPLD memory,
//! regardless of its implementation language. A Python harness, for example,
//! only needs to `json.dumps` an [`Envelope`] per event — no FFI, no bindings.
//!
//! Phase 2 adds the streaming reader, the event→node mapping, host/project/
//! session scoped names, session checkpoints, and idempotent ingest:
//!
//! ```text
//! harness emits JSONL events -> ingest() -> mem put/bind/checkpoint -> IPLD
//! ```
//!
//! ## Event → node mapping
//!
//! | event | node (`mem` type) |
//! |---|---|
//! | `user_prompt` | `prompt {text}` |
//! | `model_response` | `response {text, model}` |
//! | `tool_call_started` | *(buffered; supplies the next `tool_result.input`)* |
//! | `tool_call_finished` | `tool_result {tool, input, output, ok}` |
//! | `file_read` / `file_written` | `blob` + `file_ref {path, size, content→blob}` |
//! | `decision_recorded` | `decision {choice = text}` |
//! | `memory_recorded` | `memory {text, kind = "reference"}` |
//! | `checkpoint_requested` / `session_ended` | `checkpoint {label, root = head, parent = prev}` |
//! | `session_started` | *(resets session state; no node)* |
//!
//! ## Idempotency
//!
//! Node identity is the CID, so re-ingesting the same stream re-writes the same
//! blocks (no logical duplication) and rebinds names to the same CIDs. Within a
//! run, checkpoints chain to the previous checkpoint as `parent`; the first has
//! no parent — so the whole run is deterministic and a re-run reproduces every
//! CID. (Cross-session parent chaining is a later refinement that would trade
//! this determinism.)

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use concierge_core::{cid_link, utc_today, Cid, CoreBinding, Envelope, Event, ImportedFrom, Node};
use sha2::{Digest, Sha256};

/// Parse a single JSONL line into a host-neutral [`Envelope`].
///
/// Returns the underlying `serde_json` error on malformed input so the streaming
/// reader can report it with a line number and skip per policy (one bad line
/// must not abort the whole file).
pub fn parse_line(line: &str) -> serde_json::Result<Envelope> {
    serde_json::from_str(line)
}

/// Backfill importer contract: one source-specific reader that emits the same
/// host-neutral [`Envelope`] shape the live adapter uses.
pub trait Importer {
    fn source_system(&self) -> &'static str;

    fn read(&self, source: &Path) -> Result<Vec<Envelope>, String>;
}

/// Transcript/JSONL importer: the first backfill target in the plan.
pub struct JsonlImporter;

impl Importer for JsonlImporter {
    fn source_system(&self) -> &'static str {
        "jsonl"
    }

    fn read(&self, source: &Path) -> Result<Vec<Envelope>, String> {
        let file =
            File::open(source).map_err(|e| format!("cannot open {}: {e}", source.display()))?;
        let reader = std::io::BufReader::new(file);
        let mut envelopes = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let line_no = idx + 1;
            let line = line.map_err(|e| format!("read error at line {line_no}: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let mut env =
                parse_line(&line).map_err(|e| format!("invalid JSON at line {line_no}: {e}"))?;
            let imported_from = ImportedFrom {
                source_system: self.source_system().to_string(),
                original_id: env.event_id.clone().unwrap_or_else(|| fingerprint(&env)),
                original_ts: env.ts.clone(),
            };
            env.imported_from = Some(imported_from);
            envelopes.push(env);
        }

        Ok(envelopes)
    }
}

/// Markdown / notes-folder importer — the plan's second backfill target.
///
/// Turns an Obsidian-style vault (a directory of `.md`/`.markdown` files, walked
/// recursively) or a single markdown file into memory/decision nodes, one per
/// heading-delimited section. Read-only against the source; each emitted
/// envelope carries `markdown` provenance and a stable `relpath#index` id, so a
/// re-import is deterministic. A trailing `session_ended` per file gives the
/// import a checkpoint and scoped names (an entry point in the explorer).
///
/// A section whose first heading mentions "decision" becomes a `decision`; every
/// other section becomes a `memory` (kind `reference`).
pub struct MarkdownImporter;

impl Importer for MarkdownImporter {
    fn source_system(&self) -> &'static str {
        "markdown"
    }

    fn read(&self, source: &Path) -> Result<Vec<Envelope>, String> {
        let files = collect_markdown_files(source)?;
        let mut envelopes = Vec::new();

        for file in files {
            let rel = relative_name(source, &file);
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;

            for (idx, section) in split_sections(&text).into_iter().enumerate() {
                let original_id = format!("{rel}#{idx}");
                let event = if is_decision_section(&section) {
                    Event::DecisionRecorded { text: section }
                } else {
                    Event::MemoryRecorded { text: section }
                };
                envelopes.push(self.envelope(&rel, original_id, event));
            }

            // Close the file as a session so a checkpoint + scoped names exist.
            envelopes.push(self.envelope(&rel, format!("{rel}#end"), Event::SessionEnded));
        }

        Ok(envelopes)
    }
}

impl MarkdownImporter {
    fn envelope(&self, rel: &str, original_id: String, event: Event) -> Envelope {
        Envelope {
            host_id: "import".to_string(),
            session_id: rel.to_string(),
            project_id: None,
            event_id: Some(original_id.clone()),
            ts: String::new(),
            imported_from: Some(ImportedFrom {
                source_system: self.source_system().to_string(),
                original_id,
                original_ts: String::new(),
            }),
            // Backfilled notes carry no model reasoning.
            reasoning: None,
            event,
        }
    }
}

/// Collect markdown files: a single file as-is, or every `.md`/`.markdown` under
/// a directory (recursive), sorted for deterministic import order.
fn collect_markdown_files(source: &Path) -> Result<Vec<PathBuf>, String> {
    if !source.exists() {
        return Err(format!("path does not exist: {}", source.display()));
    }
    let mut out = Vec::new();
    if source.is_file() {
        out.push(source.to_path_buf());
    } else {
        collect_markdown_recursive(source, &mut out)?;
        out.sort();
    }
    Ok(out)
}

fn collect_markdown_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read dir {}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_markdown_recursive(&path, out)?;
        } else if is_markdown(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("md") | Some("markdown")
    )
}

/// A stable, readable id for a file relative to the import root (or just the file
/// name when importing a single file).
fn relative_name(source: &Path, file: &Path) -> String {
    if source.is_dir() {
        file.strip_prefix(source)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned()
    } else {
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string_lossy().into_owned())
    }
}

/// Split markdown into heading-delimited sections. Text before the first heading
/// is its own section; each `#`-prefixed line starts a new one. Empty sections
/// are dropped, and a file with no headings is a single section.
fn split_sections(text: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.starts_with('#') && !current.trim().is_empty() {
            sections.push(current.trim().to_string());
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current.trim().to_string());
    }
    sections
}

/// A section is a decision when its first line is a heading mentioning "decision".
fn is_decision_section(section: &str) -> bool {
    section
        .lines()
        .next()
        .map(|line| line.starts_with('#') && line.to_ascii_lowercase().contains("decision"))
        .unwrap_or(false)
}

/// A line that could not be turned into memory, with the reason. Ingest never
/// aborts the whole stream on one bad line — it records and continues.
#[derive(Debug, Clone)]
pub struct SkippedLine {
    pub line_no: usize,
    pub reason: String,
}

/// What an [`ingest`] run did. Counts are over the whole stream; `skipped`
/// carries the line-numbered reasons for anything that didn't map.
#[derive(Debug, Default)]
pub struct IngestReport {
    /// Non-empty lines seen.
    pub lines: usize,
    /// Lines that parsed into a valid envelope.
    pub events: usize,
    /// IPLD nodes written (prompts, responses, tool results, file refs, decisions).
    pub nodes_written: usize,
    /// Content/checkpoint record CIDs written during this ingest, in write order.
    /// Kernel capture uses these for event-driven index appends.
    pub record_cids: Vec<Cid>,
    /// Checkpoints created.
    pub checkpoints: usize,
    /// Name → CID bindings written (scoped names + `latest`).
    pub names_bound: usize,
    /// Blob CIDs written for file events, in order — used to show dedup.
    pub blobs_written: Vec<Cid>,
    /// Lines that were skipped, with line-numbered reasons.
    pub skipped: Vec<SkippedLine>,
}

/// Per-session running state. Reset by `session_started`; created lazily so a
/// stream without an explicit start still works.
#[derive(Default)]
struct SessionState {
    /// CID of the most recent node written — the root a checkpoint points at.
    head: Option<Cid>,
    /// Previous checkpoint in this run, chained as the next checkpoint's parent.
    last_checkpoint: Option<Cid>,
    /// `tool_call_started` args + marker name, awaiting their matching `tool_call_finished`.
    pending_tool_args: HashMap<String, VecDeque<PendingToolCall>>,
    /// Marker for the current `session_started`, bound once we emit a node or checkpoint.
    pending_session_start_marker: Option<String>,
    /// Idempotency guard for repeated events in a stream.
    seen_events: HashSet<String>,
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    args_json: String,
}

/// Ingest a JSONL stream into `binding`. `base_dir` resolves the paths in
/// `file_read`/`file_written` events so their bytes can be stored as blobs.
///
/// Never returns an error: malformed lines and per-event failures are collected
/// in [`IngestReport::skipped`] so a single bad line or unreadable file can't
/// sink the run.
pub fn ingest<R: BufRead, B: CoreBinding>(reader: R, binding: &B, base_dir: &Path) -> IngestReport {
    let mut report = IngestReport::default();
    let mut envelopes = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                report.skipped.push(SkippedLine {
                    line_no,
                    reason: format!("read error: {e}"),
                });
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        report.lines += 1;

        let env = match parse_line(&line) {
            Ok(env) => env,
            Err(e) => {
                report.skipped.push(SkippedLine {
                    line_no,
                    reason: format!("invalid JSON: {e}"),
                });
                continue;
            }
        };
        envelopes.push((line_no, env));
    }

    ingest_envelopes(envelopes, binding, base_dir, report)
}

/// Ingest already-parsed envelopes. This is the path the importer seam uses,
/// and it keeps idempotency and backfill provenance in one place.
pub fn ingest_envelopes<I, B>(
    items: I,
    binding: &B,
    base_dir: &Path,
    mut report: IngestReport,
) -> IngestReport
where
    I: IntoIterator<Item = (usize, Envelope)>,
    B: CoreBinding,
{
    let mut sessions: HashMap<String, SessionState> = HashMap::new();

    for (line_no, env) in items {
        report.events += 1;
        if let Err(reason) = handle(&env, binding, base_dir, &mut sessions, &mut report) {
            report.skipped.push(SkippedLine { line_no, reason });
        }
    }

    // Roll the day roots up into month/year manifests once per run (the tiers are
    // derived/rebuildable, so this is ~1/run, not per event). Non-fatal.
    if let Err(reason) = binding.roll_up_calendar() {
        report.skipped.push(SkippedLine {
            line_no: 0,
            reason: format!("calendar roll-up: {reason}"),
        });
    }

    report
}

/// Map one envelope onto memory operations. Returns `Err(reason)` so the caller
/// records a line-numbered skip; the stream continues either way.
fn handle<B: CoreBinding>(
    env: &Envelope,
    binding: &B,
    base_dir: &Path,
    sessions: &mut HashMap<String, SessionState>,
    report: &mut IngestReport,
) -> Result<(), String> {
    let sid = session_key(env);
    let state = sessions.entry(sid).or_default();
    let marker = marker_name(env);
    // The UTC day this event buckets into (day tier), and its stable id (the day
    // HAMT key + in-run dedup key).
    let date = event_date(env);
    let ekey = event_key(env);

    if !state.seen_events.insert(ekey.clone()) {
        return Ok(());
    }
    // Cross-run idempotency: an event already in its day's HAMT is a no-op, so a
    // re-ingest of a growing session file doesn't reprocess prior events.
    if binding
        .day_contains(&date, &ekey)
        .map_err(|e| e.to_string())?
    {
        return Ok(());
    }

    match &env.event {
        Event::SessionStarted { .. } => {
            let seen_events = std::mem::take(&mut state.seen_events);
            *state = SessionState::default();
            state.seen_events = seen_events;
            state.pending_session_start_marker = Some(marker);
        }
        Event::UserPrompt { text } => {
            let cid = put(binding, "prompt", serde_json::json!({ "text": text }))?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::ModelResponse { text } => {
            // The host-neutral event carries no model id; record "unknown" rather
            // than fabricate one (mem's `response` requires the field).
            let cid = put(
                binding,
                "response",
                serde_json::json!({ "text": text, "model": "unknown" }),
            )?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::ToolCallStarted { tool, args_json } => {
            state
                .pending_tool_args
                .entry(tool.clone())
                .or_default()
                .push_back(PendingToolCall {
                    args_json: args_json.clone().unwrap_or_default(),
                });
        }
        Event::ToolCallFinished {
            tool,
            ok,
            result_json,
        } => {
            let input = state
                .pending_tool_args
                .get_mut(tool)
                .and_then(|queue| queue.pop_front())
                .unwrap_or_else(|| PendingToolCall {
                    args_json: String::new(),
                });
            let output = result_json.clone().unwrap_or_default();
            let cid = put(
                binding,
                "tool_result",
                serde_json::json!({ "tool": tool, "input": input.args_json, "output": output, "ok": ok }),
            )?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::FileRead { path } | Event::FileWritten { path } => {
            let full = base_dir.join(path);
            let bytes = std::fs::read(&full)
                .map_err(|e| format!("file_ref: cannot read {}: {e}", full.display()))?;
            let blob = binding
                .put_blob(&bytes, guess_media_type(path))
                .map_err(|e| e.to_string())?;
            report.blobs_written.push(blob.clone());
            let link = cid_link(&blob).map_err(|e| e.to_string())?;
            let cid = put(
                binding,
                "file_ref",
                serde_json::json!({ "path": path, "size": bytes.len() as u64, "content": link }),
            )?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::DecisionRecorded { text } => {
            // The event is a single line of text; map it to the `choice` and leave
            // the structured question/rationale empty rather than invent them.
            let cid = put(
                binding,
                "decision",
                serde_json::json!({ "question": "", "choice": text, "rationale": "" }),
            )?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::MemoryRecorded { text } => {
            // Backfilled notes (e.g. from markdown) land as `memory` of kind
            // `reference` — imported reference material, not a live preference.
            let cid = put(
                binding,
                "memory",
                serde_json::json!({ "text": text, "kind": "reference" }),
            )?;
            set_head(state, &cid);
            binding
                .record_event_in_day(&date, &ekey, &cid)
                .map_err(|e| e.to_string())?;
            report.names_bound += 1;
            if let Some(start_marker) = state.pending_session_start_marker.take() {
                bind_event_marker(binding, state, &start_marker, &cid, report)?;
            }
            report.record_cids.push(cid);
            report.nodes_written += 1;
        }
        Event::CheckpointRequested { label } => {
            let label = label.clone().unwrap_or_else(|| "checkpoint".to_string());
            checkpoint(env, binding, state, report, &label, &marker)?;
        }
        Event::SessionEnded => {
            checkpoint(env, binding, state, report, "session-ended", &marker)?;
        }
    }

    Ok(())
}

fn put<B: CoreBinding>(binding: &B, kind: &str, fields: serde_json::Value) -> Result<Cid, String> {
    let node = Node {
        kind: kind.to_string(),
        fields_json: fields.to_string(),
    };
    binding.put_node(&node).map_err(|e| e.to_string())
}

fn session_key(env: &Envelope) -> String {
    format!("{}\0{}", env.host_id, env.session_id)
}

fn fingerprint(env: &Envelope) -> String {
    let mut stripped = env.clone();
    stripped.imported_from = None;
    serde_json::to_string(&stripped).unwrap_or_else(|_| {
        format!(
            "{}|{}|{}|{}",
            env.host_id,
            env.session_id,
            env.ts,
            match &env.event {
                Event::SessionStarted { .. } => "session_started".to_string(),
                Event::UserPrompt { text } => format!("user_prompt:{text}"),
                Event::ModelResponse { text } => format!("model_response:{text}"),
                Event::ToolCallStarted { tool, args_json } => format!(
                    "tool_call_started:{tool}:{}",
                    args_json.as_deref().unwrap_or_default()
                ),
                Event::ToolCallFinished {
                    tool,
                    ok,
                    result_json,
                } => format!(
                    "tool_call_finished:{tool}:{ok}:{}",
                    result_json.as_deref().unwrap_or_default()
                ),
                Event::FileRead { path } => format!("file_read:{path}"),
                Event::FileWritten { path } => format!("file_written:{path}"),
                Event::DecisionRecorded { text } => format!("decision_recorded:{text}"),
                Event::MemoryRecorded { text } => format!("memory_recorded:{text}"),
                Event::CheckpointRequested { label } => format!(
                    "checkpoint_requested:{}",
                    label.as_deref().unwrap_or_default()
                ),
                Event::SessionEnded => "session_ended".to_string(),
            }
        )
    })
}

fn event_key(env: &Envelope) -> String {
    match &env.event_id {
        Some(id) => format!("id:{id}"),
        None => format!("fp:{}", fingerprint(env)),
    }
}

/// The UTC day (`YYYY-MM-DD`) an event buckets into (Phase A.5 day tier). The
/// envelope `ts` is RFC 3339, so its first 10 chars are the date; events that
/// arrive without a timestamp (e.g. markdown backfill sets `ts = ""`) fall back
/// to today.
fn event_date(env: &Envelope) -> String {
    if env.ts.len() >= 10 && env.ts.as_bytes()[4] == b'-' {
        env.ts[..10].to_string()
    } else {
        utc_today()
    }
}

fn marker_name(env: &Envelope) -> String {
    let digest = Sha256::digest(fingerprint(env).as_bytes());
    let digest_hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "host:{}:session:{}:event:{}",
        env.host_id, env.session_id, digest_hex
    )
}

fn set_head(state: &mut SessionState, cid: &Cid) {
    state.head = Some(cid.clone());
}

fn bind_event_marker<B: CoreBinding>(
    binding: &B,
    state: &mut SessionState,
    marker: &str,
    cid: &Cid,
    report: &mut IngestReport,
) -> Result<(), String> {
    binding.bind(marker, cid).map_err(|e| e.to_string())?;
    report.names_bound += 1;
    if state.pending_session_start_marker.as_deref() == Some(marker) {
        state.pending_session_start_marker = None;
    }
    Ok(())
}

/// Write a checkpoint over the session's current head, chaining the previous
/// checkpoint as parent, then bind the scoped names to it. A session that has
/// produced no node yet is a no-op (nothing to point at).
fn checkpoint<B: CoreBinding>(
    env: &Envelope,
    binding: &B,
    state: &mut SessionState,
    report: &mut IngestReport,
    label: &str,
    marker: &str,
) -> Result<(), String> {
    let (head, parent) = match &state.head {
        Some(head) => (head.clone(), state.last_checkpoint.clone()),
        None => return Ok(()),
    };

    let cp = binding
        .checkpoint(label, &head, parent.as_ref())
        .map_err(|e| e.to_string())?;
    report.checkpoints += 1;
    report.record_cids.push(cp.clone());

    for name in scoped_names(env, label) {
        binding.bind(&name, &cp).map_err(|e| e.to_string())?;
        report.names_bound += 1;
    }

    bind_event_marker(binding, state, marker, &cp, report)?;
    if let Some(start_marker) = state.pending_session_start_marker.clone() {
        bind_event_marker(binding, state, &start_marker, &cp, report)?;
        state.pending_session_start_marker = None;
    }
    state.last_checkpoint = Some(cp);
    Ok(())
}

/// The collision-free names a checkpoint binds (plan's Names section): most
/// specific first, ending at the global `latest`.
fn scoped_names(env: &Envelope, label: &str) -> Vec<String> {
    let host = &env.host_id;
    let session = &env.session_id;
    let mut names = vec![
        format!("host:{host}:session:{session}:checkpoint:{label}"),
        format!("host:{host}:session:{session}:latest"),
        format!("host:{host}:latest"),
    ];
    if let Some(project) = &env.project_id {
        names.push(format!("project:{project}:latest"));
    }
    names.push("latest".to_string());
    names
}

/// A minimal extension → media-type guess for stored file blobs.
fn guess_media_type(path: &str) -> &'static str {
    let ext = PathBuf::from(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "rs" | "py" | "ts" | "js" | "go" | "txt" | "md" | "toml" | "yaml" | "yml" => "text/plain",
        "json" => "application/json",
        "html" => "text/html",
        "css" => "text/css",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{
        CidOrName, CoreBinding, Error, Event, GcPolicy, GcReport, MemCli, Node, Record,
    };
    use std::cell::RefCell;
    use std::collections::HashMap as StdHashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn store() -> (tempfile::TempDir, MemCli) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = MemCli::new(dir.path());
        (dir, mem)
    }

    #[derive(Default)]
    struct MockBinding {
        nodes: RefCell<Vec<Node>>,
        blobs: RefCell<Vec<Vec<u8>>>,
        blob_cids: RefCell<StdHashMap<Vec<u8>, Cid>>,
        names: RefCell<StdHashMap<String, Cid>>,
        counter: AtomicUsize,
    }

    impl MockBinding {
        fn next_cid(&self, prefix: &str) -> Cid {
            let id = self.counter.fetch_add(1, Ordering::SeqCst);
            Cid(format!("{prefix}-{id}"))
        }
    }

    impl CoreBinding for MockBinding {
        fn put_node(&self, node: &Node) -> concierge_core::Result<Cid> {
            self.nodes.borrow_mut().push(node.clone());
            Ok(self.next_cid("node"))
        }

        fn put_blob(&self, bytes: &[u8], _media_type: &str) -> concierge_core::Result<Cid> {
            let bytes = bytes.to_vec();
            if let Some(cid) = self.blob_cids.borrow().get(&bytes).cloned() {
                self.blobs.borrow_mut().push(bytes);
                return Ok(cid);
            }
            let cid = self.next_cid("blob");
            self.blob_cids
                .borrow_mut()
                .insert(bytes.clone(), cid.clone());
            self.blobs.borrow_mut().push(bytes);
            Ok(cid)
        }

        fn bind(&self, name: &str, cid: &Cid) -> concierge_core::Result<()> {
            self.names
                .borrow_mut()
                .insert(name.to_string(), cid.clone());
            Ok(())
        }

        fn resolve(&self, name: &str) -> concierge_core::Result<Cid> {
            self.names
                .borrow()
                .get(name)
                .cloned()
                .ok_or_else(|| Error::NameUnbound(name.to_string()))
        }

        fn get(&self, _key: &CidOrName) -> concierge_core::Result<Record> {
            Err(Error::Io("not implemented".to_string()))
        }

        fn checkpoint(
            &self,
            _label: &str,
            _root: &Cid,
            _parent: Option<&Cid>,
        ) -> concierge_core::Result<Cid> {
            Ok(self.next_cid("checkpoint"))
        }

        fn walk(&self, _root: &Cid) -> concierge_core::Result<Vec<Cid>> {
            Ok(Vec::new())
        }

        fn gc(&self, _policy: &GcPolicy) -> concierge_core::Result<GcReport> {
            Ok(GcReport::default())
        }

        fn record_event_in_day(
            &self,
            date: &str,
            _event_key: &str,
            _record: &Cid,
        ) -> concierge_core::Result<Cid> {
            // Mirror the real rebind: a fresh day CID bound to the day root, so
            // tests can observe day-tier indexing without a real HAMT/store.
            let cid = self.next_cid("day");
            self.names
                .borrow_mut()
                .insert(format!("day-{date}"), cid.clone());
            Ok(cid)
        }

        fn day_contains(&self, _date: &str, _event_key: &str) -> concierge_core::Result<bool> {
            // The mock has no HAMT; in-run dedup (`seen_events`) covers its tests.
            Ok(false)
        }

        fn roll_up_calendar(&self) -> concierge_core::Result<()> {
            Ok(()) // no calendar tiers in the mock
        }
    }

    #[test]
    fn parses_a_user_prompt_line() {
        let line = r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hi"}"#;
        let env = parse_line(line).expect("valid line should parse");
        assert_eq!(env.host_id, "hermes");
        assert_eq!(env.session_id, "s1");
        assert!(matches!(env.event, Event::UserPrompt { text } if text == "hi"));
    }

    #[test]
    fn rejects_malformed_line() {
        assert!(parse_line("{not json").is_err());
    }

    const STREAM: &str = concat!(
        r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:00Z","type":"session_started"}"#,
        "\n",
        r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:01Z","type":"user_prompt","text":"add a health check"}"#,
        "\n",
        r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:02Z","type":"model_response","text":"done"}"#,
        "\n",
        r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:03Z","type":"decision_recorded","text":"use a Rust core"}"#,
        "\n",
        r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:04Z","type":"session_ended"}"#,
        "\n",
    );

    #[test]
    fn valid_stream_writes_nodes_and_a_resolvable_checkpoint() {
        let (dir, mem) = store();
        let report = ingest(STREAM.as_bytes(), &mem, dir.path());

        assert_eq!(report.events, 5, "five well-formed events");
        assert_eq!(report.nodes_written, 3, "prompt + response + decision");
        assert_eq!(report.checkpoints, 1, "session_ended writes one checkpoint");
        assert_eq!(
            report.record_cids.len(),
            4,
            "kernel capture receives prompt + response + decision + checkpoint CIDs"
        );
        assert!(
            report.skipped.is_empty(),
            "nothing should be skipped: {:?}",
            report.skipped
        );

        // The scoped names and global `latest` resolve to the checkpoint.
        for name in [
            "latest",
            "host:hermes:latest",
            "host:hermes:session:s1:latest",
            "host:hermes:session:s1:checkpoint:session-ended",
        ] {
            let cid = mem
                .resolve(name)
                .unwrap_or_else(|e| panic!("resolve {name}: {e}"));
            match mem.get(&CidOrName::Cid(cid)).expect("get checkpoint") {
                Record::Live { kind, .. } => assert_eq!(kind, "checkpoint", "{name} → checkpoint"),
                other => panic!("expected live checkpoint for {name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn malformed_line_is_skipped_with_line_number_and_stream_continues() {
        let (dir, mem) = store();
        let stream = concat!(
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"user_prompt","text":"one"}"#,
            "\n",
            "{ this is not json\n",
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"user_prompt","text":"two"}"#,
            "\n",
        );
        let report = ingest(stream.as_bytes(), &mem, dir.path());

        assert_eq!(report.events, 2, "two valid events around the bad line");
        assert_eq!(report.nodes_written, 2);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].line_no, 2, "the bad line is line 2");
    }

    #[test]
    fn reingesting_the_same_stream_is_idempotent() {
        let (dir, mem) = store();

        let first = ingest(STREAM.as_bytes(), &mem, dir.path());
        let cid_after_first = mem.resolve("latest").expect("resolve after first ingest");

        let second = ingest(STREAM.as_bytes(), &mem, dir.path());
        let cid_after_second = mem.resolve("latest").expect("resolve after second ingest");

        assert_eq!(
            cid_after_first, cid_after_second,
            "content-addressing makes a re-ingest land on the same checkpoint CID"
        );
        assert_eq!(first.checkpoints, 1);
        assert_eq!(second.checkpoints, 0, "the replay should be a no-op");
        assert_eq!(
            second.nodes_written, 0,
            "no duplicate nodes should be written"
        );
        assert!(second.skipped.is_empty());
    }

    #[test]
    fn identical_file_content_dedups_to_one_blob_cid() {
        let (dir, mem) = store();
        // Two different paths, identical bytes → one blob CID. A third differs.
        std::fs::write(dir.path().join("a.txt"), b"same bytes").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"same bytes").unwrap();
        std::fs::write(dir.path().join("c.txt"), b"different").unwrap();
        let stream = concat!(
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"file_written","path":"a.txt"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"file_written","path":"b.txt"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"file_written","path":"c.txt"}"#,
            "\n",
        );
        let report = ingest(stream.as_bytes(), &mem, dir.path());

        assert_eq!(report.blobs_written.len(), 3, "one blob per file event");
        assert_eq!(
            report.blobs_written[0], report.blobs_written[1],
            "identical content must produce the same blob CID"
        );
        assert_ne!(
            report.blobs_written[0], report.blobs_written[2],
            "different content must produce a different blob CID"
        );
    }

    #[test]
    fn unreadable_file_is_skipped_not_fatal() {
        let (dir, mem) = store();
        let stream = concat!(
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"file_written","path":"missing.txt"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"t","type":"user_prompt","text":"still works"}"#,
            "\n",
        );
        let report = ingest(stream.as_bytes(), &mem, dir.path());

        assert_eq!(report.skipped.len(), 1, "the missing file is skipped");
        assert_eq!(report.skipped[0].line_no, 1);
        assert_eq!(report.nodes_written, 1, "the prompt after it still lands");
    }

    #[test]
    fn duplicate_events_are_skipped_by_event_fingerprint() {
        let binding = MockBinding::default();
        let stream = concat!(
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hello"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hello"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:01Z","type":"session_ended"}"#,
            "\n",
        );

        let report = ingest(stream.as_bytes(), &binding, std::path::Path::new("."));

        assert_eq!(report.events, 3);
        assert_eq!(report.nodes_written, 1, "duplicate prompt is suppressed");
        assert_eq!(binding.nodes.borrow().len(), 1);
        assert_eq!(report.checkpoints, 1);
    }

    #[test]
    fn overlapping_tool_calls_use_fifo_matching() {
        let binding = MockBinding::default();
        let stream = concat!(
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:00Z","type":"tool_call_started","tool":"search","args_json":"{\"q\":\"one\"}"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:01Z","type":"tool_call_started","tool":"search","args_json":"{\"q\":\"two\"}"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:02Z","type":"tool_call_finished","tool":"search","ok":true,"result_json":"{\"ok\":true}"}"#,
            "\n",
            r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:03Z","type":"tool_call_finished","tool":"search","ok":true,"result_json":"{\"ok\":true}"}"#,
            "\n",
        );

        let report = ingest(stream.as_bytes(), &binding, std::path::Path::new("."));

        assert_eq!(report.nodes_written, 2);
        let tool_results: Vec<_> = binding
            .nodes
            .borrow()
            .iter()
            .filter(|node| node.kind == "tool_result")
            .cloned()
            .collect();
        assert_eq!(tool_results.len(), 2);
        assert!(tool_results[0]
            .fields_json
            .contains(r#""input":"{\"q\":\"one\"}""#));
        assert!(tool_results[1]
            .fields_json
            .contains(r#""input":"{\"q\":\"two\"}""#));
    }

    #[test]
    fn jsonl_importer_attaches_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("source.jsonl"),
            concat!(
                r#"{"host_id":"h","session_id":"s","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hello"}"#,
                "\n"
            ),
        )
        .unwrap();

        let importer = JsonlImporter;
        let envelopes = importer
            .read(&dir.path().join("source.jsonl"))
            .expect("import");
        assert_eq!(envelopes.len(), 1);
        let imported = envelopes[0].imported_from.as_ref().expect("provenance");
        assert_eq!(imported.source_system, "jsonl");
        assert_eq!(imported.original_ts, "2026-06-02T00:00:00Z");
    }

    #[test]
    fn markdown_importer_splits_sections_with_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("notes.md"),
            "intro line\n\n## First\nbody one\n\n## Second\nbody two\n",
        )
        .unwrap();

        let envelopes = MarkdownImporter.read(dir.path()).expect("read markdown");
        // intro + two sections + a trailing session_ended.
        assert_eq!(envelopes.len(), 4);
        assert!(matches!(envelopes[0].event, Event::MemoryRecorded { .. }));
        assert!(matches!(
            envelopes.last().unwrap().event,
            Event::SessionEnded
        ));
        let prov = envelopes[0].imported_from.as_ref().expect("provenance");
        assert_eq!(prov.source_system, "markdown");
        assert_eq!(prov.original_id, "notes.md#0");
        assert_eq!(envelopes[0].session_id, "notes.md");
    }

    #[test]
    fn markdown_decision_heading_becomes_a_decision_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("d.md");
        std::fs::write(&file, "## Decision: use Rust\nbecause fidelity\n").unwrap();

        let envelopes = MarkdownImporter.read(&file).expect("read");
        assert!(matches!(envelopes[0].event, Event::DecisionRecorded { .. }));
    }

    #[test]
    fn markdown_import_writes_memory_and_decision_nodes() {
        let binding = MockBinding::default();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("vault.md"),
            "## Note\nremember this\n\n## Decision: pick X\nrationale\n",
        )
        .unwrap();

        let envelopes = MarkdownImporter.read(dir.path()).expect("read");
        let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
        let report = ingest_envelopes(items, &binding, dir.path(), IngestReport::default());

        let kinds: Vec<String> = binding
            .nodes
            .borrow()
            .iter()
            .map(|n| n.kind.clone())
            .collect();
        assert!(kinds.contains(&"memory".to_string()), "kinds: {kinds:?}");
        assert!(kinds.contains(&"decision".to_string()), "kinds: {kinds:?}");
        assert_eq!(report.checkpoints, 1, "session_ended closes the file");
    }

    #[test]
    fn markdown_reimport_is_idempotent() {
        let (dir, mem) = store();
        let file = dir.path().join("n.md");
        std::fs::write(&file, "## A\none\n\n## B\ntwo\n").unwrap();

        let run = |mem: &MemCli| {
            let envelopes = MarkdownImporter.read(&file).expect("read");
            let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
            ingest_envelopes(items, mem, dir.path(), IngestReport::default())
        };

        let first = run(&mem);
        let latest1 = mem.resolve("latest").expect("latest after first import");
        let _second = run(&mem);
        let latest2 = mem.resolve("latest").expect("latest after second import");

        assert_eq!(
            latest1, latest2,
            "re-import lands on the same checkpoint CID"
        );
        assert!(
            first.nodes_written >= 2,
            "two sections become two memory nodes"
        );
    }
}
