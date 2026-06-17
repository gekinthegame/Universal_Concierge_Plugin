//! YouTube upload integration (YOUTUBE_UPLOAD_INTEGRATION_PLAN.md).
//!
//! Uploads a rendered video from the Concierge canvas to the user's YouTube channel
//! through the official **YouTube Data API v3**, with explicit user approval and a
//! local receipt. Mirrors the Firebase deploy path in `deploy.rs`: a Google
//! **Desktop OAuth** (PKCE, loopback redirect) login, tokens stored in the security
//! vault (never in canvas/receipts/git), and refresh-before-use.
//!
//! ## Safety invariants (plan §7)
//! - No automatic upload from AI tools — the upload entry point is only reachable
//!   from an explicit user action (CLI command / GUI click).
//! - The source file must live inside the canvas/export output ([`MemCli::youtube_upload`]
//!   canonicalizes and boundary-checks it).
//! - Uploads default to `private`/`unlisted`; `public` is opt-in in the review screen.
//! - Receipts never contain secrets ([`YouTubeUploadReceipt`] has no token fields).
//!
//! ## Network testability
//! The resumable-upload state machine ([`upload_loop`]) drives a [`UploadTransport`]
//! trait, so retry/resume is exercised by a mock in tests; [`HttpTransport`] is the
//! real `reqwest` implementation. Live API calls are not made in unit tests.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::deploy::{OAuthStart, OAuthToken};
use crate::egress::{atomic_private_write, ensure_private_dir, validate_private_file};
use crate::error::{Error, Result};

// ── Google Desktop OAuth client (an "installed app" client, like the one the
// Firebase CLI uses). There is no public YouTube client, so a real build MUST inject
// its own Google Cloud Desktop client at compile time via `UCP_YT_CLIENT_ID` /
// `UCP_YT_CLIENT_SECRET` (see crates/core/src/youtube/SETUP.md). A Desktop client
// permits a loopback redirect on any local port, which the GUI binds ephemerally.
// Unset (local/dev) builds carry obvious placeholders so a misconfig fails loudly at
// the consent screen rather than silently. ──
const YT_OAUTH_CLIENT_ID: &str = match option_env!("UCP_YT_CLIENT_ID") {
    Some(value) => value,
    None => "UNSET.apps.googleusercontent.com",
};
const YT_OAUTH_CLIENT_SECRET: &str = match option_env!("UCP_YT_CLIENT_SECRET") {
    Some(value) => value,
    None => "UNSET",
};

/// Whether this build carries a real Google OAuth client (vs the dev placeholder).
pub fn oauth_configured() -> bool {
    !YT_OAUTH_CLIENT_ID.starts_with("UNSET") && !YT_OAUTH_CLIENT_SECRET.starts_with("UNSET")
}
const YT_OAUTH_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const YT_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// `youtube.upload` is the narrow upload scope (plan §2). `youtube.force-ssl` is also
/// requested because Phase 3 (captions + playlist management) requires it; a user who
/// only wants uploads can decline — captions/playlists then simply fail with a clear
/// scope error and the upload itself still works.
const YT_OAUTH_SCOPES: &str =
    "https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.force-ssl";

/// Resumable upload chunk size. Must be a multiple of 256 KiB per the YouTube
/// resumable protocol; 8 MiB balances throughput and resume granularity.
const CHUNK_SIZE: u64 = 8 * 256 * 1024;
/// Max transient-failure retries per resumable step.
const MAX_RETRIES: u32 = 5;

// ──────────────────────────────────────────────────────────────────────────────
// OAuth (Google Desktop / PKCE) — mirrors deploy::firebase_oauth_*
// ──────────────────────────────────────────────────────────────────────────────

/// Begin a YouTube (Google) PKCE login for the given loopback `redirect_uri`.
pub fn oauth_start(redirect_uri: &str) -> OAuthStart {
    let verifier = random_b64url(64);
    let state = random_b64url(24);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let authorize_url = format!(
        "{YT_OAUTH_AUTH}?response_type=code&client_id={YT_OAUTH_CLIENT_ID}&redirect_uri={redirect}&scope={scope}&state={state}&code_challenge={challenge}&code_challenge_method=S256&access_type=offline&prompt=consent",
        redirect = urlencode(redirect_uri),
        scope = urlencode(YT_OAUTH_SCOPES),
    );
    OAuthStart {
        authorize_url,
        verifier,
        state,
    }
}

/// Exchange the authorization `code` (+ PKCE `verifier`) for access + refresh tokens.
pub fn oauth_exchange(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> std::result::Result<OAuthToken, String> {
    let resp = client()
        .post(YT_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", YT_OAUTH_CLIENT_ID),
            ("client_secret", YT_OAUTH_CLIENT_SECRET),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|e| format!("youtube token exchange: {e}"))?;
    parse_oauth_token(resp)
}

/// Refresh an expired Google access token (Google keeps the same refresh token).
pub fn oauth_refresh(refresh_token: &str) -> std::result::Result<OAuthToken, String> {
    let resp = client()
        .post(YT_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", YT_OAUTH_CLIENT_ID),
            ("client_secret", YT_OAUTH_CLIENT_SECRET),
        ])
        .send()
        .map_err(|e| format!("youtube token refresh: {e}"))?;
    parse_oauth_token(resp)
}

// ──────────────────────────────────────────────────────────────────────────────
// Metadata
// ──────────────────────────────────────────────────────────────────────────────

/// The review-screen metadata for an upload (plan §1/§3). Defaults to a private,
/// non-kids, non-synthetic video in "People & Blogs" (category 22).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VideoMetadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category_id: String,
    /// `private` | `unlisted` | `public` — defaults to `private`.
    pub privacy_status: String,
    pub made_for_kids: bool,
    pub contains_synthetic_media: bool,
    /// RFC3339 timestamp for a scheduled publish; only valid with `private` privacy.
    pub publish_at: Option<String>,
    pub notify_subscribers: bool,
}

impl Default for VideoMetadata {
    fn default() -> Self {
        Self {
            title: "Untitled".to_string(),
            description: String::new(),
            tags: Vec::new(),
            category_id: "22".to_string(),
            privacy_status: "private".to_string(),
            made_for_kids: false,
            contains_synthetic_media: false,
            publish_at: None,
            notify_subscribers: false,
        }
    }
}

/// Build the `videos.insert` request body (snippet + status). Pure — unit-tested.
pub fn build_insert_body(meta: &VideoMetadata) -> serde_json::Value {
    let mut status = serde_json::json!({
        "privacyStatus": meta.privacy_status,
        "selfDeclaredMadeForKids": meta.made_for_kids,
        "containsSyntheticMedia": meta.contains_synthetic_media,
    });
    // publishAt is only honored when privacyStatus is "private".
    if let Some(when) = &meta.publish_at {
        if meta.privacy_status == "private" {
            status["publishAt"] = serde_json::Value::String(when.clone());
        }
    }
    serde_json::json!({
        "snippet": {
            "title": meta.title,
            "description": meta.description,
            "tags": meta.tags,
            "categoryId": meta.category_id,
        },
        "status": status,
    })
}

/// The canonical watch URL for an uploaded video id.
pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

// ──────────────────────────────────────────────────────────────────────────────
// Resumable upload — transport-trait so the state machine is testable offline
// ──────────────────────────────────────────────────────────────────────────────

/// A transient (retryable) vs permanent upload error.
#[derive(Debug)]
pub enum YtError {
    /// Network blip / 5xx / 308-resume — safe to retry the step.
    Transient(String),
    /// 4xx / auth / malformed — do not retry.
    Permanent(String),
}

impl YtError {
    pub fn is_transient(&self) -> bool {
        matches!(self, YtError::Transient(_))
    }
}

impl std::fmt::Display for YtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YtError::Transient(m) => write!(f, "transient: {m}"),
            YtError::Permanent(m) => write!(f, "permanent: {m}"),
        }
    }
}

/// The outcome of uploading one resumable chunk.
#[derive(Debug, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// Server acknowledged up to (but not including) `next_offset`.
    Incomplete { next_offset: u64 },
    /// Upload finished; the created video's id.
    Complete { video_id: String },
}

/// The seam the resumable state machine drives. The real impl talks to YouTube; the
/// test impl scripts responses to exercise retry/resume.
pub trait UploadTransport {
    /// Open a resumable session for a video of `size` bytes; return the session URI.
    fn start(
        &mut self,
        insert_body: &serde_json::Value,
        size: u64,
        mime: &str,
    ) -> std::result::Result<String, YtError>;
    /// PUT one chunk at `offset` of a `total`-byte upload.
    fn put_chunk(
        &mut self,
        session: &str,
        offset: u64,
        chunk: &[u8],
        total: u64,
    ) -> std::result::Result<ChunkOutcome, YtError>;
}

/// Drive a resumable upload to completion with retry + resume. Returns the video id.
/// `progress(sent, total)` is called after each acknowledged step.
pub fn upload_loop(
    transport: &mut dyn UploadTransport,
    data: &[u8],
    meta: &VideoMetadata,
    mime: &str,
    chunk_size: u64,
    max_retries: u32,
    progress: &mut dyn FnMut(u64, u64),
) -> std::result::Result<String, YtError> {
    let total = data.len() as u64;
    if total == 0 {
        return Err(YtError::Permanent(
            "refusing to upload an empty file".to_string(),
        ));
    }
    let body = build_insert_body(meta);

    // Open the session (retry transient failures).
    let mut attempts = 0;
    let session = loop {
        match transport.start(&body, total, mime) {
            Ok(s) => break s,
            Err(e) if e.is_transient() && attempts < max_retries => attempts += 1,
            Err(e) => return Err(e),
        }
    };

    let mut offset = 0u64;
    progress(0, total);
    loop {
        let end = (offset + chunk_size).min(total);
        let chunk = &data[offset as usize..end as usize];
        let mut attempts = 0;
        loop {
            match transport.put_chunk(&session, offset, chunk, total) {
                Ok(ChunkOutcome::Complete { video_id }) => {
                    progress(total, total);
                    return Ok(video_id);
                }
                Ok(ChunkOutcome::Incomplete { next_offset }) => {
                    // The server tells us how much it durably has; resume from there
                    // (this is also how an interrupted upload recovers).
                    offset = next_offset.min(total);
                    progress(offset, total);
                    break;
                }
                Err(e) if e.is_transient() && attempts < max_retries => attempts += 1,
                Err(e) => return Err(e),
            }
        }
        if offset >= total {
            // All bytes acknowledged but no Complete — treat as a transient protocol
            // hiccup and let the next put (a zero-length finalize) surface the id.
            match transport.put_chunk(&session, offset, &[], total) {
                Ok(ChunkOutcome::Complete { video_id }) => return Ok(video_id),
                Ok(ChunkOutcome::Incomplete { .. }) => {
                    return Err(YtError::Permanent(
                        "server never returned a completed video id".to_string(),
                    ))
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Parse the next resume offset from a YouTube `Range: bytes=0-262143` header.
/// Returns the first byte the server still needs.
pub fn parse_range_next(range_header: &str) -> Option<u64> {
    let last = range_header.trim().rsplit('-').next()?;
    last.trim().parse::<u64>().ok().map(|end| end + 1)
}

/// The real `reqwest` transport against the YouTube resumable upload endpoint.
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    access_token: String,
    notify_subscribers: bool,
}

impl HttpTransport {
    pub fn new(access_token: impl Into<String>, notify_subscribers: bool) -> Self {
        Self {
            client: client(),
            access_token: access_token.into(),
            notify_subscribers,
        }
    }

    fn upload_base() -> String {
        // Test/integration override (point at a local mock), mirroring deploy::base.
        std::env::var("CONCIERGE_YOUTUBE_UPLOAD_BASE")
            .unwrap_or_else(|_| "https://www.googleapis.com/upload/youtube/v3".to_string())
    }
}

impl UploadTransport for HttpTransport {
    fn start(
        &mut self,
        insert_body: &serde_json::Value,
        size: u64,
        mime: &str,
    ) -> std::result::Result<String, YtError> {
        let url = format!(
            "{}/videos?uploadType=resumable&part=snippet,status&notifySubscribers={}",
            HttpTransport::upload_base(),
            self.notify_subscribers
        );
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.access_token)
            .header("X-Upload-Content-Length", size.to_string())
            .header("X-Upload-Content-Type", mime)
            .json(insert_body)
            .send()
            .map_err(|e| YtError::Transient(format!("start session: {e}")))?;
        if !resp.status().is_success() {
            return Err(classify(resp.status().as_u16(), "start session"));
        }
        resp.headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| YtError::Permanent("no resumable session Location header".to_string()))
    }

    fn put_chunk(
        &mut self,
        session: &str,
        offset: u64,
        chunk: &[u8],
        total: u64,
    ) -> std::result::Result<ChunkOutcome, YtError> {
        let end = offset + chunk.len() as u64;
        let content_range = if chunk.is_empty() {
            format!("bytes */{total}")
        } else {
            format!("bytes {offset}-{}/{total}", end - 1)
        };
        let resp = self
            .client
            .put(session)
            .bearer_auth(&self.access_token)
            .header(reqwest::header::CONTENT_RANGE, content_range)
            .body(chunk.to_vec())
            .send()
            .map_err(|e| YtError::Transient(format!("put chunk: {e}")))?;
        let code = resp.status().as_u16();
        // 308 Resume Incomplete carries a Range header with what the server has.
        if code == 308 {
            let next = resp
                .headers()
                .get(reqwest::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_range_next)
                .unwrap_or(end);
            return Ok(ChunkOutcome::Incomplete { next_offset: next });
        }
        if resp.status().is_success() {
            let body: serde_json::Value = resp
                .json()
                .map_err(|e| YtError::Permanent(format!("parse insert response: {e}")))?;
            let id = body.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                YtError::Permanent("insert response missing video id".to_string())
            })?;
            return Ok(ChunkOutcome::Complete {
                video_id: id.to_string(),
            });
        }
        Err(classify(code, "put chunk"))
    }
}

/// Map an HTTP status to transient (retry) vs permanent.
fn classify(code: u16, what: &str) -> YtError {
    if code == 408 || code == 429 || (500..=599).contains(&code) {
        YtError::Transient(format!("{what}: http {code}"))
    } else {
        YtError::Permanent(format!("{what}: http {code}"))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Phase 3 — thumbnails, captions, playlists (require youtube.force-ssl)
// ──────────────────────────────────────────────────────────────────────────────

/// Set a custom thumbnail (`thumbnails.set`). `mime` like `image/png`/`image/jpeg`.
pub fn set_thumbnail(
    access_token: &str,
    video_id: &str,
    image: &[u8],
    mime: &str,
) -> std::result::Result<(), String> {
    let url = format!(
        "{}/thumbnails/set?videoId={video_id}",
        HttpTransport::upload_base()
    );
    let resp = client()
        .post(&url)
        .bearer_auth(access_token)
        .header(reqwest::header::CONTENT_TYPE, mime)
        .body(image.to_vec())
        .send()
        .map_err(|e| format!("thumbnail set: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("thumbnail set: http {}", resp.status()))
    }
}

/// Insert a caption track (`captions.insert`). `srt_or_vtt` is the caption file body.
pub fn insert_caption(
    access_token: &str,
    video_id: &str,
    language: &str,
    name: &str,
    body: &[u8],
) -> std::result::Result<String, String> {
    // captions.insert is a multipart upload: metadata JSON part + the caption file.
    let meta = serde_json::json!({
        "snippet": { "videoId": video_id, "language": language, "name": name }
    });
    let part_meta = reqwest::blocking::multipart::Part::text(meta.to_string())
        .mime_str("application/json")
        .map_err(|e| format!("caption meta part: {e}"))?;
    let part_body = reqwest::blocking::multipart::Part::bytes(body.to_vec())
        .mime_str("application/octet-stream")
        .map_err(|e| format!("caption body part: {e}"))?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("snippet", part_meta)
        .part("file", part_body);
    let url =
        "https://www.googleapis.com/upload/youtube/v3/captions?part=snippet&uploadType=multipart";
    let resp = client()
        .post(url)
        .bearer_auth(access_token)
        .multipart(form)
        .send()
        .map_err(|e| format!("caption insert: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("caption insert: http {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("caption response: {e}"))?;
    Ok(v.get("id")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string())
}

/// Add an uploaded video to a playlist (`playlistItems.insert`).
pub fn add_to_playlist(
    access_token: &str,
    playlist_id: &str,
    video_id: &str,
) -> std::result::Result<String, String> {
    let body = serde_json::json!({
        "snippet": {
            "playlistId": playlist_id,
            "resourceId": { "kind": "youtube#video", "videoId": video_id }
        }
    });
    let resp = client()
        .post("https://www.googleapis.com/youtube/v3/playlistItems?part=snippet")
        .bearer_auth(access_token)
        .json(&body)
        .send()
        .map_err(|e| format!("playlist add: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("playlist add: http {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("playlist response: {e}"))?;
    Ok(v.get("id")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string())
}

// ──────────────────────────────────────────────────────────────────────────────
// Credentials (vault) + status + receipts
// ──────────────────────────────────────────────────────────────────────────────

/// OAuth credentials persisted under `<store>/security/youtube.json` (0600). Custom
/// [`std::fmt::Debug`] redacts the tokens so they never leak into logs.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct YouTubeCreds {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    /// Absolute unix expiry of the access token.
    pub expires_at: Option<u64>,
    /// The connected channel/account label, if known.
    pub channel: Option<String>,
}

impl YouTubeCreds {
    pub fn connected(&self) -> bool {
        self.refresh_token.is_some() || self.access_token.is_some()
    }
}

impl std::fmt::Debug for YouTubeCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeCreds")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("channel", &self.channel)
            .finish()
    }
}

/// A safe, GUI-facing snapshot of connection state (never carries tokens).
#[derive(Debug, Clone, Serialize)]
pub struct YouTubeStatus {
    pub connected: bool,
    pub channel: Option<String>,
    pub expires_at: Option<u64>,
}

/// A full upload request from the review screen.
#[derive(Debug, Clone, Deserialize)]
pub struct UploadRequest {
    /// Absolute path to the rendered video (must be inside the canvas/export output).
    pub file_path: String,
    #[serde(flatten)]
    pub metadata: VideoMetadata,
    /// Optional custom thumbnail file (Phase 3).
    pub thumbnail_path: Option<String>,
    /// Optional caption track: (path, language, name) (Phase 3).
    pub caption_path: Option<String>,
    #[serde(default)]
    pub caption_language: String,
    /// Optional playlist to add the video to (Phase 3).
    pub playlist_id: Option<String>,
}

/// A local receipt of a completed upload (plan §6). Contains **no secrets**.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouTubeUploadReceipt {
    pub video_id: String,
    pub video_url: String,
    pub channel: Option<String>,
    pub backend: String,
    pub unix_time: u64,
    /// The source video's path (relative to the canvas root) and content hash.
    pub source_rel: String,
    pub source_sha256: String,
    pub privacy_status: String,
    pub title: String,
    /// Best-effort summary of Phase-3 extras (thumbnail/caption/playlist results).
    pub extras: Vec<String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("webm") => "video/webm",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

impl crate::binding::MemCli {
    fn youtube_creds_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("youtube.json"))
    }

    /// Begin a login; the GUI opens [`OAuthStart::authorize_url`] and finishes via
    /// [`Self::youtube_save_oauth`] after the loopback redirect.
    pub fn youtube_oauth_start(&self, redirect_uri: &str) -> OAuthStart {
        oauth_start(redirect_uri)
    }

    /// Persist tokens from a completed OAuth exchange/refresh.
    pub fn youtube_save_oauth(&self, token: OAuthToken, channel: Option<String>) -> Result<()> {
        let mut creds = self.youtube_credentials().unwrap_or_default();
        creds.access_token = Some(token.access_token);
        if token.refresh_token.is_some() {
            creds.refresh_token = token.refresh_token;
        }
        creds.expires_at = token.expires_at;
        if channel.is_some() {
            creds.channel = channel;
        }
        self.write_youtube_creds(&creds)
    }

    fn write_youtube_creds(&self, creds: &YouTubeCreds) -> Result<()> {
        self.ensure_security_dir()?;
        let path = self.youtube_creds_path()?;
        let json = serde_json::to_vec_pretty(creds)
            .map_err(|e| Error::Io(format!("serialize youtube creds: {e}")))?;
        atomic_private_write(&path, &json)
    }

    /// Load persisted credentials (defaults to disconnected if none).
    pub fn youtube_credentials(&self) -> Result<YouTubeCreds> {
        let path = self.youtube_creds_path()?;
        if !path.try_exists().unwrap_or(false) {
            return Ok(YouTubeCreds::default());
        }
        validate_private_file(&path)?;
        let bytes =
            std::fs::read(&path).map_err(|e| Error::Io(format!("read youtube creds: {e}")))?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Io(format!("parse youtube creds: {e}")))
    }

    /// Disconnect: delete stored tokens.
    pub fn youtube_disconnect(&self) -> Result<()> {
        let path = self.youtube_creds_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(format!("disconnect youtube: {e}"))),
        }
    }

    /// Safe connection status for the GUI/CLI.
    pub fn youtube_status(&self) -> Result<YouTubeStatus> {
        let c = self.youtube_credentials()?;
        Ok(YouTubeStatus {
            connected: c.connected(),
            channel: c.channel,
            expires_at: c.expires_at,
        })
    }

    /// Return credentials with a valid access token, refreshing first if expired and
    /// persisting the new token (mirror of `firebase_refreshed`). Call before upload.
    pub fn youtube_refreshed(&self) -> Result<YouTubeCreds> {
        let mut creds = self.youtube_credentials()?;
        if !creds.connected() {
            return Err(Error::SecurityPolicy(
                "YouTube is not connected — authorize an account first".to_string(),
            ));
        }
        let fresh_enough = creds
            .expires_at
            .map(|exp| exp > now_secs() + 60)
            .unwrap_or(false);
        if fresh_enough && creds.access_token.is_some() {
            return Ok(creds);
        }
        let refresh = creds.refresh_token.clone().ok_or_else(|| {
            Error::SecurityPolicy("no refresh token; reconnect YouTube".to_string())
        })?;
        let token = oauth_refresh(&refresh).map_err(Error::Io)?;
        creds.access_token = Some(token.access_token);
        creds.expires_at = token.expires_at;
        if token.refresh_token.is_some() {
            creds.refresh_token = token.refresh_token;
        }
        self.write_youtube_creds(&creds)?;
        Ok(creds)
    }

    /// Validate that `file_path` is a real file inside the canvas/export output
    /// (plan §7). Canonicalization resolves symlinks, so comparing canonical paths
    /// under the canonical canvas root also rejects symlink escapes.
    fn validate_upload_source(&self, file_path: &str) -> Result<(PathBuf, String)> {
        let canvas = self.store_dir()?.join("canvas");
        let canon_root = canvas
            .canonicalize()
            .map_err(|e| Error::SecurityPolicy(format!("canvas output not found: {e}")))?;
        let canon = Path::new(file_path)
            .canonicalize()
            .map_err(|e| Error::SecurityPolicy(format!("video not found: {e}")))?;
        if !canon.starts_with(&canon_root) {
            return Err(Error::SecurityPolicy(
                "video must be inside the canvas/export output".to_string(),
            ));
        }
        if !canon.is_file() {
            return Err(Error::SecurityPolicy("not a file".to_string()));
        }
        let rel = canon
            .strip_prefix(&canon_root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| canon.to_string_lossy().to_string());
        Ok((canon, rel))
    }

    /// Upload a rendered video to YouTube and write a local receipt. `progress(sent,
    /// total)` is invoked as bytes are acknowledged. This is the only upload entry
    /// point and must be called from an explicit user action (plan §7).
    pub fn youtube_upload(
        &self,
        req: &UploadRequest,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<YouTubeUploadReceipt> {
        let creds = self.youtube_refreshed()?;
        let access = creds
            .access_token
            .clone()
            .ok_or_else(|| Error::SecurityPolicy("no access token".to_string()))?;

        let (src, source_rel) = self.validate_upload_source(&req.file_path)?;
        let data = std::fs::read(&src).map_err(|e| Error::Io(format!("read video: {e}")))?;
        let source_sha256 = sha256_hex(&data);
        let mime = mime_for(&src);

        let mut transport = HttpTransport::new(access.clone(), req.metadata.notify_subscribers);
        let video_id = upload_loop(
            &mut transport,
            &data,
            &req.metadata,
            mime,
            CHUNK_SIZE,
            MAX_RETRIES,
            progress,
        )
        .map_err(|e| Error::Io(format!("youtube upload: {e}")))?;

        // Phase 3 extras — best-effort; failures are recorded, not fatal (the video
        // is already uploaded).
        let mut extras = Vec::new();
        if let Some(thumb) = &req.thumbnail_path {
            extras.push(match self.do_thumbnail(&access, &video_id, thumb) {
                Ok(()) => "thumbnail: set".to_string(),
                Err(e) => format!("thumbnail: failed ({e})"),
            });
        }
        if let Some(cap) = &req.caption_path {
            extras.push(
                match self.do_caption(&access, &video_id, cap, &req.caption_language) {
                    Ok(id) => format!("caption: added ({id})"),
                    Err(e) => format!("caption: failed ({e})"),
                },
            );
        }
        if let Some(pl) = &req.playlist_id {
            extras.push(match add_to_playlist(&access, pl, &video_id) {
                Ok(_) => format!("playlist: added to {pl}"),
                Err(e) => format!("playlist: failed ({e})"),
            });
        }

        let receipt = YouTubeUploadReceipt {
            video_id: video_id.clone(),
            video_url: watch_url(&video_id),
            channel: creds.channel.clone(),
            backend: "youtube".to_string(),
            unix_time: now_secs(),
            source_rel,
            source_sha256,
            privacy_status: req.metadata.privacy_status.clone(),
            title: req.metadata.title.clone(),
            extras,
        };
        self.append_youtube_receipt(&receipt)?;
        Ok(receipt)
    }

    fn do_thumbnail(
        &self,
        access: &str,
        video_id: &str,
        thumb_path: &str,
    ) -> std::result::Result<(), String> {
        let (path, _) = self
            .validate_upload_source(thumb_path)
            .map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let mime = match path.extension().and_then(|e| e.to_str()) {
            Some("png") => "image/png",
            _ => "image/jpeg",
        };
        set_thumbnail(access, video_id, &bytes, mime)
    }

    fn do_caption(
        &self,
        access: &str,
        video_id: &str,
        cap_path: &str,
        lang: &str,
    ) -> std::result::Result<String, String> {
        let (path, rel) = self
            .validate_upload_source(cap_path)
            .map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let language = if lang.is_empty() { "en" } else { lang };
        insert_caption(access, video_id, language, &rel, &bytes)
    }

    fn youtube_receipts_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("youtube-upload-receipts.jsonl"))
    }

    fn append_youtube_receipt(&self, receipt: &YouTubeUploadReceipt) -> Result<()> {
        use std::io::Write;
        let path = self.youtube_receipts_path()?;
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent).ok();
        }
        let mut line = serde_json::to_vec(receipt)
            .map_err(|e| Error::Io(format!("serialize receipt: {e}")))?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Io(format!("open receipts: {e}")))?;
        f.write_all(&line)
            .map_err(|e| Error::Io(format!("write receipt: {e}")))
    }

    /// Read the local upload history (newest last).
    pub fn youtube_receipts(&self) -> Result<Vec<YouTubeUploadReceipt>> {
        let path = self.youtube_receipts_path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Io(format!("read receipts: {e}"))),
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

// ── small local helpers (mirror deploy.rs; kept here so youtube is self-contained) ──

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("concierge-plugin")
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

fn random_b64url(bytes: usize) -> String {
    use rand_core::RngCore;
    let mut buf = vec![0u8; bytes];
    rand_core::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_oauth_token(resp: reqwest::blocking::Response) -> std::result::Result<OAuthToken, String> {
    if !resp.status().is_success() {
        return Err(format!("oauth token http {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("oauth token json: {e}"))?;
    let access_token = v
        .get("access_token")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "oauth: no access_token".to_string())?
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(|s| s.as_str())
        .map(String::from);
    let expires_at = v
        .get("expires_in")
        .and_then(|s| s.as_u64())
        .map(|secs| now_secs() + secs);
    Ok(OAuthToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_body_has_required_fields_and_defaults_private() {
        let meta = VideoMetadata {
            title: "My Render".into(),
            description: "desc".into(),
            tags: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let body = build_insert_body(&meta);
        assert_eq!(body["snippet"]["title"], "My Render");
        assert_eq!(body["snippet"]["categoryId"], "22");
        assert_eq!(body["status"]["privacyStatus"], "private");
        assert_eq!(body["status"]["selfDeclaredMadeForKids"], false);
        assert_eq!(body["status"]["containsSyntheticMedia"], false);
        assert_eq!(body["snippet"]["tags"][1], "b");
    }

    #[test]
    fn publish_at_only_applies_to_private() {
        let mut meta = VideoMetadata {
            publish_at: Some("2026-07-01T00:00:00Z".into()),
            privacy_status: "public".into(),
            ..Default::default()
        };
        assert!(build_insert_body(&meta)["status"]
            .get("publishAt")
            .is_none());
        meta.privacy_status = "private".into();
        assert_eq!(
            build_insert_body(&meta)["status"]["publishAt"],
            "2026-07-01T00:00:00Z"
        );
    }

    #[test]
    fn creds_debug_redacts_tokens() {
        let creds = YouTubeCreds {
            access_token: Some("ya29.SECRET".into()),
            refresh_token: Some("1//REFRESH".into()),
            expires_at: Some(123),
            channel: Some("My Channel".into()),
        };
        let dbg = format!("{creds:?}");
        assert!(!dbg.contains("SECRET"));
        assert!(!dbg.contains("REFRESH"));
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("My Channel"));
    }

    #[test]
    fn receipt_serialization_has_no_secret_fields() {
        let r = YouTubeUploadReceipt {
            video_id: "abc".into(),
            video_url: watch_url("abc"),
            channel: Some("c".into()),
            backend: "youtube".into(),
            unix_time: 1,
            source_rel: "proj/animation.webm".into(),
            source_sha256: "00".into(),
            privacy_status: "unlisted".into(),
            title: "t".into(),
            extras: vec![],
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("token"));
        assert!(!json.contains("access"));
        assert!(!json.contains("refresh"));
        assert!(json.contains("watch?v=abc"));
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range_next("bytes=0-262143"), Some(262144));
        assert_eq!(parse_range_next("0-9"), Some(10));
        assert_eq!(parse_range_next("garbage"), None);
    }

    // ── Mock transport to exercise the resumable state machine offline ──
    struct Mock {
        script: Vec<std::result::Result<ChunkOutcome, YtError>>,
        idx: usize,
        start_fail_once: bool,
    }
    impl UploadTransport for Mock {
        fn start(
            &mut self,
            _b: &serde_json::Value,
            _s: u64,
            _m: &str,
        ) -> std::result::Result<String, YtError> {
            if self.start_fail_once {
                self.start_fail_once = false;
                return Err(YtError::Transient("start blip".into()));
            }
            Ok("https://mock/session".into())
        }
        fn put_chunk(
            &mut self,
            _s: &str,
            _o: u64,
            _c: &[u8],
            _t: u64,
        ) -> std::result::Result<ChunkOutcome, YtError> {
            let out = self.script.get(self.idx).map(|r| match r {
                Ok(o) => Ok(o.clone_for_test()),
                Err(e) => Err(e.clone_for_test()),
            });
            self.idx += 1;
            out.unwrap_or(Ok(ChunkOutcome::Complete {
                video_id: "FALLBACK".into(),
            }))
        }
    }
    impl ChunkOutcome {
        fn clone_for_test(&self) -> ChunkOutcome {
            match self {
                ChunkOutcome::Incomplete { next_offset } => ChunkOutcome::Incomplete {
                    next_offset: *next_offset,
                },
                ChunkOutcome::Complete { video_id } => ChunkOutcome::Complete {
                    video_id: video_id.clone(),
                },
            }
        }
    }
    impl YtError {
        fn clone_for_test(&self) -> YtError {
            match self {
                YtError::Transient(m) => YtError::Transient(m.clone()),
                YtError::Permanent(m) => YtError::Permanent(m.clone()),
            }
        }
    }

    fn noop(_s: u64, _t: u64) {}

    #[test]
    fn single_chunk_success() {
        let mut m = Mock {
            script: vec![Ok(ChunkOutcome::Complete {
                video_id: "vid1".into(),
            })],
            idx: 0,
            start_fail_once: false,
        };
        let meta = VideoMetadata::default();
        let id = upload_loop(
            &mut m,
            b"hello world",
            &meta,
            "video/webm",
            1024,
            3,
            &mut noop,
        )
        .unwrap();
        assert_eq!(id, "vid1");
    }

    #[test]
    fn resume_then_complete_across_chunks() {
        let data = vec![7u8; 2500];
        let mut m = Mock {
            script: vec![
                Ok(ChunkOutcome::Incomplete { next_offset: 1000 }),
                Ok(ChunkOutcome::Incomplete { next_offset: 2000 }),
                Ok(ChunkOutcome::Complete {
                    video_id: "vid2".into(),
                }),
            ],
            idx: 0,
            start_fail_once: false,
        };
        let meta = VideoMetadata::default();
        let mut last = (0u64, 0u64);
        let mut prog = |s: u64, t: u64| last = (s, t);
        let id = upload_loop(&mut m, &data, &meta, "video/webm", 1000, 3, &mut prog).unwrap();
        assert_eq!(id, "vid2");
        assert_eq!(last, (2500, 2500));
    }

    #[test]
    fn transient_chunk_error_is_retried() {
        let mut m = Mock {
            script: vec![
                Err(YtError::Transient("blip".into())),
                Ok(ChunkOutcome::Complete {
                    video_id: "vid3".into(),
                }),
            ],
            idx: 0,
            start_fail_once: true, // also retry the session open
        };
        let meta = VideoMetadata::default();
        let id = upload_loop(&mut m, b"data", &meta, "video/webm", 1024, 3, &mut noop).unwrap();
        assert_eq!(id, "vid3");
    }

    #[test]
    fn permanent_error_fails_fast() {
        let mut m = Mock {
            script: vec![Err(YtError::Permanent("bad request".into()))],
            idx: 0,
            start_fail_once: false,
        };
        let meta = VideoMetadata::default();
        let err =
            upload_loop(&mut m, b"data", &meta, "video/webm", 1024, 3, &mut noop).unwrap_err();
        assert!(matches!(err, YtError::Permanent(_)));
    }

    #[test]
    fn empty_file_is_refused() {
        let mut m = Mock {
            script: vec![],
            idx: 0,
            start_fail_once: false,
        };
        let meta = VideoMetadata::default();
        let err = upload_loop(&mut m, b"", &meta, "video/webm", 1024, 3, &mut noop).unwrap_err();
        assert!(matches!(err, YtError::Permanent(_)));
    }
}
