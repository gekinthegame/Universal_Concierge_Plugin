//! Layer 3 — **Cline** harness adapter.
//!
//! Cline stores task histories in `ui_messages.json` under its global task directories.
//! We translate these logs into canonical Concierge envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom};
use serde::{Deserialize, Serialize};

pub mod discovery;

const HOST_ID: &str = "cline";
const SOURCE_SYSTEM: &str = "cline";
const EPOCH: &str = "1970-01-01T00:00:00Z";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ClineMessage {
    pub ts: f64,
    #[serde(rename = "type")]
    pub kind: String,
    pub ask: Option<String>,
    pub say: Option<String>,
    pub text: Option<String>,
}

fn iso_ts(ms: f64) -> String {
    if ms <= 0.0 {
        return EPOCH.to_string();
    }
    let secs = (ms / 1000.0) as i64;
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

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

pub fn source_id_for(path: &Path) -> String {
    if let Some(session) = discovery::session_of(path) {
        format!("cline-{session}")
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("cline-{hash:016x}")
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

    fn push(&mut self, ts: &str, event: Event) {
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
            reasoning: None,
            event,
        });
    }
}

pub fn translate(
    records: &[ClineMessage],
    source_id: &str,
    project_id: Option<String>,
) -> Vec<Envelope> {
    let mut e = Emit::new(source_id, project_id);

    let initial_ts = records
        .first()
        .map(|r| iso_ts(r.ts))
        .unwrap_or_else(|| EPOCH.to_string());

    e.push(&initial_ts, Event::SessionStarted { cwd: None });

    for rec in records {
        let ts = iso_ts(rec.ts);
        let text = rec.text.clone().unwrap_or_default();

        if rec.kind == "say" {
            if let Some(say) = &rec.say {
                match say.as_str() {
                    "user_feedback" if !text.is_empty() => {
                        e.push(&ts, Event::UserPrompt { text });
                    }
                    "text" if !text.is_empty() => {
                        e.push(&ts, Event::ModelResponse { text });
                    }
                    "command" => {
                        e.push(
                            &ts,
                            Event::ToolCallStarted {
                                tool: "command".to_string(),
                                args_json: Some(text),
                            },
                        );
                    }
                    "command_output" => {
                        e.push(
                            &ts,
                            Event::ToolCallFinished {
                                tool: "command".to_string(),
                                ok: true,
                                result_json: Some(text),
                            },
                        );
                    }
                    "tool" => {
                        e.push(
                            &ts,
                            Event::ToolCallStarted {
                                tool: "tool".to_string(),
                                args_json: Some(text),
                            },
                        );
                    }
                    "error" => {
                        e.push(
                            &ts,
                            Event::ToolCallFinished {
                                tool: "tool".to_string(),
                                ok: false,
                                result_json: Some(text),
                            },
                        );
                    }
                    _ => {}
                }
            }
        } else if rec.kind == "ask" {
            if let Some(ask) = &rec.ask {
                match ask.as_str() {
                    "followup" if !text.is_empty() => {
                        e.push(&ts, Event::ModelResponse { text });
                    }
                    "command" => {
                        e.push(
                            &ts,
                            Event::ToolCallStarted {
                                tool: "command".to_string(),
                                args_json: Some(text),
                            },
                        );
                    }
                    "tool" => {
                        e.push(
                            &ts,
                            Event::ToolCallStarted {
                                tool: "tool".to_string(),
                                args_json: Some(text),
                            },
                        );
                    }
                    "completion_result" => {
                        e.push(
                            &ts,
                            Event::ModelResponse {
                                text: format!("Task Completed: {text}"),
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    let final_ts = records
        .last()
        .map(|r| iso_ts(r.ts))
        .unwrap_or_else(|| EPOCH.to_string());
    e.push(&final_ts, Event::SessionEnded);
    e.out
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let records: Vec<ClineMessage> = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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
    fn translates_cline_events() {
        let text = r#"[
            {"ts":1715340600000.0,"type":"say","say":"text","text":"Hello, task starting!"},
            {"ts":1715340605000.0,"type":"say","say":"user_feedback","text":"Please do some work"},
            {"ts":1715340610000.0,"type":"ask","ask":"command","text":"cargo build"},
            {"ts":1715340615000.0,"type":"say","say":"command_output","text":"Finished successfully"}
        ]"#;
        let msgs: Vec<ClineMessage> = serde_json::from_str(text).unwrap();
        let evs = translate(&msgs, "task-123", None);
        assert_eq!(evs.len(), 6); // Started + ModelResponse + UserPrompt + ToolCallStarted + ToolCallFinished + Ended
        assert!(matches!(evs[0].event, Event::SessionStarted { .. }));
        assert!(
            matches!(&evs[1].event, Event::ModelResponse { text } if text == "Hello, task starting!")
        );
        assert!(
            matches!(&evs[2].event, Event::UserPrompt { text } if text == "Please do some work")
        );
        assert!(matches!(&evs[3].event, Event::ToolCallStarted { tool, .. } if tool == "command"));
        assert!(
            matches!(&evs[4].event, Event::ToolCallFinished { tool, ok, .. } if tool == "command" && *ok)
        );
        assert!(matches!(evs[5].event, Event::SessionEnded));
    }
}
