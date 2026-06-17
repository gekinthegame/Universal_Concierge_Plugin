//! The autoupdater — two channels, one trust model (`AUTOUPDATER_PLAN.md`).
//!
//! - **App channel** ([`app`]) updates the *binary* via the GitHub release pipeline:
//!   poll → semver compare → download → SHASUMS + detached-signature verify → stage →
//!   apply on next launch.
//! - **Rules channel** ([`rules`]) updates the YARA-X *detection rules* via a single
//!   signed **IPNS** pointer the user's Kubo node resolves: resolve → fetch manifest →
//!   verify ladder → fetch bundle → yara-x compile gate → atomic swap with rollback.
//!
//! Both share the [`verify`] ladder (Ed25519 over the artifact vs a **baked** pubkey,
//! monotonic [`epoch`](verify::Manifest::epoch) anti-rollback, freshness window) and
//! the [`baseline`] baked floor + trust anchors. IPNS/GitHub are only mutable
//! pointers, never the trust anchor (Decision D-AU-3).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod app;
pub mod baseline;
pub mod rules;
pub mod verify;

pub use app::{AppUpdateStatus, ReleaseInfo, StagedUpdate};
pub use rules::{RefreshOutcome, RuleFetcher, RulesChannel, RulesStatus};
pub use verify::{Freshness, Manifest};

/// Every distinct way an update can fail. Kept typed (project rule: callers never
/// parse error strings) and converted into [`crate::error::Error::Update`] only at
/// the core-binding boundary.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The detached signature did not verify against any baked/pinned key.
    #[error("signature rejected: {0}")]
    BadSignature(String),
    /// A pinned/baked key could not be parsed.
    #[error("bad key: {0}")]
    BadKey(String),
    /// The manifest JSON was malformed or missing required fields.
    #[error("manifest invalid: {0}")]
    Manifest(String),
    /// Anti-rollback: the offered epoch is not strictly newer than what we trust.
    #[error("rollback refused: manifest epoch {manifest_epoch} <= last applied {last_epoch}")]
    Rollback {
        manifest_epoch: u64,
        last_epoch: u64,
    },
    /// The bytes fetched were under a different CID than the manifest names.
    #[error("cid mismatch: expected {expected}, fetched {got}")]
    CidMismatch { expected: String, got: String },
    /// The bundle's SHA-256 did not match the (signed) manifest.
    #[error("content hash mismatch: expected {expected}, got {got}")]
    HashMismatch { expected: String, got: String },
    /// The new ruleset failed to compile under the live `yara-x` — rejected; the
    /// last-known-good ruleset stays live (Decision D-AU-5, never break the scanner).
    #[error("ruleset failed yara-x validation: {0}")]
    RulesetCompile(String),
    /// Automatic rules updates are paused by the user (the kill switch).
    #[error("automatic rules updates are paused (kill switch on)")]
    Paused,
    /// The local Kubo node is needed (IPNS resolve/fetch) but is not reachable.
    #[error("node unavailable: {0}")]
    NodeUnavailable(String),
    /// A semver string (release tag or current version) could not be parsed.
    #[error("version parse: {0}")]
    Version(String),
    /// The release host / network call failed.
    #[error("network: {0}")]
    Network(String),
    /// A filesystem operation in the staging/swap path failed.
    #[error("io: {0}")]
    Io(String),
}

/// Result alias for the update module.
pub type Result<T> = std::result::Result<T, UpdateError>;

/// Current unix time in seconds (saturating to 0 before the epoch — only possible
/// with a badly wrong clock, which the freshness logic tolerates).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The autoupdater's working directory under the store: `<store>/updates`. Holds the
/// active ruleset, the last-known-good copy, persisted [`RulesState`], and the app
/// staging area — all derived from the store, never the project root, so the
/// updater is self-contained.
pub fn updates_dir(store_dir: &Path) -> PathBuf {
    store_dir.join("updates")
}

/// Persisted rules-channel state. Small, human-inspectable JSON at
/// `<store>/updates/rules_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesState {
    /// The epoch currently applied. Starts at the baked baseline epoch.
    pub epoch: u64,
    /// The applied ruleset's human version.
    pub version: String,
    /// The CID the applied bundle came from (`"baked"` for the baseline).
    pub cid: String,
    /// When the applied ruleset was activated (unix secs).
    pub applied_ts: u64,
    /// The signed publish timestamp of the applied manifest (for freshness display).
    pub manifest_ts: u64,
    /// Fingerprint (first 16 hex of SHA-256) of the publisher key that signed it.
    pub publisher_fpr: String,
    /// The kill switch: when true, automatic refresh is skipped.
    pub paused: bool,
    /// Last time a poll ran (unix secs), for the jittered interval and the GUI.
    pub last_poll_ts: u64,
    /// Extra publisher keys the user pinned (`concierge rules pin <key>`), hex-encoded.
    /// Merged with the baked anchors when verifying.
    pub pinned_pubkeys: Vec<String>,
}

impl Default for RulesState {
    fn default() -> Self {
        Self {
            epoch: baseline::BAKED_EPOCH,
            version: baseline::BAKED_VERSION.to_string(),
            cid: "baked".to_string(),
            applied_ts: 0,
            manifest_ts: 0,
            publisher_fpr: "baked".to_string(),
            paused: false,
            last_poll_ts: 0,
            pinned_pubkeys: Vec::new(),
        }
    }
}

impl RulesState {
    fn path(store_dir: &Path) -> PathBuf {
        updates_dir(store_dir).join("rules_state.json")
    }

    /// Load persisted state, or the default (baked baseline) if none exists.
    pub fn load(store_dir: &Path) -> Result<Self> {
        let path = Self::path(store_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| UpdateError::Io(format!("parse rules state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(UpdateError::Io(format!("read rules state: {e}"))),
        }
    }

    /// Persist state atomically (write-temp-then-rename) so a crash mid-write can
    /// never leave a half-written state file.
    pub fn save(&self, store_dir: &Path) -> Result<()> {
        let dir = updates_dir(store_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| UpdateError::Io(format!("create updates dir: {e}")))?;
        let path = Self::path(store_dir);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| UpdateError::Io(format!("serialize rules state: {e}")))?;
        std::fs::write(&tmp, &bytes)
            .map_err(|e| UpdateError::Io(format!("write state tmp: {e}")))?;
        std::fs::rename(&tmp, &path).map_err(|e| UpdateError::Io(format!("rename state: {e}")))?;
        Ok(())
    }
}

/// Core-binding surface for the CLI and GUI. These map the typed [`UpdateError`] into
/// [`crate::error::Error::Update`] at the boundary (via the `From` impl in `error.rs`)
/// and pull the IPNS name / freshness window / repo from the project [`crate::Config`].
impl crate::binding::MemCli {
    fn rules_channel(&self) -> crate::error::Result<RulesChannel> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        Ok(RulesChannel::new(store, cfg.update.freshness_secs()))
    }

    /// Status of the rules channel (epoch, version, freshness, publisher, kill switch).
    pub fn rules_status(&self) -> crate::error::Result<RulesStatus> {
        Ok(self.rules_channel()?.status()?)
    }

    /// Run one rules refresh: resolve the publisher IPNS pointer through the user's
    /// **public** Kubo node (the private PNET node cannot reach public IPNS), verify,
    /// and atomically swap on success.
    pub fn rules_refresh(&self) -> crate::error::Result<RefreshOutcome> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        let ipns = rules_ipns_name(&cfg)?;
        crate::node::launch_public_node(&store)?;
        let repo = crate::node::public_repo_for(&store);
        let fetcher = rules::IpnsRuleFetcher::new(repo, ipns);
        Ok(self.rules_channel()?.refresh(&fetcher, now_secs())?)
    }

    /// Run the automatic rules poll if config + persisted state say it is due.
    /// Missing publisher configuration is a no-op for local/dev builds; explicit
    /// `rules refresh` still returns a clear error.
    pub fn rules_auto_refresh_if_due(&self) -> crate::error::Result<Option<RefreshOutcome>> {
        let cfg = self.config()?;
        if !cfg.update.auto_rules || rules_ipns_name(&cfg).is_err() {
            return Ok(None);
        }
        let channel = self.rules_channel()?;
        let status = channel.status()?;
        if status.paused || !channel.poll_due(cfg.update.poll_interval_secs)? {
            return Ok(None);
        }
        self.rules_refresh().map(Some)
    }

    /// Pin an additional trusted publisher key (`concierge rules pin <key>`).
    pub fn rules_pin(&self, pubkey_hex: &str) -> crate::error::Result<()> {
        Ok(self.rules_channel()?.pin_publisher_key(pubkey_hex)?)
    }

    /// Configure the rules IPNS source (`k51…` or `/ipns/k51…`) for this project.
    pub fn rules_set_source(&self, ipns_name: &str) -> crate::error::Result<()> {
        let ipns_name = normalized_ipns_name(ipns_name)?;
        let mut cfg = self.config()?;
        cfg.update.rules_ipns = ipns_name;
        cfg.save_to_project_root(self.working_dir())
            .map_err(crate::error::Error::Io)
    }

    /// The kill switch — pause or resume automatic rules updates.
    pub fn rules_set_paused(&self, paused: bool) -> crate::error::Result<()> {
        Ok(self.rules_channel()?.set_paused(paused)?)
    }

    /// Check the app channel for a newer release (read-only).
    pub fn update_check(&self) -> crate::error::Result<Option<ReleaseInfo>> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        let release = app::check(&cfg.update.app_repo, app::current_version())?;
        let status = app::AppUpdateStatus {
            checked_ts: now_secs(),
            release: release.clone(),
            error: None,
        };
        app::save_cached_status(&store, &status)?;
        Ok(release)
    }

    pub fn update_cached_status(&self) -> crate::error::Result<Option<app::AppUpdateStatus>> {
        let store = self.store_dir()?;
        Ok(app::cached_status(&store)?)
    }

    /// Background app poll. It records success or failure for the GUI without
    /// failing the long-running GUI process on transient network errors.
    pub fn update_poll_cache(&self) -> crate::error::Result<app::AppUpdateStatus> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        let checked_ts = now_secs();
        let status = match app::check(&cfg.update.app_repo, app::current_version()) {
            Ok(release) => app::AppUpdateStatus {
                checked_ts,
                release,
                error: None,
            },
            Err(error) => app::AppUpdateStatus {
                checked_ts,
                release: None,
                error: Some(error.to_string()),
            },
        };
        app::save_cached_status(&store, &status)?;
        Ok(status)
    }

    /// Download, verify, and stage the latest release for apply-on-relaunch.
    pub fn update_apply(&self) -> crate::error::Result<Option<StagedUpdate>> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        let Some(release) = app::check(&cfg.update.app_repo, app::current_version())? else {
            return Ok(None);
        };
        Ok(Some(app::stage(&store, &release)?))
    }

    /// Apply a staged update on startup; returns the applied version, if any.
    pub fn update_apply_on_launch(&self) -> crate::error::Result<Option<String>> {
        let store = self.store_dir()?;
        Ok(app::apply_on_launch(&store)?)
    }
}

fn rules_ipns_name(cfg: &crate::Config) -> Result<String> {
    let configured = cfg.update.rules_ipns.trim();
    if !configured.is_empty() {
        return normalized_ipns_name(configured);
    }
    baseline::default_rules_ipns()
        .map(str::to_string)
        .ok_or_else(|| {
            UpdateError::NodeUnavailable(
                "no rules IPNS name configured (set config.update.rules_ipns, run `concierge-plugin rules source <k51...>`, or build with UCP_RULES_IPNS)"
                    .to_string(),
            )
        })
}

fn normalized_ipns_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    let bare = trimmed.strip_prefix("/ipns/").unwrap_or(trimmed);
    let valid = !bare.is_empty()
        && bare.len() <= 128
        && bare
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    valid
        .then(|| bare.to_string())
        .ok_or_else(|| UpdateError::Manifest(format!("invalid IPNS name `{name}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn state_roundtrips_and_defaults_to_baseline() {
        let dir = TempDir::new().unwrap();
        let store = dir.path();
        let loaded = RulesState::load(store).unwrap();
        assert_eq!(loaded.epoch, baseline::BAKED_EPOCH);
        assert!(!loaded.paused);

        let mut s = loaded;
        s.epoch = 9;
        s.paused = true;
        s.save(store).unwrap();
        let again = RulesState::load(store).unwrap();
        assert_eq!(again.epoch, 9);
        assert!(again.paused);
    }

    #[test]
    fn ipns_source_normalizes_and_rejects_bad_names() {
        assert_eq!(
            normalized_ipns_name("/ipns/k51example").unwrap(),
            "k51example"
        );
        assert!(normalized_ipns_name("").is_err());
        assert!(normalized_ipns_name("https://example.com").is_err());
        assert!(normalized_ipns_name("../k51bad").is_err());
    }
}
