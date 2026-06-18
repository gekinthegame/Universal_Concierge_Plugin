//! Locate Gemini CLI's on-disk chat sessions.
//!
//! Gemini writes each chat to `~/.gemini/tmp/<project>/chats/session-*.jsonl`.
//! We walk `~/.gemini/tmp` (bounded depth) collecting every `session-*.jsonl`.
//! Each file is exactly one session. Extra roots can be added via
//! `CONCIERGE_GEMINI_ROOTS` (`:`-separated).

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 5;

/// One discovered Gemini session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiSession {
    /// Absolute path to the `session-*.jsonl` file.
    pub file: PathBuf,
    /// Sessions in the file — always 1 for Gemini.
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: `~/.gemini/tmp` plus any `CONCIERGE_GEMINI_ROOTS` entries.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".gemini").join("tmp"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_GEMINI_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

fn is_session(name: &str) -> bool {
    name.starts_with("session-") && name.ends_with(".jsonl")
}

/// The readable project dir under `~/.gemini/tmp/<project>/chats/…` — skipped when
/// it's an opaque 64-hex hash (Gemini sometimes hashes the project path).
pub fn project_of(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let name = comps.windows(3).rev().find_map(|window| {
        (window[0] == "tmp" && window[2] == "chats").then(|| window[1].as_str())
    })?;
    let is_hash = name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hash || name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
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
            .map(is_session)
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// Every Gemini session discoverable from [`scan_roots`], de-duplicated.
pub fn discover() -> Vec<GeminiSession> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| GeminiSession {
            file,
            session_count: 1,
        })
        .collect()
}

/// Total sessions across every discovered chat — what the Host card shows.
pub fn total_sessions(sessions: &[GeminiSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_session_files_and_reads_project() {
        let root = tempfile::tempdir().unwrap();
        let chats = root.path().join("my-proj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        let f = chats.join("session-2026-06-05.jsonl");
        fs::write(&f, "{}\n").unwrap();
        let mut out = Vec::new();
        scan(root.path(), 0, &mut out);
        assert_eq!(out, vec![f.clone()]);
        // project_of needs a `tmp` component above the project dir.
        let full = root
            .path()
            .join("tmp")
            .join("my-proj")
            .join("chats")
            .join("session-x.jsonl");
        assert_eq!(project_of(&full).as_deref(), Some("my-proj"));
        let under_system_tmp = Path::new("/tmp")
            .join(".tmpnCh2ga")
            .join("tmp")
            .join("my-proj")
            .join("chats")
            .join("session-x.jsonl");
        assert_eq!(project_of(&under_system_tmp).as_deref(), Some("my-proj"));
    }
}
