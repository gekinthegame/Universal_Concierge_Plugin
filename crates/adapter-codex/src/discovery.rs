//! Locate Codex CLI's on-disk session transcripts.
//!
//! Codex writes one rollout file per session under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. We walk that tree (bounded
//! depth) and collect every `rollout-*.jsonl`. Each file is exactly one session.
//! Extra roots can be added via `CONCIERGE_CODEX_ROOTS` (`:`-separated, each a
//! directory containing rollout files or a parent to scan).

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 6;

/// One discovered Codex session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSession {
    /// Absolute path to the `rollout-*.jsonl` file.
    pub file: PathBuf,
    /// Sessions in the file — always 1 for Codex (one rollout = one session).
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: `~/.codex/sessions` plus any `CONCIERGE_CODEX_ROOTS` entries.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".codex").join("sessions"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_CODEX_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Is this a Codex rollout transcript file?
fn is_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && name.ends_with(".jsonl")
}

/// Walk `root` (bounded depth) collecting every `rollout-*.jsonl`. Never errors —
/// an unreadable dir is just skipped.
fn scan(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                scan(&path, depth + 1, out);
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(is_rollout)
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// Every Codex rollout discoverable from [`scan_roots`], de-duplicated.
pub fn discover() -> Vec<CodexSession> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| CodexSession {
            file,
            session_count: 1,
        })
        .collect()
}

/// Total sessions across every discovered rollout — what the Host card shows.
pub fn total_sessions(sessions: &[CodexSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_rollout_files_nested_by_date() {
        let root = tempfile::tempdir().unwrap();
        let day = root.path().join("2026").join("06").join("05");
        fs::create_dir_all(&day).unwrap();
        let f = day.join("rollout-2026-06-05T10-00-00-uuid.jsonl");
        fs::write(&f, "{}\n").unwrap();
        fs::write(day.join("notes.txt"), "ignore").unwrap();
        let mut out = Vec::new();
        scan(root.path(), 0, &mut out);
        assert_eq!(out, vec![f]);
    }
}
