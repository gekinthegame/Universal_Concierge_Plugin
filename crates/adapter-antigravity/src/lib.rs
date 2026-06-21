//! Layer 3 — **Antigravity** harness adapter.
//!
//! Antigravity records each session as a JSONL transcript under
//! `~/.gemini/antigravity-ide/brain/<session-uuid>/.system_generated/logs/transcript.jsonl`.
//!
//! We translate these logs into canonical Concierge envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom, Reasoning};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "antigravity";
const SOURCE_SYSTEM: &str = "antigravity";
const EPOCH: &str = "1970-01-01T00:00:00Z";

#[derive(Debug, Deserialize, Serialize)]
pub struct StepLine {
    pub step_index: usize,
    pub source: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
    #[serde(default, rename = "toolAction")]
    pub tool_action: Option<String>,
    #[serde(default, rename = "toolSummary")]
    pub tool_summary: Option<String>,
}

fn map_step_type_to_tool_name(step_type: &str) -> Option<&'static str> {
    match step_type {
        "LIST_DIRECTORY" => Some("list_dir"),
        "VIEW_FILE" => Some("view_file"),
        "RUN_COMMAND" => Some("run_command"),
        "SEARCH_WEB" => Some("search_web"),
        "REPLACE_FILE_CONTENT" => Some("replace_file_content"),
        "WRITE_TO_FILE" => Some("write_to_file"),
        "MULTI_REPLACE_FILE_CONTENT" => Some("multi_replace_file_content"),
        "BROWSER_SUBAGENT" => Some("browser_subagent"),
        "GENERATE_IMAGE" => Some("generate_image"),
        "READ_URL_CONTENT" => Some("read_url_content"),
        "ASK_QUESTION" => Some("ask_question"),
        "ASK_PERMISSION" => Some("ask_permission"),
        "SCHEDULE" => Some("schedule"),
        "MANAGE_TASK" => Some("manage_task"),
        "LIST_PERMISSIONS" => Some("list_permissions"),
        _ => None,
    }
}

fn clean_user_prompt(content: &str) -> String {
    if let Some(start) = content.find("<USER_REQUEST>") {
        if let Some(end) = content.find("</USER_REQUEST>") {
            let start_idx = start + "<USER_REQUEST>".len();
            if end > start_idx {
                return content[start_idx..end].trim().to_string();
            }
        }
    }
    content.to_string()
}

pub fn source_id_for(path: &Path) -> String {
    if let Some(session) = discovery::session_of(path) {
        format!("antigravity-{session}")
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("antigravity-{hash:016x}")
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

pub fn translate(
    records: &[StepLine],
    source_id: &str,
    project_id: Option<String>,
) -> Vec<Envelope> {
    let mut e = Emit::new(source_id, project_id);
    let mut started = false;

    // First record's timestamp opens the session
    if let Some(first) = records.first() {
        let ts = first.created_at.as_deref().unwrap_or(EPOCH);
        e.push(ts, None, Event::SessionStarted { cwd: None });
        started = true;
    }

    for rec in records {
        let ts = rec.created_at.as_deref().unwrap_or(EPOCH);

        match rec.kind.as_str() {
            "USER_INPUT" => {
                if let Some(content) = &rec.content {
                    let cleaned = clean_user_prompt(content);
                    e.push(ts, None, Event::UserPrompt { text: cleaned });
                }
            }
            "PLANNER_RESPONSE" => {
                if let Some(calls) = &rec.tool_calls {
                    for call in calls {
                        e.push(
                            ts,
                            None,
                            Event::ToolCallStarted {
                                tool: call.name.clone(),
                                args_json: Some(call.args.to_string()),
                            },
                        );
                    }
                }
                if let Some(content) = &rec.content {
                    if !content.trim().is_empty() {
                        e.push(
                            ts,
                            None,
                            Event::ModelResponse {
                                text: content.clone(),
                            },
                        );
                    }
                }
            }
            other => {
                if let Some(tool_name) = map_step_type_to_tool_name(other) {
                    let ok = rec.status.as_deref() == Some("DONE");
                    let result_json = rec.content.clone();
                    e.push(
                        ts,
                        None,
                        Event::ToolCallFinished {
                            tool: tool_name.to_string(),
                            ok,
                            result_json,
                        },
                    );
                }
            }
        }
    }

    if started {
        e.push(EPOCH, None, Event::SessionEnded);
    }
    e.out
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let records: Vec<StepLine> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<StepLine>(l).ok())
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
            continue; // unchanged since last ingest
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
        let records: Vec<StepLine> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        translate(&records, "src", None)
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn translates_antigravity_session() {
        let evs = events(&[
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-06-20T12:38:39Z","content":"<USER_REQUEST>\nWhat do you think?\n</USER_REQUEST>"}"#,
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-06-20T12:38:40Z","content":"Let's check","tool_calls":[{"name":"list_dir","args":{"DirectoryPath":"/Users"},"toolAction":"Listing","toolSummary":"List"}]}"#,
            r#"{"step_index":4,"source":"MODEL","type":"LIST_DIRECTORY","status":"DONE","created_at":"2026-06-20T12:38:41Z","content":"file1"}"#,
        ]);
        assert!(matches!(evs[0], Event::SessionStarted { .. }));
        assert!(matches!(&evs[1], Event::UserPrompt { text } if text == "What do you think?"));
        assert!(matches!(&evs[2], Event::ToolCallStarted { tool, .. } if tool == "list_dir"));
        assert!(matches!(&evs[3], Event::ModelResponse { text } if text == "Let's check"));
        assert!(
            matches!(&evs[4], Event::ToolCallFinished { tool, ok, .. } if tool == "list_dir" && *ok)
        );
        assert!(matches!(evs.last(), Some(Event::SessionEnded)));
    }
}
