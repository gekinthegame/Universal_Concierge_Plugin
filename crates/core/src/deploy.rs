//! Multi-platform website publishing — deploy a static folder to **GitHub Pages**,
//! **Netlify**, **Vercel**, or **Cloudflare Pages**.
//!
//! Each `deploy_*` takes the user's stored [`DeployCredentials`] + the staged files
//! and returns the **live URL**. Grounded in each platform's documented API:
//! - GitHub: REST Git Data API (blobs→tree→commit→ref) + enable Pages.
//! - Netlify: create site + zip deploy (`POST /api/v1/sites/{id}/deploys`, app/zip).
//! - Vercel: upload files (`POST /v2/files`, `x-vercel-digest` = SHA1) + create
//!   deployment (`POST /v13/deployments`, `files:[{file,sha,size}]`).
//! - Cloudflare Pages direct-upload: upload-token JWT → `pages/assets/upload`
//!   (base64, blake3(content+ext)[..32] hashes) → `upsert-hashes` → deployment
//!   (multipart `manifest` of path→hash).
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
    pub account_id: String,
    pub project: String,
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

    // Ensure a site (reuse the stored id, else create one named after the site).
    let site_id = match &c.site_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            let site = ok_json(
                cl.post(format!("{api}/api/v1/sites"))
                    .bearer_auth(&c.token)
                    .json(&serde_json::json!({ "name": name })),
                "netlify create site",
            )?;
            site.get("id")
                .and_then(|s| s.as_str())
                .ok_or("netlify: no site id")?
                .to_string()
        }
    };

    // Direct static deploy: a zip of the folder, Content-Type application/zip.
    let zip = zip_files(files)?;
    let deploy = ok_json(
        cl.post(format!("{api}/api/v1/sites/{site_id}/deploys"))
            .bearer_auth(&c.token)
            .header("Content-Type", "application/zip")
            .body(zip),
        "netlify deploy",
    )?;
    let url = deploy
        .get("ssl_url")
        .or_else(|| deploy.get("deploy_ssl_url"))
        .or_else(|| deploy.get("url"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://{name}.netlify.app"));
    Ok(url)
}

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

    let project = c.project.clone().unwrap_or_else(|| name.to_string());
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

pub fn deploy_cloudflare(c: &CloudflareCreds, files: &[DeployFile]) -> Result<String, String> {
    let api = base(
        "CONCIERGE_DEPLOY_CLOUDFLARE_BASE",
        "https://api.cloudflare.com/client/v4",
    );
    let cl = client();
    let (acct, proj) = (&c.account_id, &c.project);

    // Ensure the project exists (best-effort; ignore "already exists").
    let _ = cl
        .post(format!("{api}/accounts/{acct}/pages/projects"))
        .bearer_auth(&c.token)
        .json(&serde_json::json!({ "name": proj, "production_branch": "main" }))
        .send();

    // Upload JWT.
    let token_resp = ok_json(
        cl.get(format!(
            "{api}/accounts/{acct}/pages/projects/{proj}/upload-token"
        ))
        .bearer_auth(&c.token),
        "cloudflare upload-token",
    )?;
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
        // ── Netlify: create site → zip deploy → ssl_url ──
        let (base, seen, h) = spawn_mock(2, |_m, path, _b| {
            if path.ends_with("/deploys") {
                (200, r#"{"ssl_url":"https://my.netlify.app"}"#.into())
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
            },
            &one_file(),
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
}
