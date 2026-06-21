//! Layer 3 — **Cursor** harness adapter.
//!
//! Cursor stores chats inside a local SQLite database at `state.vscdb`.
//! We extract the raw key-value pairs, reconstruct conversation bubbles, and map
//! them to host-neutral envelopes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{Cid, CoreBinding, Envelope, Event, ImportedFrom};
use serde_json::Value;

pub mod discovery;

const HOST_ID: &str = "cursor";
const SOURCE_SYSTEM: &str = "cursor";
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
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("cursor-{hash:016x}")
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

pub fn translate_conversation(composer_id: &str, meta: &Value, bubbles: &[Value]) -> Vec<Envelope> {
    let project_id = meta
        .get("name")
        .or_else(|| meta.get("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut e = Emit::new(composer_id, project_id);

    let ts = meta
        .get("createdAt")
        .and_then(|v| v.as_f64())
        .map(iso_ts)
        .unwrap_or_else(|| EPOCH.to_string());

    e.push(&ts, Event::SessionStarted { cwd: None });

    for bubble in bubbles {
        let btype = bubble.get("type").and_then(|v| v.as_i64()).unwrap_or(0);

        // Handle tool call info
        if let Some(tool) = bubble.get("toolFormerData") {
            let name = tool
                .get("name")
                .or_else(|| tool.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();

            let args_json = tool
                .get("rawArgs")
                .or_else(|| tool.get("params"))
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else {
                        v.to_string()
                    }
                })
                .map(|s| cap(&s));

            let status = tool
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let ok = status != "error" && status != "failed";

            let result_json = tool
                .get("result")
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else {
                        v.to_string()
                    }
                })
                .map(|s| cap(&s));

            e.push(
                &ts,
                Event::ToolCallStarted {
                    tool: name.clone(),
                    args_json,
                },
            );
            e.push(
                &ts,
                Event::ToolCallFinished {
                    tool: name,
                    ok,
                    result_json,
                },
            );
        }

        // Handle text content
        let text = bubble
            .get("text")
            .or_else(|| bubble.get("richText"))
            .or_else(|| bubble.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .unwrap_or("");

        if !text.is_empty() {
            if btype == 1 {
                e.push(&ts, Event::UserPrompt { text: cap(text) });
            } else if btype == 2 {
                e.push(&ts, Event::ModelResponse { text: cap(text) });
            }
        }
    }

    e.push(&ts, Event::SessionEnded);
    e.out
}

fn read_kv_rows(conn: &rusqlite::Connection) -> HashMap<String, String> {
    let mut rows = HashMap::new();
    for table in &["cursorDiskKV", "ItemTable"] {
        let query = format!("SELECT key, value FROM {}", table);
        let Ok(mut stmt) = conn.prepare(&query) else {
            continue;
        };
        let Ok(mut iter) = stmt.query([]) else {
            continue;
        };
        while let Ok(Some(row)) = iter.next() {
            let Ok(k) = row.get::<_, String>(0) else {
                continue;
            };
            let Ok(v) = row.get::<_, String>(1) else {
                continue;
            };
            rows.insert(k, v);
        }
        if !rows.is_empty() {
            break; // Don't query ItemTable if cursorDiskKV worked
        }
    }
    rows
}

pub fn read_state_vscdb(path: &Path) -> std::io::Result<Vec<Envelope>> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(std::io::Error::other)?;

    let rows = read_kv_rows(&conn);
    let mut bubbles: HashMap<String, Value> = HashMap::new();
    let mut composers: HashMap<String, Value> = HashMap::new();

    for (k, v) in rows {
        if let Some(rest) = k.strip_prefix("bubbleId:") {
            if let Ok(val) = serde_json::from_str::<Value>(&v) {
                bubbles.insert(rest.to_string(), val);
            }
        } else if let Some(rest) = k.strip_prefix("composerData:") {
            if let Ok(mut val) = serde_json::from_str::<Value>(&v) {
                if val.is_object() {
                    let obj = val.as_object_mut().unwrap();
                    if !obj.contains_key("composerId") {
                        obj.insert("composerId".to_string(), Value::String(rest.to_string()));
                    }
                    composers.insert(rest.to_string(), val);
                }
            }
        }
    }

    let mut all_envelopes = Vec::new();
    for (cid, meta) in composers {
        // Collect bubbles ordered by fullConversationHeadersOnly or conversation
        let headers = meta
            .get("fullConversationHeadersOnly")
            .or_else(|| meta.get("conversation"))
            .and_then(|v| v.as_array());

        let mut ordered_bubbles = Vec::new();
        if let Some(arr) = headers {
            for h in arr {
                let bid = if h.is_object() {
                    h.get("bubbleId").and_then(|v| v.as_str())
                } else {
                    h.as_str()
                };
                if let Some(id) = bid {
                    let key = format!("{cid}:{id}");
                    if let Some(b) = bubbles.get(&key) {
                        let mut bubble_clone = b.clone();
                        // If type is defined in headers but not bubble itself, override it
                        if let Some(h_obj) = h.as_object() {
                            if let Some(h_type) = h_obj.get("type") {
                                if bubble_clone.get("type").is_none() {
                                    bubble_clone
                                        .as_object_mut()
                                        .unwrap()
                                        .insert("type".to_string(), h_type.clone());
                                }
                            }
                        }
                        ordered_bubbles.push(bubble_clone);
                    }
                }
            }
        } else {
            // Fallback: collect all matching bubbles
            let prefix = format!("{cid}:");
            let mut list: Vec<(&String, &Value)> = bubbles
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .collect();
            list.sort_by_key(|(k, _)| *k);
            for (_, b) in list {
                ordered_bubbles.push(b.clone());
            }
        }

        all_envelopes.extend(translate_conversation(&cid, &meta, &ordered_bubbles));
    }

    Ok(all_envelopes)
}

pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let envelopes = read_state_vscdb(path)?;
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
    use rusqlite::Connection;

    #[test]
    fn translates_cursor_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.vscdb");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            (
                "composerData:comp-1",
                r#"{"name":"Test Project","createdAt":1715340600000.0,"fullConversationHeadersOnly":[{"bubbleId":"b1","type":1},{"bubbleId":"b2","type":2}]}"#
            )
        ).unwrap();

        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            (
                "bubbleId:comp-1:b1",
                r#"{"type":1,"text":"Hello assistant"}"#,
            ),
        )
        .unwrap();

        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            (
                "bubbleId:comp-1:b2",
                r#"{"type":2,"text":"Hello user","toolFormerData":{"name":"run_cmd","params":"cargo check","status":"success","result":"ok"}}"#
            )
        ).unwrap();

        let envelopes = read_state_vscdb(&db_path).unwrap();
        assert_eq!(envelopes.len(), 6); // Started + UserPrompt + ToolCallStarted + ToolCallFinished + ModelResponse + Ended

        assert_eq!(envelopes[0].project_id.as_deref(), Some("Test Project"));
        assert!(
            matches!(&envelopes[1].event, Event::UserPrompt { text } if text == "Hello assistant")
        );
        assert!(
            matches!(&envelopes[2].event, Event::ToolCallStarted { tool, .. } if tool == "run_cmd")
        );
        assert!(
            matches!(&envelopes[3].event, Event::ToolCallFinished { tool, ok, .. } if tool == "run_cmd" && *ok)
        );
        assert!(
            matches!(&envelopes[4].event, Event::ModelResponse { text } if text == "Hello user")
        );
    }
}
