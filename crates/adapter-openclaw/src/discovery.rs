//! Locate OpenClaw on-disk chat sessions.
//!
//! OpenClaw writes each chat to `~/.openclaw/agents/<agent-id>/sessions/<session-uuid>.jsonl`.
//! We walk `~/.openclaw/agents/` collecting every `<session-uuid>.jsonl` under `sessions/` subdirectories.
//! Extra roots can be added via `CONCIERGE_OPENCLAW_ROOTS` (`:`-separated).

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 5;

/// One discovered OpenClaw session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenClawSession {
    /// Absolute path to the session JSONL file.
    pub file: PathBuf,
    /// Sessions in the file — always 1 for OpenClaw.
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: `~/.openclaw` plus any `CONCIERGE_OPENCLAW_ROOTS` entries.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".openclaw"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_OPENCLAW_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Extract session ID from the path (the filename without `.jsonl`).
pub fn session_of(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.ends_with(".jsonl") && !file_name.ends_with(".trajectory.jsonl") {
        let name = file_name.strip_suffix(".jsonl")?;
        if name != "sessions" {
            return Some(name.to_string());
        }
    }
    None
}

/// Recursively scan directories for valid OpenClaw session files.
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
        } else {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".jsonl")
                    && !name.ends_with(".trajectory.jsonl")
                    && name != "sessions.json"
                {
                    out.push(path);
                }
            }
        }
    }
}

/// Every OpenClaw session discoverable from [`scan_roots`], de-duplicated.
pub fn discover() -> Vec<OpenClawSession> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| OpenClawSession {
            file,
            session_count: 1,
        })
        .collect()
}

/// Total sessions across every discovered chat — what the Host card shows.
pub fn total_sessions(sessions: &[OpenClawSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_session_files() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("agents").join("main").join("sessions");
        fs::create_dir_all(&session_dir).unwrap();

        let f1 = session_dir.join("29468113-f927-4030-859b-34d7020ba6de.jsonl");
        let f2 = session_dir.join("29468113-f927-4030-859b-34d7020ba6de.trajectory.jsonl");
        let f3 = session_dir.join("sessions.json");

        fs::write(&f1, "{}\n").unwrap();
        fs::write(&f2, "{}\n").unwrap();
        fs::write(&f3, "{}\n").unwrap();

        let mut out = Vec::new();
        scan(root.path(), 0, &mut out);
        assert_eq!(out, vec![f1.clone()]);
        assert_eq!(
            session_of(&f1).as_deref(),
            Some("29468113-f927-4030-859b-34d7020ba6de")
        );
        assert_eq!(session_of(&f2), None);
        assert_eq!(session_of(&f3), None);
    }
}
