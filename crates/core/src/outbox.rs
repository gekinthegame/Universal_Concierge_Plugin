//! Phase 8 §2/§4 — the node→host **write-back seam**.
//!
//! Almost every event in this system flows host→node (capture). The Context
//! Compiler (§2) and synthesis surfacing (§4) need the *reverse* direction: the
//! node telling the host "here is relevant context." This module is that
//! transport — an append-only JSONL **outbox** the harness drains.
//!
//! It is deliberately small and dumb: it carries already-gated payloads. The
//! gating lives in [`crate::compiler::ContextCompiler`] (opt-in + trusted
//! authority); nothing reaches the outbox unless both gates passed. A default
//! (tool-only) node never writes a line here.
//!
//! Consumption is **offset-based** (like the claude-code adapter's incremental
//! ingest), so a harness can drain idempotently and a crash never drops or
//! double-delivers: `peek` reads pending without advancing, `drain` reads pending
//! and advances the persisted offset past them.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::binding::MemCli;
use crate::error::{Error, Result};
use crate::event::ContextSuggested;

pub const OUTBOX_FILE: &str = "outbox.jsonl";
pub const OUTBOX_OFFSET_FILE: &str = "outbox.offset";

/// A node→host event. Internally tagged by `type` on the wire. Today there is one
/// variant; the enum keeps the transport forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundEvent {
    ContextSuggested(ContextSuggested),
}

/// One line in the outbox: a timestamp plus the (flattened) event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Unix seconds when the node enqueued this.
    pub at: u64,
    #[serde(flatten)]
    pub event: OutboundEvent,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MemCli {
    fn outbox_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join(OUTBOX_FILE))
    }

    fn outbox_offset_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join(OUTBOX_OFFSET_FILE))
    }

    /// Append a gated `ContextSuggested` to the outbox (node→host). Callers must
    /// only reach here *after* the §2 gates pass — see [`MemCli::proactive_wake`].
    pub fn emit_context_suggested(&self, suggested: &ContextSuggested) -> Result<()> {
        let entry = OutboxEntry {
            at: now_secs(),
            event: OutboundEvent::ContextSuggested(suggested.clone()),
        };
        let line = serde_json::to_string(&entry)
            .map_err(|e| Error::Io(format!("serialize outbox entry: {e}")))?;
        let path = self.outbox_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("create store dir: {e}")))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Io(format!("open outbox: {e}")))?;
        writeln!(file, "{line}").map_err(|e| Error::Io(format!("write outbox: {e}")))
    }

    fn read_outbox_from(&self, offset: u64) -> Result<(Vec<OutboxEntry>, u64)> {
        let path = self.outbox_path()?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
            Err(e) => return Err(Error::Io(format!("read outbox: {e}"))),
        };
        let end = bytes.len() as u64;
        // A truncated/rotated outbox (offset past EOF) resets to the start rather
        // than silently reading nothing forever.
        let start = if offset > end { 0 } else { offset as usize };
        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(&bytes[start..]).lines() {
            if line.trim().is_empty() {
                continue;
            }
            // A partially-written trailing line is skipped, not fatal.
            if let Ok(entry) = serde_json::from_str::<OutboxEntry>(line) {
                entries.push(entry);
            }
        }
        Ok((entries, end))
    }

    fn persisted_offset(&self) -> Result<u64> {
        let path = self.outbox_offset_path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(s.trim().parse().unwrap_or(0)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(Error::Io(format!("read outbox offset: {e}"))),
        }
    }

    /// Pending node→host entries the harness has not yet drained — **without**
    /// advancing the offset (idempotent inspection).
    pub fn outbox_peek(&self) -> Result<Vec<OutboxEntry>> {
        let offset = self.persisted_offset()?;
        Ok(self.read_outbox_from(offset)?.0)
    }

    /// Drain pending entries and advance the persisted offset past them, so a
    /// later drain returns only what arrived since. Returns what was drained.
    pub fn outbox_drain(&self) -> Result<Vec<OutboxEntry>> {
        let offset = self.persisted_offset()?;
        let (entries, new_offset) = self.read_outbox_from(offset)?;
        if new_offset != offset {
            std::fs::write(self.outbox_offset_path()?, new_offset.to_string())
                .map_err(|e| Error::Io(format!("write outbox offset: {e}")))?;
        }
        Ok(entries)
    }

    /// The live §2 wake trigger: on a captured `event_type`, run the gated
    /// look-ahead and, if it produces a suggestion, **enqueue it on the outbox**
    /// for the harness. This is the call a capture loop (or the CLI demo) makes to
    /// close the proactive-injection loop. Returns the suggestion if one was
    /// emitted; `None` when any gate is closed (the default, tool-only path).
    ///
    /// `authority_id` is the trusted-authority grant the host presents; without it
    /// (or with `injection.proactive = false`) nothing is emitted.
    pub fn proactive_wake(
        &self,
        event_type: &str,
        query: &str,
        authority_id: Option<&str>,
    ) -> Result<Option<ContextSuggested>> {
        use crate::compiler::{ContextCompiler, TrustedAuthority};
        let config = self.config()?;
        // Gate 0: the wake policy — does this event type wake a look-ahead at all?
        if !ContextCompiler::should_wake(&config.injection, event_type) {
            return Ok(None);
        }
        let embedder = crate::retrieval::default_embedder(&config.librarian);
        let librarian = crate::retrieval::Librarian::index_all_persistent(self, embedder)?;
        let authority = authority_id.map(TrustedAuthority::new);
        match ContextCompiler::suggest(&config.injection, authority.as_ref(), &librarian, query) {
            Some(suggestion) => {
                self.emit_context_suggested(&suggestion)?;
                Ok(Some(suggestion))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(cid: &str) -> ContextSuggested {
        ContextSuggested {
            cids: vec![cid.to_string()],
            reason: "test".to_string(),
            authority: "user".to_string(),
        }
    }

    #[test]
    fn peek_does_not_advance_but_drain_does() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        mem.emit_context_suggested(&suggestion("bafyA")).unwrap();
        mem.emit_context_suggested(&suggestion("bafyB")).unwrap();

        // Peek is idempotent: repeated peeks see the same two pending entries.
        assert_eq!(mem.outbox_peek().unwrap().len(), 2);
        assert_eq!(mem.outbox_peek().unwrap().len(), 2, "peek never advances");

        // Drain returns them and advances past them.
        let drained = mem.outbox_drain().unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(
            drained[0].event,
            OutboundEvent::ContextSuggested(suggestion("bafyA"))
        );
        assert!(
            mem.outbox_peek().unwrap().is_empty(),
            "nothing pending after drain"
        );

        // A new arrival is delivered exactly once.
        mem.emit_context_suggested(&suggestion("bafyC")).unwrap();
        let again = mem.outbox_drain().unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].event,
            OutboundEvent::ContextSuggested(suggestion("bafyC"))
        );
        assert!(
            mem.outbox_drain().unwrap().is_empty(),
            "second drain is empty"
        );
    }

    #[test]
    fn proactive_wake_with_opt_in_and_grant_emits_to_the_outbox() {
        use crate::binding::{CoreBinding, Node};
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        // Opt in: proactive on + no confidence floor (so a real match always pushes).
        let mut cfg = crate::config::Config::default();
        cfg.injection.proactive = true;
        cfg.injection.confidence = 0.0;
        cfg.save_to_project_root(dir.path()).unwrap();
        // Index content to match against.
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"the egress lock fences data from leaving the device","kind":"reference"}"#.to_string(),
            })
            .unwrap();
        mem.bind("latest", &cid).unwrap();

        // Wake on a configured trigger WITH a trusted-authority grant.
        let suggestion = mem
            .proactive_wake("user_prompt", "egress lock device", Some("claude-code"))
            .unwrap()
            .expect("opt-in + grant + match → a suggestion");
        assert_eq!(
            suggestion.authority, "claude-code",
            "attributed to the grant"
        );

        // …and it was enqueued on the outbox for the harness to drain.
        let pending = mem.outbox_peek().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].event,
            OutboundEvent::ContextSuggested(suggestion)
        );
    }

    #[test]
    fn proactive_wake_emits_nothing_on_the_default_tool_only_path() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        // Default config: proactive off → no wake, no emission, even with a grant.
        let out = mem
            .proactive_wake("user_prompt", "anything", Some("claude-code"))
            .unwrap();
        assert!(out.is_none(), "default node stays tool-only");
        assert!(
            mem.outbox_peek().unwrap().is_empty(),
            "nothing written to the outbox"
        );
    }
}
