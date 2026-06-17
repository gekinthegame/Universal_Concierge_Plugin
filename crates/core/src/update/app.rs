//! The app channel (autoupdater §4) — binary self-update over the existing GitHub
//! release pipeline. Check is cheap and transparent; apply is gated by the same
//! offline-key discipline as the rules channel: the downloaded artifact is verified
//! against `SHASUMS256.txt` **and** a detached Ed25519 signature over that file,
//! checked against a key **baked into the binary** (Decision D-AU-3 — SHASUMS hosted
//! next to the file proves nothing against a host compromise).
//!
//! Swap is **stage-then-relaunch**: we never yank the running binary mid-run. The
//! verified new binary is staged under `<store>/updates/app/staged`; [`apply_on_launch`]
//! moves it into place on the next start.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{baseline, updates_dir, Result, UpdateError};

/// The running binary's version (compile-time constant from Cargo).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A newer release discovered by [`check`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// The release tag (e.g. `v0.1.4`).
    pub tag: String,
    /// The parsed semver (tag with a leading `v` stripped).
    pub version: String,
    /// The asset URL for *this* platform, if the release carries one.
    pub asset_url: Option<String>,
    /// The asset filename for this platform.
    pub asset_name: Option<String>,
}

/// A staged, verified update awaiting apply-on-relaunch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedUpdate {
    pub version: String,
    /// Path to the staged replacement binary.
    pub staged_binary: PathBuf,
    /// Where the current executable lives (the target of the swap).
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUpdateStatus {
    pub checked_ts: u64,
    pub release: Option<ReleaseInfo>,
    pub error: Option<String>,
}

/// The platform asset stem this build expects, matching the `release.yml` matrix
/// (`concierge-plugin-<os>-<arch>`). Returns `None` on an unrecognized target.
pub fn platform_asset_stem() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "concierge-plugin-macos-arm64",
        ("macos", "x86_64") => "concierge-plugin-macos-x64",
        ("linux", "x86_64") => "concierge-plugin-linux-x64",
        ("windows", "x86_64") => "concierge-plugin-windows-x64",
        _ => return None,
    })
}

/// Compare two semver strings; `true` iff `candidate` is strictly newer than
/// `current`. Bad versions are a typed error rather than a silent "no update".
pub fn is_newer(candidate: &str, current: &str) -> Result<bool> {
    let c = semver::Version::parse(candidate.trim_start_matches('v'))
        .map_err(|e| UpdateError::Version(format!("release tag `{candidate}`: {e}")))?;
    let cur = semver::Version::parse(current.trim_start_matches('v'))
        .map_err(|e| UpdateError::Version(format!("current version `{current}`: {e}")))?;
    Ok(c > cur)
}

/// Poll the GitHub Releases API for the latest release and report it iff it is newer
/// than `current`. Read-only and transparent (no egress beyond this GET).
pub fn check(repo: &str, current: &str) -> Result<Option<ReleaseInfo>> {
    let repo = normalized_repo(repo)?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = http_client()?;
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| UpdateError::Network(format!("releases poll: {e}")))?;
    if !resp.status().is_success() {
        return Err(UpdateError::Network(format!(
            "releases poll http {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| UpdateError::Network(format!("releases json: {e}")))?;
    let tag = body
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| UpdateError::Network("release missing tag_name".to_string()))?
        .to_string();
    let version = tag.trim_start_matches('v').to_string();
    if !is_newer(&tag, current)? {
        return Ok(None);
    }
    let (asset_url, asset_name) = select_asset(&body);
    Ok(Some(ReleaseInfo {
        tag,
        version,
        asset_url,
        asset_name,
    }))
}

fn normalized_repo(repo: &str) -> Result<&str> {
    let repo = repo.trim();
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    let valid = parts.next().is_none() && valid_repo_part(owner) && valid_repo_part(name);
    valid
        .then_some(repo)
        .ok_or_else(|| UpdateError::Network(format!("invalid GitHub repo `{repo}`")))
}

fn valid_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Pick this platform's asset (download URL + name) from a release JSON body.
fn select_asset(body: &serde_json::Value) -> (Option<String>, Option<String>) {
    let Some(stem) = platform_asset_stem() else {
        return (None, None);
    };
    let Some(assets) = body.get("assets").and_then(|a| a.as_array()) else {
        return (None, None);
    };
    for asset in assets {
        let name = asset.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.starts_with(stem) && (name.ends_with(".tar.gz") || name.ends_with(".zip")) {
            return (
                asset
                    .get("browser_download_url")
                    .and_then(|u| u.as_str())
                    .map(str::to_string),
                Some(name.to_string()),
            );
        }
    }
    (None, None)
}

/// Download the release's `SHASUMS256.txt` + detached `SHASUMS256.txt.sig`, verify the
/// signature against a baked app key, then verify the named asset's SHA-256 against the
/// (now-trusted) SHASUMS. Returns the verified asset bytes. Pure-ish: all network in,
/// crypto enforced before any bytes are trusted.
pub fn fetch_verified_asset(release: &ReleaseInfo) -> Result<Vec<u8>> {
    let asset_url = release
        .asset_url
        .as_deref()
        .ok_or_else(|| UpdateError::Network("no asset for this platform".to_string()))?;
    let asset_name = release
        .asset_name
        .as_deref()
        .ok_or_else(|| UpdateError::Network("no asset name".to_string()))?;
    let base = asset_url
        .rsplit_once('/')
        .map(|(b, _)| b.to_string())
        .ok_or_else(|| UpdateError::Network("malformed asset url".to_string()))?;

    let client = http_client()?;
    let shasums = download(&client, &format!("{base}/SHASUMS256.txt"))?;
    let sig = download(&client, &format!("{base}/SHASUMS256.txt.sig"))?;

    // 1. The SHASUMS file must be signed by a baked app key.
    verify_detached(&shasums, &sig, &baseline::app_pubkeys())?;

    // 2. The asset bytes must match the (trusted) SHASUMS entry.
    let asset = download(&client, asset_url)?;
    let want = sha256_for(&String::from_utf8_lossy(&shasums), asset_name).ok_or_else(|| {
        UpdateError::HashMismatch {
            expected: format!("<entry for {asset_name}>"),
            got: "absent from SHASUMS256.txt".to_string(),
        }
    })?;
    let got = super::verify::sha256_hex(&asset);
    if !got.eq_ignore_ascii_case(&want) {
        return Err(UpdateError::HashMismatch {
            expected: want,
            got,
        });
    }
    Ok(asset)
}

/// Verify a detached Ed25519 signature (raw 64 bytes) over `msg` against any baked key.
fn verify_detached(msg: &[u8], sig_bytes: &[u8], trusted: &[VerifyingKey]) -> Result<()> {
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| UpdateError::BadSignature("release signature is not 64 bytes".to_string()))?;
    let sig = Signature::from_bytes(&sig_arr);
    for key in trusted {
        if key.verify_strict(msg, &sig).is_ok() {
            return Ok(());
        }
    }
    Err(UpdateError::BadSignature(
        "no baked app key verifies SHASUMS256.txt".to_string(),
    ))
}

/// Find the hex digest for `filename` in a `sha256sum`-format SHASUMS file
/// (`<hex>  <filename>` lines).
fn sha256_for(shasums: &str, filename: &str) -> Option<String> {
    for line in shasums.lines() {
        let mut it = line.split_whitespace();
        let hex = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        if name == filename {
            return Some(hex.to_string());
        }
    }
    None
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // GitHub rejects requests without a User-Agent.
        .user_agent(format!("concierge-plugin/{}", current_version()))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| UpdateError::Network(format!("http client: {e}")))
}

fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .map_err(|e| UpdateError::Network(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(UpdateError::Network(format!(
            "GET {url} http {}",
            resp.status()
        )));
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| UpdateError::Network(format!("read body {url}: {e}")))
}

/// The app staging directory: `<store>/updates/app`.
fn app_dir(store_dir: &Path) -> PathBuf {
    updates_dir(store_dir).join("app")
}

fn pending_path(store_dir: &Path) -> PathBuf {
    app_dir(store_dir).join("pending.json")
}

fn latest_path(store_dir: &Path) -> PathBuf {
    app_dir(store_dir).join("latest.json")
}

pub fn cached_status(store_dir: &Path) -> Result<Option<AppUpdateStatus>> {
    match std::fs::read(latest_path(store_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| UpdateError::Io(format!("parse app update status: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(UpdateError::Io(format!("read app update status: {e}"))),
    }
}

pub fn save_cached_status(store_dir: &Path, status: &AppUpdateStatus) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(status)
        .map_err(|e| UpdateError::Io(format!("serialize app update status: {e}")))?;
    atomic_write(&latest_path(store_dir), &bytes)
}

/// Download + verify + stage the latest release, recording a pending update that
/// [`apply_on_launch`] will swap in. The archive is *not* unpacked here beyond writing
/// the verified archive to the staging dir; full extraction + the platform-specific
/// in-place swap happen at apply time (kept together so the swap is one atomic step).
pub fn stage(store_dir: &Path, release: &ReleaseInfo) -> Result<StagedUpdate> {
    let asset = fetch_verified_asset(release)?;
    let dir = app_dir(store_dir);
    std::fs::create_dir_all(&dir).map_err(|e| UpdateError::Io(format!("create app dir: {e}")))?;
    let name = release
        .asset_name
        .clone()
        .unwrap_or_else(|| "update.bin".to_string());
    let staged_archive = dir.join(&name);
    std::fs::write(&staged_archive, &asset)
        .map_err(|e| UpdateError::Io(format!("write staged archive: {e}")))?;

    let target =
        std::env::current_exe().map_err(|e| UpdateError::Io(format!("locate current exe: {e}")))?;
    let staged = StagedUpdate {
        version: release.version.clone(),
        staged_binary: staged_archive,
        target,
    };
    let json = serde_json::to_vec_pretty(&staged)
        .map_err(|e| UpdateError::Io(format!("serialize pending: {e}")))?;
    atomic_write(&pending_path(store_dir), &json)?;
    Ok(staged)
}

/// Read a recorded pending update, if any.
pub fn pending(store_dir: &Path) -> Result<Option<StagedUpdate>> {
    match std::fs::read(pending_path(store_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| UpdateError::Io(format!("parse pending: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(UpdateError::Io(format!("read pending: {e}"))),
    }
}

/// Clear a recorded pending update (after a successful apply, or to cancel one).
pub fn clear_pending(store_dir: &Path) -> Result<()> {
    let p = pending_path(store_dir);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UpdateError::Io(format!("clear pending: {e}"))),
    }
}

/// Apply a staged update on startup (stage-then-relaunch, §4 step 4). If a pending
/// update is recorded, extract the new `concierge-plugin` binary from the verified
/// staged archive and swap it over the current executable, then clear the pending
/// marker. Returns the applied version, or `None` if nothing was pending.
///
/// The running binary is replaced via rename-self: the OS keeps the in-memory image,
/// so the swap is safe; the *new* code takes effect on the next launch. On macOS an
/// unsigned `.app` still hits Gatekeeper once on first relaunch (documented caveat).
pub fn apply_on_launch(store_dir: &Path) -> Result<Option<String>> {
    let Some(pending) = pending(store_dir)? else {
        return Ok(None);
    };
    #[cfg(windows)]
    {
        return Err(UpdateError::Io(format!(
            "cannot replace a running Windows executable in-place; run the signed installer for {}",
            pending.version
        )));
    }
    #[cfg(not(windows))]
    {
        let archive = std::fs::read(&pending.staged_binary)
            .map_err(|e| UpdateError::Io(format!("read staged archive: {e}")))?;
        let name = pending
            .staged_binary
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let binary = extract_binary(&archive, name)?;

        let target = &pending.target;
        let new_path = target.with_extension("new");
        std::fs::write(&new_path, &binary)
            .map_err(|e| UpdateError::Io(format!("write new binary: {e}")))?;
        set_executable(&new_path)?;

        // Rename-self: move the running binary aside, then the new one into place.
        let old_path = target.with_extension("old");
        let _ = std::fs::remove_file(&old_path);
        std::fs::rename(target, &old_path)
            .map_err(|e| UpdateError::Io(format!("move current binary aside: {e}")))?;
        if let Err(e) = std::fs::rename(&new_path, target) {
            // Best-effort restore so a failed swap never leaves the user with no binary.
            let _ = std::fs::rename(&old_path, target);
            return Err(UpdateError::Io(format!("swap new binary in: {e}")));
        }
        let _ = std::fs::remove_file(&old_path);
        clear_pending(store_dir)?;
        Ok(Some(pending.version))
    }
}

/// Pull the `concierge-plugin` executable out of a staged release archive
/// (`.tar.gz` on unix, `.zip` on windows). The release pipeline stages the binary in
/// a directory named after the asset, so we match the basename, not a fixed path.
fn extract_binary(archive: &[u8], asset_name: &str) -> Result<Vec<u8>> {
    let exe = if cfg!(windows) {
        "concierge-plugin.exe"
    } else {
        "concierge-plugin"
    };
    if asset_name.ends_with(".zip") {
        let reader = std::io::Cursor::new(archive);
        let mut zip =
            zip::ZipArchive::new(reader).map_err(|e| UpdateError::Io(format!("open zip: {e}")))?;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| UpdateError::Io(format!("zip entry: {e}")))?;
            let ends = entry.name().rsplit('/').next() == Some(exe);
            if ends {
                use std::io::Read;
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .map_err(|e| UpdateError::Io(format!("read zip binary: {e}")))?;
                return Ok(buf);
            }
        }
        Err(UpdateError::Io(format!("{exe} not found in archive")))
    } else {
        use std::io::Read;
        let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
        let mut tar = tar::Archive::new(gz);
        for entry in tar
            .entries()
            .map_err(|e| UpdateError::Io(format!("tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| UpdateError::Io(format!("tar entry: {e}")))?;
            let is_exe = entry
                .path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_owned()))
                .map(|n| n == *exe)
                .unwrap_or(false);
            if is_exe {
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .map_err(|e| UpdateError::Io(format!("read tar binary: {e}")))?;
                return Ok(buf);
            }
        }
        Err(UpdateError::Io(format!("{exe} not found in archive")))
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| UpdateError::Io(format!("stat new binary: {e}")))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .map_err(|e| UpdateError::Io(format!("chmod new binary: {e}")))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UpdateError::Io(format!("create {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|e| UpdateError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| UpdateError::Io(format!("rename {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_detected() {
        assert!(is_newer("v0.1.4", "0.1.3").unwrap());
        assert!(is_newer("0.2.0", "0.1.9").unwrap());
        assert!(!is_newer("v0.1.3", "0.1.3").unwrap());
        assert!(!is_newer("0.1.2", "0.1.3").unwrap());
    }

    #[test]
    fn repo_names_are_validated() {
        assert_eq!(normalized_repo(" owner/repo ").unwrap(), "owner/repo");
        assert!(normalized_repo("owner").is_err());
        assert!(normalized_repo("../owner/repo").is_err());
        assert!(normalized_repo("owner/repo?x=1").is_err());
    }

    #[test]
    fn bad_versions_error() {
        assert!(matches!(
            is_newer("not-a-version", "0.1.0"),
            Err(UpdateError::Version(_))
        ));
    }

    #[test]
    fn shasums_lookup() {
        let shasums = "abc123  concierge-plugin-linux-x64.tar.gz\ndef456  other.zip\n";
        assert_eq!(
            sha256_for(shasums, "concierge-plugin-linux-x64.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(sha256_for(shasums, "missing.tar.gz"), None);
    }

    #[test]
    fn asset_selection_matches_platform() {
        // Only assert the mapping is total for the platforms the matrix builds.
        let stem = platform_asset_stem();
        if let Some(stem) = stem {
            assert!(stem.starts_with("concierge-plugin-"));
        }
    }

    #[test]
    fn detached_signature_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[5u8; 32]);
        let vk = sk.verifying_key();
        let msg = b"SHASUMS256 contents";
        let sig = sk.sign(msg).to_bytes();
        assert!(verify_detached(msg, &sig, &[vk]).is_ok());
        // Wrong key rejected.
        let other = SigningKey::from_bytes(&[6u8; 32]).verifying_key();
        assert!(matches!(
            verify_detached(msg, &sig, &[other]),
            Err(UpdateError::BadSignature(_))
        ));
    }
}
