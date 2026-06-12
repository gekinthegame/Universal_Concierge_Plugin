//! Transparency — see the river move. Every joint calls into here, so each
//! block minted, name bound, graph walked, and sync performed is visible.
//!
//! **Observer, not a stage.** Callers' behavior is identical with or without
//! tracing; this only prints (to stderr). The pure `render`/`abbrev` helpers
//! carry the formatting and are unit-tested; `trace` is the thin IO wrapper.
//!
//! Verbosity is a process-global level. Phase 3.1 wires the `[trace]` config
//! knob to `set_verbosity`; until then it defaults to Normal.

use crate::cid::Cid;
use std::sync::atomic::{AtomicU8, Ordering};

/// 0 = silent, 1 = normal (default), 2 = verbose.
static VERBOSITY: AtomicU8 = AtomicU8::new(1);

pub fn set_verbosity(level: u8) {
    VERBOSITY.store(level, Ordering::Relaxed);
}

pub fn verbosity() -> u8 {
    VERBOSITY.load(Ordering::Relaxed)
}

/// Pure renderer: `[mem] {joint} {detail}`, joint padded to a stable width so
/// columns line up. String in, string out — this is what tests assert on.
pub fn render(joint: &str, detail: &str) -> String {
    format!("[mem] {joint:<5} {detail}")
}

/// The single mechanism every river joint calls. Prints to stderr when the
/// verbosity level allows; otherwise a no-op. Never changes caller behavior.
pub fn trace(joint: &str, detail: &str) {
    if verbosity() >= 1 {
        eprintln!("{}", render(joint, detail));
    }
}

/// Abbreviate a CID for trace lines: `bafyrei…a1` (first 8 … last 2).
pub fn abbrev(cid: &Cid) -> String {
    abbrev_str(&cid.to_string())
}

fn abbrev_str(s: &str) -> String {
    // CID strings are ASCII (base32), so byte slicing stays on char boundaries.
    if s.len() <= 12 {
        return s.to_string();
    }
    format!("{}…{}", &s[..8], &s[s.len() - 2..])
}

// --- joint helpers: build the detail string and call `trace` ---

/// A block was written. e.g. `[mem] put   response     → bafyrei…a1  (412 B)`
pub fn put(kind: &str, cid: &Cid, bytes_len: usize) {
    trace(
        "put",
        &format!("{kind:<12} → {}  ({bytes_len} B)", abbrev(cid)),
    );
}

/// A name was (re)bound. e.g. `[mem] bind  current-project    → bafyrei…c3`
pub fn bind(name: &str, cid: &Cid) {
    trace("bind", &format!("{name:<18} → {}", abbrev(cid)));
}

/// A DAG walk completed. e.g. `[mem] walk  reachable(root)  12 blocks`
pub fn walk(label: &str, blocks: usize) {
    trace("walk", &format!("{label}  {blocks} blocks"));
}

/// A sync to a backend completed. e.g.
/// `[mem] sync  → pinata  12 blocks, pinned bafyrei…c3`
pub fn sync(target: &str, blocks: usize, root: &Cid) {
    trace(
        "sync",
        &format!("→ {target}  {blocks} blocks, pinned {}", abbrev(root)),
    );
}

/// A pruned block was encountered — a receipt of truth, not an error. e.g.
/// `[mem] prune bafyrei…c1  died 2026-06-02T… — auto-checkpoint trimmed → bafyrei…c0`
pub fn pruned(cid: &Cid, t: &crate::tombstones::Tombstone) {
    trace("prune", &render_pruned(cid, t));
}

/// Pure renderer for a tombstone receipt, so the wording is unit-tested.
pub fn render_pruned(cid: &Cid, t: &crate::tombstones::Tombstone) -> String {
    let died = crate::tombstones::iso8601(t.pruned_at);
    match t.superseded_by {
        Some(next) => format!(
            "{}  died {died} — {} → {}",
            abbrev(cid),
            t.reason,
            abbrev(&next)
        ),
        None => format!("{}  died {died} — {}", abbrev(cid), t.reason),
    }
}

// NOTE: the `edge` joint helper (e.g. `response --produced--> file`) is added in
// Phase 2.2 when edges are first written — it renders an `EdgeRel`, so it lands
// with that consumer to keep the relationship vocabulary single-sourced.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prefixes_and_pads_the_joint() {
        // joint padded to width 5, single space before detail
        assert_eq!(render("put", "x"), "[mem] put   x");
        assert_eq!(render("bind", "y"), "[mem] bind  y");
        assert_eq!(render("sync", "z"), "[mem] sync  z");
    }

    #[test]
    fn render_does_not_truncate_long_joints() {
        assert_eq!(render("reachable", "n"), "[mem] reachable n");
    }

    #[test]
    fn abbrev_shortens_long_strings_keeping_head_and_tail() {
        let s = "bafyreibvjvcv745gig4mvqs4hctx4zfkono4rjejm2ta6gtyzkqxfjeily";
        let a = abbrev_str(s);
        assert!(a.contains('…'));
        assert_eq!(a, format!("{}…{}", &s[..8], &s[s.len() - 2..]));
        assert!(a.starts_with("bafyrei"));
        assert!(a.len() < s.len());
    }

    #[test]
    fn abbrev_leaves_short_strings_untouched() {
        assert_eq!(abbrev_str("short"), "short");
        assert_eq!(abbrev_str("twelvechars1"), "twelvechars1"); // len 12 boundary
    }

    #[test]
    fn verbosity_roundtrips() {
        let prev = verbosity();
        set_verbosity(0);
        assert_eq!(verbosity(), 0);
        set_verbosity(2);
        assert_eq!(verbosity(), 2);
        set_verbosity(prev);
    }
}
