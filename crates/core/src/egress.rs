//! Data Platter Privacy Lock - the core egress guard (Phase B).
//!
//! Every path that can push store data out of this device builds an
//! [`EgressPlan`] and executes through [`MemCli::execute_approved_egress`].
//! Execution takes a cross-process policy lock, rebuilds the plan, and refuses
//! any changed or lock-intersecting manifest before the egress closure runs.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::binding::{Cid, CoreBinding, MemCli, Record};
use crate::error::{Error, Result};

pub const LOCK_REGISTRY_VERSION: u16 = 1;
pub const SECURITY_EVENT_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressOperation {
    PublicPublish,
    PlaintextCarExport,
    PublicRoomAttach,
    PrivateEncryptedReplicate,
    EncryptAndSharePrivate,
}

impl EgressOperation {
    pub fn is_public(self) -> bool {
        matches!(
            self,
            Self::PublicPublish | Self::PlaintextCarExport | Self::PublicRoomAttach
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PublicPublish => "public publish",
            Self::PlaintextCarExport => "plaintext CAR export",
            Self::PublicRoomAttach => "public room attach",
            Self::PrivateEncryptedReplicate => "private encrypted replicate",
            Self::EncryptAndSharePrivate => "encrypt and share private",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LockScope {
    #[default]
    ReachableSubgraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LockReason {
    #[default]
    UserLocked,
    DefaultPersonal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub root: String,
    #[serde(default)]
    pub scope: LockScope,
    pub created_at: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub reason: LockReason,
}

/// A root the user has explicitly **cleared for egress** (Decision 0026). Under
/// the default-fenced posture, a root may only leave the device once it (or an
/// ancestor whose subgraph contains it) has been cleared. The exception set; the
/// default — nothing cleared — fences everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearanceRecord {
    pub root: String,
    pub created_at: u64,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRegistry {
    pub version: u16,
    #[serde(default)]
    pub locks: Vec<LockRecord>,
    /// Roots explicitly cleared for egress (Decision 0026). Default empty → the
    /// whole store is fenced from egress. Old registries (no field) decode to
    /// empty, which is the safe — fully-fenced — default.
    #[serde(default)]
    pub cleared: Vec<ClearanceRecord>,
}

impl Default for LockRegistry {
    fn default() -> Self {
        Self {
            version: LOCK_REGISTRY_VERSION,
            locks: Vec::new(),
            cleared: Vec::new(),
        }
    }
}

impl LockRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path
            .try_exists()
            .map_err(|e| security_io("inspect lock registry", e))?
        {
            return Ok(Self::default());
        }
        validate_private_file(path)?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io(format!("read lock registry: {e}")))?;
        if text.trim().is_empty() {
            return Err(Error::SecurityPolicy(
                "lock registry is empty; refusing to treat it as unlocked".to_string(),
            ));
        }
        let registry: Self = serde_json::from_str(&text)
            .map_err(|e| Error::SecurityPolicy(format!("parse lock registry: {e}")))?;
        if registry.version != LOCK_REGISTRY_VERSION {
            return Err(Error::SecurityPolicy(format!(
                "unsupported lock registry version {}",
                registry.version
            )));
        }
        Ok(registry)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        if self.version != LOCK_REGISTRY_VERSION {
            return Err(Error::SecurityPolicy(format!(
                "refusing to write lock registry version {}",
                self.version
            )));
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::Io(format!("serialize lock registry: {e}")))?;
        atomic_private_write(path, &json)
    }

    pub fn contains_root(&self, root: &str) -> bool {
        self.locks.iter().any(|lock| lock.root == root)
    }

    pub(crate) fn upsert(&mut self, record: LockRecord) {
        self.locks.retain(|lock| lock.root != record.root);
        self.locks.push(record);
    }

    /// Drop the lock for `root`. Returns whether a record was actually removed.
    pub(crate) fn remove(&mut self, root: &str) -> bool {
        let before = self.locks.len();
        self.locks.retain(|lock| lock.root != root);
        self.locks.len() != before
    }

    /// Whether `root` has been explicitly cleared for egress (Decision 0026).
    pub fn is_cleared(&self, root: &str) -> bool {
        self.cleared.iter().any(|c| c.root == root)
    }

    pub(crate) fn upsert_clearance(&mut self, record: ClearanceRecord) {
        self.cleared.retain(|c| c.root != record.root);
        self.cleared.push(record);
    }

    /// Re-fence `root` (remove its egress clearance). Returns whether one was removed.
    pub(crate) fn remove_clearance(&mut self, root: &str) -> bool {
        let before = self.cleared.len();
        self.cleared.retain(|c| c.root != root);
        self.cleared.len() != before
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHit {
    pub lock_root: String,
    pub lock_label: String,
    pub first_intersecting_cid: String,
    pub intersecting_count: usize,
    pub intersecting_cids: Vec<String>,
    #[serde(default)]
    pub intersecting_file_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPlan {
    pub root: Cid,
    pub resolved_name: Option<String>,
    pub operation: EgressOperation,
    pub backend: String,
    pub backend_target: String,
    pub network_posture: String,
    pub manifest: Vec<Cid>,
    pub manifest_digest: String,
    pub lock_registry_digest: String,
    pub block_count: usize,
    pub byte_size: u64,
    pub decoded_node_kinds: Vec<String>,
    pub file_paths: Vec<String>,
    pub media_types: Vec<String>,
    pub sensitivity_warnings: Vec<String>,
    pub known_public_receipts: usize,
    pub blocking_locks: Vec<LockHit>,
}

/// A non-public record that an approved egress operation completed. Unlike a
/// [`crate::PublishReceipt`], this never creates known-public exposure evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressReceipt {
    pub operation: EgressOperation,
    pub root: String,
    pub manifest_digest: String,
    pub backend: String,
    pub backend_target: String,
    pub network_posture: String,
    pub block_count: usize,
    pub byte_size: u64,
    pub created_at: u64,
}

impl EgressPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blocking_locks.is_empty()
    }

    pub fn blocker_summary(&self) -> String {
        if self.blocking_locks.is_empty() {
            return "no locks intersect this manifest".to_string();
        }
        let nodes = self
            .blocking_locks
            .iter()
            .flat_map(|hit| hit.intersecting_cids.iter())
            .collect::<BTreeSet<_>>()
            .len();
        let files = self
            .blocking_locks
            .iter()
            .flat_map(|hit| hit.intersecting_file_paths.iter())
            .collect::<BTreeSet<_>>()
            .len();
        format!(
            "{} locked subgraph{} reaching {} node{} and {} file path{} in this manifest",
            self.blocking_locks.len(),
            if self.blocking_locks.len() == 1 {
                ""
            } else {
                "s"
            },
            nodes,
            if nodes == 1 { "" } else { "s" },
            files,
            if files == 1 { "" } else { "s" },
        )
    }

    pub fn blocking_sensitive_findings(&self) -> Vec<&str> {
        self.sensitivity_warnings
            .iter()
            .filter_map(|warning| {
                let warning = warning.as_str();
                (warning.starts_with("sensitive-looking path:")
                    || warning == "sensitive-looking content metadata")
                    .then_some(warning)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub version: u16,
    pub created_at: u64,
    pub action: String,
    pub root: String,
    pub detail: String,
}

pub fn manifest_digest(cids: &[Cid]) -> String {
    let mut sorted = cids.iter().map(|cid| cid.0.as_str()).collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hasher = Sha256::new();
    for cid in sorted {
        hasher.update(cid.as_bytes());
        hasher.update(b"\n");
    }
    hex(&hasher.finalize())
}

pub(crate) fn registry_digest(registry: &LockRegistry) -> Result<String> {
    let bytes = serde_json::to_vec(registry)
        .map_err(|e| Error::Io(format!("serialize lock registry digest: {e}")))?;
    Ok(hex(&Sha256::digest(bytes)))
}

impl MemCli {
    /// The store root directory (`<workdir>/.concierge`).
    pub fn store_dir(&self) -> Result<PathBuf> {
        Ok(self.working_dir().join(self.config()?.store.root))
    }

    pub fn security_dir(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("security"))
    }

    /// Ensure the security vault root exists with **owner-only (0700)** permissions,
    /// and return it. Phase N modules call this before writing any security material
    /// so the directory is private from creation — matching the egress-lock
    /// convention that other paths (e.g. the checkpoint auto-lock) validate.
    pub(crate) fn ensure_security_dir(&self) -> Result<PathBuf> {
        let dir = self.security_dir()?;
        ensure_private_dir(&dir)?;
        Ok(dir)
    }

    fn locks_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("locks.json"))
    }

    fn security_events_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("security-events.jsonl"))
    }

    pub(crate) fn policy_lock(&self) -> Result<PolicyLock> {
        let dir = self.security_dir()?;
        ensure_private_dir(&dir)?;
        PolicyLock::acquire(&dir.join("guard.lock"))
    }

    pub fn lock_registry(&self) -> Result<LockRegistry> {
        let dir = self.security_dir()?;
        if dir
            .try_exists()
            .map_err(|e| security_io("inspect security dir", e))?
        {
            validate_private_dir(&dir)?;
        }
        LockRegistry::load(&self.locks_path()?)
    }

    pub fn locks(&self) -> Result<Vec<LockRecord>> {
        Ok(self.lock_registry()?.locks)
    }

    pub fn security_events(&self) -> Result<Vec<SecurityEvent>> {
        let path = self.security_events_path()?;
        if !path
            .try_exists()
            .map_err(|e| security_io("inspect security events", e))?
        {
            return Ok(Vec::new());
        }
        validate_private_file(&path)?;
        std::fs::read_to_string(&path)
            .map_err(|e| Error::Io(format!("read security events: {e}")))?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let event: SecurityEvent = serde_json::from_str(line)
                    .map_err(|e| Error::SecurityPolicy(format!("parse security event: {e}")))?;
                if event.version != SECURITY_EVENT_VERSION {
                    return Err(Error::SecurityPolicy(format!(
                        "unsupported security event version {}",
                        event.version
                    )));
                }
                Ok(event)
            })
            .collect()
    }

    pub fn lock_subgraph(&self, root: &Cid, label: &str) -> Result<()> {
        self.lock_subgraph_with_reason(root, label, LockReason::UserLocked)
    }

    pub(crate) fn lock_default_checkpoint(&self, root: &Cid, label: &str) -> Result<()> {
        self.lock_subgraph_with_reason(root, label, LockReason::DefaultPersonal)
    }

    fn lock_subgraph_with_reason(&self, root: &Cid, label: &str, reason: LockReason) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.lock_subgraph_unlocked(root, label, reason)
    }

    pub(crate) fn lock_subgraph_unlocked(
        &self,
        root: &Cid,
        label: &str,
        reason: LockReason,
    ) -> Result<()> {
        let _ = self.walk(root)?;
        let mut registry = self.lock_registry()?;
        registry.upsert(LockRecord {
            root: root.0.clone(),
            scope: LockScope::ReachableSubgraph,
            created_at: now_secs(),
            label: label.to_string(),
            reason,
        });
        registry.save(&self.locks_path()?)?;
        self.append_security_event_unlocked(
            match reason {
                LockReason::UserLocked => "lock_created",
                LockReason::DefaultPersonal => "default_checkpoint_locked",
            },
            root,
            label,
        )
    }

    /// Permanently remove a user lock after verifying the store password, lifting
    /// the local-only posture so the subgraph can be reviewed for egress again.
    /// Unlike a view grant this is durable: it deletes the lock record outright.
    pub fn unlock_subgraph(&self, root: &Cid, password: &str) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let mut registry = self.lock_registry()?;
        if !registry.remove(&root.0) {
            return Err(Error::SecurityPolicy(format!(
                "no lock to remove for {}",
                root.0
            )));
        }
        registry.save(&self.locks_path()?)?;
        self.append_security_event_unlocked("lock_removed", root, "lock permanently removed")
    }

    /// The roots explicitly cleared for egress (Decision 0026).
    pub fn cleared_roots(&self) -> Result<Vec<ClearanceRecord>> {
        Ok(self.lock_registry()?.cleared)
    }

    /// Clear `root` for egress after verifying the store password (Decision 0026).
    /// The store is fenced by default; this is the deliberate, password-gated act
    /// that lets a subgraph leave the device. Re-fencing ([`Self::refence`]) needs
    /// no password — returning to private is always free.
    pub fn clear_for_egress(&self, root: &Cid, label: &str, password: &str) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let _ = self.walk(root)?;
        let mut registry = self.lock_registry()?;
        registry.upsert_clearance(ClearanceRecord {
            root: root.0.clone(),
            created_at: now_secs(),
            label: label.to_string(),
        });
        registry.save(&self.locks_path()?)?;
        self.append_security_event_unlocked("egress_cleared", root, label)
    }

    /// Re-fence `root` (remove its egress clearance) — return it to private. No
    /// password: the safe direction is always free.
    pub fn refence(&self, root: &Cid) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        let mut registry = self.lock_registry()?;
        if !registry.remove_clearance(&root.0) {
            return Err(Error::SecurityPolicy(format!(
                "no egress clearance to remove for {}",
                root.0
            )));
        }
        registry.save(&self.locks_path()?)?;
        self.append_security_event_unlocked("egress_refenced", root, "re-fenced (private)")
    }

    pub fn build_egress_plan(&self, root: &Cid, operation: EgressOperation) -> Result<EgressPlan> {
        let (backend, backend_target, posture) = self.default_egress_target(operation)?;
        self.build_egress_plan_internal(root, None, operation, &backend, &backend_target, &posture)
    }

    pub fn build_egress_plan_for_target(
        &self,
        target: &str,
        operation: EgressOperation,
    ) -> Result<EgressPlan> {
        let (root, resolved_name) = if target.parse::<cid::Cid>().is_ok() {
            (Cid(target.to_string()), None)
        } else {
            (self.resolve(target)?, Some(target.to_string()))
        };
        let (backend, backend_target, posture) = self.default_egress_target(operation)?;
        self.build_egress_plan_internal(
            &root,
            resolved_name,
            operation,
            &backend,
            &backend_target,
            &posture,
        )
    }

    pub fn build_egress_plan_for_backend(
        &self,
        root: &Cid,
        operation: EgressOperation,
        backend: &str,
        network_posture: &str,
    ) -> Result<EgressPlan> {
        self.build_egress_plan_internal(root, None, operation, backend, backend, network_posture)
    }

    pub fn build_egress_plan_for_target_and_backend(
        &self,
        target: &str,
        operation: EgressOperation,
        backend: &str,
        backend_target: &str,
        network_posture: &str,
    ) -> Result<EgressPlan> {
        let (root, resolved_name) = if target.parse::<cid::Cid>().is_ok() {
            (Cid(target.to_string()), None)
        } else {
            (self.resolve(target)?, Some(target.to_string()))
        };
        self.build_egress_plan_internal(
            &root,
            resolved_name,
            operation,
            backend,
            backend_target,
            network_posture,
        )
    }

    fn default_egress_target(
        &self,
        operation: EgressOperation,
    ) -> Result<(String, String, String)> {
        Ok(match operation {
            EgressOperation::PublicPublish => {
                let publishing = self.config()?.publishing;
                (
                    publishing.backend,
                    publishing.ipfs_api,
                    "public-networked".to_string(),
                )
            }
            EgressOperation::PlaintextCarExport => (
                "local-file".to_string(),
                "caller-selected-local-file".to_string(),
                "plaintext-portable".to_string(),
            ),
            EgressOperation::PublicRoomAttach => (
                "gossipsub-room".to_string(),
                "caller-selected-public-room".to_string(),
                "public-room".to_string(),
            ),
            EgressOperation::PrivateEncryptedReplicate => (
                "private-swarm".to_string(),
                "caller-selected-private-namespace".to_string(),
                "private-swarm-no-public-dht".to_string(),
            ),
            EgressOperation::EncryptAndSharePrivate => (
                "local-conversion".to_string(),
                "reviewed-private-destination".to_string(),
                "no-plaintext-egress".to_string(),
            ),
        })
    }

    fn build_egress_plan_internal(
        &self,
        root: &Cid,
        resolved_name: Option<String>,
        operation: EgressOperation,
        backend: &str,
        backend_target: &str,
        network_posture: &str,
    ) -> Result<EgressPlan> {
        let (manifest, byte_size) = self.export_car_manifest(root)?;
        let registry = self.lock_registry()?;
        let manifest_set = manifest
            .iter()
            .map(|cid| cid.0.as_str())
            .collect::<BTreeSet<_>>();
        let mut blocking_locks = Vec::new();
        for lock in &registry.locks {
            let locked_reachable = self
                .walk_batched(&Cid(lock.root.clone()))
                .map_err(|error| {
                    Error::SecurityPolicy(format!("cannot verify locked root {}: {error}", lock.root))
                })?;
            let locked_set = locked_reachable
                .iter()
                .map(|cid| cid.0.as_str())
                .collect::<BTreeSet<_>>();
            let intersect = manifest
                .iter()
                .map(|cid| cid.0.as_str())
                .filter(|cid| locked_set.contains(cid) && manifest_set.contains(cid))
                .collect::<Vec<_>>();
            if let Some(first) = intersect.first() {
                let mut intersecting_file_paths = BTreeSet::new();
                let intersect_cids: Vec<Cid> =
                    intersect.iter().map(|cid| Cid((*cid).to_string())).collect();
                let intersect_fetched = self.get_many(&intersect_cids)?;
                for cid in &intersect {
                    if let Some(Record::Live { body_json, .. }) = intersect_fetched.get(*cid) {
                        let mut media_types = BTreeSet::new();
                        let mut warnings = BTreeSet::new();
                        inspect_metadata(
                            body_json,
                            &mut intersecting_file_paths,
                            &mut media_types,
                            &mut warnings,
                        );
                    }
                }
                blocking_locks.push(LockHit {
                    lock_root: lock.root.clone(),
                    lock_label: lock.label.clone(),
                    first_intersecting_cid: (*first).to_string(),
                    intersecting_count: intersect.len(),
                    intersecting_cids: intersect.into_iter().map(str::to_string).collect(),
                    intersecting_file_paths: intersecting_file_paths.into_iter().collect(),
                });
            }
        }

        let mut decoded_node_kinds = BTreeSet::new();
        let mut file_paths = BTreeSet::new();
        let mut media_types = BTreeSet::new();
        let mut sensitivity_warnings = BTreeSet::new();
        // Fetch the whole manifest in one `mem get-many` rather than a `mem`
        // process per node — this scan walks every reachable record and was the
        // dominant cost of the privacy panel on large subgraphs.
        let fetched = self.get_many(&manifest)?;
        for cid in &manifest {
            if let Some(Record::Live { kind, body_json, .. }) = fetched.get(&cid.0) {
                decoded_node_kinds.insert(kind.clone());
                inspect_metadata(
                    body_json,
                    &mut file_paths,
                    &mut media_types,
                    &mut sensitivity_warnings,
                );
            }
        }
        let known_public_receipts = self
            .publish_receipts()?
            .into_iter()
            .filter(|receipt| receipt.root == root.0)
            .count();

        // Default-fenced (Decision 0026): the store is private by default. Unless
        // this exact root has been explicitly cleared for egress — or is already
        // known-public (publication is irreversible) — the whole manifest is fenced
        // from leaving the device. Clearing is a deliberate, password-gated act.
        if !registry.is_cleared(&root.0) && known_public_receipts == 0 {
            blocking_locks.push(LockHit {
                lock_root: root.0.clone(),
                lock_label: "private by default".to_string(),
                first_intersecting_cid: manifest
                    .first()
                    .map(|cid| cid.0.clone())
                    .unwrap_or_default(),
                intersecting_count: manifest.len(),
                intersecting_cids: manifest.iter().map(|cid| cid.0.clone()).collect(),
                intersecting_file_paths: file_paths.iter().cloned().collect(),
            });
        }

        // Quarantined content (Guardian §3) is withheld from egress too, not just
        // local surfacing — a flagged CID cannot be published or exported, even if
        // the root was cleared for egress (safety overrides clearance). Reversible:
        // `release` the quarantine to allow it.
        let quarantine = self.quarantine_registry().unwrap_or_default();
        let quarantined: Vec<String> = manifest
            .iter()
            .map(|cid| cid.0.clone())
            .filter(|cid| quarantine.is_quarantined(cid))
            .collect();
        if let Some(first) = quarantined.first() {
            blocking_locks.push(LockHit {
                lock_root: root.0.clone(),
                lock_label: "quarantined (unsafe — release to allow)".to_string(),
                first_intersecting_cid: first.clone(),
                intersecting_count: quarantined.len(),
                intersecting_cids: quarantined,
                intersecting_file_paths: Vec::new(),
            });
        }

        Ok(EgressPlan {
            root: root.clone(),
            resolved_name,
            operation,
            backend: backend.to_string(),
            backend_target: backend_target.to_string(),
            network_posture: network_posture.to_string(),
            manifest_digest: manifest_digest(&manifest),
            lock_registry_digest: registry_digest(&registry)?,
            block_count: manifest.len(),
            manifest,
            byte_size,
            decoded_node_kinds: decoded_node_kinds.into_iter().collect(),
            file_paths: file_paths.into_iter().collect(),
            media_types: media_types.into_iter().collect(),
            sensitivity_warnings: sensitivity_warnings.into_iter().collect(),
            known_public_receipts,
            blocking_locks,
        })
    }

    pub fn check_egress(&self, plan: &EgressPlan) -> Result<()> {
        if plan.operation == EgressOperation::PrivateEncryptedReplicate {
            return self.check_private_replication_posture(plan);
        }
        if plan.operation == EgressOperation::EncryptAndSharePrivate {
            return Err(Error::SecurityPolicy(
                "encrypt-and-share-private requires an exact password-authorized conversion grant"
                    .to_string(),
            ));
        }
        let sensitive = plan.blocking_sensitive_findings();
        if plan.operation.is_public() && !sensitive.is_empty() {
            return Err(Error::SensitiveContentBlocked {
                operation: plan.operation.label(),
                findings: sensitive.into_iter().map(str::to_string).collect(),
            });
        }
        if !plan.is_blocked() {
            return Ok(());
        }
        Err(Error::PublicationBlocked {
            operation: plan.operation.label(),
            summary: plan.blocker_summary(),
            blockers: plan.blocking_locks.clone(),
        })
    }

    pub(crate) fn check_private_replication_posture(&self, plan: &EgressPlan) -> Result<()> {
        if plan.operation != EgressOperation::PrivateEncryptedReplicate
            || plan.backend != "private-swarm"
            || plan.network_posture != "private-swarm-no-public-dht"
            || plan.backend_target.is_empty()
        {
            return Err(Error::SecurityPolicy(
                "private encrypted replication requires an explicit private-swarm destination and no-public-DHT posture"
                    .to_string(),
            ));
        }
        if plan.is_blocked() {
            return Err(Error::SecurityPolicy(
                "private encrypted replication manifest unexpectedly intersects local plaintext locks"
                    .to_string(),
            ));
        }
        let registered = self.private_conversions()?.into_iter().any(|conversion| {
            conversion.ciphertext_root == plan.root.0
                && conversion.destination_namespace == plan.backend_target
                && conversion.ciphertext_manifest
                    == plan
                        .manifest
                        .iter()
                        .map(|cid| cid.0.clone())
                        .collect::<Vec<_>>()
        });
        if !registered {
            return Err(Error::SecurityPolicy(
                "private encrypted replication requires an exact registered ciphertext manifest"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub fn execute_public_room_attach<T>(
        &self,
        reviewed: &EgressPlan,
        action: impl FnOnce(&EgressPlan) -> Result<T>,
    ) -> Result<T> {
        if reviewed.operation != EgressOperation::PublicRoomAttach
            || reviewed.network_posture != "public-room"
        {
            return Err(Error::EgressPlanChanged(
                "reviewed plan is not a public-room attachment".to_string(),
            ));
        }
        self.execute_approved_egress(reviewed, action)
    }

    pub(crate) fn execute_approved_egress<T>(
        &self,
        reviewed: &EgressPlan,
        action: impl FnOnce(&EgressPlan) -> Result<T>,
    ) -> Result<T> {
        let _policy_lock = self.policy_lock()?;
        let root = match &reviewed.resolved_name {
            Some(name) => {
                let current = self.resolve(name)?;
                if current != reviewed.root {
                    return Err(Error::EgressPlanChanged(format!(
                        "name `{name}` no longer resolves to reviewed root {}",
                        reviewed.root.0
                    )));
                }
                current
            }
            None => reviewed.root.clone(),
        };
        let (backend, backend_target, network_posture) =
            if reviewed.operation == EgressOperation::PublicPublish {
                self.default_egress_target(reviewed.operation)?
            } else {
                (
                    reviewed.backend.clone(),
                    reviewed.backend_target.clone(),
                    reviewed.network_posture.clone(),
                )
            };
        let current = self.build_egress_plan_internal(
            &root,
            reviewed.resolved_name.clone(),
            reviewed.operation,
            &backend,
            &backend_target,
            &network_posture,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "root, manifest, lock registry, backend, posture, or review metadata changed"
                    .to_string(),
            ));
        }
        // Phase C: a clear plan passes; a lock-blocked plan proceeds only by
        // atomically consuming a matching one-shot grant inside this policy lock.
        self.authorize_egress(&current)?;
        self.append_security_event_unlocked(
            "egress_approved",
            &current.root,
            &format!(
                "{} via {} ({})",
                current.operation.label(),
                current.backend,
                current.backend_target
            ),
        )?;
        action(&current)
    }

    pub(crate) fn append_security_event_unlocked(
        &self,
        action: &str,
        root: &Cid,
        detail: &str,
    ) -> Result<()> {
        let path = self.security_events_path()?;
        let mut text = if path
            .try_exists()
            .map_err(|e| security_io("inspect security events", e))?
        {
            validate_private_file(&path)?;
            std::fs::read_to_string(&path)
                .map_err(|e| Error::Io(format!("read security events: {e}")))?
        } else {
            String::new()
        };
        let event = SecurityEvent {
            version: SECURITY_EVENT_VERSION,
            created_at: now_secs(),
            action: action.to_string(),
            root: root.0.clone(),
            detail: detail.to_string(),
        };
        text.push_str(
            &serde_json::to_string(&event)
                .map_err(|e| Error::Io(format!("serialize security event: {e}")))?,
        );
        text.push('\n');
        atomic_private_write(&path, text.as_bytes())
    }
}

fn inspect_metadata(
    body_json: &str,
    file_paths: &mut BTreeSet<String>,
    media_types: &mut BTreeSet<String>,
    warnings: &mut BTreeSet<String>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body_json) else {
        warnings.insert("record metadata could not be decoded".to_string());
        return;
    };
    let body = value.get("body").unwrap_or(&value);
    if let Some(path) = body.get("path").and_then(|value| value.as_str()) {
        file_paths.insert(path.to_string());
        let lower = path.to_ascii_lowercase();
        if lower.contains(".env")
            || lower.contains("credential")
            || lower.contains("private")
            || lower.contains("wallet")
            || lower.contains("secret")
            || lower.contains("token")
            || lower.contains(".ssh/")
            || lower.contains("id_rsa")
            || lower.contains("id_ed25519")
            || lower.contains(".concierge/security")
            || lower.ends_with(".key")
            || lower.ends_with(".pem")
            || lower.ends_with(".p12")
            || lower.ends_with(".pfx")
            || lower.ends_with(".jks")
        {
            warnings.insert(format!("sensitive-looking path: {path}"));
        }
    }
    if let Some(media_type) = body.get("media_type").and_then(|value| value.as_str()) {
        media_types.insert(media_type.to_string());
    }
    let lower = body_json.to_ascii_lowercase();
    if lower.contains("-----begin private key-----")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("client_secret")
        || lower.contains("aws_secret_access_key")
        || lower.contains("authorization: bearer ")
    {
        warnings.insert("sensitive-looking content metadata".to_string());
    }
}

pub(crate) struct PolicyLock {
    file: File,
}

impl PolicyLock {
    fn acquire(path: &Path) -> Result<Self> {
        reject_symlink(path)?;
        let existed = path
            .try_exists()
            .map_err(|error| security_io("inspect policy lock", error))?;
        if existed {
            validate_private_file(path)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| Error::Io(format!("open policy lock: {e}")))?;
        if !existed {
            set_private_file_perms(path)?;
        }
        validate_private_file(path)?;
        lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for PolicyLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

pub(crate) fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::SecurityPolicy("security file has no parent".to_string()))?;
    ensure_private_dir(parent)?;
    reject_symlink(path)?;
    if path
        .try_exists()
        .map_err(|error| security_io("inspect security file", error))?
    {
        validate_private_file(path)?;
    }
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("security"),
        std::process::id(),
        now_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| Error::Io(format!("create atomic security file: {e}")))?;
        set_private_file_perms(&tmp)?;
        file.write_all(bytes)
            .map_err(|e| Error::Io(format!("write security file: {e}")))?;
        file.sync_all()
            .map_err(|e| Error::Io(format!("sync security file: {e}")))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::Io(format!("commit security file: {e}")))?;
        validate_private_file(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    let existed = path
        .try_exists()
        .map_err(|error| security_io("inspect security dir", error))?;
    if existed {
        return validate_private_dir(path);
    }
    std::fs::create_dir_all(path).map_err(|e| Error::Io(format!("create security dir: {e}")))?;
    set_private_dir_perms(path)?;
    validate_private_dir(path)
}

fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::SecurityPolicy(format!(
            "security path must not be a symlink: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(security_io("inspect security path", error)),
    }
}

#[cfg(unix)]
pub(crate) fn validate_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    reject_symlink(path)?;
    let metadata = std::fs::metadata(path).map_err(|e| security_io("read security metadata", e))?;
    if !metadata.is_file() {
        return Err(Error::SecurityPolicy(format!(
            "security path is not a file: {}",
            path.display()
        )));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(Error::SecurityPolicy(format!(
            "security file has unexpected owner: {}",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::SecurityPolicy(format!(
            "security file permissions are not owner-only: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_private_file(path: &Path) -> Result<()> {
    reject_symlink(path)
}

#[cfg(unix)]
pub(crate) fn validate_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    reject_symlink(path)?;
    let metadata = std::fs::metadata(path).map_err(|e| security_io("read security metadata", e))?;
    if !metadata.is_dir() {
        return Err(Error::SecurityPolicy(format!(
            "security path is not a directory: {}",
            path.display()
        )));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(Error::SecurityPolicy(format!(
            "security directory has unexpected owner: {}",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::SecurityPolicy(format!(
            "security directory permissions are not owner-only: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_private_dir(path: &Path) -> Result<()> {
    reject_symlink(path)
}

#[cfg(unix)]
fn set_private_file_perms(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::Io(format!("set security file permissions: {e}")))
}

#[cfg(not(unix))]
fn set_private_file_perms(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_perms(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| Error::Io(format!("set security directory permissions: {e}")))
}

#[cfg(not(unix))]
fn set_private_dir_perms(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(())
    } else {
        Err(Error::Io(format!(
            "lock publication policy: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn unlock(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(Error::Io(format!(
            "unlock publication policy: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> Result<()> {
    Ok(())
}

pub(crate) fn security_io(action: &str, error: std::io::Error) -> Error {
    Error::SecurityPolicy(format!("{action}: {error}"))
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::Node;

    fn temp_workdir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "concierge-egress-{tag}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn memory_node(text: &str) -> Node {
        Node {
            kind: "memory".to_string(),
            fields_json: format!(r#"{{"text":"{text}","kind":"project"}}"#),
        }
    }

    #[test]
    fn direct_parent_descendant_and_unrelated_lock_semantics() {
        let dir = temp_workdir("lock-semantics");
        let mem = MemCli::new(&dir);
        let child = mem.put_node(&memory_node("secret")).unwrap();
        let parent = mem
            .put_node(&Node {
                kind: "plan".to_string(),
                fields_json: serde_json::json!({
                    "title": "parent",
                    "prose": "links to child",
                    "spec": crate::binding::cid_link(&child).unwrap(),
                })
                .to_string(),
            })
            .unwrap();
        let other = mem.put_node(&memory_node("public")).unwrap();

        // Everything is fenced by default (Decision 0026); clear these roots so we
        // can isolate the explicit-lock *intersection* semantics below.
        mem.set_password("pw").unwrap();
        mem.clear_for_egress(&child, "child", "pw").unwrap();
        mem.clear_for_egress(&parent, "parent", "pw").unwrap();
        mem.clear_for_egress(&other, "other", "pw").unwrap();

        let child_plan = mem
            .build_egress_plan(&child, EgressOperation::PublicPublish)
            .unwrap();
        assert!(!child_plan.is_blocked());
        mem.lock_subgraph(&child, "secret").unwrap();
        assert!(mem
            .build_egress_plan(&child, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        assert!(mem
            .build_egress_plan(&parent, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        assert!(!mem
            .build_egress_plan(&other, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn everything_is_fenced_from_egress_by_default_until_cleared() {
        let dir = temp_workdir("default-fenced");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let root = mem.put_node(&memory_node("x")).unwrap();

        // Default (Decision 0026): the store is private — a fresh root cannot egress.
        assert!(mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());

        // Clearing it (password-gated) lifts the fence for that root only.
        mem.clear_for_egress(&root, "x", "pw").unwrap();
        assert!(!mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());

        // A wrong password cannot clear; the root stays fenced.
        let other = mem.put_node(&memory_node("y")).unwrap();
        assert!(mem.clear_for_egress(&other, "y", "WRONG").is_err());
        assert!(mem
            .build_egress_plan(&other, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());

        // Re-fencing returns it to private (no password — the safe direction is free).
        mem.refence(&root).unwrap();
        assert!(mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quarantined_content_is_blocked_from_egress_even_when_cleared() {
        let dir = temp_workdir("quarantine-egress");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let root = mem.put_node(&memory_node("flagged content")).unwrap();
        // Clear it for egress → normally publishable.
        mem.clear_for_egress(&root, "x", "pw").unwrap();
        assert!(!mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        // Quarantine the CID → blocked from egress even though it was cleared
        // (safety overrides clearance).
        mem.quarantine_cid(&root, "yara: test").unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap();
        assert!(plan.is_blocked(), "quarantined content cannot egress");
        assert!(
            plan.blocking_locks.iter().any(|l| l.lock_label.contains("quarantined")),
            "blocked with a quarantine reason"
        );
        // Release → publishable again (reversible).
        mem.release_cid(&root).unwrap();
        assert!(!mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
            .is_blocked());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn registry_is_versioned_private_and_events_are_recorded() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_workdir("registry");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("x")).unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        let registry = mem.lock_registry().unwrap();
        assert_eq!(registry.version, LOCK_REGISTRY_VERSION);
        assert!(registry.contains_root(&root.0));
        let mode = std::fs::metadata(mem.security_dir().unwrap().join("locks.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        assert_eq!(mem.security_events().unwrap()[0].action, "lock_created");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn permanent_unlock_removes_the_lock_after_the_password() {
        let dir = temp_workdir("permanent-unlock");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("x")).unwrap();
        mem.set_password("pw").unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        assert!(mem.lock_registry().unwrap().contains_root(&root.0));

        // Wrong password leaves the lock intact.
        assert!(matches!(
            mem.unlock_subgraph(&root, "WRONG"),
            Err(Error::AuthenticationFailed)
        ));
        assert!(mem.lock_registry().unwrap().contains_root(&root.0));

        // Correct password durably removes the lock and records the event.
        mem.unlock_subgraph(&root, "pw").unwrap();
        assert!(!mem.lock_registry().unwrap().contains_root(&root.0));
        assert!(mem
            .security_events()
            .unwrap()
            .iter()
            .any(|event| event.action == "lock_removed"));

        // Removing a lock that no longer exists is an error, not a silent no-op.
        assert!(matches!(
            mem.unlock_subgraph(&root, "pw"),
            Err(Error::SecurityPolicy(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_registry_permissions_fail_closed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_workdir("bad-perms");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("x")).unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        let path = mem.security_dir().unwrap().join("locks.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            mem.build_egress_plan(&root, EgressOperation::PublicPublish),
            Err(Error::SecurityPolicy(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_security_directory_permissions_fail_closed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_workdir("bad-dir-perms");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("x")).unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        std::fs::set_permissions(
            mem.security_dir().unwrap(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(matches!(
            mem.lock_subgraph(&root, "still private"),
            Err(Error::SecurityPolicy(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reviewed_plan_is_invalidated_by_a_new_lock() {
        let dir = temp_workdir("changed-plan");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("x")).unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PlaintextCarExport)
            .unwrap();
        mem.lock_subgraph(&root, "new lock").unwrap();
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::EgressPlanChanged(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reviewed_name_plan_is_invalidated_by_rebinding() {
        let dir = temp_workdir("rebind");
        let mem = MemCli::new(&dir);
        let first = mem.put_node(&memory_node("first")).unwrap();
        let second = mem.put_node(&memory_node("second")).unwrap();
        mem.bind("latest", &first).unwrap();
        let plan = mem
            .build_egress_plan_for_target("latest", EgressOperation::PublicPublish)
            .unwrap();
        mem.bind("latest", &second).unwrap();
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::EgressPlanChanged(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn public_room_attach_uses_the_same_lock_guard() {
        let dir = temp_workdir("room-guard");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("message")).unwrap();
        mem.lock_subgraph(&root, "private message").unwrap();
        let plan = mem
            .build_egress_plan_for_backend(
                &root,
                EgressOperation::PublicRoomAttach,
                "gossipsub-room:test",
                "public-room",
            )
            .unwrap();
        let mut called = false;
        assert!(matches!(
            mem.execute_public_room_attach(&plan, |_| {
                called = true;
                Ok(())
            }),
            Err(Error::PublicationBlocked { .. })
        ));
        assert!(!called);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn private_replication_rejects_plaintext_disguised_as_ciphertext() {
        let dir = temp_workdir("reserved-private");
        let mem = MemCli::new(&dir);
        let root = mem.put_node(&memory_node("plaintext")).unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PrivateEncryptedReplicate)
            .unwrap();
        let mut called = false;
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| {
                called = true;
                Ok(())
            }),
            Err(Error::SecurityPolicy(_))
        ));
        assert!(!called);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_locked_root_fails_closed() {
        let dir = temp_workdir("missing-locked-root");
        let mem = MemCli::new(&dir);
        let locked = mem.put_node(&memory_node("locked")).unwrap();
        let other = mem.put_node(&memory_node("other")).unwrap();
        mem.lock_subgraph(&locked, "private").unwrap();
        std::fs::remove_file(dir.join(".concierge/blocks").join(&locked.0)).unwrap();
        assert!(matches!(
            mem.build_egress_plan(&other, EgressOperation::PublicPublish),
            Err(Error::SecurityPolicy(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn manifest_digest_is_deterministic_and_order_independent() {
        let a = Cid("bafyA".to_string());
        let b = Cid("bafyB".to_string());
        let c = Cid("bafyC".to_string());
        let one = manifest_digest(&[a.clone(), b.clone(), c.clone()]);
        let two = manifest_digest(&[c, a, b]);
        assert_eq!(one, two);
    }
}
