//! Layer 3 — **OpenDevin/OpenHands** harness adapter.
//!
//! OpenHands saves agent runs/trajectories as event log arrays in JSON/JSONL format.
//! We translate them into Concierge envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "openhands";
const SOURCE_SYSTEM: &str = "openhands";
const EPOCH: &str = "1970-01-01T00:00:00Z";
const MAX_FIELD: usize = 8192;

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

fn get_ts(ev: &Value) -> String {
    ev.get("timestamp")
        .and_then(|t| t.as_str())
        .map(|s| {
            if s.contains('T') {
                s.to_string()
            } else {
                EPOCH.to_string()
            }
        })
        .unwrap_or_else(|| EPOCH.to_string())
}

pub fn source_id_for(path: &Path) -> String {
    if let Some(session) = discovery::session_of(path) {
        format!("openhands-{session}")
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("openhands-{hash:016x}")
    }
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

    fn push(&mut self, ts: &str, event: Event) {
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
            reasoning: None,
            event,
        });
    }
}

fn translate_native(e: &mut Emit, ev: &Value) {
    let ts = get_ts(ev);
    let source = ev.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let args = ev.get("args").and_then(|v| v.as_object());

    if let Some(action) = ev.get("action").and_then(|v| v.as_str()) {
        if action == "message" {
            let text = args
                .and_then(|a| a.get("content"))
                .and_then(|v| v.as_str())
                .or_else(|| ev.get("message").and_then(|v| v.as_str()))
                .unwrap_or("")
                .trim();
            if !text.is_empty() {
                let etype = if source == "user" {
                    Event::UserPrompt { text: cap(text) }
                } else {
                    Event::ModelResponse { text: cap(text) }
                };
                e.push(&ts, etype);
            }
            return;
        }

        if matches!(
            action,
            "finish" | "system" | "change_agent_state" | "recall"
        ) {
            let thought = args
                .and_then(|a| a.get("thought").or_else(|| a.get("final_thought")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if !thought.is_empty() {
                e.push(&ts, Event::ModelResponse { text: cap(thought) });
            }
            return;
        }

        // Tool action
        let thought = args
            .and_then(|a| a.get("thought"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !thought.is_empty() {
            e.push(&ts, Event::ModelResponse { text: cap(thought) });
        }

        let args_json = ev.get("args").map(|v| v.to_string());
        e.push(
            &ts,
            Event::ToolCallStarted {
                tool: action.to_string(),
                args_json,
            },
        );

        if let Some(args_obj) = args {
            if let Some(path) = args_obj.get("path").and_then(|v| v.as_str()) {
                if matches!(action, "edit" | "write" | "str_replace_editor") {
                    e.push(
                        &ts,
                        Event::FileWritten {
                            path: path.to_string(),
                        },
                    );
                } else if action == "read" {
                    e.push(
                        &ts,
                        Event::FileRead {
                            path: path.to_string(),
                        },
                    );
                }
            }
        }
        return;
    }

    if let Some(observation) = ev.get("observation").and_then(|v| v.as_str()) {
        let is_error = observation == "error"
            || ev
                .get("extras")
                .and_then(|ex| ex.get("error"))
                .and_then(|err| err.as_bool())
                .unwrap_or(false);
        e.push(
            &ts,
            Event::ToolCallFinished {
                tool: observation.to_string(),
                ok: !is_error,
                result_json: None,
            },
        );
    }
}

fn translate_entries(e: &mut Emit, ev: &Value) {
    let ts = get_ts(ev);
    let etype = ev
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let actor = ev
        .get("actorType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let content = ev
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if etype == "message" || etype == "thought" {
        if !content.is_empty() {
            let etype = if actor == "user" {
                Event::UserPrompt { text: cap(content) }
            } else {
                Event::ModelResponse { text: cap(content) }
            };
            e.push(&ts, etype);
        }
    } else if etype == "command" {
        let cmd = ev.get("command").and_then(|v| v.as_str());
        let args_json = cmd.map(|c| serde_json::json!({ "command": c }).to_string());
        e.push(
            &ts,
            Event::ToolCallStarted {
                tool: "run".to_string(),
                args_json,
            },
        );
    }
}

pub fn translate(trajectory: &Value, source_id: &str) -> Vec<Envelope> {
    let mut e = Emit::new(source_id);

    let mut entries = None;
    let mut native = true;

    if let Some(arr) = trajectory.as_array() {
        entries = Some(arr);
    } else if let Some(obj) = trajectory.as_object() {
        if let Some(arr) = obj.get("entries").and_then(|v| v.as_array()) {
            entries = Some(arr);
            native = false;
        } else if let Some(arr) = obj.get("events").and_then(|v| v.as_array()) {
            entries = Some(arr);
        } else if let Some(arr) = obj.get("history").and_then(|v| v.as_array()) {
            entries = Some(arr);
        }
    }

    let Some(arr) = entries else {
        return e.out;
    };

    let first_ts = arr.first().map(get_ts).unwrap_or_else(|| EPOCH.to_string());

    e.push(&first_ts, Event::SessionStarted { cwd: None });

    for ev in arr {
        if native {
            translate_native(&mut e, ev);
        } else {
            translate_entries(&mut e, ev);
        }
    }

    let final_ts = e.out.last().map(|env| env.ts.clone()).unwrap_or(first_ts);
    e.push(&final_ts, Event::SessionEnded);
    e.out
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let data: Value = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let envelopes = translate(&data, &source_id_for(path));
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    Ok(ingest_envelopes(
        items,
        binding,
        base_dir,
        IngestReport::default(),
    ))
}

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
            continue;
        }
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
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    captured
}

pub fn seed_lens(lens: &mut HashMap<PathBuf, u64>) {
    for session in discovery::discover() {
        if let Ok(meta) = std::fs::metadata(&session.file) {
            lens.insert(session.file, meta.len());
        }
    }
}

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

    #[test]
    fn translates_native_trajectory() {
        let text = r#"[
            {"timestamp":"2025-03-07T17:45:20Z","source":"user","action":"message","args":{"content":"Hello"}},
            {"timestamp":"2025-03-07T17:45:22Z","source":"agent","action":"run","args":{"command":"ls","thought":"I will list files"}},
            {"timestamp":"2025-03-07T17:45:24Z","source":"environment","observation":"run","content":"Cargo.toml\nsrc"}
        ]"#;
        let v: Value = serde_json::from_str(text).unwrap();
        let evs = translate(&v, "session-1");
        assert_eq!(evs.len(), 6); // Started + UserPrompt + ModelResponse(thought) + ToolCallStarted + ToolCallFinished + Ended
        assert!(matches!(evs[0].event, Event::SessionStarted { .. }));
        assert!(matches!(&evs[1].event, Event::UserPrompt { text } if text == "Hello"));
        assert!(
            matches!(&evs[2].event, Event::ModelResponse { text } if text == "I will list files")
        );
        assert!(matches!(&evs[3].event, Event::ToolCallStarted { tool, .. } if tool == "run"));
        assert!(
            matches!(&evs[4].event, Event::ToolCallFinished { tool, ok, .. } if tool == "run" && *ok)
        );
        assert!(matches!(evs[5].event, Event::SessionEnded));
    }
}
