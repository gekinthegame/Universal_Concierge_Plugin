//! Commit an entire Studio project folder to GitHub with the local `git` CLI.
//!
//! Distinct from [`crate::deploy`] (which deploys a *built site* to GitHub **Pages** via
//! the REST API): this stages the whole project (`git add -A`, honouring `.gitignore`),
//! commits it, and pushes it to a normal repo — the "upload my project to GitHub" button.
//! The Concierge seeds a sensible `.gitignore` first so secrets and build junk never get
//! committed, and creates the repo through the API if it doesn't exist yet.

use std::path::Path;
use std::process::Command;

/// Outcome of a project commit, surfaced to the Studio.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommitReceipt {
    pub repo_url: String,
    pub branch: String,
    pub committed: bool,
    pub created_repo: bool,
    pub private: bool,
}

/// Seeded only when the project has no `.gitignore` of its own — the Concierge "ensuring
/// the files are set up correctly" before the first commit.
const DEFAULT_GITIGNORE: &str = "# Seeded by Concierge — keep secrets and build junk out of git.\n\
node_modules/\ntarget/\ndist/\nbuild/\nout/\n.next/\n.DS_Store\n*.log\n.env\n.env.*\n\
.venv/\n__pycache__/\n*.pyc\n.concierge/\n";

fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

fn gh_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("concierge-plugin")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// Ensure the GitHub repo exists, creating it (private or public) if it doesn't. Returns
/// whether it was freshly created. `auto_init:false` keeps the new repo empty so our
/// first push isn't rejected by a server-side initial commit.
fn ensure_repo(token: &str, owner: &str, repo: &str, private: bool) -> Result<bool, String> {
    let api = "https://api.github.com";
    let cl = gh_client();
    let resp = cl
        .get(format!("{api}/repos/{owner}/{repo}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("github repo check: {e}"))?;
    if resp.status().is_success() {
        return Ok(false);
    }
    if resp.status().as_u16() != 404 {
        let code = resp.status().as_u16();
        return Err(format!(
            "github repo check HTTP {code} — {}",
            clip(&resp.text().unwrap_or_default())
        ));
    }
    let resp = cl
        .post(format!("{api}/user/repos"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({ "name": repo, "private": private, "auto_init": false }))
        .send()
        .map_err(|e| format!("github create repo: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        return Err(format!(
            "github create repo HTTP {code} — {} (the token needs the 'repo' scope)",
            clip(&resp.text().unwrap_or_default())
        ));
    }
    Ok(true)
}

fn git(folder: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

fn git_ok(folder: &Path, args: &[&str]) -> Result<(), String> {
    let out = git(folder, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Stage every project file (honouring `.gitignore`), commit it, and push to GitHub.
/// The auth token is passed inline to `git push` only — never written into `.git/config`.
pub fn commit_and_push(
    token: &str,
    owner: &str,
    repo: &str,
    branch: &str,
    message: &str,
    private: bool,
    folder: &Path,
) -> Result<CommitReceipt, String> {
    if !folder.is_dir() {
        return Err(format!("project folder not found: {}", folder.display()));
    }
    Command::new("git").arg("--version").output().map_err(|_| {
        "git is not installed — install Git to commit projects to GitHub".to_string()
    })?;

    let created_repo = ensure_repo(token, owner, repo, private)?;

    if !folder.join(".git").exists() {
        git_ok(folder, &["init", "-b", branch])?;
    }
    let gitignore = folder.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, DEFAULT_GITIGNORE)
            .map_err(|e| format!("write .gitignore: {e}"))?;
    }

    git_ok(folder, &["add", "-A"])?;

    // Commit with an explicit identity — the environment may carry no git config.
    let commit = git(
        folder,
        &[
            "-c",
            "user.name=Concierge",
            "-c",
            "user.email=concierge@localhost",
            "commit",
            "-m",
            message,
        ],
    )?;
    let nothing = String::from_utf8_lossy(&commit.stdout).contains("nothing to commit")
        || String::from_utf8_lossy(&commit.stderr).contains("nothing to commit");
    let committed = commit.status.success();
    if !committed && !nothing {
        return Err(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }

    // Normalise the branch name (init may have defaulted elsewhere) and set a tokenless
    // origin for the user's convenience.
    let _ = git(folder, &["branch", "-M", branch]);
    let _ = git(folder, &["remote", "remove", "origin"]);
    let _ = git(
        folder,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{owner}/{repo}.git"),
        ],
    );

    let authed = format!("https://{token}@github.com/{owner}/{repo}.git");
    let push = git(folder, &["push", &authed, &format!("{branch}:{branch}")])?;
    if !push.status.success() {
        let err = String::from_utf8_lossy(&push.stderr);
        // Scrub the token out of any echoed URL before surfacing the error.
        let safe = err.replace(token, "***");
        return Err(format!("git push failed: {}", clip(safe.trim())));
    }

    Ok(CommitReceipt {
        repo_url: format!("https://github.com/{owner}/{repo}"),
        branch: branch.to_string(),
        committed,
        created_repo,
        private,
    })
}

/// Sanitize a name into a valid GitHub repo slug: keep letters, digits, `-_.`; collapse
/// the rest to `-`; trim to 100 chars; never empty.
pub fn sanitize_repo(name: &str) -> String {
    let mut out: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(100)
        .collect();
    while out.starts_with(['-', '.']) {
        out.remove(0);
    }
    let trimmed = out.trim_end_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_names_are_sanitized() {
        assert_eq!(sanitize_repo("My Cool Project!"), "My-Cool-Project");
        assert_eq!(sanitize_repo("  ../weird/.."), "weird");
        assert_eq!(sanitize_repo(""), "project");
        assert_eq!(sanitize_repo("keep-this_one.v2"), "keep-this_one.v2");
    }
}
