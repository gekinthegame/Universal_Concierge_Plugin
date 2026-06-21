//! Locate OpenHands/OpenDevin trajectory persistence files.
//!
//! OpenHands saves trajectory records inside the workspace or ~/.openhands directory.
//! We scan for any `.json` or `.jsonl` files.

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 5;

/// One discovered OpenHands trajectory session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenHandsSession {
    /// Absolute path to the trajectory file.
    pub file: PathBuf,
    /// Number of sessions (always 1).
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Scan roots: default OpenHands storage directories.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".openhands"));
        roots.push(home.join(".opendevin"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_OPENHANDS_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Extract session ID from the file name.
pub fn session_of(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.ends_with(".json") {
        Some(file_name.strip_suffix(".json")?.to_string())
    } else if file_name.ends_with(".jsonl") {
        Some(file_name.strip_suffix(".jsonl")?.to_string())
    } else {
        None
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
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if (name.ends_with(".json") || name.ends_with(".jsonl"))
                && !name.ends_with("sessions.json")
                && name != "base_state.json"
            {
                out.push(path);
            }
        }
    }
}

/// Discover all OpenHands trajectory files.
pub fn discover() -> Vec<OpenHandsSession> {
    let mut files = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    sessions_from_files(files)
}

fn sessions_from_files(files: Vec<PathBuf>) -> Vec<OpenHandsSession> {
    files
        .into_iter()
        .map(|file| OpenHandsSession {
            file,
            session_count: 1,
        })
        .collect()
}

pub fn total_sessions(sessions: &[OpenHandsSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}
