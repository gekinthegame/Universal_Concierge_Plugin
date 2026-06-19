//! Layer 3 — **Aider** harness adapter.
//!
//! Aider appends a Markdown chat transcript to `.aider.chat.history.md`. This
//! crate discovers those files ([`discovery`]) and translates them into the
//! host-neutral Concierge [`Envelope`] stream, then reuses the Phase 2 JSONL
//! ingest path ([`ingest_envelopes`]) for the actual IPLD writes — exactly the
//! shape of the Claude Code adapter, just a different on-disk format.
//!
//! The Markdown format (per Aider's `io.py append_chat_history`):
//! - `# aider chat started at <ts>` — a session boundary
//! - `#### <text>` (consecutive lines merge) — a user prompt
//! - `> Applied edit to <path>` — a file write
//! - other `> …` console lines — skipped
//! - unprefixed prose / code — the model's response
//!
//! Ingest is content-addressed (dedup by CID), so re-reading a growing
//! transcript on every capture pass is safe and idempotent.

use std::path::{Path, PathBuf};

use concierge_adapter_jsonl::{ingest_envelopes, IngestReport};
use concierge_core::{CoreBinding, Envelope, Event, ImportedFrom};

pub mod discovery;

const HOST_ID: &str = "aider";
const SOURCE_SYSTEM: &str = "aider";

/// A stable, path-derived id so a session's nodes get the same CIDs on re-read
/// (FNV-1a over the absolute path — deterministic, no deps).
pub fn source_id_for(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("aider-{hash:016x}")
}

/// Normalize Aider's session timestamp to ISO-8601, matching the reference
/// translate.py and what downstream date handling expects. Aider writes
/// `datetime.now().strftime("%Y-%m-%d %H:%M:%S")` (e.g. `2025-03-07 17:45:00`,
/// local time, labeled `Z` like the reference). Tolerant: anything unparseable
/// becomes the epoch. No date dependency — the format is fixed.
fn iso_ts(raw: &str) -> String {
    const EPOCH: &str = "1970-01-01T00:00:00Z";
    let s = raw.trim();
    // `YYYY-MM-DD HH:MM[:SS]` -> `YYYY-MM-DDTHH:MM[:SS]`.
    let t = s.replacen(' ', "T", 1);
    let b = t.as_bytes();
    let shaped = b.len() >= 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && t[..4].bytes().all(|c| c.is_ascii_digit());
    if !shaped {
        return EPOCH.to_string();
    }
    // Pad `…HH:MM` (16) to `…HH:MM:00`, then keep through seconds + `Z`.
    let with_secs = if t.len() == 16 { format!("{t}:00") } else { t };
    if with_secs.len() < 19 {
        return EPOCH.to_string();
    }
    format!("{}Z", &with_secs[..19])
}

/// Translate one `.aider.chat.history.md` document into canonical envelopes.
/// `source_id` keys the session ids so re-translation is stable (use
/// [`source_id_for`] on the file path).
pub fn translate(text: &str, source_id: &str) -> Vec<Envelope> {
    let mut x = Xlate::new(source_id);
    for line in text.lines() {
        x.line(line);
    }
    x.finish();
    x.out
}

struct Xlate {
    source_id: String,
    session_idx: usize,
    session_id: Option<String>,
    ts: String,
    seq: usize,
    pending_user: Vec<String>,
    pending_model: Vec<String>,
    out: Vec<Envelope>,
}

impl Xlate {
    fn new(source_id: &str) -> Self {
        Self {
            source_id: source_id.to_string(),
            session_idx: 0,
            session_id: None,
            ts: String::new(),
            seq: 0,
            pending_user: Vec::new(),
            pending_model: Vec::new(),
            out: Vec::new(),
        }
    }

    fn push(&mut self, event: Event) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let event_id = format!("{session_id}#{}", self.seq);
        self.seq += 1;
        self.out.push(Envelope {
            host_id: HOST_ID.to_string(),
            session_id,
            project_id: None,
            event_id: Some(event_id.clone()),
            ts: self.ts.clone(),
            imported_from: Some(ImportedFrom {
                source_system: SOURCE_SYSTEM.to_string(),
                original_id: event_id,
                original_ts: self.ts.clone(),
            }),
            reasoning: None,
            event,
        });
    }

    fn flush_user(&mut self) {
        if !self.pending_user.is_empty() {
            let text = std::mem::take(&mut self.pending_user).join("\n");
            if !text.trim().is_empty() {
                self.push(Event::UserPrompt { text });
            }
        }
    }

    fn flush_model(&mut self) {
        if !self.pending_model.is_empty() {
            let text = std::mem::take(&mut self.pending_model).join("\n");
            if !text.trim().is_empty() {
                self.push(Event::ModelResponse {
                    text: text.trim().to_string(),
                });
            }
        }
    }

    fn flush(&mut self) {
        self.flush_user();
        self.flush_model();
    }

    /// Open a session: close the previous one, start a new id, emit SessionStarted.
    fn open_session(&mut self, ts: &str) {
        self.flush();
        if self.session_id.is_some() {
            self.push(Event::SessionEnded);
        }
        self.session_idx += 1;
        self.session_id = Some(format!("{}-s{}", self.source_id, self.session_idx));
        self.ts = iso_ts(ts);
        self.seq = 0;
        self.push(Event::SessionStarted { cwd: None });
    }

    /// Ensure a session exists (content before any header opens an implicit one).
    fn ensure_session(&mut self) {
        if self.session_id.is_none() {
            self.open_session("");
        }
    }

    fn line(&mut self, line: &str) {
        // Session boundary: `# aider chat started at <ts>`.
        if let Some(rest) = line.trim_start().strip_prefix("# aider chat started at") {
            self.open_session(rest.trim());
            return;
        }
        // User prompt: `#### <text>` (consecutive lines merge into one prompt). Aider always
        // writes `#### ` with a space; tolerate an absent space, matching the reference.
        if let Some(rest) = line.strip_prefix("####") {
            self.flush_model();
            self.ensure_session();
            self.pending_user
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            return;
        }
        // Console line (a `> ` blockquote): `> Applied edit to <path>` is a file write; the rest
        // (token counts, warnings) is noise. Tolerate an absent space after `>`, per the reference.
        if let Some(inner) = line.trim_start().strip_prefix('>') {
            self.flush();
            let inner = inner.strip_prefix(' ').unwrap_or(inner);
            if let Some(path) = inner.strip_prefix("Applied edit to ") {
                self.ensure_session();
                self.push(Event::FileWritten {
                    path: path.trim().to_string(),
                });
            }
            return;
        }
        // Blank line: a soft separator. Flush a pending user prompt; keep model
        // prose accumulating so multi-paragraph responses stay one node.
        if line.trim().is_empty() {
            if !self.pending_user.is_empty() {
                self.flush_user();
            } else if !self.pending_model.is_empty() {
                self.pending_model.push(String::new());
            }
            return;
        }
        // Anything else is prose/code: the model's response.
        self.flush_user();
        self.ensure_session();
        self.pending_model.push(line.to_string());
    }

    fn finish(&mut self) {
        self.flush();
        // Mirror the Claude adapter: a trailing checkpoint marker so §4 synthesis
        // can summarize the (possibly still-live) last session. No final
        // SessionEnded — the transcript may grow; the next capture re-reads it.
        if let Some(session_id) = self.session_id.clone() {
            self.push(Event::CheckpointRequested {
                label: Some(format!("aider session {session_id}")),
            });
        }
    }
}

/// Translate + ingest a whole `.aider.chat.history.md` file. Re-ingesting a grown
/// transcript is safe (CID dedup), so the capture loop simply re-reads on change.
pub fn ingest_file<B: CoreBinding>(
    path: &Path,
    binding: &B,
    base_dir: &Path,
) -> std::io::Result<IngestReport> {
    let text = std::fs::read_to_string(path)?;
    let envelopes = translate(&text, &source_id_for(path));
    let items = envelopes.into_iter().enumerate().map(|(i, e)| (i + 1, e));
    Ok(ingest_envelopes(
        items,
        binding,
        base_dir,
        IngestReport::default(),
    ))
}

/// One incremental capture pass: discover every Aider transcript and re-ingest
/// any whose byte length changed since last pass (cheap stat; CID-dedup makes the
/// re-read idempotent). `lens` tracks per-file length across calls. Returns the
/// number of new events ingested.
pub fn capture_once<B: CoreBinding>(
    lens: &mut std::collections::HashMap<PathBuf, u64>,
    binding: &B,
    base_dir: &Path,
) -> usize {
    let mut total = 0usize;
    for transcript in discovery::discover() {
        let len = std::fs::metadata(&transcript.file)
            .map(|m| m.len())
            .unwrap_or(0);
        if lens.get(&transcript.file).copied() == Some(len) {
            continue; // unchanged
        }
        if let Ok(report) = ingest_file(&transcript.file, binding, base_dir) {
            total += report.events;
        }
        lens.insert(transcript.file, len);
    }
    total
}

/// Record the current size of every discovered transcript WITHOUT ingesting, so
/// attaching/loading does not backfill history — only growth from here is captured.
pub fn seed_lens(lens: &mut std::collections::HashMap<PathBuf, u64>) {
    for transcript in discovery::discover() {
        if let Ok(meta) = std::fs::metadata(&transcript.file) {
            lens.insert(transcript.file, meta.len());
        }
    }
}

/// Full historical backfill: ingest EVERY discovered transcript (the manual
/// "Ingest" action). Idempotent via CID dedup; yields between files so a large
/// backfill never starves the web server. Returns the number of events ingested.
pub fn ingest_all<B: CoreBinding>(binding: &B, base_dir: &Path) -> usize {
    let mut total = 0usize;
    for transcript in discovery::discover() {
        if let Ok(report) = ingest_file(&transcript.file, binding, base_dir) {
            total += report.events;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Vec<Event> {
        translate(text, "src")
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    fn translates_the_documented_aider_format() {
        let text = "# aider chat started at 2025-03-07 17:45:00\n\
                    #### add a readme\n\
                    #### with install steps\n\
                    Sure, here is the README.\n\
                    It has two sections.\n\
                    > Applied edit to README.md\n\
                    > Tokens: 1.2k sent\n";
        let evs = events(text);
        // SessionStarted, UserPrompt(merged), ModelResponse(merged), FileWritten, CheckpointRequested
        assert!(matches!(evs[0], Event::SessionStarted { .. }));
        assert!(
            matches!(&evs[1], Event::UserPrompt { text } if text == "add a readme\nwith install steps")
        );
        assert!(matches!(&evs[2], Event::ModelResponse { text } if text.contains("two sections")));
        assert!(matches!(&evs[3], Event::FileWritten { path } if path == "README.md"));
        assert!(matches!(
            evs.last(),
            Some(Event::CheckpointRequested { .. })
        ));
    }

    #[test]
    fn session_timestamp_is_iso_normalized() {
        // Grounded in aider/io.py: `datetime.now().strftime("%Y-%m-%d %H:%M:%S")`.
        let envs = translate(
            "# aider chat started at 2025-03-07 17:45:00\n#### hi\nyo\n",
            "src",
        );
        assert_eq!(
            envs[0].ts, "2025-03-07T17:45:00Z",
            "the raw `YYYY-MM-DD HH:MM:SS` stamp must be normalized to ISO-8601 (matches the reference)"
        );
        assert_eq!(
            iso_ts("garbage"),
            "1970-01-01T00:00:00Z",
            "bad stamps fall back to the epoch"
        );
    }

    #[test]
    fn tolerates_missing_space_after_prefixes() {
        // aider always writes `#### ` / `> `, but the reference parser is lenient; match it.
        let evs = events("####squished prompt\n>Applied edit to a.txt\n");
        assert!(matches!(&evs[1], Event::UserPrompt { text } if text == "squished prompt"));
        assert!(evs
            .iter()
            .any(|e| matches!(e, Event::FileWritten { path } if path == "a.txt")));
    }

    #[test]
    fn multiple_sessions_close_the_previous_one() {
        let text = "# aider chat started at 2025-03-07 17:45:00\n#### one\nreply one\n\
                    # aider chat started at 2025-03-08 09:00:00\n#### two\nreply two\n";
        let evs = events(text);
        // The second header must emit SessionEnded for the first session.
        assert!(
            evs.iter().any(|e| matches!(e, Event::SessionEnded)),
            "a new session header closes the previous session"
        );
        let starts = evs
            .iter()
            .filter(|e| matches!(e, Event::SessionStarted { .. }))
            .count();
        assert_eq!(starts, 2, "two sessions started");
    }

    #[test]
    fn stable_event_ids_so_reingest_dedupes() {
        let text = "#### hi\nyo\n";
        let a = translate(text, "src");
        let b = translate(text, "src");
        let ids_a: Vec<_> = a.iter().map(|e| e.event_id.clone()).collect();
        let ids_b: Vec<_> = b.iter().map(|e| e.event_id.clone()).collect();
        assert_eq!(ids_a, ids_b, "re-translation yields identical event ids");
    }
}
