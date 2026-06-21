//! Layer 3 — **OpenClaw** harness adapter.
//!
//! OpenClaw records each session as a JSONL transcript under
//! `~/.openclaw/agents/<agentId>/sessions/<sessionId>.jsonl`.
//!
//! We translate these logs into canonical Concierge envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom};
use serde::{Deserialize, Serialize};

pub mod discovery;

const HOST_ID: &str = "openclaw";
const SOURCE_SYSTEM: &str = "openclaw";
const EPOCH: &str = "1970-01-01T00:00:00Z";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OpenClawLine {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub message: Option<OpenClawMessage>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OpenClawMessage {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: Option<String>,
    #[serde(rename = "toolName")]
    pub tool_name: Option<String>,
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
}

/// A helper to flatten any `serde_json::Value` (representing message content) into a String.
fn value_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let texts: Vec<String> = items
                .iter()
                .filter_map(|item| {
                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                        Some(t.to_string())
                    } else if item.is_string() {
                        Some(item.as_str().unwrap().to_string())
                    } else {
                        None
                    }
                })
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

pub fn source_id_for(path: &Path) -> String {
    if let Some(session) = discovery::session_of(path) {
        format!("openclaw-{session}")
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("openclaw-{hash:016x}")
    }
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

    fn push(&mut self, ts: &str, original_id: Option<String>, event: Event) {
        let seq_id = format!("{}#{}", self.session_id, self.seq);
        self.seq += 1;
        let event_id = original_id.unwrap_or(seq_id.clone());
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
            reasoning: None,
            event,
        });
    }
}

pub fn translate(
    records: &[OpenClawLine],
    source_id: &str,
    project_id: Option<String>,
) -> Vec<Envelope> {
    let mut e = Emit::new(source_id, project_id);

    // Find cwd from first session line
    let cwd = records.iter().find_map(|r| {
        if r.kind == "session" {
            r.cwd.clone()
        } else {
            None
        }
    });

    let initial_ts = records
        .first()
        .and_then(|r| r.timestamp.as_deref())
        .unwrap_or(EPOCH);

    e.push(initial_ts, None, Event::SessionStarted { cwd });

    for rec in records {
        let ts = rec.timestamp.as_deref().unwrap_or(EPOCH);
        let orig_id = rec.id.clone();

        if rec.kind == "message" {
            if let Some(msg) = &rec.message {
                match msg.role.as_str() {
                    "user" => {
                        let text = value_to_text(&msg.content);
                        if !text.is_empty() {
                            e.push(ts, orig_id, Event::UserPrompt { text });
                        }
                    }
                    "assistant" => {
                        // Extract text
                        let text = if let serde_json::Value::Array(blocks) = &msg.content {
                            let text_parts: Vec<String> = blocks
                                .iter()
                                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                                .filter_map(|b| {
                                    b.get("text").and_then(|t| t.as_str()).map(String::from)
                                })
                                .collect();
                            if text_parts.is_empty() {
                                String::new()
                            } else {
                                text_parts.join("\n")
                            }
                        } else {
                            value_to_text(&msg.content)
                        };

                        if !text.is_empty() {
                            e.push(ts, orig_id.clone(), Event::ModelResponse { text });
                        }

                        // Extract tool calls
                        if let serde_json::Value::Array(blocks) = &msg.content {
                            for b in blocks {
                                if b.get("type").and_then(|t| t.as_str()) == Some("toolCall") {
                                    if let (Some(name), Some(args)) =
                                        (b.get("name").and_then(|n| n.as_str()), b.get("arguments"))
                                    {
                                        let tool_call_id = b
                                            .get("id")
                                            .and_then(|id| id.as_str())
                                            .map(|id| id.to_string());
                                        e.push(
                                            ts,
                                            tool_call_id,
                                            Event::ToolCallStarted {
                                                tool: name.to_string(),
                                                args_json: Some(args.to_string()),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    "toolResult" => {
                        let tool = msg.tool_name.clone().unwrap_or_else(|| "tool".to_string());
                        let ok = !msg.is_error.unwrap_or(false);
                        let result_json = Some(value_to_text(&msg.content));
                        e.push(
                            ts,
                            orig_id,
                            Event::ToolCallFinished {
                                tool,
                                ok,
                                result_json,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    e.push(EPOCH, None, Event::SessionEnded);
    e.out
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let records: Vec<OpenClawLine> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<OpenClawLine>(l).ok())
        .collect();
    let envelopes = translate(&records, &source_id_for(path), None);
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
        // Debounce: wait until it has settled
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

    fn events(lines: &[&str]) -> Vec<Event> {
        let records: Vec<OpenClawLine> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        translate(&records, "session-123", None)
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn translates_openclaw_session() {
        let evs = events(&[
            r##"{"type":"session","version":3,"id":"29468113-f927-4030-859b-34d7020ba6de","timestamp":"2026-06-19T23:42:01.534Z","cwd":"/Users/quinonesfam/.openclaw/workspace"}"##,
            r##"{"type":"message","id":"d1353534","parentId":"26423184","timestamp":"2026-06-19T23:42:01.642Z","message":{"role":"user","content":"Wake up, my friend!"}}"##,
            r##"{"type":"message","id":"fef3495b","parentId":"e2ce973d","timestamp":"2026-06-20T00:52:04.371Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_evfrjypm","name":"read","arguments":{"path":"/Users/quinonesfam/.openclaw/workspace/UCP/README.md"}}]}}"##,
            r##"{"type":"message","id":"08623904","parentId":"fef3495b","timestamp":"2026-06-20T00:52:04.585Z","message":{"role":"toolResult","toolCallId":"call_evfrjypm","toolName":"read","content":[{"type":"text","text":"# UCP"}]}}"##,
        ]);
        assert_eq!(evs.len(), 5); // Started + UserPrompt + ToolCallStarted + ToolCallFinished + SessionEnded
        assert!(matches!(evs[0], Event::SessionStarted { cwd: Some(_) }));
        assert!(matches!(&evs[1], Event::UserPrompt { text } if text == "Wake up, my friend!"));
        assert!(matches!(&evs[2], Event::ToolCallStarted { tool, .. } if tool == "read"));
        assert!(
            matches!(&evs[3], Event::ToolCallFinished { tool, ok, .. } if tool == "read" && *ok)
        );
        assert!(matches!(evs.last(), Some(Event::SessionEnded)));
    }
}
