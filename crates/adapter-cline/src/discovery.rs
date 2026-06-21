//! Locate Cline/Roo-Cline on-disk session files.
//!
//! Cline stores task histories in `globalStorage/saoudrizwan.claude-dev/tasks/{taskId}/ui_messages.json`.
//! We recursively scan globalStorage folders for any `ui_messages.json` files.

use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 4;

/// One discovered Cline task session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClineSession {
    /// Absolute path to the `ui_messages.json` file.
    pub file: PathBuf,
    /// Number of sessions (always 1 per task).
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Scan roots: default globalStorage directories for Claude-Dev / Cline / Roo-Cline.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        if cfg!(target_os = "macos") {
            let support = home.join("Library/Application Support/Code/User/globalStorage");
            roots.push(support.join("saoudrizwan.claude-dev").join("tasks"));
            roots.push(support.join("rooveterinaryinc.roo-cline").join("tasks"));
        } else if cfg!(target_os = "windows") {
            if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
                let support = appdata.join("Code/User/globalStorage");
                roots.push(support.join("saoudrizwan.claude-dev").join("tasks"));
                roots.push(support.join("rooveterinaryinc.roo-cline").join("tasks"));
            }
        } else {
            let support = home.join(".config/Code/User/globalStorage");
            roots.push(support.join("saoudrizwan.claude-dev").join("tasks"));
            roots.push(support.join("rooveterinaryinc.roo-cline").join("tasks"));
        }
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_CLINE_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Extract task/session ID from the path (the parent directory name).
pub fn session_of(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    let name = parent.file_name()?.to_str()?;
    Some(name.to_string())
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
            if name == "ui_messages.json" {
                out.push(path);
            }
        }
    }
}

/// Discover all Cline/Roo-Cline session files.
pub fn discover() -> Vec<ClineSession> {
    let mut files = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| ClineSession {
            file,
            session_count: 1,
        })
        .collect()
}

pub fn total_sessions(sessions: &[ClineSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_ui_messages_json() {
        let root = tempfile::tempdir().unwrap();
        let task_dir = root
            .path()
            .join("saoudrizwan.claude-dev")
            .join("tasks")
            .join("task-123");
        fs::create_dir_all(&task_dir).unwrap();
        let f1 = task_dir.join("ui_messages.json");
        fs::write(&f1, "[]").unwrap();

        let mut out = Vec::new();
        scan(root.path(), 0, &mut out);
        assert_eq!(out, vec![f1.clone()]);
        assert_eq!(session_of(&f1).as_deref(), Some("task-123"));
    }
}
