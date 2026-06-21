//! Layer 3 — **VS Code Copilot** harness adapter.
//!
//! VS Code Copilot chat sessions are stored as JSON/JSONL mutation files.
//! We parse the initial snapshot (`kind: 0`) and map the requests/responses to Concierge envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "copilot";
const SOURCE_SYSTEM: &str = "copilot";
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
        format!("copilot-{session}")
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("copilot-{hash:016x}")
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

fn extract_message_text(message: &Value) -> String {
    if let Some(s) = message.as_str() {
        return s.trim().to_string();
    }
    if let Some(obj) = message.as_object() {
        for key in &["text", "prompt", "content", "value", "message"] {
            if let Some(v) = obj.get(*key) {
                if let Some(s) = v.as_str() {
                    return s.trim().to_string();
                }
            }
        }
    }
    String::new()
}

fn extract_response_text(response: &Value) -> String {
    if let Some(s) = response.as_str() {
        return s.trim().to_string();
    }
    if let Some(arr) = response.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            } else if let Some(obj) = item.as_object() {
                for key in &["value", "text", "message"] {
                    if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
                        parts.push(v.to_string());
                        break;
                    }
                }
            }
        }
        return parts.concat().trim().to_string();
    }
    if let Some(obj) = response.as_object() {
        for key in &["value", "text", "message"] {
            if let Some(v) = obj.get(*key) {
                if let Some(s) = v.as_str() {
                    return s.trim().to_string();
                }
            }
        }
    }
    String::new()
}

pub fn translate(session: &Value, source_id: &str) -> Vec<Envelope> {
    let mut e = Emit::new(source_id);

    let created = session
        .get("creationDate")
        .or_else(|| session.get("created"))
        .and_then(|v| v.as_f64())
        .map(iso_ts)
        .unwrap_or_else(|| EPOCH.to_string());

    e.push(&created, Event::SessionStarted { cwd: None });

    if let Some(requests) = session.get("requests").and_then(|v| v.as_array()) {
        for req in requests {
            let req_ts = req
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .map(iso_ts)
                .unwrap_or_else(|| created.clone());

            // Extract prompt
            if let Some(msg_val) = req.get("message") {
                let text = extract_message_text(msg_val);
                if !text.is_empty() {
                    e.push(&req_ts, Event::UserPrompt { text: cap(&text) });
                }
            }

            // Extract response
            if let Some(resp_val) = req.get("response") {
                let text = extract_response_text(resp_val);
                if !text.is_empty() {
                    e.push(&req_ts, Event::ModelResponse { text: cap(&text) });
                }
            }
        }
    }

    let final_ts = e.out.last().map(|env| env.ts.clone()).unwrap_or(created);
    e.push(&final_ts, Event::SessionEnded);
    e.out
}

pub fn read_copilot_session(path: &Path) -> std::io::Result<Vec<Envelope>> {
    let text = std::fs::read_to_string(path)?;
    let source_id = source_id_for(path);

    // Try parsing as single JSON object (legacy format)
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if v.is_object() && v.get("requests").is_some() {
            return Ok(translate(&v, &source_id));
        }
    }

    // Try parsing as JSONL mutation stream, looking for kind: 0 snapshot
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            if let Some(kind) = val.get("kind").and_then(|k| k.as_i64()) {
                if kind == 0 {
                    if let Some(v_obj) = val.get("v") {
                        return Ok(translate(v_obj, &source_id));
                    }
                }
            }
        }
    }

    // Fallback: empty session
    let mut e = Emit::new(&source_id);
    e.push(EPOCH, Event::SessionStarted { cwd: None });
    e.push(EPOCH, Event::SessionEnded);
    Ok(e.out)
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let envelopes = read_copilot_session(path)?;
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
    fn translates_copilot_jsonl() {
        let text = r#"{"kind":0,"v":{"sessionId":"s1","creationDate":1715340600000.0,"requests":[{"timestamp":1715340605000.0,"message":{"text":"explain rust"},"response":[{"value":"Rust is a language"}]}]}}"#;
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("s1.jsonl");
        std::fs::write(&file_path, text).unwrap();

        let envelopes = read_copilot_session(&file_path).unwrap();
        assert_eq!(envelopes.len(), 4); // Started + UserPrompt + ModelResponse + Ended
        assert!(matches!(envelopes[0].event, Event::SessionStarted { .. }));
        assert!(
            matches!(&envelopes[1].event, Event::UserPrompt { text } if text == "explain rust")
        );
        assert!(
            matches!(&envelopes[2].event, Event::ModelResponse { text } if text == "Rust is a language")
        );
        assert!(matches!(envelopes[3].event, Event::SessionEnded));
    }
}
