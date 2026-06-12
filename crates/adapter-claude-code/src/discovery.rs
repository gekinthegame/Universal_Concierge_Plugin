//! Locate and enumerate Claude Code's on-disk session transcripts.
//!
//! Claude Code stores sessions at `~/.claude/projects/<slug>/<session>.jsonl`,
//! where `<slug>` is the project's absolute path with every non-alphanumeric
//! character replaced by `-` (e.g. `/Users/x/Desktop/My_Proj` →
//! `-Users-x-Desktop-My-Proj`). Per Decision 0013 the watcher attaches to the
//! **whole** `projects` directory and backfills every session, so the slug is a
//! convenience for "which folder is *this* project" — not required for capture.

use std::path::{Path, PathBuf};

/// One discovered Claude Code session transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSession {
    /// The `<slug>` directory under `projects/`.
    pub project_slug: String,
    /// Absolute path to the session `*.jsonl` file.
    pub session_file: PathBuf,
    /// The session id (the file stem — a Claude Code session UUID).
    pub session_id: String,
}

/// The user's `~/.claude/projects` directory, if `HOME`/`USERPROFILE` is set.
/// Does not check existence — callers decide what an absent directory means.
pub fn claude_projects_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(claude_projects_dir_in(&home))
}

/// `~/.claude/projects` rooted at an explicit home (testable).
pub fn claude_projects_dir_in(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

/// Claude Code's slug for a project path: every non-alphanumeric character
/// becomes `-` (so a leading `/` becomes a leading `-`).
pub fn slug_for_path(project_path: &str) -> String {
    project_path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Enumerate every `*.jsonl` session under a `projects` directory, across all
/// project slugs. Returns empty (never errors) if the directory is absent or
/// unreadable — an un-attached node is a normal state, not a failure.
pub fn enumerate_sessions(projects_dir: &Path) -> Vec<ProjectSession> {
    let mut sessions = Vec::new();
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return sessions;
    };
    for project in projects.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let slug = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let Ok(files) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            sessions.push(ProjectSession {
                project_slug: slug.clone(),
                session_file: path,
                session_id,
            });
        }
    }
    sessions.sort_by(|a, b| a.session_file.cmp(&b.session_file));
    sessions
}

/// Convenience: enumerate sessions under the real `~/.claude/projects`.
pub fn discover() -> Vec<ProjectSession> {
    claude_projects_dir()
        .map(|dir| enumerate_sessions(&dir))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn slug_matches_claude_codes_non_alphanumeric_to_dash_rule() {
        assert_eq!(
            slug_for_path("/Users/x/Desktop/Universal_Concierge_Pluggin"),
            "-Users-x-Desktop-Universal-Concierge-Pluggin"
        );
    }

    #[test]
    fn enumerate_finds_jsonl_across_project_slugs_and_skips_other_files() {
        let home = tempfile::tempdir().unwrap();
        let projects = claude_projects_dir_in(home.path());
        let proj_a = projects.join("-proj-a");
        let proj_b = projects.join("-proj-b");
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();
        fs::write(proj_a.join("sess1.jsonl"), "{}").unwrap();
        fs::write(proj_a.join("notes.txt"), "ignore me").unwrap();
        fs::write(proj_b.join("sess2.jsonl"), "{}").unwrap();

        let found = enumerate_sessions(&projects);
        assert_eq!(found.len(), 2, "two jsonl across two slugs, txt skipped");
        let ids: Vec<_> = found.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"sess1"));
        assert!(ids.contains(&"sess2"));
        let slugs: Vec<_> = found.iter().map(|s| s.project_slug.as_str()).collect();
        assert!(slugs.contains(&"-proj-a"));
    }

    #[test]
    fn an_absent_projects_dir_is_empty_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert!(enumerate_sessions(&claude_projects_dir_in(home.path())).is_empty());
    }
}
