//! Multi-platform website publishing — deploy a static folder to **GitHub Pages**,
//! **Netlify**, **Vercel**, **Cloudflare Pages**, or **Firebase Hosting**.
//!
//! Each `deploy_*` takes the user's stored [`DeployCredentials`] + the staged files
//! and returns the **live URL**. Grounded in each platform's documented API:
//! - GitHub: REST Git Data API (blobs→tree→commit→ref) + enable Pages.
//! - Netlify: create site + digest deploy (`POST /api/v1/sites/{id}/deploys` with a
//!   file→SHA1 manifest, then `PUT /api/v1/deploys/{id}/files/{path}` for each required).
//! - Vercel: upload files (`POST /v2/files`, `x-vercel-digest` = SHA1) + create
//!   deployment (`POST /v13/deployments`, `files:[{file,sha,size}]`).
//! - Cloudflare Pages direct-upload: upload-token JWT → `pages/assets/upload`
//!   (base64, blake3(content+ext)[..32] hashes) → `upsert-hashes` → deployment
//!   (multipart `manifest` of path→hash).
//! - Firebase Hosting: service-account JWT (RS256) → OAuth token → create version →
//!   `:populateFiles` (SHA256-of-gzip hashes) → upload required → finalize → release.
//!
//! These are explicit, reviewed and password-gated egress. Tokens stay on-device
//! and production builds use fixed HTTPS API origins.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_DEPLOY_FILES: usize = 2_000;
pub const MAX_DEPLOY_FILE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_DEPLOY_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_DEPLOY_DEPTH: usize = 32;

// ── stored credentials (persisted 0600 by binding.rs as `<store>/deploy.json`) ──

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployCredentials {
    pub github: Option<GithubCreds>,
    pub netlify: Option<NetlifyCreds>,
    pub vercel: Option<VercelCreds>,
    pub cloudflare: Option<CloudflareCreds>,
    pub firebase: Option<FirebaseCreds>,
    pub ftp: Option<FtpCreds>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubCreds {
    pub token: String,
    pub owner: String,
    pub repo: String,
    #[serde(default = "gh_branch")]
    pub branch: String,
}
fn gh_branch() -> String {
    "gh-pages".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetlifyCreds {
    pub token: String,
    #[serde(default)]
    pub site_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VercelCreds {
    pub token: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareCreds {
    pub token: String,
    /// Optional with one-click OAuth — auto-detected from the token when blank.
    #[serde(default)]
    pub account_id: String,
    /// Optional — falls back to the (sanitized) site name when blank.
    #[serde(default)]
    pub project: String,
    /// OAuth refresh token + access-token expiry (one-click login). Absent for a
    /// manually-pasted API token, which doesn't expire.
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

/// Firebase Hosting. `site_id` is the Hosting site (defaults to the project id);
/// `service_account` is the full service-account JSON key (its `client_email` +
/// `private_key` mint a short-lived Google OAuth token — no key leaves the device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirebaseCreds {
    /// Hosting site id — optional with one-click login (auto-detected from your projects).
    #[serde(default)]
    pub site_id: String,
    /// Service-account JSON key (the manual path). Empty when connected via OAuth.
    #[serde(default)]
    pub service_account: String,
    /// One-click Google OAuth (`firebase login`) tokens — present instead of a service
    /// account when connected that way.
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtpCreds {
    #[serde(default = "ftp_host")]
    pub host: String,
    pub user: String,
    pub password: String,
    #[serde(default = "ftp_dir")]
    pub dir: String,
    #[serde(default)]
    pub site_url: Option<String>,
}
fn ftp_host() -> String {
    "ftpupload.net".to_string()
}
fn ftp_dir() -> String {
    "htdocs".to_string()
}

// ── staged files ────────────────────────────────────────────────────────────

/// One file to deploy: a forward-slash relative path, its bytes, and a MIME type.
#[derive(Debug, Clone)]
pub struct DeployFile {
    pub rel: String,
    pub bytes: Vec<u8>,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployManifestEntry {
    pub rel: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteDeployPlan {
    pub name: String,
    pub folder: String,
    pub kind: String,
    pub platform: String,
    pub destination: String,
    pub manifest: Vec<DeployManifestEntry>,
    pub manifest_digest: String,
    pub total_bytes: u64,
}

impl SiteDeployPlan {
    pub fn from_files(
        name: &str,
        folder: &Path,
        kind: &str,
        platform: &str,
        destination: &str,
        files: &[DeployFile],
    ) -> Result<Self, String> {
        let folder = folder
            .canonicalize()
            .map_err(|e| format!("canonicalize deploy root: {e}"))?;
        let mut manifest = files
            .iter()
            .map(|file| DeployManifestEntry {
                rel: file.rel.clone(),
                size: file.bytes.len() as u64,
                sha256: sha256_hex(&file.bytes),
            })
            .collect::<Vec<_>>();
        manifest.sort_by(|a, b| a.rel.cmp(&b.rel));
        let total_bytes = manifest.iter().map(|entry| entry.size).sum();
        let encoded = serde_json::to_vec(&manifest)
            .map_err(|e| format!("serialize deployment manifest: {e}"))?;
        Ok(Self {
            name: name.to_string(),
            folder: folder.to_string_lossy().into_owned(),
            kind: kind.to_string(),
            platform: platform.to_string(),
            destination: destination.to_string(),
            manifest,
            manifest_digest: sha256_hex(&encoded),
            total_bytes,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sensitive_deploy_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || matches!(
            lower.as_str(),
            ".npmrc"
                | ".pypirc"
                | ".netrc"
                | "credentials"
                | "credentials.json"
                | "secrets.json"
                | "id_rsa"
                | "id_ed25519"
        )
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
}

/// Walk a folder into a deterministic flat list of [`DeployFile`]s.
///
/// Symlinks, special files, secret-like names, and deployments beyond the
/// bounded resource budget fail closed before network egress.
pub fn walk_files(root: &Path) -> Result<Vec<DeployFile>, String> {
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|e| format!("inspect deploy root {}: {e}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("deployment root must be a real directory, not a symlink".to_string());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize deploy root: {e}"))?;
    let mut out = Vec::new();
    let mut total_bytes = 0u64;
    walk_into(&root, &root, 0, &mut total_bytes, &mut out)?;
    if out.is_empty() {
        return Err("nothing to deploy: the folder has no files".to_string());
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk_into(
    root: &Path,
    dir: &Path,
    depth: usize,
    total_bytes: &mut u64,
    out: &mut Vec<DeployFile>,
) -> Result<(), String> {
    if depth > MAX_DEPLOY_DEPTH {
        return Err(format!(
            "deployment exceeds maximum directory depth of {MAX_DEPLOY_DEPTH}"
        ));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read directory entry in {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(
            name.as_str(),
            ".git" | "node_modules" | ".netlify" | ".DS_Store"
        ) || name.ends_with(".zip")
        {
            continue;
        }
        if sensitive_deploy_name(&name) {
            return Err(format!(
                "sensitive-looking deployment path is blocked: {}",
                path.display()
            ));
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("inspect {}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("deployment contains a symlink: {}", path.display()));
        }
        if metadata.is_dir() {
            walk_into(root, &path, depth + 1, total_bytes, out)?;
        } else if metadata.is_file() {
            if out.len() >= MAX_DEPLOY_FILES {
                return Err(format!(
                    "deployment exceeds maximum file count of {MAX_DEPLOY_FILES}"
                ));
            }
            if metadata.len() > MAX_DEPLOY_FILE_BYTES {
                return Err(format!(
                    "deployment file exceeds size limit: {}",
                    path.display()
                ));
            }
            *total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| "deployment byte count overflow".to_string())?;
            if *total_bytes > MAX_DEPLOY_TOTAL_BYTES {
                return Err(format!(
                    "deployment exceeds total size limit of {MAX_DEPLOY_TOTAL_BYTES} bytes"
                ));
            }
            let canon = path
                .canonicalize()
                .map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
            if !canon.starts_with(root) {
                return Err(format!("deployment path escapes root: {}", path.display()));
            }
            let rel = canon
                .strip_prefix(root)
                .map_err(|_| "path escapes root".to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes =
                std::fs::read(&canon).map_err(|e| format!("read {}: {e}", canon.display()))?;
            out.push(DeployFile {
                content_type: content_type(&rel).to_string(),
                rel,
                bytes,
            });
        } else {
            return Err(format!(
                "deployment contains a special file: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Sanitize a site name into a valid **Vercel project name**: lowercase; only letters,
/// digits, `.`, `_`, `-`; never the sequence `---`; ≤100 chars. (Vercel rejects e.g.
/// "ConciergeSideKick" for being non-lowercase.)
fn vercel_project_name(name: &str) -> String {
    let mut out: String = name
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    while out.contains("---") {
        out = out.replace("---", "--");
    }
    let out: String = out
        .trim_matches(|c: char| c == '-' || c == '.' || c == '_')
        .chars()
        .take(100)
        .collect();
    if out.is_empty() {
        "site".to_string()
    } else {
        out
    }
}

/// Sanitize into a valid **Cloudflare Pages project name**: lowercase letters/digits and
/// single hyphens, no leading/trailing hyphen, ≤58 chars.
fn cf_project_name(name: &str) -> String {
    let mut out: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out: String = out.trim_matches('-').chars().take(58).collect();
    if out.is_empty() {
        "site".to_string()
    } else {
        out
    }
}

/// Sanitize a site name into a valid **Netlify site name** (its subdomain): lowercase
/// letters/digits and single hyphens, no leading/trailing hyphen, ≤63 chars.
fn netlify_site_name(name: &str) -> String {
    let mut out: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out: String = out.trim_matches('-').chars().take(63).collect();
    if out.is_empty() {
        "site".to_string()
    } else {
        out
    }
}

fn content_type(rel: &str) -> &'static str {
    match rel
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn extension(rel: &str) -> String {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    match base.rsplit_once('.') {
        Some((_, ext)) => ext.to_ascii_lowercase(),
        None => String::new(),
    }
}

// ── shared HTTP helpers ──────────────────────────────────────────────────────

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("concierge-plugin")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

#[cfg(test)]
fn base(env_key: &str, default: &str) -> String {
    std::env::var(env_key).unwrap_or_else(|_| default.to_string())
}

#[cfg(not(test))]
fn base(_env_key: &str, default: &str) -> String {
    default.to_string()
}

/// Send a request, require 2xx, parse the JSON body (empty body → `{}`).
fn ok_json(rb: reqwest::blocking::RequestBuilder, what: &str) -> Result<serde_json::Value, String> {
    let resp = rb.send().map_err(|e| format!("{what}: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "{what}: HTTP {} — {}",
            status.as_u16(),
            clip(&text)
        ));
    }
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&text).map_err(|e| format!("{what}: invalid JSON: {e}"))
}

/// Like [`ok_json`] but returns `None` on a 404 (e.g. "branch/site not found yet").
fn opt_json(
    rb: reqwest::blocking::RequestBuilder,
    what: &str,
) -> Result<Option<serde_json::Value>, String> {
    let resp = rb.send().map_err(|e| format!("{what}: {e}"))?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "{what}: HTTP {} — {}",
            status.as_u16(),
            clip(&text)
        ));
    }
    Ok(Some(
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({})),
    ))
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── Connection test ──────────────────────────────────────────────────────────

/// Verify a platform's credentials live against its identity endpoint (no deploy,
/// no writes). Returns a short account label on success, or a clear error the GUI
/// can show during the "connect" walk-through. Reuses the same base origins as the
/// deploy calls, so a verified token will deploy.
pub fn verify(platform: &str, creds: &DeployCredentials) -> Result<String, String> {
    let cl = client();
    match platform {
        "github" => {
            let c = creds.github.as_ref().ok_or("no GitHub credentials")?;
            let api = base("CONCIERGE_DEPLOY_GITHUB_BASE", "https://api.github.com");
            let me = ok_json(
                cl.get(format!("{api}/user"))
                    .bearer_auth(&c.token)
                    .header("Accept", "application/vnd.github+json"),
                "github sign-in",
            )?;
            let login = me.get("login").and_then(|s| s.as_str()).unwrap_or("?");
            // The repo is the usual failure — confirm it's reachable with this token.
            let repo_ok = cl
                .get(format!("{api}/repos/{}/{}", c.owner, c.repo))
                .bearer_auth(&c.token)
                .header("Accept", "application/vnd.github+json")
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if !repo_ok {
                return Err(format!(
                    "signed in as {login}, but {}/{} isn't accessible — check the owner/repo and that the token has the `repo` scope",
                    c.owner, c.repo
                ));
            }
            Ok(format!("{login} · {}/{}", c.owner, c.repo))
        }
        "netlify" => {
            let c = creds.netlify.as_ref().ok_or("no Netlify credentials")?;
            let api = base("CONCIERGE_DEPLOY_NETLIFY_BASE", "https://api.netlify.com");
            let me = ok_json(
                cl.get(format!("{api}/api/v1/user")).bearer_auth(&c.token),
                "netlify sign-in",
            )?;
            Ok(me
                .get("full_name")
                .or_else(|| me.get("email"))
                .or_else(|| me.get("slug"))
                .and_then(|s| s.as_str())
                .unwrap_or("connected")
                .to_string())
        }
        "vercel" => {
            let c = creds.vercel.as_ref().ok_or("no Vercel credentials")?;
            let api = base("CONCIERGE_DEPLOY_VERCEL_BASE", "https://api.vercel.com");
            let me = ok_json(
                cl.get(format!("{api}/v2/user")).bearer_auth(&c.token),
                "vercel sign-in",
            )?;
            let u = me.get("user").unwrap_or(&me);
            Ok(u.get("username")
                .or_else(|| u.get("name"))
                .or_else(|| u.get("email"))
                .and_then(|s| s.as_str())
                .unwrap_or("connected")
                .to_string())
        }
        "cloudflare" => {
            let c = creds
                .cloudflare
                .as_ref()
                .ok_or("no Cloudflare credentials")?;
            let api = base(
                "CONCIERGE_DEPLOY_CLOUDFLARE_BASE",
                "https://api.cloudflare.com/client/v4",
            );
            let v = ok_json(
                cl.get(format!("{api}/user/tokens/verify"))
                    .bearer_auth(&c.token),
                "cloudflare token",
            )?;
            let status = v
                .get("result")
                .and_then(|r| r.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if status != "active" {
                return Err(format!("token status is '{status}', expected 'active'"));
            }
            let acct_ok = cl
                .get(format!("{api}/accounts/{}", c.account_id))
                .bearer_auth(&c.token)
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if !acct_ok {
                return Err("token is active, but the Account ID isn't accessible — check the Account ID and that the token has the Cloudflare Pages: Edit permission".to_string());
            }
            Ok(format!("token active · account {}", c.account_id))
        }
        "firebase" => {
            let c = creds.firebase.as_ref().ok_or("no Firebase credentials")?;
            let sa = parse_service_account(&c.service_account)?;
            // Minting a token proves the service-account key is valid and authorized.
            let token = firebase_access_token(&cl, &sa)?;
            let api = base(
                "CONCIERGE_DEPLOY_FIREBASE_BASE",
                "https://firebasehosting.googleapis.com",
            );
            let site_ok = cl
                .get(format!("{api}/v1beta1/sites/{}", c.site_id))
                .bearer_auth(&token)
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if !site_ok {
                return Err(format!(
                    "authenticated as {}, but the Hosting site '{}' isn't accessible — check the Site ID and that the service account has the Firebase Hosting Admin role",
                    sa.client_email, c.site_id
                ));
            }
            Ok(format!("{} · site {}", sa.client_email, c.site_id))
        }
        other => Err(format!("unknown platform: {other}")),
    }
}

// ── GitHub Pages (Git Data API + enable Pages) ───────────────────────────────

pub fn deploy_github(c: &GithubCreds, files: &[DeployFile]) -> Result<String, String> {
    let api = base("CONCIERGE_DEPLOY_GITHUB_BASE", "https://api.github.com");
    let cl = client();
    let (o, r, branch) = (&c.owner, &c.repo, &c.branch);
    let auth = |rb: reqwest::blocking::RequestBuilder| {
        rb.bearer_auth(&c.token)
            .header("Accept", "application/vnd.github+json")
    };

    // 1. Current branch head (parent commit), if the branch exists.
    let head = opt_json(
        auth(cl.get(format!("{api}/repos/{o}/{r}/git/ref/heads/{branch}"))),
        "github ref",
    )?;
    let parent = head
        .as_ref()
        .and_then(|v| v.get("object"))
        .and_then(|o| o.get("sha"))
        .and_then(|s| s.as_str())
        .map(String::from);

    // 2. One blob per file.
    let mut tree = Vec::new();
    for f in files {
        let blob = ok_json(
            auth(cl.post(format!("{api}/repos/{o}/{r}/git/blobs")))
                .json(&serde_json::json!({ "content": b64(&f.bytes), "encoding": "base64" })),
            "github blob",
        )?;
        let sha = blob
            .get("sha")
            .and_then(|s| s.as_str())
            .ok_or("github blob: no sha")?;
        tree.push(
            serde_json::json!({ "path": f.rel, "mode": "100644", "type": "blob", "sha": sha }),
        );
    }

    // 3. Tree (full replacement — no base_tree).
    let tree_obj = ok_json(
        auth(cl.post(format!("{api}/repos/{o}/{r}/git/trees")))
            .json(&serde_json::json!({ "tree": tree })),
        "github tree",
    )?;
    let tree_sha = tree_obj
        .get("sha")
        .and_then(|s| s.as_str())
        .ok_or("github tree: no sha")?;

    // 4. Commit.
    let mut commit_body =
        serde_json::json!({ "message": "Published via Concierge", "tree": tree_sha });
    commit_body["parents"] = match &parent {
        Some(p) => serde_json::json!([p]),
        None => serde_json::json!([]),
    };
    let commit = ok_json(
        auth(cl.post(format!("{api}/repos/{o}/{r}/git/commits"))).json(&commit_body),
        "github commit",
    )?;
    let commit_sha = commit
        .get("sha")
        .and_then(|s| s.as_str())
        .ok_or("github commit: no sha")?;

    // 5. Point the branch at the new commit.
    if parent.is_some() {
        ok_json(
            auth(cl.patch(format!("{api}/repos/{o}/{r}/git/refs/heads/{branch}")))
                .json(&serde_json::json!({ "sha": commit_sha, "force": false })),
            "github update ref",
        )?;
    } else {
        ok_json(
            auth(cl.post(format!("{api}/repos/{o}/{r}/git/refs"))).json(
                &serde_json::json!({ "ref": format!("refs/heads/{branch}"), "sha": commit_sha }),
            ),
            "github create ref",
        )?;
    }

    // 6. Enable Pages if not already, and read the live URL.
    let existing = opt_json(
        auth(cl.get(format!("{api}/repos/{o}/{r}/pages"))),
        "github pages",
    )?;
    let html_url = match existing {
        Some(v) => v.get("html_url").and_then(|s| s.as_str()).map(String::from),
        None => {
            let created = ok_json(
                auth(cl.post(format!("{api}/repos/{o}/{r}/pages")))
                    .json(&serde_json::json!({ "source": { "branch": branch, "path": "/" } })),
                "github enable pages",
            )?;
            created
                .get("html_url")
                .and_then(|s| s.as_str())
                .map(String::from)
        }
    };
    Ok(html_url.unwrap_or_else(|| format!("https://{o}.github.io/{r}/")))
}

// ── Netlify (create site + zip deploy) ───────────────────────────────────────

pub fn deploy_netlify(
    c: &NetlifyCreds,
    files: &[DeployFile],
    name: &str,
) -> Result<String, String> {
    let api = base("CONCIERGE_DEPLOY_NETLIFY_BASE", "https://api.netlify.com");
    let cl = client();

    // Resolve the target site: the stored id, else an existing site with this name (so
    // re-publishing OVERWRITES it instead of failing "subdomain must be unique"), else a
    // freshly created one.
    let site_name = netlify_site_name(name);
    let site_id = match &c.site_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => match netlify_find_site(&cl, &api, &c.token, &site_name)? {
            Some(id) => id,
            None => {
                let site = ok_json(
                    cl.post(format!("{api}/api/v1/sites"))
                        .bearer_auth(&c.token)
                        .json(&serde_json::json!({ "name": site_name })),
                    "netlify create site",
                )?;
                site.get("id")
                    .and_then(|s| s.as_str())
                    .ok_or("netlify: no site id")?
                    .to_string()
            }
        },
    };

    // Digest deploy (exactly what the Netlify CLI does): declare every file by the SHA1
    // of its contents, then upload only the ones Netlify asks for. Netlify infers the
    // Content-Type from each file's path — so index.html is served as text/html. (The
    // older zip deploy could mis-serve the page as text/plain, showing raw source.)
    let mut by_digest: std::collections::HashMap<String, &DeployFile> =
        std::collections::HashMap::new();
    let mut manifest = serde_json::Map::new();
    for f in files {
        let sha = sha1_hex(&f.bytes);
        manifest.insert(
            format!("/{}", f.rel),
            serde_json::Value::String(sha.clone()),
        );
        by_digest.insert(sha, f);
    }
    let deploy = ok_json(
        cl.post(format!("{api}/api/v1/sites/{site_id}/deploys"))
            .bearer_auth(&c.token)
            .json(&serde_json::json!({ "files": manifest })),
        "netlify deploy",
    )?;
    let deploy_id = deploy
        .get("id")
        .and_then(|s| s.as_str())
        .ok_or("netlify: no deploy id")?;
    // Netlify returns the SHA1s it still needs (others are already stored/deduped).
    if let Some(required) = deploy.get("required").and_then(|r| r.as_array()) {
        for sha in required.iter().filter_map(|s| s.as_str()) {
            let Some(f) = by_digest.get(sha) else {
                continue;
            };
            let resp = cl
                .put(format!("{api}/api/v1/deploys/{deploy_id}/files/{}", f.rel))
                .bearer_auth(&c.token)
                .header("Content-Type", "application/octet-stream")
                .body(f.bytes.clone())
                .send()
                .map_err(|e| format!("netlify upload {}: {e}", f.rel))?;
            if !resp.status().is_success() {
                return Err(format!(
                    "netlify upload {}: HTTP {} — {}",
                    f.rel,
                    resp.status().as_u16(),
                    clip(&resp.text().unwrap_or_default())
                ));
            }
        }
    }
    let url = deploy
        .get("ssl_url")
        .or_else(|| deploy.get("deploy_ssl_url"))
        .or_else(|| deploy.get("url"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://{name}.netlify.app"));
    Ok(url)
}

#[allow(dead_code)]
/// Find an existing Netlify site by its exact name (subdomain), so a re-publish reuses
/// it. `?name=` is a substring filter, so we confirm an exact match. `None` if none.
fn netlify_find_site(
    cl: &reqwest::blocking::Client,
    api: &str,
    token: &str,
    name: &str,
) -> Result<Option<String>, String> {
    let resp = cl
        .get(format!("{api}/api/v1/sites?name={name}&per_page=100"))
        .bearer_auth(token)
        .send()
        .map_err(|e| format!("netlify list sites: {e}"))?;
    if !resp.status().is_success() {
        // Listing failed (e.g. token scope); fall through to create.
        return Ok(None);
    }
    let sites: serde_json::Value = resp
        .json()
        .map_err(|e| format!("netlify list sites: {e}"))?;
    Ok(sites.as_array().and_then(|arr| {
        arr.iter()
            .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|s| s.get("id").and_then(|i| i.as_str()).map(String::from))
    }))
}

#[cfg(test)] // a legacy zip path kept only for the deploy-format regression test
fn zip_files(files: &[DeployFile]) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for f in files {
            zw.start_file(&f.rel, opts)
                .map_err(|e| format!("zip: {e}"))?;
            zw.write_all(&f.bytes)
                .map_err(|e| format!("zip write: {e}"))?;
        }
        zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    }
    Ok(buf)
}

// ── Vercel (upload files by SHA1 + create deployment) ────────────────────────

pub fn deploy_vercel(c: &VercelCreds, files: &[DeployFile], name: &str) -> Result<String, String> {
    let api = base("CONCIERGE_DEPLOY_VERCEL_BASE", "https://api.vercel.com");
    let cl = client();
    let team = c.team_id.as_deref().filter(|t| !t.is_empty());
    let q = |path: &str| {
        let sep = if path.contains('?') { '&' } else { '?' };
        match team {
            Some(t) => format!("{api}{path}{sep}teamId={t}"),
            None => format!("{api}{path}"),
        }
    };

    let mut manifest = Vec::new();
    for f in files {
        let sha = sha1_hex(&f.bytes);
        let resp = cl
            .post(q("/v2/files"))
            .bearer_auth(&c.token)
            .header("Content-Type", "application/octet-stream")
            .header("x-vercel-digest", &sha)
            .header("Content-Length", f.bytes.len().to_string())
            .body(f.bytes.clone())
            .send()
            .map_err(|e| format!("vercel upload {}: {e}", f.rel))?;
        if !resp.status().is_success() {
            return Err(format!(
                "vercel upload {}: HTTP {} — {}",
                f.rel,
                resp.status().as_u16(),
                clip(&resp.text().unwrap_or_default())
            ));
        }
        manifest.push(serde_json::json!({ "file": f.rel, "sha": sha, "size": f.bytes.len() }));
    }

    let project = vercel_project_name(
        &c.project
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| name.to_string()),
    );
    let deployment = ok_json(
        cl.post(q("/v13/deployments?skipAutoDetectionConfirmation=1"))
            .bearer_auth(&c.token)
            .json(&serde_json::json!({
                "name": project,
                "files": manifest,
                "projectSettings": { "framework": serde_json::Value::Null },
                "target": "production",
            })),
        "vercel deployment",
    )?;
    let url = deployment
        .get("url")
        .and_then(|s| s.as_str())
        .map(|u| format!("https://{u}"))
        .or_else(|| {
            deployment
                .get("alias")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.as_str())
                .map(|u| format!("https://{u}"))
        })
        .ok_or("vercel: no deployment url")?;
    Ok(url)
}

fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ── Cloudflare Pages (direct upload) ─────────────────────────────────────────

// ── Cloudflare one-click OAuth (PKCE) — reuses the public Wrangler OAuth client, the
// same way third-party CLI tools authenticate, so the user never pastes a token. ──
const CF_OAUTH_CLIENT_ID: &str = "54d11594-84e4-41aa-b438-e81b8fa78ee7";
const CF_OAUTH_REDIRECT: &str = "http://localhost:8976/oauth/callback";
const CF_OAUTH_AUTH: &str = "https://dash.cloudflare.com/oauth2/auth";
const CF_OAUTH_TOKEN_URL: &str = "https://dash.cloudflare.com/oauth2/token";
const CF_OAUTH_SCOPES: &str = "account:read user:read pages:write zone:read offline_access";
/// The exact localhost redirect Cloudflare registered for the Wrangler client.
pub const CF_OAUTH_CALLBACK_ADDR: &str = "127.0.0.1:8976";

/// Everything the GUI needs to drive a login: the URL to open, plus the PKCE verifier
/// and CSRF state it must hold to finish the exchange.
pub struct OAuthStart {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
}

/// Begin a Cloudflare PKCE login: fresh verifier + challenge + state + authorize URL.
pub fn cloudflare_oauth_start() -> OAuthStart {
    let verifier = random_b64url(64);
    let state = random_b64url(24);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    OAuthStart {
        authorize_url: cloudflare_oauth_authorize_url(&challenge, &state),
        verifier,
        state,
    }
}

fn random_b64url(bytes: usize) -> String {
    use rand_core::RngCore;
    let mut buf = vec![0u8; bytes];
    rand_core::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// One-click OAuth access token (+ refresh token and absolute expiry).
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

/// Build the Cloudflare authorize URL for a PKCE login (open this in the browser).
pub fn cloudflare_oauth_authorize_url(code_challenge: &str, state: &str) -> String {
    format!(
        "{CF_OAUTH_AUTH}?response_type=code&client_id={CF_OAUTH_CLIENT_ID}&redirect_uri={redirect}&scope={scope}&state={state}&code_challenge={code_challenge}&code_challenge_method=S256",
        redirect = urlencode(CF_OAUTH_REDIRECT),
        scope = urlencode(CF_OAUTH_SCOPES),
    )
}

/// Exchange the authorization `code` (+ the PKCE `verifier`) for an access token.
pub fn cloudflare_oauth_exchange(code: &str, verifier: &str) -> Result<OAuthToken, String> {
    let resp = client()
        .post(CF_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CF_OAUTH_REDIRECT),
            ("client_id", CF_OAUTH_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|e| format!("cloudflare token exchange: {e}"))?;
    parse_oauth_token(resp)
}

/// Refresh an expired access token using the stored refresh token.
pub fn cloudflare_oauth_refresh(refresh_token: &str) -> Result<OAuthToken, String> {
    let resp = client()
        .post(CF_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CF_OAUTH_CLIENT_ID),
        ])
        .send()
        .map_err(|e| format!("cloudflare token refresh: {e}"))?;
    parse_oauth_token(resp)
}

fn parse_oauth_token(resp: reqwest::blocking::Response) -> Result<OAuthToken, String> {
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("HTTP {} — {}", status.as_u16(), clip(&body)));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse token response: {e}"))?;
    let access_token = value
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("cloudflare: no access_token in response")?
        .to_string();
    let refresh_token = value
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(String::from);
    // Renew a minute early to stay ahead of clock skew.
    let expires_at = value
        .get("expires_in")
        .and_then(|x| x.as_u64())
        .map(|secs| now_secs() + secs.saturating_sub(60));
    Ok(OAuthToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

/// The first account id this token can see (`account:read`), so the user never pastes one.
pub fn cloudflare_list_account_id(token: &str) -> Result<String, String> {
    let api = base(
        "CONCIERGE_DEPLOY_CLOUDFLARE_BASE",
        "https://api.cloudflare.com/client/v4",
    );
    let value = ok_json(
        client()
            .get(format!("{api}/accounts?per_page=1"))
            .bearer_auth(token),
        "cloudflare accounts",
    )?;
    value
        .get("result")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("id"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .ok_or_else(|| "cloudflare: no account is visible to this token".to_string())
}

/// Percent-encode a string for a URL query value (unreserved chars pass through).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn deploy_cloudflare(
    c: &CloudflareCreds,
    files: &[DeployFile],
    site_name: &str,
) -> Result<String, String> {
    let api = base(
        "CONCIERGE_DEPLOY_CLOUDFLARE_BASE",
        "https://api.cloudflare.com/client/v4",
    );
    let cl = client();
    // With OAuth the account id is blank — auto-detect it; the project falls back to the
    // (sanitized) site name when not explicitly set.
    let acct = if c.account_id.trim().is_empty() {
        cloudflare_list_account_id(&c.token)?
    } else {
        c.account_id.trim().to_string()
    };
    let proj = cf_project_name(if c.project.trim().is_empty() {
        site_name
    } else {
        &c.project
    });

    // Ensure the project exists. A failure here is the real error (bad/insufficient API
    // token, wrong Account ID, invalid name) — surface it clearly instead of letting the
    // downstream upload-token step fail with a cryptic "9106 Authentication failed".
    // "Already exists" (code 8000007) is expected and fine.
    let permission_hint = " — the API token needs Account → Cloudflare Pages → Edit, and the Account ID must match (use an API Token from the dashboard, not the Global API Key).";
    match cl
        .post(format!("{api}/accounts/{acct}/pages/projects"))
        .bearer_auth(&c.token)
        .json(&serde_json::json!({ "name": proj, "production_branch": "main" }))
        .send()
    {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                if !body.contains("8000007") && !body.to_lowercase().contains("already") {
                    return Err(format!(
                        "cloudflare create project: HTTP {} — {}{}",
                        status.as_u16(),
                        clip(&body),
                        if matches!(status.as_u16(), 400 | 401 | 403) {
                            permission_hint
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
        Err(error) => return Err(format!("cloudflare create project: {error}")),
    }

    // Upload JWT.
    let token_resp = ok_json(
        cl.get(format!(
            "{api}/accounts/{acct}/pages/projects/{proj}/upload-token"
        ))
        .bearer_auth(&c.token),
        "cloudflare upload-token",
    )
    .map_err(|error| format!("{error}{permission_hint}"))?;
    let jwt = token_resp
        .get("result")
        .and_then(|r| r.get("jwt"))
        .and_then(|s| s.as_str())
        .ok_or("cloudflare: no upload jwt")?
        .to_string();

    // Hash each file: blake3(content ++ extension), first 32 hex chars.
    let mut manifest = serde_json::Map::new();
    let mut uploads = Vec::new();
    let mut hashes = Vec::new();
    for f in files {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&f.bytes);
        hasher.update(extension(&f.rel).as_bytes());
        let hash: String = hasher.finalize().to_hex().chars().take(32).collect();
        manifest.insert(
            format!("/{}", f.rel),
            serde_json::Value::String(hash.clone()),
        );
        uploads.push(serde_json::json!({
            "key": hash, "value": b64(&f.bytes),
            "metadata": { "contentType": f.content_type }, "base64": true,
        }));
        hashes.push(hash);
    }

    // Upload assets (JWT-authed). Chunk under ~40MB of base64 per request.
    let mut batch = Vec::new();
    let mut batch_bytes = 0usize;
    for item in uploads {
        let sz = item.to_string().len();
        if batch_bytes + sz > 40_000_000 && !batch.is_empty() {
            cf_upload(&cl, &api, &jwt, &batch)?;
            batch.clear();
            batch_bytes = 0;
        }
        batch_bytes += sz;
        batch.push(item);
    }
    if !batch.is_empty() {
        cf_upload(&cl, &api, &jwt, &batch)?;
    }

    // Register the full manifest of hashes.
    ok_json(
        cl.post(format!("{api}/pages/assets/upsert-hashes"))
            .bearer_auth(&jwt)
            .json(&serde_json::json!({ "hashes": hashes })),
        "cloudflare upsert-hashes",
    )?;

    // Create the deployment with a multipart `manifest` (path → hash).
    let form = reqwest::blocking::multipart::Form::new()
        .text("manifest", serde_json::Value::Object(manifest).to_string());
    ok_json(
        cl.post(format!(
            "{api}/accounts/{acct}/pages/projects/{proj}/deployments"
        ))
        .bearer_auth(&c.token)
        .multipart(form),
        "cloudflare deployment",
    )?;
    Ok(format!("https://{proj}.pages.dev"))
}

fn cf_upload(
    cl: &reqwest::blocking::Client,
    api: &str,
    jwt: &str,
    batch: &[serde_json::Value],
) -> Result<(), String> {
    ok_json(
        cl.post(format!("{api}/pages/assets/upload"))
            .bearer_auth(jwt)
            .json(&serde_json::Value::Array(batch.to_vec())),
        "cloudflare upload",
    )?;
    Ok(())
}

// ── Firebase Hosting (service-account JWT → version → populate → upload → release) ─

/// The fields we need from a Google service-account JSON key.
#[derive(Debug, Clone, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}
fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

/// Whether `json` is a usable Google service-account key (has client_email +
/// private_key). Used by the credential validator before anything is stored.
pub fn is_service_account_key(json: &str) -> bool {
    parse_service_account(json).is_ok()
}

fn parse_service_account(json: &str) -> Result<ServiceAccount, String> {
    let sa: ServiceAccount = serde_json::from_str(json.trim())
        .map_err(|e| format!("firebase: invalid service-account JSON: {e}"))?;
    if sa.client_email.trim().is_empty() || sa.private_key.trim().is_empty() {
        return Err("firebase: service-account JSON is missing client_email or private_key".into());
    }
    Ok(sa)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).map_err(|e| format!("gzip: {e}"))?;
    enc.finish().map_err(|e| format!("gzip finish: {e}"))
}

/// Mint a short-lived Google OAuth access token from a service account by signing a
/// JWT (RS256) with its private key and exchanging it at the token endpoint.
fn firebase_access_token(
    cl: &reqwest::blocking::Client,
    sa: &ServiceAccount,
) -> Result<String, String> {
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::{Pkcs1v15Sign, RsaPrivateKey};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("firebase: clock error: {e}"))?
        .as_secs();
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = serde_json::json!({
        "iss": sa.client_email,
        "scope": "https://www.googleapis.com/auth/firebase.hosting",
        "aud": sa.token_uri,
        "iat": now,
        "exp": now + 3600,
    });
    let claims = b64url(
        &serde_json::to_vec(&claims).map_err(|e| format!("firebase: encode JWT claims: {e}"))?,
    );
    let signing_input = format!("{header}.{claims}");

    let key = RsaPrivateKey::from_pkcs8_pem(sa.private_key.trim())
        .map_err(|e| format!("firebase: parse service-account private key: {e}"))?;
    let digest = Sha256::digest(signing_input.as_bytes());
    let sig = key
        .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
        .map_err(|e| format!("firebase: sign JWT: {e}"))?;
    let jwt = format!("{signing_input}.{}", b64url(&sig));

    // `base` lets mock tests point the exchange at a local server in test builds.
    let token_uri = base("CONCIERGE_DEPLOY_FIREBASE_TOKEN", &sa.token_uri);
    let resp = ok_json(
        cl.post(&token_uri).form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
        ]),
        "firebase token exchange",
    )?;
    resp.get("access_token")
        .and_then(|s| s.as_str())
        .map(String::from)
        .ok_or_else(|| "firebase: no access_token in token response".to_string())
}

// ── Firebase one-click OAuth (Google login, PKCE) — reuses the public Firebase CLI
// OAuth client exactly as `firebase login` does, so the user never makes a service
// account. The Firebase CLI client is a Google "Desktop" client, which allows a
// loopback redirect on any local port (the GUI binds an ephemeral one). ──
const FB_OAUTH_CLIENT_ID: &str =
    "563584335869-fgrhgmd47bqnekij5i8b5pr03ho849e6.apps.googleusercontent.com";
const FB_OAUTH_CLIENT_SECRET: &str = "j9iVZfS8kkCEFUPaAeJV0sAi";
const FB_OAUTH_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const FB_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const FB_OAUTH_SCOPES: &str =
    "openid email https://www.googleapis.com/auth/firebase https://www.googleapis.com/auth/cloud-platform";

/// Begin a Firebase (Google) PKCE login for the given loopback `redirect_uri`.
pub fn firebase_oauth_start(redirect_uri: &str) -> OAuthStart {
    let verifier = random_b64url(64);
    let state = random_b64url(24);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let authorize_url = format!(
        "{FB_OAUTH_AUTH}?response_type=code&client_id={FB_OAUTH_CLIENT_ID}&redirect_uri={redirect}&scope={scope}&state={state}&code_challenge={challenge}&code_challenge_method=S256&access_type=offline&prompt=consent",
        redirect = urlencode(redirect_uri),
        scope = urlencode(FB_OAUTH_SCOPES),
    );
    OAuthStart {
        authorize_url,
        verifier,
        state,
    }
}

/// Exchange the authorization code for a Google access + refresh token.
pub fn firebase_oauth_exchange(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthToken, String> {
    let resp = client()
        .post(FB_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", FB_OAUTH_CLIENT_ID),
            ("client_secret", FB_OAUTH_CLIENT_SECRET),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|e| format!("firebase token exchange: {e}"))?;
    parse_oauth_token(resp)
}

/// Refresh an expired Google access token (Google keeps the same refresh token).
pub fn firebase_oauth_refresh(refresh_token: &str) -> Result<OAuthToken, String> {
    let resp = client()
        .post(FB_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", FB_OAUTH_CLIENT_ID),
            ("client_secret", FB_OAUTH_CLIENT_SECRET),
        ])
        .send()
        .map_err(|e| format!("firebase token refresh: {e}"))?;
    parse_oauth_token(resp)
}

/// The first Firebase project's id (= its default Hosting site), so the user never
/// pastes a site id. Uses the `firebase` scope granted at login.
pub fn firebase_default_site(access_token: &str) -> Result<String, String> {
    let api = base(
        "CONCIERGE_DEPLOY_FIREBASE_PROJECTS_BASE",
        "https://firebase.googleapis.com",
    );
    let value = ok_json(
        client()
            .get(format!("{api}/v1beta1/projects?pageSize=1"))
            .bearer_auth(access_token),
        "firebase projects",
    )?;
    value
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|p| p.get("projectId"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .ok_or_else(|| {
            "firebase: no project found — create one at console.firebase.google.com".to_string()
        })
}

pub fn deploy_firebase(c: &FirebaseCreds, files: &[DeployFile]) -> Result<String, String> {
    let api = base(
        "CONCIERGE_DEPLOY_FIREBASE_BASE",
        "https://firebasehosting.googleapis.com",
    );
    let cl = client();
    // OAuth one-click login provides the access token directly (refreshed upstream);
    // otherwise mint one from the service-account key.
    let token = match c.access_token.as_ref().filter(|t| !t.is_empty()) {
        Some(access_token) => access_token.clone(),
        None => firebase_access_token(&cl, &parse_service_account(&c.service_account)?)?,
    };
    // With OAuth the site id is blank — auto-detect the default Hosting site.
    let site = if c.site_id.trim().is_empty() {
        firebase_default_site(&token)?
    } else {
        c.site_id.trim().to_string()
    };

    // 1. Create a new (unfinalized) version for the site.
    let version = ok_json(
        cl.post(format!("{api}/v1beta1/sites/{site}/versions"))
            .bearer_auth(&token)
            .json(&serde_json::json!({})),
        "firebase create version",
    )?;
    let version_name = version
        .get("name")
        .and_then(|s| s.as_str())
        .ok_or("firebase: version response had no name")?
        .to_string();

    // 2. Hash each file as SHA256 of its gzipped content; keep the gzip for upload.
    let mut files_map = serde_json::Map::new();
    let mut by_hash: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    for f in files {
        let gz = gzip(&f.bytes)?;
        let hash = sha256_hex(&gz);
        let path = if f.rel.starts_with('/') {
            f.rel.clone()
        } else {
            format!("/{}", f.rel)
        };
        files_map.insert(path, serde_json::Value::String(hash.clone()));
        by_hash.insert(hash, gz);
    }

    // 3. Declare the files; Firebase replies with the upload URL + which to send.
    let populated = ok_json(
        cl.post(format!("{api}/v1beta1/{version_name}:populateFiles"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "files": files_map })),
        "firebase populateFiles",
    )?;
    let upload_url = populated
        .get("uploadUrl")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    let required: Vec<String> = populated
        .get("uploadRequiredHashes")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|h| h.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // 4. Upload each required (gzipped) file to {uploadUrl}/{hash}.
    if !required.is_empty() && upload_url.is_empty() {
        return Err("firebase: populateFiles asked for uploads but gave no uploadUrl".into());
    }
    for hash in &required {
        let gz = by_hash
            .get(hash)
            .ok_or_else(|| format!("firebase: server requested an unknown file hash {hash}"))?;
        let resp = cl
            .post(format!("{upload_url}/{hash}"))
            .bearer_auth(&token)
            .header("Content-Type", "application/octet-stream")
            .body(gz.clone())
            .send()
            .map_err(|e| format!("firebase upload {hash}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "firebase upload {hash}: HTTP {} — {}",
                resp.status().as_u16(),
                clip(&resp.text().unwrap_or_default())
            ));
        }
    }

    // 5. Finalize the version (no more files accepted after this).
    ok_json(
        cl.patch(format!("{api}/v1beta1/{version_name}?update_mask=status"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "status": "FINALIZED" })),
        "firebase finalize version",
    )?;

    // 6. Release the finalized version live.
    ok_json(
        cl.post(format!(
            "{api}/v1beta1/sites/{site}/releases?versionName={version_name}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({})),
        "firebase create release",
    )?;

    Ok(format!("https://{site}.web.app"))
}

// ── InfinityFree / generic FTP ───────────────────────────────────────────────

pub fn deploy_ftp(c: &FtpCreds, files: &[DeployFile]) -> Result<String, String> {
    let _ = (c, files);
    Err(
        "plaintext FTP deployment is disabled; use an authenticated HTTPS deployment platform"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_collects_files_with_relative_paths_and_types() {
        let dir = std::env::temp_dir().join(format!("ccg-deploy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log(1)").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "x").unwrap();

        let files = walk_files(&dir).unwrap();
        let rels: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert!(rels.contains(&"index.html"));
        assert!(rels.contains(&"assets/app.js"));
        assert!(!rels.iter().any(|r| r.contains(".git")), "VCS skipped");
        let html = files.iter().find(|f| f.rel == "index.html").unwrap();
        assert!(html.content_type.starts_with("text/html"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_rejects_sensitive_files_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.production"), "TOKEN=secret").unwrap();
        let error = walk_files(dir.path()).unwrap_err();
        assert!(error.contains("sensitive-looking"));

        std::fs::remove_file(dir.path().join(".env.production")).unwrap();
        std::fs::write(
            dir.path().join("large.bin"),
            vec![0; (MAX_DEPLOY_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        let error = walk_files(dir.path()).unwrap_err();
        assert!(error.contains("size limit"));
    }

    #[cfg(unix)]
    #[test]
    fn walk_rejects_file_and_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let site = tempfile::tempdir().unwrap();
        std::fs::write(site.path().join("index.html"), "ok").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            site.path().join("linked.txt"),
        )
        .unwrap();
        assert!(walk_files(site.path()).unwrap_err().contains("symlink"));

        std::fs::remove_file(site.path().join("linked.txt")).unwrap();
        symlink(outside.path(), site.path().join("linked-dir")).unwrap();
        assert!(walk_files(site.path()).unwrap_err().contains("symlink"));
    }

    #[test]
    fn deployment_manifest_is_deterministic_and_content_bound() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "one").unwrap();
        let files = walk_files(dir.path()).unwrap();
        let first =
            SiteDeployPlan::from_files("site", dir.path(), "site", "github", "dest", &files)
                .unwrap();
        let second =
            SiteDeployPlan::from_files("site", dir.path(), "site", "github", "dest", &files)
                .unwrap();
        assert_eq!(first, second);

        std::fs::write(dir.path().join("index.html"), "two").unwrap();
        let changed = SiteDeployPlan::from_files(
            "site",
            dir.path(),
            "site",
            "github",
            "dest",
            &walk_files(dir.path()).unwrap(),
        )
        .unwrap();
        assert_ne!(first.manifest_digest, changed.manifest_digest);
    }

    #[test]
    fn plaintext_ftp_is_disabled() {
        assert!(deploy_ftp(
            &FtpCreds {
                host: "example.com".into(),
                user: "u".into(),
                password: "p".into(),
                dir: "htdocs".into(),
                site_url: None,
            },
            &[]
        )
        .unwrap_err()
        .contains("disabled"));
    }

    #[test]
    fn cloudflare_oauth_url_is_well_formed() {
        let start = cloudflare_oauth_start();
        let url = &start.authorize_url;
        assert!(
            url.starts_with("https://dash.cloudflare.com/oauth2/auth?"),
            "{url}"
        );
        assert!(
            url.contains("client_id=54d11594-84e4-41aa-b438-e81b8fa78ee7"),
            "{url}"
        );
        assert!(
            url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8976%2Foauth%2Fcallback"),
            "{url}"
        );
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert!(url.contains(&format!("state={}", start.state)), "{url}");
        assert!(url.contains("scope=account%3Aread"), "{url}");
        // The challenge in the URL must be base64url-nopad(sha256(verifier)).
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(start.verifier.as_bytes()));
        assert!(url.contains(&format!("code_challenge={expect}")), "{url}");
        assert!(!start.verifier.is_empty() && start.verifier != start.state);
    }

    #[test]
    fn firebase_oauth_url_is_well_formed() {
        let redirect = "http://127.0.0.1:51234";
        let start = firebase_oauth_start(redirect);
        let url = &start.authorize_url;
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );
        assert!(url.contains("client_id=563584335869-"), "{url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A51234"),
            "{url}"
        );
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert!(url.contains("access_type=offline"), "{url}");
        assert!(url.contains("scope=openid"), "{url}");
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(start.verifier.as_bytes()));
        assert!(url.contains(&format!("code_challenge={expect}")), "{url}");
    }

    #[test]
    fn provider_names_are_sanitized() {
        // Vercel: lowercase, allowed punctuation kept, '---' collapsed, edges trimmed.
        assert_eq!(
            vercel_project_name("ConciergeSideKick"),
            "conciergesidekick"
        );
        assert_eq!(vercel_project_name("My Site!"), "my-site");
        assert_eq!(vercel_project_name("a---b"), "a--b");
        assert_eq!(vercel_project_name("--Hello.World_1--"), "hello.world_1");
        assert_eq!(vercel_project_name("***"), "site");
        // Netlify: only a-z0-9 + single hyphens, trimmed.
        assert_eq!(netlify_site_name("ConciergeSideKick"), "conciergesidekick");
        assert_eq!(netlify_site_name("My Cool Site"), "my-cool-site");
        assert_eq!(netlify_site_name("a..b__c"), "a-b-c");
        // Cloudflare Pages: same lowercase/hyphen rule, ≤58.
        assert_eq!(cf_project_name("ConciergeSideKick"), "conciergesidekick");
        assert_eq!(cf_project_name("My.Cool_Site!"), "my-cool-site");
        assert_eq!(cf_project_name("-Edge-"), "edge");
        assert!(vercel_project_name("ConciergeSideKick")
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')));
    }

    #[test]
    fn zip_round_trips() {
        let files = vec![DeployFile {
            rel: "index.html".into(),
            bytes: b"<h1>x</h1>".to_vec(),
            content_type: "text/html".into(),
        }];
        let z = zip_files(&files).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(z)).unwrap();
        assert_eq!(archive.len(), 1);
        let entry = archive.by_index(0).unwrap();
        assert_eq!(entry.name(), "index.html");
    }

    #[test]
    fn sha1_and_blake3_hashing_are_stable() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let mut h = blake3::Hasher::new();
        h.update(b"<h1>x</h1>");
        h.update(b"html");
        let cf: String = h.finalize().to_hex().chars().take(32).collect();
        assert_eq!(cf.len(), 32);
        assert!(cf.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn extension_and_content_type() {
        assert_eq!(extension("a/b/app.JS"), "js");
        assert_eq!(extension("noext"), "");
        assert_eq!(content_type("x.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("x.unknown"), "application/octet-stream");
    }
}

/// Mock-server tests: each platform flow is driven against a local `TcpListener`
/// (base URL injected via `CONCIERGE_DEPLOY_<X>_BASE`), asserting the right API
/// sequence runs and the live URL is parsed from canned responses. Kept in one
/// `#[test]` so the process-global base env vars never race with other tests.
#[cfg(test)]
mod mock_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, String)>>>; // (method, path)

    /// Spawn a mock HTTP server that serves exactly `n` requests via `handler`
    /// (method, path, body) -> (status, json_body). Returns its base URL + the
    /// captured (method, path) log + the join handle.
    fn spawn_mock<F>(n: usize, handler: F) -> (String, Seen, std::thread::JoinHandle<()>)
    where
        F: Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..n {
                let (mut stream, _) = listener.accept().unwrap();
                let (method, path, body) = read_request(&mut stream);
                seen2.lock().unwrap().push((method.clone(), path.clone()));
                let (status, json) = handler(&method, &path, &body);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (base, seen, handle)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, String, String) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                let cl = head
                    .lines()
                    .find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = buf[pos + 4..].to_vec();
                while body.len() < cl {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                let first = head.lines().next().unwrap_or("");
                let mut p = first.split_whitespace();
                return (
                    p.next().unwrap_or("").to_string(),
                    p.next().unwrap_or("").to_string(),
                    String::from_utf8_lossy(&body).to_string(),
                );
            }
        }
        (String::new(), String::new(), String::new())
    }

    fn one_file() -> Vec<DeployFile> {
        vec![DeployFile {
            rel: "index.html".into(),
            bytes: b"<h1>hi</h1>".to_vec(),
            content_type: "text/html; charset=utf-8".into(),
        }]
    }

    #[test]
    fn mock_protocols() {
        // ── Netlify: look up existing site (none) → create → digest deploy → upload ──
        let (base, seen, h) = spawn_mock(4, |method, path, b| {
            if path.contains("/files/") {
                (200, "{}".into())
            } else if path.ends_with("/deploys") {
                // Echo the declared digest back as "required" so the upload path runs.
                let v: serde_json::Value = serde_json::from_str(b).unwrap_or_default();
                let sha = v
                    .get("files")
                    .and_then(|f| f.as_object())
                    .and_then(|o| o.values().next())
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                (
                    200,
                    format!(
                        r#"{{"id":"dep1","ssl_url":"https://my.netlify.app","required":["{sha}"]}}"#
                    ),
                )
            } else if method == "GET" && path.contains("/sites") {
                (200, "[]".into()) // no existing site with this name → create one
            } else {
                (200, r#"{"id":"site123"}"#.into())
            }
        });
        std::env::set_var("CONCIERGE_DEPLOY_NETLIFY_BASE", &base);
        let url = deploy_netlify(
            &NetlifyCreds {
                token: "t".into(),
                site_id: None,
            },
            &one_file(),
            "mysite",
        )
        .unwrap();
        assert_eq!(url, "https://my.netlify.app");
        h.join().unwrap();
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("/api/v1/sites")));
        assert!(paths.iter().any(|p| p.contains("/sites/site123/deploys")));
        assert!(paths
            .iter()
            .any(|p| p.contains("/deploys/dep1/files/index.html")));
        std::env::remove_var("CONCIERGE_DEPLOY_NETLIFY_BASE");

        // ── GitHub: ref(404) → blob → tree → commit → create-ref → pages(404) → enable ──
        let (base, seen, h) = spawn_mock(7, |method, path, _b| {
            if path.ends_with("/git/ref/heads/gh-pages") {
                (404, "{}".into())
            } else if path.ends_with("/git/blobs") {
                (201, r#"{"sha":"blobsha"}"#.into())
            } else if path.ends_with("/git/trees") {
                (201, r#"{"sha":"treesha"}"#.into())
            } else if path.ends_with("/git/commits") {
                (201, r#"{"sha":"commitsha"}"#.into())
            } else if path.ends_with("/git/refs") {
                (201, "{}".into())
            } else if path.ends_with("/pages") && method == "GET" {
                (404, "{}".into())
            } else {
                (201, r#"{"html_url":"https://o.github.io/r/"}"#.into())
            }
        });
        std::env::set_var("CONCIERGE_DEPLOY_GITHUB_BASE", &base);
        let url = deploy_github(
            &GithubCreds {
                token: "t".into(),
                owner: "o".into(),
                repo: "r".into(),
                branch: "gh-pages".into(),
            },
            &one_file(),
        )
        .unwrap();
        assert_eq!(url, "https://o.github.io/r/");
        h.join().unwrap();
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("/git/blobs")));
        assert!(paths.iter().any(|p| p.ends_with("/git/commits")));
        std::env::remove_var("CONCIERGE_DEPLOY_GITHUB_BASE");

        // ── Vercel: upload file (sha1) → create deployment → url ──
        let (base, seen, h) = spawn_mock(2, |_m, path, _b| {
            if path.starts_with("/v2/files") {
                (200, "{}".into())
            } else {
                (200, r#"{"url":"proj-abc.vercel.app"}"#.into())
            }
        });
        std::env::set_var("CONCIERGE_DEPLOY_VERCEL_BASE", &base);
        let url = deploy_vercel(
            &VercelCreds {
                token: "t".into(),
                project: Some("proj".into()),
                team_id: None,
            },
            &one_file(),
            "proj",
        )
        .unwrap();
        assert_eq!(url, "https://proj-abc.vercel.app");
        h.join().unwrap();
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert!(paths.iter().any(|p| p.starts_with("/v2/files")));
        assert!(paths.iter().any(|p| p.contains("/v13/deployments")));
        std::env::remove_var("CONCIERGE_DEPLOY_VERCEL_BASE");

        // ── Cloudflare: ensure-project → token → upload → upsert → deployment ──
        let (base, seen, h) = spawn_mock(5, |_m, path, _b| {
            if path.ends_with("/upload-token") {
                (200, r#"{"result":{"jwt":"jwttoken"}}"#.into())
            } else {
                (200, r#"{"result":{}}"#.into())
            }
        });
        std::env::set_var("CONCIERGE_DEPLOY_CLOUDFLARE_BASE", &base);
        let url = deploy_cloudflare(
            &CloudflareCreds {
                token: "t".into(),
                account_id: "acct".into(),
                project: "proj".into(),
                refresh_token: None,
                expires_at: None,
            },
            &one_file(),
            "proj",
        )
        .unwrap();
        assert_eq!(url, "https://proj.pages.dev");
        h.join().unwrap();
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("/upload-token")));
        assert!(paths.iter().any(|p| p.ends_with("/pages/assets/upload")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("/pages/assets/upsert-hashes")));
        assert!(paths.iter().any(|p| p.ends_with("/deployments")));
        std::env::remove_var("CONCIERGE_DEPLOY_CLOUDFLARE_BASE");
    }

    /// A throwaway 2048-bit PKCS#8 RSA key, used only to drive the JWT-signing path
    /// in the Firebase mock (never a real credential).
    const TEST_RSA_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDCQC4PQthwC+5T
Wd03kymu20qy/B8scgYVnTlrfAGOG0RmbTJmW38ePMuFacRVbBbQA81gHMRf0idU
du3kK4VCGcyAY9yanK6jqmKxcOiLnQwTuA3ojruMriiKiMmVaryUmOvTmNDnsmuT
1pDw5A1w4X6MTcJBG7nW/66UWbbn0G1PNcvrYEHDk2cJ/HE2xCXqBoXvP3QxJ7YB
aQSTEJJNbXMMLFQCLbkGhGyV+J6tvI01luandxOKCz/cHXWtQ4wQjEhDWjhW2F7L
+K2zV1Ra69cMpToJhwOfd3nLgEmJXqd5UE5abVUEuTwZhHlDC0tYJdtm48CoLORi
LaoMtiYBAgMBAAECggEAAtTxgPQjnG5NmJwpfJaz+ZRBhH6GwDC3Ok3f/zNMEOGw
kY1Rag7m2XqXmVXGbJwAUQLxrb9MnPSe8XkggKJSaXlPnw5pohzDMmBMuxdNMhIZ
gcD13bJVPUzBizu5U9j2B+Tq8PM4Fi0eiq2q0DK0aBUm+YnViWpDd0UT4w5z41Eq
35QO/blaaT6hHQvPFQlORf0S8+iCavw73kjaReNhQXYKEKxPTEZEJKRziAosqn9y
aPWj04PDu9c2stJRvmbAZkhxCwxgJkZWKCOwkXO3twAEMCnjGD3BQLt8ToKR5bsH
Hpq649kweICowyPFgQoAsQGnox06oBMv312UeEpBgQKBgQDtTpG+MOiTqbXWTYgR
VOv63/YracjmRcxfJ9OKS+SeppMq2L2MIkj5zpTkey6nlC4QGJtpXoJVqGuo/seg
VAxrbaLtKLkdKr25TjN/R/LMnxRlfj4rJLgX39J1tR58XyIbzjZ3uTWAyNsREBGB
KOx2HKDjzbvxkID2oCxzYC/+wQKBgQDRjVt6v+1D2ZBM1ubfHZoWpYYB2g2Of6A1
NYPPo5CPvbd7yGMf1veROy0OAZA1gKLrXaDfCVgVjDqSClqylOTR7xOjTVxEAKiC
nThRjuUK1h0tVPDdikG67T2pNkrqpqS3pL5z3HjdDMQhmGhSTyffgROaY4sAyngJ
PfPCfos3QQKBgQDmbkLLYgaVTFhLzmFwIvw6UbtikIgKQoCfbbbWNbe77pg9JNV5
+9jM6bJe4tZ810CbVKmkeacpsi9InI4Pu02MC5wHmmGWVuh/xdXvpFe6JkbR/vIz
RqaUWDyvG76Mmnwub+EoBGpVsbQ3L1kwCCME1evNCPuVJ/JyiTpglmhEgQKBgA6+
VlhdlpD2hruRRy8dgxDi1nnc4KVM/3We7UY3qN0kKPuxjp/X3RU/x5y7qWzKPyw2
KzJmEud5NUm/JsB3z12h54zOzZYPQcvmyeabGixYAjeFSWkc6CEBvhvgsQavcNlm
4ut98JcE5evDMFvSK+kCyOFM7aPBmw5zaGofwyXBAoGAcCF1agxxzjpH8mcd318I
uj88GDRP1Qru8g1/vwvo53CyO9nGjUvgvVXC6oCMFNalxgMpwjN4uRk0K0KAGaWt
wGnTzNddy7jamxiDcX7NobAi1Ix4rcQQkSLXse4K1iHSAAG2zA1hhueirYTj1NtA
wqpMeGWXht6yqjEaGCERenA=
-----END PRIVATE KEY-----";

    /// The first run of 64 hex chars in `s` (the file hash echoed in a populate body).
    fn first_hex64(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        for w in chars.windows(64) {
            if w.iter().all(|c| c.is_ascii_hexdigit()) {
                return w.iter().collect();
            }
        }
        String::new()
    }

    #[test]
    fn mock_firebase_protocol() {
        // token exchange → create version → populateFiles → upload → finalize → release.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let base2 = base.clone();
        let h = std::thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let (method, path, body) = read_request(&mut stream);
                seen2.lock().unwrap().push((method.clone(), path.clone()));
                let (status, json) = if path == "/token" {
                    (200u16, r#"{"access_token":"ya29.test"}"#.to_string())
                } else if path.contains(":populateFiles") {
                    // Echo the client's gzip hash so it uploads exactly that file.
                    let hash = first_hex64(&body);
                    (
                        200,
                        format!(
                            r#"{{"uploadUrl":"{base2}/upload","uploadRequiredHashes":["{hash}"]}}"#
                        ),
                    )
                } else if path.ends_with("/versions") {
                    (200, r#"{"name":"sites/demo/versions/v1"}"#.to_string())
                } else {
                    // update_mask=status, /releases, /upload/, and anything else all ack.
                    (200, "{}".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        std::env::set_var("CONCIERGE_DEPLOY_FIREBASE_BASE", &base);
        std::env::set_var("CONCIERGE_DEPLOY_FIREBASE_TOKEN", format!("{base}/token"));
        let sa = serde_json::json!({
            "client_email": "test@demo.iam.gserviceaccount.com",
            "private_key": TEST_RSA_PKCS8,
            "token_uri": format!("{base}/token"),
        })
        .to_string();
        let url = deploy_firebase(
            &FirebaseCreds {
                site_id: "demo".into(),
                service_account: sa,
                access_token: None,
                refresh_token: None,
                expires_at: None,
            },
            &one_file(),
        )
        .unwrap();
        assert_eq!(url, "https://demo.web.app");
        h.join().unwrap();

        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect();
        assert!(paths.iter().any(|p| p == "/token"), "minted a token");
        assert!(
            paths.iter().any(|p| p.ends_with("/versions")),
            "created a version"
        );
        assert!(
            paths.iter().any(|p| p.contains(":populateFiles")),
            "populated files"
        );
        assert!(
            paths.iter().any(|p| p.contains("/upload/")),
            "uploaded the required file"
        );
        assert!(
            paths.iter().any(|p| p.contains("update_mask=status")),
            "finalized"
        );
        assert!(paths.iter().any(|p| p.contains("/releases")), "released");
        std::env::remove_var("CONCIERGE_DEPLOY_FIREBASE_BASE");
        std::env::remove_var("CONCIERGE_DEPLOY_FIREBASE_TOKEN");
    }

    #[test]
    fn verify_github_reports_account_and_repo() {
        // The "Test connection" path: GET /user (account) + GET /repos/o/r (access).
        let (base, _seen, h) = spawn_mock(2, |_m, path, _b| {
            if path.ends_with("/user") {
                (200, r#"{"login":"octocat"}"#.into())
            } else {
                (200, "{}".into()) // /repos/o/r reachable
            }
        });
        std::env::set_var("CONCIERGE_DEPLOY_GITHUB_BASE", &base);
        let creds = DeployCredentials {
            github: Some(GithubCreds {
                token: "t".into(),
                owner: "o".into(),
                repo: "r".into(),
                branch: "gh-pages".into(),
            }),
            ..Default::default()
        };
        let label = verify("github", &creds).unwrap();
        assert!(label.contains("octocat"), "reports the signed-in account");
        assert!(label.contains("o/r"), "reports the target repo");
        h.join().unwrap();
        std::env::remove_var("CONCIERGE_DEPLOY_GITHUB_BASE");
    }
}
