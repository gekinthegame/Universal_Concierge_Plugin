//! The rules channel (autoupdater §5) — the YARA-X detection rules that update on
//! their own over IPNS, the one scoped exception to egress-locked-by-default
//! (Decision D-AU-2: stale malware rules are a *safety regression*, not a
//! convenience gap).
//!
//! Consumer pipeline (§5b): resolve IPNS → fetch manifest → [`verify`] ladder →
//! fetch bundle by CID → **yara-x compile gate** → **atomic swap** with **rollback
//! to last-known-good**. Any failure keeps the previous ruleset live (Decision
//! D-AU-5, never break the scanner).

use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::verify::{self, Freshness, Manifest};
use super::{baseline, now_secs, updates_dir, Result, RulesState, UpdateError};
use crate::node;

/// A source of rules updates. IPNS in production ([`IpnsRuleFetcher`]); a local
/// in-memory source in tests ([`LocalRuleFetcher`]). Keeping the fetch behind a
/// trait makes the whole verify/swap/rollback pipeline testable without a node.
pub trait RuleFetcher {
    /// Resolve the publisher's "latest" pointer and return the signed manifest.
    fn resolve_manifest(&self) -> Result<Manifest>;
    /// Fetch the content-addressed bundle bytes named by `cid`.
    fn fetch_bundle(&self, cid: &str) -> Result<FetchedBundle>;
}

/// Bytes returned by a fetcher and the CID under which those bytes were fetched.
#[derive(Debug, Clone)]
pub struct FetchedBundle {
    pub cid: String,
    pub bytes: Vec<u8>,
}

/// Production fetcher: resolves the publisher IPNS name through the user's Kubo node
/// and pulls the manifest + bundle by CID (content-addressed, so the node verifies
/// the bytes hash to the CID it asked for).
pub struct IpnsRuleFetcher {
    repo: PathBuf,
    ipns_name: String,
}

impl IpnsRuleFetcher {
    pub fn new(repo: impl Into<PathBuf>, ipns_name: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            ipns_name: ipns_name.into(),
        }
    }
}

impl RuleFetcher for IpnsRuleFetcher {
    fn resolve_manifest(&self) -> Result<Manifest> {
        if self.ipns_name.trim().is_empty() {
            return Err(UpdateError::NodeUnavailable(
                "no rules IPNS name configured (set config.update.rules_ipns or `concierge-plugin rules source <k51...>`)"
                    .to_string(),
            ));
        }
        let manifest_cid = node::ipns_resolve(&self.repo, &self.ipns_name)
            .map_err(|e| UpdateError::NodeUnavailable(e.to_string()))?;
        let bytes = node::ipfs_cat(&self.repo, &manifest_cid)
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        Manifest::from_json(&bytes)
    }

    fn fetch_bundle(&self, cid: &str) -> Result<FetchedBundle> {
        let rules_path = format!("{cid}/rules.yar");
        let bytes = match node::ipfs_cat(&self.repo, &rules_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                node::ipfs_cat(&self.repo, cid).map_err(|e| UpdateError::Network(e.to_string()))?
            }
        };
        Ok(FetchedBundle {
            cid: cid.to_string(),
            bytes,
        })
    }
}

/// A point-in-time, serializable view of the rules channel for the CLI/GUI.
#[derive(Debug, Clone, Serialize)]
pub struct RulesStatus {
    pub epoch: u64,
    pub version: String,
    pub cid: String,
    /// Age of the applied manifest in seconds (`0` for the baked baseline).
    pub age_secs: u64,
    /// `true` while within the freshness window (or baked).
    pub fresh: bool,
    /// Fingerprint of the publisher key that signed the active ruleset.
    pub publisher_fpr: String,
    /// Kill-switch state.
    pub paused: bool,
    /// Last poll time (unix secs), `0` if never polled.
    pub last_poll_ts: u64,
    /// Number of rules in the active ruleset (best-effort compile count).
    pub rule_count: usize,
}

/// The result of a refresh attempt.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshOutcome {
    /// `true` if a newer ruleset was activated; `false` if already current.
    pub updated: bool,
    pub epoch: u64,
    pub version: String,
    /// `true` if the (verified) manifest is older than the freshness window.
    pub stale_publisher: bool,
    pub age_secs: u64,
}

/// The rules channel rooted at a store's `<store>/updates` directory.
pub struct RulesChannel {
    store_dir: PathBuf,
    freshness_secs: u64,
}

impl RulesChannel {
    pub fn new(store_dir: impl Into<PathBuf>, freshness_secs: u64) -> Self {
        Self {
            store_dir: store_dir.into(),
            freshness_secs,
        }
    }

    fn rules_dir(&self) -> PathBuf {
        updates_dir(&self.store_dir).join("rules")
    }

    /// Path of the live ruleset the scanner loads.
    pub fn active_rules_path(&self) -> PathBuf {
        self.rules_dir().join("active.yar")
    }

    fn lkg_rules_path(&self) -> PathBuf {
        self.rules_dir().join("last_known_good.yar")
    }

    /// First-run bootstrap (Decision D-AU-4): if there is no active ruleset yet, lay
    /// down the baked baseline as both active and last-known-good and persist the
    /// baseline state. Idempotent — safe to call on every start.
    pub fn ensure_baseline(&self) -> Result<()> {
        let active = self.active_rules_path();
        if active.exists() {
            let text = std::fs::read_to_string(&active)
                .map_err(|e| UpdateError::Io(format!("read active ruleset: {e}")))?;
            compile_gate(&text)?;
            if !self.lkg_rules_path().exists() {
                atomic_write(&self.lkg_rules_path(), text.as_bytes())?;
            }
            if !RulesState::path(&self.store_dir).exists()
                && text.as_bytes() == baseline::BAKED_RULES
            {
                RulesState::default().save(&self.store_dir)?;
            }
            return Ok(());
        }
        // The baseline must itself compile under the live engine, or we ship a binary
        // whose scanner is dead on arrival — fail loudly in that (build-time) case.
        compile_gate(&String::from_utf8_lossy(baseline::BAKED_RULES))?;
        std::fs::create_dir_all(self.rules_dir())
            .map_err(|e| UpdateError::Io(format!("create rules dir: {e}")))?;
        atomic_write(&active, baseline::BAKED_RULES)?;
        atomic_write(&self.lkg_rules_path(), baseline::BAKED_RULES)?;
        // Persist baseline state only if none exists (don't clobber a real epoch).
        if RulesState::load(&self.store_dir)?.applied_ts == 0 {
            RulesState::default().save(&self.store_dir)?;
        }
        Ok(())
    }

    /// The trusted verifying keys: the baked anchors plus any user-pinned keys.
    fn trusted_keys(&self, state: &RulesState) -> Vec<VerifyingKey> {
        let mut keys = baseline::publisher_pubkeys();
        for h in &state.pinned_pubkeys {
            if let Ok(k) = baseline::parse_pubkey_hex(h) {
                keys.push(k);
            }
        }
        keys
    }

    /// Current status (ensures the baseline exists first).
    pub fn status(&self) -> Result<RulesStatus> {
        self.ensure_baseline()?;
        let state = RulesState::load(&self.store_dir)?;
        let now = now_secs();
        let age = if state.manifest_ts == 0 {
            0
        } else {
            now.saturating_sub(state.manifest_ts)
        };
        let fresh = state.manifest_ts == 0 || age <= self.freshness_secs;
        let rule_count = std::fs::read_to_string(self.active_rules_path())
            .ok()
            .map(|text| count_rules(&text))
            .unwrap_or(0);
        Ok(RulesStatus {
            epoch: state.epoch,
            version: state.version,
            cid: state.cid,
            age_secs: age,
            fresh,
            publisher_fpr: state.publisher_fpr,
            paused: state.paused,
            last_poll_ts: state.last_poll_ts,
            rule_count,
        })
    }

    /// The kill switch: pause or resume automatic rules updates.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.ensure_baseline()?;
        let mut state = RulesState::load(&self.store_dir)?;
        state.paused = paused;
        state.save(&self.store_dir)
    }

    /// Pin an additional publisher key (`concierge rules pin <key>`). Validated before
    /// it is persisted, and de-duplicated.
    pub fn pin_publisher_key(&self, hex: &str) -> Result<()> {
        let key = baseline::parse_pubkey_hex(hex)?; // validate
        let canonical = hex_lower(&key.to_bytes());
        self.ensure_baseline()?;
        let mut state = RulesState::load(&self.store_dir)?;
        if !state
            .pinned_pubkeys
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&canonical))
        {
            state.pinned_pubkeys.push(canonical);
            state.save(&self.store_dir)?;
        }
        Ok(())
    }

    /// Whether enough time has elapsed since the last poll, with ±10% jitter so a
    /// fleet of nodes doesn't stampede the publisher at the same instant.
    pub fn poll_due(&self, interval_secs: u64) -> Result<bool> {
        self.poll_due_at(interval_secs, now_secs())
    }

    pub fn poll_due_at(&self, interval_secs: u64, now: u64) -> Result<bool> {
        let state = RulesState::load(&self.store_dir)?;
        if state.last_poll_ts == 0 {
            return Ok(true);
        }
        let elapsed = now.saturating_sub(state.last_poll_ts);
        let due_after = jittered_interval(interval_secs, state.last_poll_ts, &self.store_dir);
        Ok(elapsed >= due_after)
    }

    /// The consumer pipeline. Verifies and atomically activates a strictly-newer
    /// ruleset, or no-ops if already current. On any failure the active ruleset is
    /// left untouched (the swap only happens after verify + compile both pass).
    pub fn refresh<F: RuleFetcher>(&self, fetcher: &F, now: u64) -> Result<RefreshOutcome> {
        self.ensure_baseline()?;
        let mut state = RulesState::load(&self.store_dir)?;

        // Record the poll attempt regardless of outcome (best-effort; ignore a write
        // race here so a transient FS error doesn't mask the real result).
        state.last_poll_ts = now;
        let _ = state.save(&self.store_dir);

        if state.paused {
            return Err(UpdateError::Paused);
        }

        let manifest = fetcher.resolve_manifest()?;
        let bundle = fetcher.fetch_bundle(&manifest.cid)?;
        let trusted = self.trusted_keys(&state);

        // Already current is a no-op, but still verifies the publisher signature and
        // content hash so a compromised/broken channel cannot look healthy.
        if manifest.epoch == state.epoch {
            let freshness = verify::verify_current_bundle(
                &manifest,
                &bundle.bytes,
                &bundle.cid,
                &trusted,
                now,
                self.freshness_secs,
            )?;
            let (stale, age) = match freshness {
                Freshness::Stale { age_secs } => (true, age_secs),
                Freshness::Fresh => (false, now.saturating_sub(manifest.ts)),
            };
            return Ok(RefreshOutcome {
                updated: false,
                epoch: state.epoch,
                version: state.version,
                stale_publisher: stale,
                age_secs: age,
            });
        }

        // Steps 1–4 (sig / epoch / cid+sha256 / freshness).
        let freshness = verify::verify_bundle(
            &manifest,
            &bundle.bytes,
            &bundle.cid,
            &trusted,
            state.epoch,
            now,
            self.freshness_secs,
        )?;

        // Step 5 — must compile under the live engine, else reject and keep LKG.
        let text = String::from_utf8(bundle.bytes.clone())
            .map_err(|e| UpdateError::RulesetCompile(format!("ruleset is not utf-8: {e}")))?;
        compile_gate(&text)?;

        // Atomic swap with last-known-good preserved.
        self.activate(&bundle.bytes)?;

        // Which baked/pinned key signed it → fingerprint for the UI.
        let key_idx = verify::verify_signature(&manifest, &trusted)?;
        let fpr = trusted
            .get(key_idx)
            .map(|k| fingerprint(&k.to_bytes()))
            .unwrap_or_else(|| "unknown".to_string());

        state.epoch = manifest.epoch;
        state.version = manifest.version.clone();
        state.cid = manifest.cid.clone();
        state.applied_ts = now;
        state.manifest_ts = manifest.ts;
        state.publisher_fpr = fpr;
        state.save(&self.store_dir)?;

        let (stale, age) = match freshness {
            Freshness::Stale { age_secs } => (true, age_secs),
            Freshness::Fresh => (false, now.saturating_sub(manifest.ts)),
        };
        Ok(RefreshOutcome {
            updated: true,
            epoch: manifest.epoch,
            version: manifest.version,
            stale_publisher: stale,
            age_secs: age,
        })
    }

    /// Write the new ruleset live: snapshot the current active copy as last-known-good
    /// first, then atomically replace active. The previous bytes remain on disk as the
    /// rollback target if a later activation fails.
    fn activate(&self, bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(self.rules_dir())
            .map_err(|e| UpdateError::Io(format!("create rules dir: {e}")))?;
        let active = self.active_rules_path();
        if let Ok(prev) = std::fs::read(&active) {
            atomic_write(&self.lkg_rules_path(), &prev)?;
        }
        atomic_write(&active, bytes)
    }

    /// Roll the active ruleset back to the last-known-good copy. Used if a freshly
    /// activated ruleset is later found to misbehave at scan time.
    pub fn rollback(&self) -> Result<()> {
        let lkg = std::fs::read(self.lkg_rules_path())
            .map_err(|e| UpdateError::Io(format!("no last-known-good ruleset: {e}")))?;
        compile_gate(&String::from_utf8_lossy(&lkg))?;
        atomic_write(&self.active_rules_path(), &lkg)
    }
}

/// Compile the ruleset under the live `yara-x`. This is the gate in step 5 of the
/// ladder: a bundle that does not compile is rejected so a broken update can never
/// take the scanner down (Decision D-AU-5). We only need compile success here — the
/// compiled `Rules` are loaded by the scanner elsewhere.
fn compile_gate(text: &str) -> Result<()> {
    yara_x::compile(text).map_err(|e| UpdateError::RulesetCompile(e.to_string()))?;
    Ok(())
}

/// Count top-level `rule <name>` declarations for the status display. A cheap textual
/// pass (the ruleset has already passed [`compile_gate`], so it is well-formed); this
/// avoids depending on the engine's iteration API for a cosmetic number.
fn count_rules(text: &str) -> usize {
    text.lines()
        .map(str::trim_start)
        .filter(|l| {
            let rest = l
                .strip_prefix("private ")
                .or_else(|| l.strip_prefix("global "))
                .unwrap_or(l);
            rest.strip_prefix("rule ")
                .map(|after| {
                    after
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                })
                .unwrap_or(false)
        })
        .count()
}

/// Write `bytes` to `path` atomically (temp file in the same dir + rename).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("yar.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| UpdateError::Io(format!("write {tmp:?}: {e}")))?;
    std::fs::rename(&tmp, path).map_err(|e| UpdateError::Io(format!("rename into {path:?}: {e}")))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Short publisher key fingerprint: first 16 hex chars of SHA-256(key).
fn fingerprint(key_bytes: &[u8]) -> String {
    let digest = Sha256::digest(key_bytes);
    hex_lower(&digest)[..16].to_string()
}

/// Stable, non-cryptographic jitter in `[90%, 110%]` of the interval. It is derived
/// from store path + last poll time, so repeated status checks do not move the goal.
fn jittered_interval(interval_secs: u64, last_poll_ts: u64, store_dir: &Path) -> u64 {
    if interval_secs == 0 {
        return 0;
    }
    let span = (interval_secs / 10).max(1);
    let mut hasher = Sha256::new();
    hasher.update(store_dir.to_string_lossy().as_bytes());
    hasher.update(last_poll_ts.to_le_bytes());
    let digest = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    let offset = u64::from_le_bytes(first) % (span.saturating_mul(2).saturating_add(1));
    interval_secs.saturating_sub(span).saturating_add(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use tempfile::TempDir;

    // A test fetcher signed by a key we control. To exercise the *baked* trust path
    // we instead pin the test key (pin_publisher_key) before refreshing.
    struct Local {
        manifest: Manifest,
        bundle: Vec<u8>,
    }
    impl RuleFetcher for Local {
        fn resolve_manifest(&self) -> Result<Manifest> {
            Ok(self.manifest.clone())
        }
        fn fetch_bundle(&self, cid: &str) -> Result<FetchedBundle> {
            assert_eq!(cid, self.manifest.cid);
            Ok(FetchedBundle {
                cid: cid.to_string(),
                bytes: self.bundle.clone(),
            })
        }
    }

    fn make(epoch: u64, bundle: &[u8], ts: u64, sk: &SigningKey) -> Local {
        let mut m = Manifest {
            epoch,
            version: format!("v{epoch}"),
            cid: format!("bafy-epoch-{epoch}"),
            sha256: verify::sha256_hex(bundle),
            ts,
            engine: "yara-x".into(),
            sig: String::new(),
        };
        m.sig = base64::engine::general_purpose::STANDARD
            .encode(sk.sign(&m.signing_bytes()).to_bytes());
        Local {
            manifest: m,
            bundle: bundle.to_vec(),
        }
    }

    fn channel() -> (TempDir, RulesChannel, SigningKey) {
        let dir = TempDir::new().unwrap();
        let ch = RulesChannel::new(dir.path(), 100);
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        ch.ensure_baseline().unwrap();
        ch.pin_publisher_key(&hex_lower(&sk.verifying_key().to_bytes()))
            .unwrap();
        (dir, ch, sk)
    }

    const GOOD: &[u8] = b"rule t { condition: true }";

    #[test]
    fn baseline_is_laid_down_and_compiles() {
        let dir = TempDir::new().unwrap();
        let ch = RulesChannel::new(dir.path(), 100);
        ch.ensure_baseline().unwrap();
        assert!(ch.active_rules_path().exists());
        let st = ch.status().unwrap();
        assert_eq!(st.epoch, baseline::BAKED_EPOCH);
        assert!(st.rule_count >= 1);
    }

    #[test]
    fn good_update_activates_and_persists() {
        let (_d, ch, sk) = channel();
        let out = ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap();
        assert!(out.updated);
        assert_eq!(out.epoch, 2);
        let active = std::fs::read(ch.active_rules_path()).unwrap();
        assert_eq!(active, GOOD);
        assert_eq!(ch.status().unwrap().epoch, 2);
    }

    #[test]
    fn rollback_is_refused_and_keeps_active() {
        let (_d, ch, sk) = channel();
        ch.refresh(&make(5, GOOD, 1000, &sk), 1000).unwrap();
        let err = ch
            .refresh(&make(3, b"rule other { condition: true }", 1000, &sk), 1000)
            .unwrap_err();
        assert!(matches!(err, UpdateError::Rollback { .. }));
        // Active ruleset is unchanged (still epoch 5's bytes).
        assert_eq!(std::fs::read(ch.active_rules_path()).unwrap(), GOOD);
        assert_eq!(ch.status().unwrap().epoch, 5);
    }

    #[test]
    fn uncompilable_update_is_rejected_lkg_stays() {
        let (_d, ch, sk) = channel();
        ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap();
        let broken = b"rule bad { this is not yara }";
        let err = ch.refresh(&make(3, broken, 1000, &sk), 1000).unwrap_err();
        assert!(matches!(err, UpdateError::RulesetCompile(_)));
        assert_eq!(std::fs::read(ch.active_rules_path()).unwrap(), GOOD);
        assert_eq!(ch.status().unwrap().epoch, 2);
    }

    #[test]
    fn untrusted_signer_is_rejected() {
        let dir = TempDir::new().unwrap();
        let ch = RulesChannel::new(dir.path(), 100);
        ch.ensure_baseline().unwrap();
        // Do NOT pin this key → not trusted.
        let rogue = SigningKey::from_bytes(&[99u8; 32]);
        let err = ch.refresh(&make(2, GOOD, 1000, &rogue), 1000).unwrap_err();
        assert!(matches!(err, UpdateError::BadSignature(_)));
    }

    #[test]
    fn paused_channel_refuses_refresh() {
        let (_d, ch, sk) = channel();
        ch.set_paused(true).unwrap();
        let err = ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap_err();
        assert!(matches!(err, UpdateError::Paused));
    }

    #[test]
    fn already_current_is_a_noop() {
        let (_d, ch, sk) = channel();
        ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap();
        let out = ch.refresh(&make(2, GOOD, 1000, &sk), 1001).unwrap();
        assert!(!out.updated);
        assert_eq!(out.epoch, 2);
    }

    #[test]
    fn already_current_still_verifies_signature_and_hash() {
        let (_d, ch, sk) = channel();
        ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap();

        let rogue = SigningKey::from_bytes(&[99u8; 32]);
        let err = ch.refresh(&make(2, GOOD, 1000, &rogue), 1001).unwrap_err();
        assert!(matches!(err, UpdateError::BadSignature(_)));

        let mut bad = make(2, b"rule other { condition: true }", 1001, &sk);
        bad.bundle = GOOD.to_vec();
        let err = ch.refresh(&bad, 1001).unwrap_err();
        assert!(matches!(err, UpdateError::HashMismatch { .. }));
    }

    #[test]
    fn fetched_cid_mismatch_is_rejected() {
        struct Mismatch(Local);
        impl RuleFetcher for Mismatch {
            fn resolve_manifest(&self) -> Result<Manifest> {
                Ok(self.0.manifest.clone())
            }

            fn fetch_bundle(&self, _cid: &str) -> Result<FetchedBundle> {
                Ok(FetchedBundle {
                    cid: "bafy-wrong".to_string(),
                    bytes: self.0.bundle.clone(),
                })
            }
        }

        let (_d, ch, sk) = channel();
        let err = ch
            .refresh(&Mismatch(make(2, GOOD, 1000, &sk)), 1000)
            .unwrap_err();
        assert!(matches!(err, UpdateError::CidMismatch { .. }));
    }

    #[test]
    fn poll_due_uses_stable_jitter() {
        let dir = TempDir::new().unwrap();
        let ch = RulesChannel::new(dir.path(), 100);
        ch.ensure_baseline().unwrap();
        let mut state = RulesState::load(dir.path()).unwrap();
        state.last_poll_ts = 1_000;
        state.save(dir.path()).unwrap();

        let first = ch.poll_due_at(1_000, 1_900).unwrap();
        for _ in 0..10 {
            assert_eq!(ch.poll_due_at(1_000, 1_900).unwrap(), first);
        }
        assert!(ch.poll_due_at(1_000, 2_200).unwrap());
    }

    #[test]
    fn manual_rollback_restores_lkg() {
        let (_d, ch, sk) = channel();
        ch.refresh(&make(2, GOOD, 1000, &sk), 1000).unwrap();
        let v3 = b"rule three { condition: false }";
        ch.refresh(&make(3, v3, 1000, &sk), 1000).unwrap();
        assert_eq!(std::fs::read(ch.active_rules_path()).unwrap(), v3);
        ch.rollback().unwrap();
        // LKG was epoch-2's bytes.
        assert_eq!(std::fs::read(ch.active_rules_path()).unwrap(), GOOD);
    }
}
