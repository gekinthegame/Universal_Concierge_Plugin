//! Locate Continue's on-disk session files.
//!
//! Continue stores each conversation as `~/.continue/sessions/<id>.json`, indexed
//! by `~/.continue/sessions/sessions.json` (which we skip — it's the index, not a
//! conversation). Extra roots can be added via `CONCIERGE_CONTINUE_ROOTS`
//! (`:`-separated, each a directory of session `.json` files).

use std::path::PathBuf;

/// One discovered Continue session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueSession {
    /// Absolute path to the `<id>.json` session file.
    pub file: PathBuf,
    /// Sessions in the file — 1 in the common case (a single conversation).
    pub session_count: usize,
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: `~/.continue/sessions` plus any `CONCIERGE_CONTINUE_ROOTS`.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".continue").join("sessions"));
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_CONTINUE_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Is this a Continue session file (a `.json` that isn't the `sessions.json` index)?
fn is_session(name: &str) -> bool {
    name.ends_with(".json") && name != "sessions.json"
}

/// Every Continue session discoverable from [`scan_roots`], de-duplicated. The
/// sessions directory is flat (no recursion needed).
pub fn discover() -> Vec<ContinueSession> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_file = entry.metadata().map(|m| m.is_file()).unwrap_or(false);
            if is_file
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(is_session)
                    .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| ContinueSession {
            file,
            session_count: 1,
        })
        .collect()
}

/// Total sessions across every discovered file — what the Host card shows.
pub fn total_sessions(sessions: &[ContinueSession]) -> usize {
    sessions.iter().map(|s| s.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_session_json_and_skips_the_index() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("CONCIERGE_CONTINUE_ROOTS", dir.path());
        fs::write(dir.path().join("abc.json"), "{}").unwrap();
        fs::write(dir.path().join("sessions.json"), "[]").unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let found = discover();
        std::env::remove_var("CONCIERGE_CONTINUE_ROOTS");
        let names: Vec<_> = found
            .iter()
            .filter_map(|s| s.file.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .collect();
        assert!(names.contains(&"abc.json".to_string()));
        assert!(!names.contains(&"sessions.json".to_string()));
        assert!(!names.contains(&"notes.txt".to_string()));
    }
}
