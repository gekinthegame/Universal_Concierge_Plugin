//! Locate Aider's on-disk chat transcripts.
//!
//! Unlike Claude Code (one central `~/.claude/projects`), Aider appends a
//! Markdown transcript named **`.aider.chat.history.md`** to *each repo you run
//! it in* (plus `~/.aider.chat.history.md` for some setups). There is no central
//! registry, so we discover by a bounded scan of the home directory for that
//! filename — skipping the usual noise (hidden dirs except the target, `Library`,
//! `node_modules`, `target`, `.git`, …) so it stays cheap enough for the capture
//! loop. Extra roots can be added via `CONCIERGE_AIDER_ROOTS` (`:`-separated).

use std::path::{Path, PathBuf};

/// Aider's transcript filename.
pub const AIDER_HISTORY: &str = ".aider.chat.history.md";

/// How deep the home scan walks before giving up (project repos live a few
/// levels under home; deeper trees are almost always dependencies, not repos).
const MAX_DEPTH: usize = 5;

/// One discovered Aider transcript and how many sessions it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiderTranscript {
    /// Absolute path to the `.aider.chat.history.md` file.
    pub file: PathBuf,
    /// Number of `# aider chat started at …` session headers in the file.
    pub session_count: usize,
}

/// Directory names we never descend into during the scan — pure noise for repos.
fn is_noise(dir_name: &str) -> bool {
    matches!(
        dir_name,
        "Library"
            | "node_modules"
            | "target"
            | ".git"
            | ".cargo"
            | ".rustup"
            | ".npm"
            | "Applications"
            | "venv"
            | ".venv"
            | "__pycache__"
            | "dist"
            | "build"
    )
}

/// The home directory, from `HOME`/`USERPROFILE`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Roots to scan: the home directory plus any `CONCIERGE_AIDER_ROOTS` entries.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home);
    }
    if let Some(extra) = std::env::var_os("CONCIERGE_AIDER_ROOTS") {
        for part in extra.to_string_lossy().split(':') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Count `# aider chat started at` session headers in a transcript. A file with
/// content but no header still counts as one session (Aider's older format).
pub fn count_sessions(file: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(file) else {
        return 0;
    };
    let headers = text
        .lines()
        .filter(|l| l.trim_start().starts_with("# aider chat started at"))
        .count();
    if headers == 0 && !text.trim().is_empty() {
        1
    } else {
        headers
    }
}

/// Walk `root` (bounded depth, skipping noise) collecting every
/// `.aider.chat.history.md`. Never errors — an unreadable dir is just skipped.
fn scan(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    // The home-level transcript itself (`~/.aider.chat.history.md`).
    let direct = root.join(AIDER_HISTORY);
    if direct.is_file() {
        out.push(direct);
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip noise and hidden directories (the transcript itself starts with a
        // dot but is a *file*, handled above — repos are not hidden).
        if is_noise(name) || name.starts_with('.') {
            continue;
        }
        scan(&path, depth + 1, out);
    }
}

/// Every Aider transcript discoverable from [`scan_roots`], de-duplicated, with
/// its session count. Empty (never errors) when none exist.
pub fn discover() -> Vec<AiderTranscript> {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in scan_roots() {
        scan(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|file| {
            let session_count = count_sessions(&file);
            AiderTranscript {
                file,
                session_count,
            }
        })
        .collect()
}

/// Total sessions across every discovered transcript — the number the Host card
/// and banner show ("Aider · N sessions").
pub fn total_sessions(transcripts: &[AiderTranscript]) -> usize {
    transcripts.iter().map(|t| t.session_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn counts_session_headers_and_falls_back_to_one() {
        let dir = tempfile::tempdir().unwrap();
        let multi = dir.path().join("multi.md");
        fs::write(
            &multi,
            "# aider chat started at 2025-03-07 17:45:00\n#### hi\nresponse\n\
             # aider chat started at 2025-03-08 09:00:00\n#### again\nok\n",
        )
        .unwrap();
        assert_eq!(count_sessions(&multi), 2);

        let headerless = dir.path().join("old.md");
        fs::write(&headerless, "#### just a prompt\na reply\n").unwrap();
        assert_eq!(count_sessions(&headerless), 1, "content but no header = 1");

        let empty = dir.path().join("empty.md");
        fs::write(&empty, "   \n").unwrap();
        assert_eq!(count_sessions(&empty), 0);
    }

    #[test]
    fn scan_finds_transcripts_and_skips_noise() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        // A repo a couple levels down with a transcript.
        let repo = root.join("Desktop").join("my-proj");
        fs::create_dir_all(&repo).unwrap();
        fs::write(
            repo.join(AIDER_HISTORY),
            "# aider chat started at 2025-03-07 17:45:00\n#### hi\nyo\n",
        )
        .unwrap();
        // A transcript buried in node_modules must NOT be found.
        let noise = root
            .join("Desktop")
            .join("my-proj")
            .join("node_modules")
            .join("pkg");
        fs::create_dir_all(&noise).unwrap();
        fs::write(noise.join(AIDER_HISTORY), "# aider chat started at x\n").unwrap();
        // The home-level transcript.
        fs::write(root.join(AIDER_HISTORY), "#### root prompt\nreply\n").unwrap();

        let mut found = Vec::new();
        scan(root, 0, &mut found);
        assert!(
            found.contains(&repo.join(AIDER_HISTORY)),
            "repo transcript found"
        );
        assert!(
            found.contains(&root.join(AIDER_HISTORY)),
            "home transcript found"
        );
        assert!(
            !found
                .iter()
                .any(|p| p.to_string_lossy().contains("node_modules")),
            "node_modules transcript skipped"
        );
    }
}
