//! Layer 3 — Claude Code harness adapter.
//!
//! Claude Code records each session as its own JSONL transcript under
//! `~/.claude/projects/<slug>/<session-uuid>.jsonl`. That format is **not** the
//! Concierge [`Envelope`] schema — lines are tagged by `type` (`user`,
//! `assistant`, `system`, `mode`, …) with a nested `message` whose `content` is
//! either a plain string (a user prompt) or a list of typed blocks (`text`,
//! `thinking`, `tool_use`, `tool_result`).
//!
//! This adapter translates those native lines into a stream of canonical
//! [`Envelope`]s and reuses the Phase 2 JSONL ingest path for the actual writes
//! (`ingest_envelopes`). The translation is the whole job; everything downstream
//! (nodes, checkpoints, scoped names, the day-tier calendar) is shared.
//!
//! ## Idempotency
//! Every emitted envelope carries the line's stable Claude Code `uuid` as its
//! `event_id` (suffixed `#0`, `#1`, … when one line fans out into several
//! events). Node identity is the CID, so re-reading a growing session file
//! re-writes the same CIDs — capture is naturally idempotent, which is what makes
//! the Phase C file-watcher safe to fire on every change.
//!
//! ## Fidelity
//! - `user` + string content        → [`Event::UserPrompt`]
//! - `user` + `tool_result` blocks   → [`Event::ToolCallFinished`]
//! - `assistant` `text` blocks       → [`Event::ModelResponse`]
//! - `assistant` `tool_use` blocks   → [`Event::ToolCallStarted`]
//! - `assistant` `thinking` block    → [`Reasoning`] carried **inline** on the
//!   same line's first content envelope (Decision 0023: the "why" rides in the
//!   same record/CID, never a separate node).
//! - other line types (`mode`, `system`, snapshots, …) are skipped.

use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{CoreBinding, Envelope, Event, ImportedFrom, Reasoning, ReasoningSource};
use serde::Deserialize;

pub mod discovery;

const HOST_ID: &str = "claude-code";
const SOURCE_SYSTEM: &str = "claude-code";

/// One raw Claude Code transcript line. Lenient: only the fields the adapter
/// reads are declared, everything else is ignored, and all are optional so a
/// metadata-only line (`mode`, `permission-mode`, …) still deserializes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    content: Option<ClaudeContent>,
}

/// `message.content` is either a plain string (user prompt) or a list of blocks.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ClaudeContent {
    Text(String),
    Blocks(Vec<ClaudeBlock>),
}

/// One content block. `type` selects which other fields are meaningful. Note:
/// unlike the camelCase top-level transcript envelope, Anthropic message content
/// blocks use snake_case keys (`tool_use_id`, `is_error`), so no `rename_all`.
#[derive(Debug, Deserialize)]
struct ClaudeBlock {
    #[serde(rename = "type")]
    kind: String,
    // text block
    #[serde(default)]
    text: Option<String>,
    // thinking block
    #[serde(default)]
    thinking: Option<String>,
    // tool_use block
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
    // tool_result block
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    is_error: Option<bool>,
}

/// Running state across a session's lines: maps a `tool_use` id to its tool name
/// so a later `tool_result` can report the real tool, and tracks whether the
/// opening `SessionStarted` has been emitted yet.
#[derive(Default)]
struct SessionXlate {
    tool_names: HashMap<String, String>,
    started: bool,
    last_session_id: Option<String>,
}

/// Translate Claude Code transcript lines into canonical [`Envelope`]s.
///
/// Unparseable or non-ingestible lines are skipped. A trailing
/// [`Event::CheckpointRequested`] is appended so the file becomes a checkpointed,
/// session-scoped entry point in the explorer — without falsely asserting the
/// session ended (it may still be live; the watcher re-ingests on append).
pub fn translate_lines<I: IntoIterator<Item = String>>(lines: I) -> Vec<Envelope> {
    translate_chunk(lines, true)
}

/// Translate a chunk of transcript lines. `emit_session_start` is false for an
/// incremental tail read (offset > 0), so the opening `SessionStarted` is only
/// emitted once for a session, not re-emitted on every appended chunk.
fn translate_chunk<I: IntoIterator<Item = String>>(
    lines: I,
    emit_session_start: bool,
) -> Vec<Envelope> {
    let mut state = SessionXlate {
        started: !emit_session_start,
        ..SessionXlate::default()
    };
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<ClaudeLine>(trimmed) else {
            continue;
        };
        translate_line(&parsed, &mut state, &mut out);
    }
    if let Some(session_id) = state.last_session_id.clone() {
        out.push(envelope(
            &session_id,
            format!("{session_id}#checkpoint"),
            "",
            Event::CheckpointRequested {
                label: Some(format!("claude-code session {session_id}")),
            },
            None,
        ));
    }
    out
}

fn translate_line(line: &ClaudeLine, state: &mut SessionXlate, out: &mut Vec<Envelope>) {
    // Only conversation lines carry ingestible content.
    if line.kind != "user" && line.kind != "assistant" {
        return;
    }
    let Some(session_id) = line.session_id.clone() else {
        return;
    };
    state.last_session_id = Some(session_id.clone());
    let base_id = line.uuid.clone().unwrap_or_else(|| fingerprint(line));
    let ts = line.timestamp.clone().unwrap_or_default();

    // The first conversation line opens the session (carrying its cwd).
    if !state.started {
        state.started = true;
        out.push(envelope(
            &session_id,
            format!("{base_id}#start"),
            &ts,
            Event::SessionStarted {
                cwd: line.cwd.clone(),
            },
            None,
        ));
    }

    let mut events: Vec<(Event, Option<Reasoning>)> = Vec::new();
    let mut pending_reasoning: Option<Reasoning> = None;

    match line.message.as_ref().and_then(|m| m.content.as_ref()) {
        Some(ClaudeContent::Text(text)) if line.kind == "user" => {
            events.push((Event::UserPrompt { text: text.clone() }, None));
        }
        Some(ClaudeContent::Text(text)) => {
            // An assistant line with bare-string content is a plain response.
            events.push((Event::ModelResponse { text: text.clone() }, None));
        }
        Some(ClaudeContent::Blocks(blocks)) => {
            // Collect thinking first so it can ride inline on the first event.
            let thinking: String = blocks
                .iter()
                .filter(|b| b.kind == "thinking")
                .filter_map(|b| b.thinking.clone())
                .collect::<Vec<_>>()
                .join("\n");
            if !thinking.is_empty() {
                pending_reasoning = Some(Reasoning {
                    text: thinking,
                    source: ReasoningSource::Thinking,
                });
            }
            // Concatenate text blocks into one response.
            let text: String = blocks
                .iter()
                .filter(|b| b.kind == "text")
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                events.push((Event::ModelResponse { text }, None));
            }
            for block in blocks {
                match block.kind.as_str() {
                    "tool_use" => {
                        let tool = block.name.clone().unwrap_or_else(|| "tool".to_string());
                        if let Some(id) = &block.id {
                            state.tool_names.insert(id.clone(), tool.clone());
                        }
                        events.push((
                            Event::ToolCallStarted {
                                tool,
                                args_json: block.input.as_ref().map(|v| v.to_string()),
                            },
                            None,
                        ));
                    }
                    "tool_result" => {
                        let tool = block
                            .tool_use_id
                            .as_ref()
                            .and_then(|id| state.tool_names.get(id).cloned())
                            .or_else(|| block.tool_use_id.clone())
                            .unwrap_or_else(|| "tool".to_string());
                        events.push((
                            Event::ToolCallFinished {
                                tool,
                                ok: !block.is_error.unwrap_or(false),
                                result_json: block.content.as_ref().map(value_to_text),
                            },
                            None,
                        ));
                    }
                    _ => {}
                }
            }
        }
        None => {}
    }

    // Attach the line's reasoning to its first emitted content event (inline,
    // Decision 0023), then stamp each event with a stable, unique id.
    if let Some(reasoning) = pending_reasoning {
        if let Some(first) = events.first_mut() {
            first.1 = Some(reasoning);
        }
    }
    for (idx, (event, reasoning)) in events.into_iter().enumerate() {
        out.push(envelope(
            &session_id,
            format!("{base_id}#{idx}"),
            &ts,
            event,
            reasoning,
        ));
    }
}

/// Build a canonical envelope routed to the Claude Code host, carrying import
/// provenance and (optionally) inline reasoning.
fn envelope(
    session_id: &str,
    event_id: String,
    ts: &str,
    event: Event,
    reasoning: Option<Reasoning>,
) -> Envelope {
    Envelope {
        host_id: HOST_ID.to_string(),
        session_id: session_id.to_string(),
        project_id: None,
        event_id: Some(event_id.clone()),
        ts: ts.to_string(),
        imported_from: Some(ImportedFrom {
            source_system: SOURCE_SYSTEM.to_string(),
            original_id: event_id,
            original_ts: ts.to_string(),
        }),
        reasoning,
        event,
    }
}

/// A `tool_result`'s `content` may be a plain string or an array of `{type:text,
/// text}` blocks. Flatten to text when possible, else the compact JSON.
fn value_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let texts: Vec<String> = items
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect();
            if texts.is_empty() {
                value.to_string()
            } else {
                texts.join("\n")
            }
        }
        other => other.to_string(),
    }
}

/// Deterministic fallback id for a line with no `uuid` (rare): hash the stable
/// fields so a re-read maps to the same node.
fn fingerprint(line: &ClaudeLine) -> String {
    format!(
        "cc:{}:{}:{}",
        line.session_id.as_deref().unwrap_or(""),
        line.timestamp.as_deref().unwrap_or(""),
        line.kind,
    )
}

/// Translate then ingest a Claude Code transcript from any reader.
pub fn ingest_reader<R: BufRead, B: CoreBinding>(
    reader: R,
    binding: &B,
    base_dir: &Path,
) -> IngestReport {
    let lines = reader.lines().map_while(Result::ok);
    let envelopes = translate_lines(lines);
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    ingest_envelopes(items, binding, base_dir, IngestReport::default())
}

/// Translate then ingest a single Claude Code session `*.jsonl` file.
pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let file = std::fs::File::open(path)?;
    Ok(ingest_reader(
        std::io::BufReader::new(file),
        binding,
        base_dir,
    ))
}

/// Incrementally ingest only the bytes appended past `offset` (Phase C watcher).
///
/// Reads from `offset` to the last complete newline, ingests those lines, and
/// returns the new offset (the byte position after the last full line). A partial
/// trailing line still being written is left for the next call. The opening
/// `SessionStarted` is emitted only when `offset == 0`. If the file is shorter
/// than `offset` (truncated/rotated), it restarts from 0 — re-ingest is
/// idempotent by CID, so this is safe.
pub fn ingest_file_from_offset<B: CoreBinding>(
    path: &Path,
    offset: u64,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<(IngestReport, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = if offset > len { 0 } else { offset };
    if start == len {
        return Ok((IngestReport::default(), len)); // nothing new
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    use std::io::Read;
    file.read_to_string(&mut buf)?;
    // Only process through the last complete line; keep any partial tail.
    let consumed = match buf.rfind('\n') {
        Some(pos) => pos + 1,
        None => return Ok((IngestReport::default(), start)), // no complete line yet
    };
    let lines: Vec<String> = buf[..consumed].lines().map(str::to_string).collect();
    let envelopes = translate_chunk(lines, start == 0);
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    let report = ingest_envelopes(items, binding, base_dir, IngestReport::default());
    Ok((report, start + consumed as u64))
}

/// One incremental capture pass over a whole `projects` directory: ingest any
/// newly-appended lines across every session, advancing the per-file offsets.
/// Returns the number of new events ingested. Cheap when nothing changed — a
/// stat plus a seek-to-EOF per file — so it is safe to call on a short interval
/// from the app's background capture loop (Phase C). The first call (empty
/// offsets) performs the full backfill; subsequent calls are the live tail.
pub fn capture_once<B: CoreBinding>(
    projects_dir: &Path,
    offsets: &mut HashMap<std::path::PathBuf, u64>,
    binding: &B,
    base_dir: &Path,
) -> usize {
    let mut total = 0usize;
    for session in discovery::enumerate_sessions(projects_dir) {
        let start = offsets.get(&session.session_file).copied().unwrap_or(0);
        if let Ok((report, new_offset)) =
            ingest_file_from_offset(&session.session_file, start, binding, base_dir)
        {
            offsets.insert(session.session_file, new_offset);
            total += report.events;
        }
    }
    total
}

/// Seed the offset of every CURRENTLY-EXISTING session to its end WITHOUT ingesting,
/// so attaching/loading does not backfill history. Only sessions not already tracked
/// are seeded (existing incremental positions are preserved); sessions created later
/// are still captured from the start. This is what stops the first-attach backfill.
pub fn seed_offsets(projects_dir: &Path, offsets: &mut HashMap<std::path::PathBuf, u64>) {
    for session in discovery::enumerate_sessions(projects_dir) {
        if let Ok(meta) = std::fs::metadata(&session.session_file) {
            offsets.entry(session.session_file).or_insert(meta.len());
        }
    }
}

/// Full historical backfill: re-read every session from offset 0 (the manual
/// "Ingest" action). Idempotent via CID dedup. Returns the number of events ingested.
pub fn ingest_all<B: CoreBinding>(projects_dir: &Path, binding: &B, base_dir: &Path) -> usize {
    let mut offsets = HashMap::new();
    capture_once(projects_dir, &mut offsets, binding, base_dir)
}

/// Render a node→host [`ContextSuggested`] (Phase 8 §2) into the text block a
/// Claude Code harness would **prepend** to its context window — the "inject" half
/// of the write-back loop. `previews` is the resolved `(cid, preview)` content for
/// the suggestion's CIDs (the adapter stays I/O-free; the caller resolves them).
///
/// The block is explicitly **attributed to the trusting authority** and labeled as
/// suggested context, never silently merged as instructions (threat-model L1 / the
/// MemoryOS "Ground Truth" discipline). A harness that does not trust the authority
/// can drop the block wholesale.
pub fn render_injection(
    suggestion: &concierge_core::ContextSuggested,
    previews: &[(String, String)],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<suggested-context source=\"concierge\" authority=\"{}\" reason=\"{}\">",
        escape_attr(&suggestion.authority),
        escape_attr(&suggestion.reason),
    );
    for cid in &suggestion.cids {
        let preview = previews
            .iter()
            .find(|(c, _)| c == cid)
            .map(|(_, p)| p.as_str())
            .unwrap_or("(content unavailable)");
        let _ = writeln!(out, "- {cid}: {}", preview.replace('\n', " "));
    }
    out.push_str("</suggested-context>");
    out
}

/// Minimal XML attribute escaping for the injection block's `authority`/`reason`.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::MemCli;

    fn ids(envs: &[Envelope]) -> Vec<String> {
        envs.iter().map(|e| e.event_id.clone().unwrap()).collect()
    }

    const USER_LINE: &str = r#"{"type":"user","sessionId":"s1","uuid":"u1","timestamp":"t","cwd":"/p","message":{"role":"user","content":"hi"}}"#;
    const ASSISTANT_LINE: &str = r#"{"type":"assistant","sessionId":"s1","uuid":"a1","timestamp":"t","message":{"content":[{"type":"text","text":"yo"}]}}"#;

    #[test]
    fn render_injection_attributes_the_block_and_inlines_previews() {
        let suggestion = concierge_core::ContextSuggested {
            cids: vec!["bafyA".to_string(), "bafyMissing".to_string()],
            reason: "relevant prior context".to_string(),
            authority: "claude-code".to_string(),
        };
        let previews = vec![(
            "bafyA".to_string(),
            "the egress lock\nfences data".to_string(),
        )];
        let block = render_injection(&suggestion, &previews);
        assert!(
            block.contains("authority=\"claude-code\""),
            "attributed to the grant"
        );
        assert!(
            block.contains("bafyA: the egress lock fences data"),
            "preview inlined, newline flattened"
        );
        assert!(
            block.contains("bafyMissing: (content unavailable)"),
            "missing content is explicit, not silent"
        );
        assert!(block.starts_with("<suggested-context") && block.ends_with("</suggested-context>"));
    }

    #[test]
    fn user_prompt_line_becomes_a_user_prompt_event() {
        let line = r#"{"type":"user","sessionId":"s1","uuid":"u1","timestamp":"2026-06-09T00:00:00Z","cwd":"/proj","message":{"role":"user","content":"hello there"}}"#;
        let out = translate_lines([line.to_string()]);
        // SessionStarted (with cwd) + UserPrompt + trailing CheckpointRequested.
        assert!(matches!(out[0].event, Event::SessionStarted { cwd: Some(ref c) } if c == "/proj"));
        assert!(matches!(&out[1].event, Event::UserPrompt { text } if text == "hello there"));
        assert!(matches!(
            out.last().unwrap().event,
            Event::CheckpointRequested { .. }
        ));
        assert_eq!(out[0].host_id, "claude-code");
        assert_eq!(out[1].session_id, "s1");
    }

    #[test]
    fn assistant_thinking_rides_inline_on_the_response() {
        let line = r#"{"type":"assistant","sessionId":"s1","uuid":"a1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"the why"},{"type":"text","text":"the answer"}]}}"#;
        let out = translate_lines([line.to_string()]);
        let resp = out
            .iter()
            .find(|e| matches!(e.event, Event::ModelResponse { .. }))
            .unwrap();
        assert!(matches!(&resp.event, Event::ModelResponse { text } if text == "the answer"));
        // Decision 0023: the reasoning is inline on the same record, not separate.
        let r = resp.reasoning.as_ref().expect("inline reasoning");
        assert_eq!(r.text, "the why");
        assert_eq!(r.source, ReasoningSource::Thinking);
    }

    #[test]
    fn tool_use_then_result_pairs_by_name() {
        let call = r#"{"type":"assistant","sessionId":"s1","uuid":"a2","timestamp":"t","message":{"content":[{"type":"tool_use","id":"tool_42","name":"Read","input":{"path":"x.rs"}}]}}"#;
        let result = r#"{"type":"user","sessionId":"s1","uuid":"u2","timestamp":"t","message":{"content":[{"type":"tool_result","tool_use_id":"tool_42","content":"file body","is_error":false}]}}"#;
        let out = translate_lines([call.to_string(), result.to_string()]);
        let started = out
            .iter()
            .find(|e| matches!(e.event, Event::ToolCallStarted { .. }))
            .unwrap();
        assert!(
            matches!(&started.event, Event::ToolCallStarted { tool, args_json } if tool == "Read" && args_json.as_deref().unwrap().contains("x.rs"))
        );
        // The result resolves the tool name from the earlier tool_use id.
        let finished = out
            .iter()
            .find(|e| matches!(e.event, Event::ToolCallFinished { .. }))
            .unwrap();
        assert!(
            matches!(&finished.event, Event::ToolCallFinished { tool, ok, result_json } if tool == "Read" && *ok && result_json.as_deref() == Some("file body"))
        );
    }

    #[test]
    fn tool_result_content_blocks_flatten_to_text() {
        let result = r#"{"type":"user","sessionId":"s1","uuid":"u3","timestamp":"t","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":[{"type":"text","text":"line one"},{"type":"text","text":"line two"}],"is_error":true}]}}"#;
        let out = translate_lines([result.to_string()]);
        let finished = out
            .iter()
            .find(|e| matches!(e.event, Event::ToolCallFinished { .. }))
            .unwrap();
        assert!(
            matches!(&finished.event, Event::ToolCallFinished { ok, result_json, .. } if !*ok && result_json.as_deref() == Some("line one\nline two"))
        );
    }

    #[test]
    fn metadata_only_lines_are_skipped() {
        let lines = [
            r#"{"type":"mode","sessionId":"s1","mode":"default"}"#.to_string(),
            r#"{"type":"permission-mode","sessionId":"s1","permissionMode":"plan"}"#.to_string(),
            r#"{"type":"file-history-snapshot","messageId":"m","snapshot":{}}"#.to_string(),
            r#"{"type":"system","sessionId":"s1","uuid":"sy","subtype":"hook"}"#.to_string(),
            r#"not even json"#.to_string(),
        ];
        // Nothing ingestible → no SessionStarted, no events, no trailing checkpoint.
        assert!(translate_lines(lines).is_empty());
    }

    #[test]
    fn incremental_offset_ingest_reads_only_new_complete_lines() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let session = dir.path().join("s1.jsonl");
        std::fs::write(&session, format!("{USER_LINE}\n")).unwrap();

        // First read (offset 0): emits SessionStarted + UserPrompt + checkpoint.
        let (r1, off1) = ingest_file_from_offset(&session, 0, &mem, dir.path()).unwrap();
        assert!(r1.events >= 2, "first read ingests the opening lines");
        assert_eq!(off1, std::fs::metadata(&session).unwrap().len());

        // Re-read with no change: nothing new, offset unchanged.
        let (r2, off2) = ingest_file_from_offset(&session, off1, &mem, dir.path()).unwrap();
        assert_eq!(r2.events, 0, "no new bytes, nothing re-ingested");
        assert_eq!(off2, off1);

        // Append a complete line + a partial (no trailing newline) line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&session)
            .unwrap();
        write!(f, "{ASSISTANT_LINE}\n{{\"partial").unwrap();
        let (r3, off3) = ingest_file_from_offset(&session, off2, &mem, dir.path()).unwrap();
        assert!(r3.events >= 1, "the new complete line is ingested");
        // The offset advances only past the last complete line, not the partial tail.
        assert_eq!(off3, off2 + format!("{ASSISTANT_LINE}\n").len() as u64);
    }

    #[test]
    fn capture_once_backfills_then_picks_up_appends_across_sessions() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let mem = MemCli::new(store.path());
        let projects = discovery::claude_projects_dir_in(home.path());
        let slug = projects.join("-proj");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::write(slug.join("s1.jsonl"), format!("{USER_LINE}\n")).unwrap();

        let mut offsets = std::collections::HashMap::new();
        // First pass backfills the existing session.
        let first = capture_once(&projects, &mut offsets, &mem, store.path());
        assert!(first >= 2, "backfilled the opening lines");
        // Idempotent: a second pass with no changes ingests nothing new.
        assert_eq!(capture_once(&projects, &mut offsets, &mem, store.path()), 0);
        // A new session file appears and is picked up.
        let mut f = std::fs::File::create(slug.join("s2.jsonl")).unwrap();
        writeln!(f, "{USER_LINE}").unwrap();
        assert!(capture_once(&projects, &mut offsets, &mem, store.path()) >= 2);
    }

    #[test]
    fn event_ids_are_stable_and_unique_for_idempotent_reingest() {
        let line = r#"{"type":"assistant","sessionId":"s1","uuid":"a3","timestamp":"t","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#;
        let first = translate_lines([line.to_string()]);
        let second = translate_lines([line.to_string()]);
        // Re-translating the same line yields identical, unique ids → CID dedup.
        assert_eq!(ids(&first), ids(&second));
        let mut sorted = ids(&first);
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids(&first).len(), "ids are unique");
        // The two emitted content events share the base uuid with distinct suffixes.
        assert!(first.iter().any(|e| e.event_id.as_deref() == Some("a3#0")));
        assert!(first.iter().any(|e| e.event_id.as_deref() == Some("a3#1")));
    }
}
