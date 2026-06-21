//! Layer 3 — **Continue** harness adapter.
//!
//! Continue stores each conversation as `~/.continue/sessions/<id>.json` (a single
//! JSON object with a `history` array). This crate discovers those files
//! ([`discovery`]) and translates them into the host-neutral Concierge
//! [`Envelope`] stream, then reuses the JSONL ingest path ([`ingest_envelopes`])
//! for the IPLD writes. Observe-only (Tier 4): reads Continue's own files, never
//! proxies, never touches keys.
//!
//! Mapping (ported from `adapters/continue/translate.py`):
//! - `workspaceDirectory` → `SessionStarted { cwd }`
//! - `title` → `MemoryRecorded`
//! - message role `user` → `UserPrompt`
//! - message role `assistant` → `ModelResponse` (+ reasoning from a paired `thinking`)
//! - message role `thinking` → folded into the next assistant's reasoning
//! - `toolCallStates[]` → `ToolCallStarted` + `ToolCallFinished`
//!
//! Ingest is content-addressed (dedup by CID), so re-reading a session on every
//! capture pass is safe and idempotent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom, Reasoning, ReasoningSource};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "continue";
const SOURCE_SYSTEM: &str = "continue";
const MAX_FIELD: usize = 8192;
const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Stable, path-derived id so a session's nodes get the same CIDs on re-read.
pub fn source_id_for(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("continue-{hash:016x}")
}

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

/// Continue `content` is a string OR a list of MessageParts (`{type,text}`).
fn content_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        let t = s.trim();
        return if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        };
    }
    if let Some(arr) = content.as_array() {
        let mut texts = Vec::new();
        for part in arr {
            if let Some(s) = part.as_str() {
                texts.push(s.to_string());
            } else if let Some(txt) = part.get("text").and_then(|v| v.as_str()) {
                texts.push(txt.to_string());
            }
        }
        let joined = texts.concat();
        let trimmed = joined.trim();
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    None
}

/// Normalize Continue's `dateCreated` (epoch seconds or millis, or an ISO string)
/// to an RFC-3339 stamp. No date dependency — pure arithmetic on the civil date.
fn iso_ts(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return if s.contains('T') {
            s.to_string()
        } else {
            EPOCH.to_string()
        };
    }
    let secs = if let Some(n) = value.as_f64() {
        let n = if n > 1e12 { n / 1000.0 } else { n };
        n as i64
    } else {
        return EPOCH.to_string();
    };
    if secs <= 0 {
        return EPOCH.to_string();
    }
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Convert Unix seconds (UTC) to civil (y, m, d, H, M, S) — Howard Hinnant's
/// `civil_from_days` algorithm, no external date crate.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hh, mm, ss)
}

struct Emit {
    session_id: String,
    seq: usize,
    out: Vec<Envelope>,
}

impl Emit {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
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
        });
    }
}

/// Pull the `{name, args_json, ok}` of every tool call attached to a history item.
fn tool_calls(item: &Value) -> Vec<(String, Option<String>, bool)> {
    let mut calls = Vec::new();
    let Some(states) = item.get("toolCallStates").and_then(|v| v.as_array()) else {
        return calls;
    };
    for state in states {
        let fns = state
            .get("toolCall")
            .and_then(|c| c.get("function"))
            .cloned()
            .unwrap_or(Value::Null);
        let Some(name) = fns.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let args_json = match fns.get("arguments") {
            Some(Value::String(s)) => Some(cap(s)),
            Some(v) if !v.is_null() => Some(cap(&v.to_string())),
            _ => None,
        };
        let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let ok = !matches!(status, "errored" | "canceled");
        calls.push((name.to_string(), args_json, ok));
    }
    calls
}

/// Translate one Continue session object into canonical envelopes. `source_id`
/// keys the session so re-translation is stable.
pub fn translate(session: &Value, source_id: &str) -> Vec<Envelope> {
    let mut e = Emit::new(source_id);
    let Some(obj) = session.as_object() else {
        return e.out;
    };
    let cwd = obj.get("workspaceDirectory").and_then(|v| v.as_str());
    let ts = iso_ts(
        obj.get("dateCreated")
            .or_else(|| obj.get("createdAt"))
            .unwrap_or(&Value::Null),
    );

    e.push(
        &ts,
        None,
        Event::SessionStarted {
            cwd: cwd.map(|s| s.to_string()),
        },
    );
    if let Some(title) = obj.get("title").and_then(|v| v.as_str()) {
        if !matches!(title, "New Session" | "New Chat") && !title.is_empty() {
            e.push(
                &ts,
                None,
                Event::MemoryRecorded {
                    text: format!("Continue session: {title}"),
                },
            );
        }
    }

    let empty = Vec::new();
    let history = obj
        .get("history")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let mut pending: Option<Reasoning> = None;
    for item in history {
        let message = item
            .get("message")
            .filter(|m| m.is_object())
            .unwrap_or(item);
        let role = message.get("role").and_then(|v| v.as_str());
        let text = message.get("content").and_then(content_text);

        match role {
            Some("thinking") => {
                if let Some(t) = text {
                    pending = Some(Reasoning {
                        text: cap(&t),
                        source: ReasoningSource::Thinking,
                    });
                }
                continue;
            }
            Some("user") => {
                if let Some(t) = text {
                    e.push(&ts, None, Event::UserPrompt { text: cap(&t) });
                }
            }
            Some("assistant") => {
                if let Some(t) = text {
                    e.push(&ts, pending.take(), Event::ModelResponse { text: cap(&t) });
                } else {
                    pending = None;
                }
            }
            _ => {} // system / tool: skipped (tool *calls* handled below)
        }

        for (name, args_json, ok) in tool_calls(item) {
            e.push(
                &ts,
                None,
                Event::ToolCallStarted {
                    tool: name.clone(),
                    args_json,
                },
            );
            e.push(
                &ts,
                None,
                Event::ToolCallFinished {
                    tool: name,
                    ok,
                    result_json: None,
                },
            );
        }
    }

    e.push(&ts, None, Event::SessionEnded);
    e.out
}

/// A session file may be one session object, an array, or `{sessions:[…]}`.
fn iter_sessions(data: &Value) -> Vec<Value> {
    if let Some(obj) = data.as_object() {
        if let Some(arr) = obj.get("sessions").and_then(|v| v.as_array()) {
            return arr.iter().filter(|s| s.is_object()).cloned().collect();
        }
        if obj.contains_key("history") || obj.contains_key("sessionId") {
            return vec![data.clone()];
        }
        return Vec::new();
    }
    if let Some(arr) = data.as_array() {
        return arr.iter().filter(|s| s.is_object()).cloned().collect();
    }
    Vec::new()
}

/// Translate + ingest one Continue session file.
pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let data: Value = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let source = source_id_for(path);
    let mut report = IngestReport::default();
    for (n, session) in iter_sessions(&data).into_iter().enumerate() {
        // Multiple sessions in one file get distinct ids so their CIDs don't collide.
        let sid = if n == 0 {
            source.clone()
        } else {
            format!("{source}-{n}")
        };
        let envelopes = translate(&session, &sid);
        let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
        report = ingest_envelopes(items, binding, base_dir, report);
    }
    Ok(report)
}

/// One incremental capture pass: re-ingest changed Continue sessions and return
/// the CIDs written by this pass.
pub fn capture_once<B: CoreBinding>(
    lens: &mut HashMap<PathBuf, u64>,
    binding: &B,
    base_dir: &Path,
) -> Vec<Cid> {
    let mut captured = Vec::new();
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
            captured.extend(report.record_cids);
        }
        lens.insert(session.file, len);
        // Yield between files so a backfill of many sessions can't monopolise the
        // CPU and starve the web server.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    captured
}

/// Record the current size of every discovered session WITHOUT ingesting, so
/// attaching/loading does not backfill history — only growth from here is captured.
pub fn seed_lens(lens: &mut HashMap<PathBuf, u64>) {
    for session in discovery::discover() {
        if let Ok(meta) = std::fs::metadata(&session.file) {
            lens.insert(session.file, meta.len());
        }
    }
}

/// Full historical backfill: ingest EVERY discovered session (the manual "Ingest"
/// action). Idempotent via CID dedup; yields between files so a large backfill
/// never starves the web server. Returns the number of events ingested.
pub fn ingest_all<B: CoreBinding>(binding: &B, base_dir: &Path) -> usize {
    let mut total = 0usize;
    for session in discovery::discover() {
        if let Ok(report) = ingest_file(&session.file, binding, base_dir) {
            total += report.events;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(json: &str) -> Vec<Event> {
        let v: Value = serde_json::from_str(json).unwrap();
        translate(&v, "src").into_iter().map(|e| e.event).collect()
    }

    #[test]
    fn translates_a_documented_session() {
        let evs = events(
            r#"{
              "sessionId":"s1","title":"Refactor","workspaceDirectory":"/home/me/proj",
              "history":[
                {"message":{"role":"user","content":"hello"}},
                {"message":{"role":"thinking","content":"i will reply"}},
                {"message":{"role":"assistant","content":"hi there"},
                 "toolCallStates":[{"toolCall":{"function":{"name":"read","arguments":"{\"p\":\"a\"}"}},"status":"done"}]}
              ]
            }"#,
        );
        assert!(
            matches!(&evs[0], Event::SessionStarted { cwd } if cwd.as_deref() == Some("/home/me/proj"))
        );
        assert!(matches!(&evs[1], Event::MemoryRecorded { text } if text.contains("Refactor")));
        assert!(matches!(&evs[2], Event::UserPrompt { text } if text == "hello"));
        assert!(matches!(&evs[3], Event::ModelResponse { text } if text == "hi there"));
        assert!(matches!(&evs[4], Event::ToolCallStarted { tool, .. } if tool == "read"));
        assert!(
            matches!(&evs[5], Event::ToolCallFinished { tool, ok, .. } if tool == "read" && *ok)
        );
        assert!(matches!(evs.last(), Some(Event::SessionEnded)));
    }

    #[test]
    fn thinking_folds_into_assistant_reasoning() {
        let v: Value = serde_json::from_str(
            r#"{"history":[{"message":{"role":"thinking","content":"why"}},{"message":{"role":"assistant","content":"ok"}}]}"#,
        )
        .unwrap();
        let envs = translate(&v, "src");
        let resp = envs
            .iter()
            .find(|e| matches!(e.event, Event::ModelResponse { .. }))
            .unwrap();
        assert!(
            matches!(&resp.reasoning, Some(r) if r.text == "why" && r.source == ReasoningSource::Thinking)
        );
    }
}
