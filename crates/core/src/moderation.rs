//! Phase 8 §3 — the Room Guardian: moderation + safety (single-node core).
//!
//! This is the **local** policy + safety logic. The network-era parts — a Guardian
//! AgentID participating in P2P rooms, tombstone-sync over Gossipsub, and inbound
//! proof-of-scan — live in `crates/net` and build on these primitives.
//!
//! Aligned with `THREAT_MODEL.md` + `NETWORK_DEFENSE_PLAN.md`:
//! - **Quarantine the block, not the actor** — quarantine withholds a specific CID;
//!   it never bans a user. It is **reversible** (`release`) — quarantine, not destroy.
//! - **Extension gating** keeps executable payloads out of shared rooms.
//! - **Hash validation** ([`verify_block`]) lets an ingesting node confirm a block's
//!   bytes actually hash to its claimed CID before accepting it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::binding::{Cid, MemCli};
use crate::error::{Error, Result};
use crate::messaging::RoomPolicy;
use crate::update::RulesChannel;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Extensions blocked by default in shared rooms — executables and scripts that
/// have no business propagating as "data" through a knowledge mesh.
pub const DEFAULT_BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "bat", "cmd", "com", "scr", "msi", "dll", "sh", "bash", "ps1", "psm1", "vbs", "js",
    "jse", "wsf", "app", "apk", "jar", "bin", "deb", "rpm", "dmg",
];

/// Per-room file-extension gate: a blocklist (default) or a strict allowlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPolicy {
    /// Block these extensions; allow everything else.
    Block(BTreeSet<String>),
    /// Allow only these extensions; block everything else.
    AllowOnly(BTreeSet<String>),
}

impl Default for ExtensionPolicy {
    fn default() -> Self {
        Self::default_shared()
    }
}

impl ExtensionPolicy {
    /// The default shared-room gate: block known executables/scripts.
    pub fn default_shared() -> Self {
        Self::Block(
            DEFAULT_BLOCKED_EXTENSIONS
                .iter()
                .map(|e| e.to_string())
                .collect(),
        )
    }

    pub fn block<I: IntoIterator<Item = String>>(exts: I) -> Self {
        Self::Block(exts.into_iter().map(|e| e.to_lowercase()).collect())
    }

    pub fn allow_only<I: IntoIterator<Item = String>>(exts: I) -> Self {
        Self::AllowOnly(exts.into_iter().map(|e| e.to_lowercase()).collect())
    }

    /// Whether a file path is permitted by this policy. Extension match is
    /// case-insensitive; a path with no extension is allowed by a blocklist and
    /// denied by an allowlist.
    pub fn allows_path(&self, path: &str) -> bool {
        let ext = extension_of(path);
        match self {
            Self::Block(blocked) => match ext {
                Some(ext) => !blocked.contains(&ext),
                None => true,
            },
            Self::AllowOnly(allowed) => match ext {
                Some(ext) => allowed.contains(&ext),
                None => false,
            },
        }
    }
}

fn extension_of(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|ext| !ext.is_empty())
}

/// One quarantine entry: why and when. Block-scoped — it names a CID, never a user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuarantineRecord {
    pub reason: String,
    pub at: u64,
}

/// A local, **reversible**, block-scoped quarantine list — the threat model's
/// "bad-CID list". A quarantined CID is withheld from relay/surfacing but never
/// deleted, and `release` always lifts it. In the network era, mesh-scoped
/// tombstone-sync feeds entries here (from authorities the node trusts).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuarantineRegistry {
    #[serde(default)]
    entries: BTreeMap<String, QuarantineRecord>,
}

impl QuarantineRegistry {
    pub fn quarantine(&mut self, cid: &str, reason: impl Into<String>, at: u64) {
        self.entries.insert(
            cid.to_string(),
            QuarantineRecord {
                reason: reason.into(),
                at,
            },
        );
    }

    /// Lift a quarantine (always available — quarantine is reversible).
    pub fn release(&mut self, cid: &str) -> bool {
        self.entries.remove(cid).is_some()
    }

    pub fn is_quarantined(&self, cid: &str) -> bool {
        self.entries.contains_key(cid)
    }

    pub fn list(&self) -> impl Iterator<Item = (&String, &QuarantineRecord)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Load from JSON; an absent/unreadable file yields an empty registry.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// Confirm a block's bytes actually hash to its claimed CID (proof-of-scan /
/// ingest hash-validation). `block_bytes` are the stored DAG-CBOR-encoded bytes.
pub fn verify_block(claimed: &Cid, block_bytes: &[u8]) -> bool {
    mem::cid::compute(block_bytes).to_string() == claimed.0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YaraMatch {
    pub namespace: String,
    pub rule: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct YaraScanReport {
    pub matches: Vec<YaraMatch>,
}

impl YaraScanReport {
    pub fn clean(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Compiled YARA-X rules loaded from the updater's active ruleset.
pub struct YaraScanner {
    rules: yara_x::Rules,
}

impl YaraScanner {
    pub fn from_rules_text(text: &str) -> Result<Self> {
        let rules = yara_x::compile(text)
            .map_err(|e| Error::SecurityPolicy(format!("active YARA rules do not compile: {e}")))?;
        Ok(Self { rules })
    }

    pub fn scan_bytes(&self, bytes: &[u8]) -> Result<YaraScanReport> {
        let mut scanner = yara_x::Scanner::new(&self.rules);
        scanner
            .set_timeout(std::time::Duration::from_secs(10))
            .max_matches_per_pattern(32)
            .fast_scan(true);
        let results = scanner
            .scan(bytes)
            .map_err(|e| Error::SecurityPolicy(format!("YARA scan failed: {e}")))?;
        let matches = results
            .matching_rules()
            .map(|rule| YaraMatch {
                namespace: rule.namespace().to_string(),
                rule: rule.identifier().to_string(),
            })
            .collect();
        Ok(YaraScanReport { matches })
    }
}

/// Store-backed quarantine registry persisted at `<store>/quarantine.json`.
impl MemCli {
    fn quarantine_path(&self) -> Result<std::path::PathBuf> {
        Ok(self.store_dir()?.join("quarantine.json"))
    }

    /// Quarantine a CID locally (reversible, block-scoped — withholds the block,
    /// never bans a user). In the network era, trusted tombstone-sync calls this.
    pub fn quarantine_cid(&self, cid: &Cid, reason: &str) -> Result<()> {
        let path = self.quarantine_path()?;
        let mut registry = QuarantineRegistry::load(&path);
        registry.quarantine(&cid.0, reason, now_secs());
        registry
            .save(&path)
            .map_err(|e| Error::Io(format!("save quarantine registry: {e}")))
    }

    /// Lift a quarantine. Returns whether the CID had been quarantined.
    pub fn release_cid(&self, cid: &Cid) -> Result<bool> {
        let path = self.quarantine_path()?;
        let mut registry = QuarantineRegistry::load(&path);
        let lifted = registry.release(&cid.0);
        registry
            .save(&path)
            .map_err(|e| Error::Io(format!("save quarantine registry: {e}")))?;
        Ok(lifted)
    }

    pub fn is_quarantined(&self, cid: &Cid) -> Result<bool> {
        Ok(QuarantineRegistry::load(&self.quarantine_path()?).is_quarantined(&cid.0))
    }

    /// The full local quarantine registry (for listing / the GUI).
    pub fn quarantine_registry(&self) -> Result<QuarantineRegistry> {
        Ok(QuarantineRegistry::load(&self.quarantine_path()?))
    }

    /// Load the active YARA-X ruleset selected by the autoupdater. First launch lays
    /// down the baked baseline, so this works offline before the rules channel updates.
    pub fn active_yara_scanner(&self) -> Result<YaraScanner> {
        let store = self.store_dir()?;
        let cfg = self.config()?;
        let channel = RulesChannel::new(&store, cfg.update.freshness_secs());
        channel.ensure_baseline()?;
        let text = std::fs::read_to_string(channel.active_rules_path())
            .map_err(|e| Error::Io(format!("read active YARA rules: {e}")))?;
        YaraScanner::from_rules_text(&text)
    }

    pub fn scan_bytes_with_active_rules(&self, bytes: &[u8]) -> Result<YaraScanReport> {
        self.active_yara_scanner()?.scan_bytes(bytes)
    }
}

/// The Guardian's decision for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Surface/accept it.
    Allow,
    /// Locally hide it (the author/room policy says so) — reversible, block-scoped.
    Mute(String),
    /// Withhold it (a quarantined/unsafe CID) — reversible.
    Quarantine(String),
    /// Refuse it outright (e.g. a blocked executable extension at a room boundary).
    Block(String),
}

/// Stateless moderation decisions, composed from the room policy, the mute list,
/// the quarantine registry, and the extension gate.
pub struct Guardian;

impl Guardian {
    /// Screen a message for surfacing in a room. Order: quarantined CID → muted
    /// author → AI-send policy → allow. Mute/quarantine are local & reversible.
    pub fn screen_message(
        policy: &RoomPolicy,
        quarantine: &QuarantineRegistry,
        author_kind: &str,
        author_id: &str,
        payload: &str,
        cid: &str,
    ) -> Verdict {
        if quarantine.is_quarantined(cid) {
            return Verdict::Quarantine("quarantined CID".to_string());
        }
        if policy.muted.contains(author_id) {
            return Verdict::Mute("muted author".to_string());
        }
        if !policy.may_send(author_kind, payload) {
            return Verdict::Mute(format!("room AI-send policy: {}", policy.ai_send));
        }
        Verdict::Allow
    }

    /// Screen a file for sharing into a room (extension gate).
    pub fn screen_file(ext_policy: &ExtensionPolicy, path: &str) -> Verdict {
        if ext_policy.allows_path(path) {
            Verdict::Allow
        } else {
            Verdict::Block(format!("file extension not permitted in this room: {path}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_gate_blocks_executables_by_default() {
        let policy = ExtensionPolicy::default_shared();
        assert!(!policy.allows_path("setup.exe"), "exe blocked");
        assert!(
            !policy.allows_path("/tmp/Payload.BAT"),
            "case-insensitive, path-aware"
        );
        assert!(!policy.allows_path("evil.sh"));
        assert!(policy.allows_path("notes.md"), "docs allowed");
        assert!(policy.allows_path("diagram.png"));
        assert!(
            policy.allows_path("README"),
            "no extension allowed under a blocklist"
        );
    }

    #[test]
    fn allowlist_mode_permits_only_listed_extensions() {
        let policy = ExtensionPolicy::allow_only(["md".to_string(), "txt".to_string()]);
        assert!(policy.allows_path("notes.md"));
        assert!(!policy.allows_path("photo.png"), "not on the allowlist");
        assert!(
            !policy.allows_path("README"),
            "no extension denied under an allowlist"
        );
    }

    #[test]
    fn quarantine_is_block_scoped_and_reversible() {
        let mut reg = QuarantineRegistry::default();
        assert!(!reg.is_quarantined("bafyBad"));
        reg.quarantine("bafyBad", "yara: trojan signature", 1000);
        assert!(reg.is_quarantined("bafyBad"));
        assert_eq!(reg.len(), 1);
        // Reversible — release always lifts it (quarantine, not destroy).
        assert!(reg.release("bafyBad"));
        assert!(!reg.is_quarantined("bafyBad"));
        assert!(!reg.release("bafyBad"), "releasing twice is a no-op");
    }

    #[test]
    fn quarantine_registry_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quarantine.json");
        let mut reg = QuarantineRegistry::default();
        reg.quarantine("bafyA", "manual", 1);
        reg.quarantine("bafyB", "tombstone-sync", 2);
        reg.save(&path).unwrap();
        let loaded = QuarantineRegistry::load(&path);
        assert!(loaded.is_quarantined("bafyA"));
        assert!(loaded.is_quarantined("bafyB"));
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn screen_message_quarantines_bad_cids_and_mutes_per_policy() {
        let mut policy = RoomPolicy::default(); // ai_send "on"
        let mut quarantine = QuarantineRegistry::default();
        quarantine.quarantine("bafyBad", "unsafe", 0);

        // A quarantined CID is withheld regardless of author.
        assert_eq!(
            Guardian::screen_message(&policy, &quarantine, "human", "alice", "hi", "bafyBad"),
            Verdict::Quarantine("quarantined CID".to_string())
        );
        // A clean message from a human is allowed.
        assert_eq!(
            Guardian::screen_message(&policy, &quarantine, "human", "alice", "hi", "bafyOk"),
            Verdict::Allow
        );
        // Human-only room mutes an AI author.
        policy.ai_send = "off".to_string();
        assert!(matches!(
            Guardian::screen_message(&policy, &quarantine, "ai", "bot", "hello", "bafyOk"),
            Verdict::Mute(_)
        ));
        // A muted author is hidden even when policy would otherwise allow.
        policy.ai_send = "on".to_string();
        policy.muted.insert("spammer".to_string());
        assert!(matches!(
            Guardian::screen_message(&policy, &quarantine, "human", "spammer", "hi", "bafyOk"),
            Verdict::Mute(_)
        ));
    }

    #[test]
    fn screen_file_blocks_disallowed_extensions() {
        let policy = ExtensionPolicy::default_shared();
        assert!(matches!(
            Guardian::screen_file(&policy, "malware.exe"),
            Verdict::Block(_)
        ));
        assert_eq!(Guardian::screen_file(&policy, "design.fig"), Verdict::Allow);
    }

    #[test]
    fn yara_scanner_detects_eicar_from_baked_baseline() {
        let rules = String::from_utf8(crate::update::baseline::BAKED_RULES.to_vec()).unwrap();
        let scanner = YaraScanner::from_rules_text(&rules).unwrap();
        let report = scanner
            .scan_bytes(b"prefix X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H* suffix")
            .unwrap();
        assert!(report
            .matches
            .iter()
            .any(|m| m.rule == "UCP_EICAR_Test_File"));
    }

    #[test]
    fn memcli_quarantine_persists_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let cid = Cid("bafyMalicious".to_string());
        assert!(!mem.is_quarantined(&cid).unwrap());
        mem.quarantine_cid(&cid, "yara: trojan").unwrap();
        assert!(mem.is_quarantined(&cid).unwrap(), "persists across loads");
        assert_eq!(mem.quarantine_registry().unwrap().len(), 1);
        assert!(mem.release_cid(&cid).unwrap(), "reversible");
        assert!(!mem.is_quarantined(&cid).unwrap());
    }
}
