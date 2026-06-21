//! Locate VS Code Copilot Chat session history files.
//!
//! VS Code stores chat sessions under `workspaceStorage/<workspace-hash>/chatSessions/`.
//! We scan for files ending with `.json` or `.jsonl` inside these folders.

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 3;

/// One discovered Copilot session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotSession {
    /// Absolute path to the session file.
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

/// Scan roots: default workspaceStorage path candidates.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        if cfg!(target_os = "macos") {
            roots.push(home.join("Library/Application Support/Code/User/workspaceStorage"));
        } else if cfg!(target_os = "windows") {
            if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
                roots.push(appdata.join("Code/User/workspaceStorage"));
            }
        } else {
            roots.push(home.join(".config/Code/User/workspaceStorage"));
        }
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_COPILOT_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Extract session ID from the path.
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
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "chatSessions" {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    for sub_entry in sub.flatten() {
                        let sub_path = sub_entry.path();
                        if let Some(sub_name) = sub_path.file_name().and_then(|n| n.to_str()) {
                            if sub_name.ends_with(".json") || sub_name.ends_with(".jsonl") {
                                out.push(sub_path);
                            }
                        }
                    }
                }
            } else if depth < MAX_DEPTH {
                scan(&path, depth + 1, out);
            }
        }
    }
}

/// Discover all Copilot session files.
pub fn discover() -> Vec<CopilotSession> {
    let mut files = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| CopilotSession {
            file,
            session_count: 1,
        })
        .collect()
}

pub fn total_sessions(sessions: &[CopilotSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}
