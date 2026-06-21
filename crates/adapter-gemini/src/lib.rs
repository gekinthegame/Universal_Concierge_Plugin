//! Layer 3 — **Gemini CLI** harness adapter.
//!
//! Gemini CLI records each chat to `~/.gemini/tmp/<project>/chats/session-*.jsonl`,
//! one JSON record per line. This crate discovers those files ([`discovery`]) and
//! translates them into the host-neutral Concierge [`Envelope`] stream, then
//! reuses the JSONL ingest path ([`ingest_envelopes`]) for the IPLD writes.
//! Observe-only (Tier 4): reads Gemini's own files, never proxies, never touches keys.
//!
//! Mapping (ported from `adapters/gemini/translate.py`):
//! - header `{sessionId, startTime, …}` → `SessionStarted`
//! - `type:"user"`, `content:[{text}]` → `UserPrompt`
//! - `type:"gemini"`, `content`, `thoughts`, `toolCalls` → `ToolCallStarted` +
//!   `ToolCallFinished` per call, then `ModelResponse` (+ reasoning from thoughts)
//! - `{"$set":…}` deltas and `type:"info"` → ignored
//!
//! Ingest is content-addressed (dedup by CID), so re-reading a growing transcript
//! on every capture pass is safe and idempotent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom, Reasoning, ReasoningSource};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "gemini";
const SOURCE_SYSTEM: &str = "gemini";
const MAX_FIELD: usize = 8192;
const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Stable, path-derived id so a session's nodes get the same CIDs on re-read.
pub fn source_id_for(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("gemini-{hash:016x}")
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

/// User content is a list of `{text}` parts; gemini content is a plain string.
fn parts_text(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        };
    }
    if let Some(arr) = content.as_array() {
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
    None
}

/// Gemini's `thoughts` (list of `{subject, description}`) → reasoning summary.
fn reasoning_of(thoughts: &Value) -> Option<Reasoning> {
    let arr = thoughts.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for t in arr {
        if let Some(o) = t.as_object() {
            let subject = o
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let desc = o
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let line = if !subject.is_empty() && !desc.is_empty() {
                format!("{subject}: {desc}")
            } else if !subject.is_empty() {
                subject.to_string()
            } else {
                desc.to_string()
            };
            if !line.is_empty() {
                lines.push(line);
            }
        }
    }
    let text = lines.join("\n");
    if text.is_empty() {
        None
    } else {
        Some(Reasoning {
            text: cap(&text),
            source: ReasoningSource::Thinking,
        })
    }
}

fn compact(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(s) = value.as_str() {
        return Some(cap(s));
    }
    Some(cap(&value.to_string()))
}

struct Emit {
    session_id: String,
    project_id: Option<String>,
    seq: usize,
    out: Vec<Envelope>,
}

impl Emit {
    fn new(session_id: &str, project_id: Option<String>) -> Self {
        Self {
            session_id: session_id.to_string(),
            project_id,
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

/// Translate parsed Gemini records into canonical envelopes. `source_id` keys the
/// session so re-translation is stable; `project_id` scopes names when known.
pub fn translate(records: &[Value], source_id: &str, project_id: Option<String>) -> Vec<Envelope> {
    let mut e = Emit::new(source_id, project_id);
    let mut started = false;

    for rec in records {
        let Some(obj) = rec.as_object() else { continue };
        if obj.contains_key("$set") {
            continue; // streaming state delta
        }
        let rtype = obj.get("type").and_then(|v| v.as_str());
        let ts = obj
            .get("timestamp")
            .and_then(|v| v.as_str())
            .or_else(|| obj.get("startTime").and_then(|v| v.as_str()))
            .unwrap_or(EPOCH)
            .to_string();

        // The header record has no `type` but carries `sessionId`.
        if rtype.is_none() && obj.contains_key("sessionId") {
            e.push(&ts, None, Event::SessionStarted { cwd: None });
            started = true;
            continue;
        }

        match rtype {
            Some("user") => {
                if let Some(text) = obj.get("content").and_then(parts_text) {
                    e.push(&ts, None, Event::UserPrompt { text });
                }
            }
            Some("gemini") => {
                if let Some(calls) = obj.get("toolCalls").and_then(|v| v.as_array()) {
                    for call in calls {
                        let Some(c) = call.as_object() else { continue };
                        let name = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let cts = c
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| ts.clone());
                        let args_json = c.get("args").and_then(compact);
                        e.push(
                            &cts,
                            None,
                            Event::ToolCallStarted {
                                tool: name.clone(),
                                args_json,
                            },
                        );
                        let status = c
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let ok = !matches!(status.as_str(), "error" | "failed" | "cancelled");
                        let result = c
                            .get("resultDisplay")
                            .filter(|v| !v.is_null())
                            .or_else(|| c.get("result"))
                            .and_then(compact);
                        e.push(
                            &cts,
                            None,
                            Event::ToolCallFinished {
                                tool: name,
                                ok,
                                result_json: result,
                            },
                        );
                    }
                }
                if let Some(text) = obj.get("content").and_then(parts_text) {
                    let reasoning = obj.get("thoughts").and_then(reasoning_of);
                    e.push(&ts, reasoning, Event::ModelResponse { text });
                }
            }
            _ => {} // "info" and anything else: ignored
        }
    }

    if started {
        e.push(EPOCH, None, Event::SessionEnded);
    }
    e.out
}

/// Translate + ingest one `session-*.jsonl` file.
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
    let envelopes = translate(&records, &source_id_for(path), discovery::project_of(path));
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    Ok(ingest_envelopes(
        items,
        binding,
        base_dir,
        IngestReport::default(),
    ))
}

/// One incremental capture pass: re-ingest changed Gemini sessions and return
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

    fn events(lines: &[&str]) -> Vec<Event> {
        let records: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        translate(&records, "src", None)
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn translates_a_documented_session() {
        let evs = events(&[
            r#"{"sessionId":"s1","startTime":"2026-06-05T10:00:00Z"}"#,
            r#"{"type":"user","timestamp":"2026-06-05T10:00:01Z","content":[{"text":"hi"}]}"#,
            r#"{"$set":{"streaming":true}}"#,
            r#"{"type":"gemini","timestamp":"2026-06-05T10:00:03Z","content":"hello","thoughts":[{"subject":"greet","description":"be friendly"}],"toolCalls":[{"name":"read_file","args":{"p":"a.txt"},"status":"success","result":"ok"}]}"#,
            r#"{"type":"info","content":"note"}"#,
        ]);
        assert!(matches!(evs[0], Event::SessionStarted { .. }));
        assert!(matches!(&evs[1], Event::UserPrompt { text } if text == "hi"));
        assert!(matches!(&evs[2], Event::ToolCallStarted { tool, .. } if tool == "read_file"));
        assert!(
            matches!(&evs[3], Event::ToolCallFinished { tool, ok, .. } if tool == "read_file" && *ok)
        );
        assert!(matches!(&evs[4], Event::ModelResponse { text } if text == "hello"));
        assert!(matches!(evs.last(), Some(Event::SessionEnded)));
    }

    #[test]
    fn thoughts_become_reasoning() {
        let envs = translate(
            &[
                serde_json::from_str(r#"{"sessionId":"s1"}"#).unwrap(),
                serde_json::from_str(r#"{"type":"gemini","content":"x","thoughts":[{"subject":"a","description":"b"}]}"#).unwrap(),
            ],
            "src",
            None,
        );
        let resp = envs
            .iter()
            .find(|e| matches!(e.event, Event::ModelResponse { .. }))
            .unwrap();
        assert!(
            matches!(&resp.reasoning, Some(r) if r.text == "a: b" && r.source == ReasoningSource::Thinking)
        );
    }
}
