//! Layer 3 — **Codex CLI** harness adapter.
//!
//! OpenAI's Codex CLI writes every session to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, one `{"type","timestamp",
//! "payload"}` record per line. This crate discovers those files ([`discovery`])
//! and translates the canonical `response_item` transcript stream (plus
//! `event_msg/patch_apply_end` for file edits) into the host-neutral Concierge
//! [`Envelope`] stream, then reuses the JSONL ingest path ([`ingest_envelopes`])
//! for the IPLD writes — exactly the shape of the Claude Code / Aider adapters,
//! just a different on-disk format. Observe-only (Tier 4): it reads Codex's own
//! files, never proxies traffic, never touches keys.
//!
//! Mapping (ported from `adapters/codex/translate.py`):
//! - `session_meta` → `SessionStarted { cwd }`
//! - `response_item/message` role=user → `UserPrompt`
//! - `response_item/message` role=assistant → `ModelResponse` (+ held reasoning)
//! - `response_item/reasoning` → held, attached to the next assistant message
//! - `response_item/function_call` / `custom_tool_call` → `ToolCallStarted`
//! - `response_item/function_call_output` / `custom_tool_call_output` → `ToolCallFinished`
//! - `event_msg/patch_apply_end` → `FileWritten` per change + an `apply_patch` finish
//!
//! Ingest is content-addressed (dedup by CID), so re-reading a growing transcript
//! on every capture pass is safe and idempotent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{CoreBinding, Envelope, Event, ImportedFrom, Reasoning, ReasoningSource};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "codex";
const SOURCE_SYSTEM: &str = "codex";
const MAX_FIELD: usize = 8192;

/// A stable, path-derived id so a session's nodes get the same CIDs on re-read
/// (FNV-1a over the absolute path — deterministic, no deps).
pub fn source_id_for(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("codex-{hash:016x}")
}

/// Truncate to the field cap, on a char boundary (never splits a UTF-8 codepoint).
fn cap(s: &str) -> String {
    if s.len() <= MAX_FIELD {
        return s.to_string();
    }
    let mut end = MAX_FIELD;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Join the text of an OpenAI Responses `content` (a string, or a list of blocks
/// like `{"type":"input_text"/"output_text"/"text","text":…}`).
fn text_of(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        };
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            if let Some(obj) = block.as_object() {
                let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if t.ends_with("text") {
                    if let Some(txt) = obj.get("text").and_then(|v| v.as_str()) {
                        if !txt.is_empty() {
                            parts.push(txt.to_string());
                        }
                    }
                }
            } else if let Some(s) = block.as_str() {
                parts.push(s.to_string());
            }
        }
        let joined = parts.join("\n");
        return if joined.is_empty() {
            None
        } else {
            Some(joined)
        };
    }
    None
}

/// The readable reasoning summary (a list of `{type:"summary_text","text"}`).
fn summary_text(summary: &Value) -> Option<String> {
    if let Some(arr) = summary.as_array() {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect();
        let joined = parts.join("\n");
        return if joined.is_empty() {
            None
        } else {
            Some(joined)
        };
    }
    if let Some(s) = summary.as_str() {
        return if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        };
    }
    None
}

/// Compact a JSON value to a single-line string (truncated), for `args_json` /
/// `result_json`. `null` → None; strings pass through; everything else is minified.
fn compact(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(s) = value.as_str() {
        return Some(cap(s));
    }
    Some(cap(&value.to_string()))
}

/// Does a tool output look like an error (non-zero exit, `is_error`, `error`)?
fn looks_error(output: &Value) -> bool {
    if let Some(obj) = output.as_object() {
        if obj
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || obj.get("error").map(|v| !v.is_null()).unwrap_or(false)
        {
            return true;
        }
        if let Some(code) = obj.get("exit_code").and_then(|v| v.as_i64()) {
            if code != 0 {
                return true;
            }
        }
    }
    false
}

/// The paths touched by a `patch_apply_end` `changes` field (a map of path→change,
/// or a list of `{path|file|file_path}` / bare strings).
fn changed_paths(changes: &Value) -> Vec<String> {
    if let Some(obj) = changes.as_object() {
        return obj.keys().cloned().collect();
    }
    if let Some(arr) = changes.as_array() {
        let mut out = Vec::new();
        for c in arr {
            if let Some(o) = c.as_object() {
                if let Some(p) = ["path", "file", "file_path"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(|v| v.as_str()))
                {
                    out.push(p.to_string());
                }
            } else if let Some(s) = c.as_str() {
                out.push(s.to_string());
            }
        }
        return out;
    }
    Vec::new()
}

/// The basename of a cwd, used as the project scope (matches the reference).
fn project_id(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or("");
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

struct Emit {
    session_id: String,
    project_id: Option<String>,
    seq: usize,
    out: Vec<Envelope>,
}

impl Emit {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            project_id: None,
            seq: 0,
            out: Vec::new(),
        }
    }

    fn push(&mut self, ts: &str, reasoning: Option<Reasoning>, event: Event) {
        let event_id = format!("{}#{}", self.session_id, self.seq);
        self.seq += 1;
        self.out.push(Envelope {
            host_id: HOST_ID.to_string(),
            session_id: self.session_id.clone(),
            project_id: self.project_id.clone(),
            event_id: Some(event_id.clone()),
            ts: ts.to_string(),
            imported_from: Some(ImportedFrom {
                source_system: SOURCE_SYSTEM.to_string(),
                original_id: event_id,
                original_ts: ts.to_string(),
            }),
            reasoning,
            event,
        });
    }
}

const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Translate parsed Codex rollout records into canonical envelopes. `source_id`
/// keys the session so re-translation is stable (use [`source_id_for`] on the path).
pub fn translate(records: &[Value], source_id: &str) -> Vec<Envelope> {
    let mut e = Emit::new(source_id);
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut pending: Option<Reasoning> = None;
    let mut started = false;

    for rec in records {
        let Some(obj) = rec.as_object() else { continue };
        let rtype = obj.get("type").and_then(|v| v.as_str());
        let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
        let ts = obj
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or(EPOCH)
            .to_string();

        if rtype == Some("session_meta") {
            let cwd = payload.get("cwd").and_then(|v| v.as_str());
            e.project_id = cwd.and_then(project_id);
            e.push(
                &ts,
                None,
                Event::SessionStarted {
                    cwd: cwd.map(|s| s.to_string()),
                },
            );
            started = true;
            continue;
        }

        let ptype = payload.get("type").and_then(|v| v.as_str());
        let is_patch = rtype == Some("event_msg") && ptype == Some("patch_apply_end");
        if rtype != Some("response_item") && !is_patch {
            continue; // token_count, task_*, turn_context, ui mirrors, etc.
        }

        match ptype {
            Some("reasoning") => {
                pending = payload
                    .get("summary")
                    .and_then(summary_text)
                    .map(|text| Reasoning {
                        text: cap(&text),
                        source: ReasoningSource::Thinking,
                    });
            }
            Some("message") => {
                let role = payload.get("role").and_then(|v| v.as_str());
                let Some(text) = payload.get("content").and_then(text_of) else {
                    continue;
                };
                match role {
                    Some("user") => {
                        e.push(&ts, None, Event::UserPrompt { text });
                        pending = None;
                    }
                    Some("assistant") => {
                        e.push(&ts, pending.take(), Event::ModelResponse { text });
                    }
                    _ => {}
                }
            }
            Some("function_call") | Some("custom_tool_call") => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                if let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) {
                    call_names.insert(call_id.to_string(), name.clone());
                }
                let args = if ptype == Some("function_call") {
                    payload.get("arguments")
                } else {
                    payload.get("input")
                };
                let args_json = args.and_then(compact);
                e.push(
                    &ts,
                    None,
                    Event::ToolCallStarted {
                        tool: name,
                        args_json,
                    },
                );
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                let name = payload
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .and_then(|id| call_names.get(id).cloned())
                    .unwrap_or_else(|| "tool".to_string());
                let output = payload.get("output").cloned().unwrap_or(Value::Null);
                e.push(
                    &ts,
                    None,
                    Event::ToolCallFinished {
                        tool: name,
                        ok: !looks_error(&output),
                        result_json: compact(&output),
                    },
                );
            }
            _ if is_patch => {
                let ok = payload
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if let Some(changes) = payload.get("changes") {
                    for path in changed_paths(changes) {
                        e.push(&ts, None, Event::FileWritten { path });
                    }
                }
                let result = payload
                    .get("stdout")
                    .filter(|v| !v.is_null())
                    .or_else(|| payload.get("stderr"))
                    .and_then(compact);
                e.push(
                    &ts,
                    None,
                    Event::ToolCallFinished {
                        tool: "apply_patch".to_string(),
                        ok,
                        result_json: result,
                    },
                );
            }
            _ => {}
        }
    }

    if started {
        e.push(EPOCH, None, Event::SessionEnded);
    }
    e.out
}

/// Translate + ingest one `rollout-*.jsonl` file. Re-ingesting a grown transcript
/// is safe (CID dedup), so the capture loop simply re-reads on change.
pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let records: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let envelopes = translate(&records, &source_id_for(path));
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    Ok(ingest_envelopes(
        items,
        binding,
        base_dir,
        IngestReport::default(),
    ))
}

/// One incremental capture pass: discover every Codex rollout and re-ingest any
/// whose byte length changed since last pass (cheap stat; CID-dedup makes the
/// re-read idempotent). `lens` tracks per-file length across calls. Returns the
/// number of new events ingested.
pub fn capture_once<B: CoreBinding>(
    lens: &mut HashMap<PathBuf, u64>,
    binding: &B,
    base_dir: &Path,
) -> usize {
    let mut total = 0usize;
    for session in discovery::discover() {
        let Ok(meta) = std::fs::metadata(&session.file) else {
            continue;
        };
        let len = meta.len();
        if lens.get(&session.file).copied() == Some(len) {
            continue; // unchanged since last ingest
        }
        // Debounce: a live session file changes every few seconds while the harness
        // writes to it, and re-reading + re-ingesting the WHOLE file each time is the
        // main cost. Wait until it has been quiet briefly, then ingest once. We do NOT
        // update `lens` here, so the next pass reconsiders it once it settles.
        let busy = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|since| since < std::time::Duration::from_secs(10))
            .unwrap_or(false);
        if busy {
            continue;
        }
        if let Ok(report) = ingest_file(&session.file, binding, base_dir) {
            total += report.events;
        }
        lens.insert(session.file, len);
        // Yield between files so a backfill of many sessions can't monopolise the
        // CPU and starve the web server.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(lines: &[&str]) -> Vec<Event> {
        let records: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        translate(&records, "src")
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn translates_a_documented_session() {
        let evs = events(&[
            r#"{"type":"session_meta","timestamp":"2026-06-05T10:00:00Z","payload":{"cwd":"/home/me/proj","id":"abc"}}"#,
            r#"{"type":"response_item","timestamp":"2026-06-05T10:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a readme"}]}}"#,
            r#"{"type":"response_item","timestamp":"2026-06-05T10:00:02Z","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"plan it"}]}}"#,
            r#"{"type":"response_item","timestamp":"2026-06-05T10:00:03Z","payload":{"type":"message","role":"assistant","content":"done"}}"#,
            r#"{"type":"response_item","timestamp":"2026-06-05T10:00:04Z","payload":{"type":"function_call","name":"bash","call_id":"c1","arguments":"{\"cmd\":\"ls\"}"}}"#,
            r#"{"type":"response_item","timestamp":"2026-06-05T10:00:05Z","payload":{"type":"function_call_output","call_id":"c1","output":{"exit_code":0}}}"#,
        ]);
        assert!(
            matches!(&evs[0], Event::SessionStarted { cwd } if cwd.as_deref() == Some("/home/me/proj"))
        );
        assert!(matches!(&evs[1], Event::UserPrompt { text } if text == "add a readme"));
        assert!(matches!(&evs[2], Event::ModelResponse { text } if text == "done"));
        assert!(matches!(&evs[3], Event::ToolCallStarted { tool, .. } if tool == "bash"));
        assert!(
            matches!(&evs[4], Event::ToolCallFinished { tool, ok, .. } if tool == "bash" && *ok)
        );
        assert!(matches!(evs.last(), Some(Event::SessionEnded)));
    }

    #[test]
    fn reasoning_attaches_to_the_assistant_message() {
        let envs = translate(
            &[
                serde_json::from_str(r#"{"type":"session_meta","timestamp":"t","payload":{"cwd":"/p"}}"#).unwrap(),
                serde_json::from_str(r#"{"type":"response_item","timestamp":"t","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"why"}]}}"#).unwrap(),
                serde_json::from_str(r#"{"type":"response_item","timestamp":"t","payload":{"type":"message","role":"assistant","content":"ok"}}"#).unwrap(),
            ],
            "src",
        );
        let resp = envs
            .iter()
            .find(|e| matches!(e.event, Event::ModelResponse { .. }))
            .unwrap();
        assert!(
            matches!(&resp.reasoning, Some(r) if r.text == "why" && r.source == ReasoningSource::Thinking)
        );
    }

    #[test]
    fn stable_event_ids_so_reingest_dedupes() {
        let lines = [r#"{"type":"session_meta","timestamp":"t","payload":{}}"#];
        let recs: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let a: Vec<_> = translate(&recs, "src")
            .iter()
            .map(|e| e.event_id.clone())
            .collect();
        let b: Vec<_> = translate(&recs, "src")
            .iter()
            .map(|e| e.event_id.clone())
            .collect();
        assert_eq!(a, b);
    }
}
