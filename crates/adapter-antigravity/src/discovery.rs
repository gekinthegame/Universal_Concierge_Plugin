//! Locate Antigravity IDE's on-disk chat sessions.
//!
//! Antigravity writes each chat to `~/.gemini/antigravity-ide/brain/<session-uuid>/.system_generated/logs/transcript.jsonl`.
//! We walk `~/.gemini/antigravity-ide/brain` (bounded depth) collecting every `transcript.jsonl` under `.system_generated/logs`.
//! Extra roots can be added via `CONCIERGE_ANTIGRAVITY_ROOTS` (`:`-separated).

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 5;

/// One discovered Antigravity session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravitySession {
    /// Absolute path to the `transcript.jsonl` file.
    pub file: PathBuf,
    /// Sessions in the file — always 1 for Antigravity.
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: `~/.gemini/antigravity-ide/brain` plus any `CONCIERGE_ANTIGRAVITY_ROOTS` entries.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".gemini").join("antigravity-ide").join("brain"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_ANTIGRAVITY_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// The session ID (the folder name representing the conversation UUID).
pub fn session_of(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let name = comps.windows(4).rev().find_map(|window| {
        (window[1] == ".system_generated" && window[2] == "logs" && window[3] == "transcript.jsonl")
            .then(|| window[0].as_str())
    })?;
    (!name.is_empty()).then(|| name.to_string())
}

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
            .map(|n| n == "transcript.jsonl")
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// Every Antigravity session discoverable from [`scan_roots`], de-duplicated.
pub fn discover() -> Vec<AntigravitySession> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| AntigravitySession {
            file,
            session_count: 1,
        })
        .collect()
}

/// Total sessions across every discovered chat — what the Host card shows.
pub fn total_sessions(sessions: &[AntigravitySession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_session_files_and_reads_project() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root
            .path()
            .join("session-123")
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&session_dir).unwrap();
        let f = session_dir.join("transcript.jsonl");
        fs::write(&f, "{}\n").unwrap();
        let mut out = Vec::new();
        scan(root.path(), 0, &mut out);
        assert_eq!(out, vec![f.clone()]);
        assert_eq!(session_of(&f).as_deref(), Some("session-123"));
    }
}
