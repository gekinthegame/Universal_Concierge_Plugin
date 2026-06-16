//! Phase E capability-encrypted private sharing.
//!
//! Plaintext is reviewed and encrypted locally. Ciphertext blocks use valid
//! CIDv1 raw-block addresses and live in the normal content-addressed block
//! store, while capabilities remain sealed in the owner-only capability vault.
//! Private replication can proceed while the vault is locked because it needs
//! only the ciphertext manifest; decrypting, converting, and issuing a
//! capability always require the store password.

use std::collections::HashMap;
use std::path::PathBuf;

use concierge_crypto::{
    build, derive_vault_key, open_node, Capability, OpenNode, Plain, SymmetricKey,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::binding::{Cid, MemCli};
use crate::egress::{
    atomic_private_write, ensure_private_dir, manifest_digest, now_secs, registry_digest,
    validate_private_file, EgressOperation, EgressPlan, EgressReceipt, LockReason,
};
use crate::error::{Error, Result};

const CAPABILITY_VAULT_VERSION: u16 = 1;
const PRIVATE_INDEX_VERSION: u16 = 2;
const PRIVATE_RECEIPT_VERSION: u16 = 1;
const MANIFEST_CHILD: &str = "__manifest__";

/// The exact source review and private destination authorized by the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateSharePlan {
    pub source: EgressPlan,
    pub destination_namespace: String,
    pub recipients: Vec<String>,
}

/// A read-only bearer capability. It is returned only to the approved local
/// share flow and is never persisted in receipts, DAG nodes, or sync records.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateCapability {
    pub ciphertext_root: String,
    pub read_key: [u8; 32],
}

impl std::fmt::Debug for PrivateCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrivateCapability")
            .field("ciphertext_root", &self.ciphertext_root)
            .field("read_key", &"[redacted]")
            .finish()
    }
}

impl Drop for PrivateCapability {
    fn drop(&mut self) {
        self.read_key.zeroize();
    }
}

impl PrivateCapability {
    fn from_capability(capability: &Capability) -> Self {
        Self {
            ciphertext_root: capability.cid.clone(),
            read_key: capability.read_key.to_bytes(),
        }
    }

    fn to_capability(&self) -> Capability {
        Capability::read_only(
            self.ciphertext_root.clone(),
            SymmetricKey::from_bytes(self.read_key),
        )
    }
}

/// What the password-authorized convert-and-share flow produced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedPrivateReceipt {
    pub ciphertext_root: String,
    pub plaintext_root: String,
    pub block_count: usize,
    pub plaintext_locked: bool,
    pub created_at: u64,
    pub destination_namespace: String,
    pub recipients: Vec<String>,
}

/// A private share handoff. The receipt is non-secret; the capability is the
/// bearer secret granted to the reviewed recipients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateShareResult {
    pub receipt: EgressReceipt,
    pub conversion: EncryptedPrivateReceipt,
    pub capability: PrivateCapability,
}

/// The result of [`MemCli::rotate_private_capability`] — a new ciphertext root
/// under fresh keys at the next capability epoch. `new_read_capability` is what
/// remaining members are re-shared; the old key no longer opens this root.
#[derive(Debug, Clone)]
pub struct RotationResult {
    pub old_ciphertext_root: String,
    pub new_ciphertext_root: String,
    pub capability_epoch: u64,
    pub block_count: usize,
    pub new_read_capability: PrivateCapability,
}

/// What `read_encrypted_private` recovered.
#[derive(Debug, Clone)]
pub struct RecoveredPrivate {
    pub plaintext_root: String,
    pub blocks: Vec<(String, Vec<u8>)>,
}

/// Owner-only metadata for a ciphertext graph. No capability key material lives
/// here, so a locked-vault device can safely relay the exact ciphertext manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateConversion {
    pub plaintext_root: String,
    pub ciphertext_root: String,
    #[serde(default)]
    pub ciphertext_manifest: Vec<String>,
    #[serde(default)]
    pub destination_namespace: String,
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Which key generation this ciphertext belongs to. Rotation (Phase N · Phase G)
    /// re-encrypts under fresh keys and advances this — so a revoked holder's old
    /// key cannot decrypt the new ciphertext root.
    #[serde(default)]
    pub capability_epoch: u64,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize)]
struct PrivateIndex {
    version: u16,
    #[serde(default)]
    entries: Vec<PrivateConversion>,
}

#[derive(Serialize, Deserialize)]
struct ManifestDoc {
    root: String,
    cids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u16,
    salt: Vec<u8>,
    /// Sealed `Vec<Capability>`; empty until the first capability is stored.
    sealed: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct PrivateReceiptTrail {
    version: u16,
    #[serde(default)]
    receipts: Vec<EgressReceipt>,
}

impl MemCli {
    fn capability_vault_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("capability-vault.json"))
    }

    fn private_index_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("private-index.json"))
    }

    fn private_receipts_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("private-egress-receipts.json"))
    }

    /// Build the exact source/destination review used by the Data Platter.
    pub fn build_encrypt_and_share_plan(
        &self,
        target: &str,
        destination_namespace: &str,
        recipients: &[String],
    ) -> Result<PrivateSharePlan> {
        validate_private_destination(destination_namespace, recipients)?;
        Ok(PrivateSharePlan {
            source: self
                .build_egress_plan_for_target(target, EgressOperation::EncryptAndSharePrivate)?,
            destination_namespace: destination_namespace.to_string(),
            recipients: normalized_recipients(recipients),
        })
    }

    /// Encrypt a subgraph for **blind off-device pinning** (the Pin → Private path).
    ///
    /// Builds a Cryptree ciphertext of the root's reachable plaintext blocks, persists
    /// the verified ciphertext blocks **and** the owner read capability locally (so the
    /// content stays decryptable here), and returns the ciphertext root plus a CAR of
    /// that ciphertext DAG — ready to hand to a pinning service. Unlike
    /// [`Self::convert_and_share_private`], there is no destination namespace, recipient,
    /// or one-shot grant: the only reader is this owner. The service only ever holds
    /// opaque ciphertext (Decision 0026 / threat-model: encrypted = blind-pinned inert
    /// ciphertext). Password-gated (vault unlock). The original plaintext is untouched.
    pub fn encrypt_subgraph_for_pin(&self, root: &Cid, password: &str) -> Result<(Cid, Vec<u8>)> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;

        // The exact reachable plaintext manifest (root + every descendant block).
        let (manifest, _bytes) = self.export_car_manifest(root)?;
        let manifest_doc = ManifestDoc {
            root: root.0.clone(),
            cids: manifest.iter().map(|cid| cid.0.clone()).collect(),
        };
        let mut children = Vec::with_capacity(manifest.len() + 1);
        children.push((
            MANIFEST_CHILD.to_string(),
            Plain::File(
                serde_json::to_vec(&manifest_doc)
                    .map_err(|error| Error::Encryption(format!("manifest: {error}")))?,
            ),
        ));
        for cid in &manifest {
            children.push((cid.0.clone(), Plain::File(self.read_block(cid)?)));
        }
        let tree =
            build(&Plain::Dir(children)).map_err(|error| Error::Encryption(error.to_string()))?;

        // Write only verified ciphertext blocks, then keep the read capability in the
        // vault so this owner can still decrypt what was blind-pinned off-device.
        for block in &tree.blocks {
            self.store_verified_raw_block(&Cid(block.cid.clone()), &block.bytes)?;
        }
        self.add_capability_to_vault_unlocked(&tree.root, password)?;

        // Assemble the CAR from the exact ciphertext blocks — a Cryptree DAG is not a
        // UCP Record graph, so it can't be reached by the record-link walk `export_car`
        // uses; the manifest is the full block set (mirrors export_reviewed_private_car).
        let mut blocks = Vec::with_capacity(tree.blocks.len());
        for block in &tree.blocks {
            blocks.push((
                block.cid.parse::<cid::Cid>().map_err(|error| {
                    Error::Encryption(format!("invalid ciphertext CID: {error}"))
                })?,
                block.bytes.clone(),
            ));
        }
        let root = tree.root.cid.parse::<cid::Cid>().map_err(|error| {
            Error::Encryption(format!("invalid ciphertext root: {error}"))
        })?;
        let car = crate::car::build_car(&root, &blocks)?;
        Ok((Cid(tree.root.cid.clone()), car))
    }

    /// Convert one exact reviewed plaintext graph, immediately consume its
    /// one-shot grant, and issue a read-only capability to the reviewed
    /// destination. The source plaintext never leaves this process.
    pub fn convert_and_share_private(
        &self,
        reviewed: &PrivateSharePlan,
        password: &str,
    ) -> Result<PrivateShareResult> {
        validate_private_destination(&reviewed.destination_namespace, &reviewed.recipients)?;
        if reviewed.source.operation != EgressOperation::EncryptAndSharePrivate {
            return Err(Error::EgressPlanChanged(
                "reviewed plan is not encrypt-and-share-private".to_string(),
            ));
        }

        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let current = self.build_encrypt_and_share_plan(
            reviewed
                .source
                .resolved_name
                .as_deref()
                .unwrap_or(&reviewed.source.root.0),
            &reviewed.destination_namespace,
            &reviewed.recipients,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "source manifest, lock policy, destination namespace, or recipients changed"
                    .to_string(),
            ));
        }
        if !self.consume_matching_private_share_grant_unlocked(reviewed)? {
            return Err(Error::SecurityPolicy(
                "encrypt-and-share-private requires a live exact one-shot grant".to_string(),
            ));
        }

        let manifest_doc = ManifestDoc {
            root: current.source.root.0.clone(),
            cids: current
                .source
                .manifest
                .iter()
                .map(|cid| cid.0.clone())
                .collect(),
        };
        let mut children = Vec::with_capacity(current.source.manifest.len() + 1);
        children.push((
            MANIFEST_CHILD.to_string(),
            Plain::File(
                serde_json::to_vec(&manifest_doc)
                    .map_err(|error| Error::Encryption(format!("manifest: {error}")))?,
            ),
        ));
        for cid in &current.source.manifest {
            children.push((cid.0.clone(), Plain::File(self.read_block(cid)?)));
        }
        let tree =
            build(&Plain::Dir(children)).map_err(|error| Error::Encryption(error.to_string()))?;

        // Write only verified ciphertext blocks. A later metadata failure can
        // leave harmless content-addressed orphans, never a plaintext egress.
        for block in &tree.blocks {
            self.store_verified_raw_block(&Cid(block.cid.clone()), &block.bytes)?;
        }
        self.add_capability_to_vault_unlocked(&tree.root, password)?;

        if !self
            .locks()?
            .iter()
            .any(|lock| lock.root == current.source.root.0)
        {
            self.lock_subgraph_unlocked(
                &current.source.root,
                "converted to encrypted private",
                LockReason::UserLocked,
            )?;
        }

        let created_at = now_secs();
        let ciphertext_manifest = tree
            .blocks
            .iter()
            .map(|block| block.cid.clone())
            .collect::<Vec<_>>();
        let conversion = PrivateConversion {
            plaintext_root: current.source.root.0.clone(),
            ciphertext_root: tree.root.cid.clone(),
            ciphertext_manifest: ciphertext_manifest.clone(),
            destination_namespace: reviewed.destination_namespace.clone(),
            recipients: reviewed.recipients.clone(),
            capability_epoch: 0,
            created_at,
        };
        self.record_private_conversion_unlocked(conversion.clone())?;

        let ciphertext_plan = self.private_replication_plan_from_conversion(
            &conversion,
            &reviewed.destination_namespace,
        )?;
        self.check_private_replication_posture(&ciphertext_plan)?;
        let receipt = EgressReceipt {
            operation: EgressOperation::EncryptAndSharePrivate,
            root: tree.root.cid.clone(),
            manifest_digest: ciphertext_plan.manifest_digest,
            backend: ciphertext_plan.backend,
            backend_target: ciphertext_plan.backend_target,
            network_posture: ciphertext_plan.network_posture,
            block_count: ciphertext_plan.block_count,
            byte_size: ciphertext_plan.byte_size,
            created_at,
        };
        self.append_private_receipt_unlocked(&receipt)?;
        self.append_security_event_unlocked(
            "converted_and_shared_private",
            &current.source.root,
            &format!(
                "ciphertext root {} to namespace {} for {} recipient(s)",
                tree.root.cid,
                reviewed.destination_namespace,
                reviewed.recipients.len()
            ),
        )?;

        Ok(PrivateShareResult {
            receipt,
            conversion: EncryptedPrivateReceipt {
                ciphertext_root: tree.root.cid.clone(),
                plaintext_root: current.source.root.0,
                block_count: tree.blocks.len(),
                plaintext_locked: true,
                created_at,
                destination_namespace: reviewed.destination_namespace.clone(),
                recipients: reviewed.recipients.clone(),
            },
            capability: PrivateCapability::from_capability(&tree.root.to_read_only()),
        })
    }

    /// **Capability-key rotation (Phase N · Phase G).** Re-encrypt a private graph
    /// under **fresh keys** (the Cryptree key-rotation operation via [`concierge_crypto::rotate_tree`]):
    /// decrypt the subtree with the owner's current key, rebuild under new keys, and
    /// record a new ciphertext root at the **next capability epoch**. A revoked
    /// holder's old read key cannot decrypt the new root, so future re-shared content
    /// is beyond it. **Honest limit (plan §Honest Limits):** this cannot un-send the
    /// *old* ciphertext blocks a removed device already fetched — it protects the
    /// re-rooted graph going forward. Requires the store password (vault unlock).
    pub fn rotate_private_capability(
        &self,
        ciphertext_root: &str,
        password: &str,
    ) -> Result<RotationResult> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let old_conversion = self
            .private_conversions()?
            .into_iter()
            .find(|entry| entry.ciphertext_root == ciphertext_root)
            .ok_or_else(|| Error::Encryption(format!("unknown private root {ciphertext_root}")))?;

        // The owner's current key for this root; then re-encrypt under fresh keys.
        let old_cap = self.find_capability_unlocked(ciphertext_root, password)?;
        let fetch = |cid: &str| self.read_block(&Cid(cid.to_string())).ok();
        let new_tree = concierge_crypto::rotate_tree(&old_cap, &fetch)
            .map_err(|error| Error::Encryption(error.to_string()))?;

        // Persist the new ciphertext blocks (verified, CID-addressed).
        for block in &new_tree.blocks {
            self.store_verified_raw_block(&Cid(block.cid.clone()), &block.bytes)?;
        }
        // Seal the new owner capability and record the new generation.
        self.add_capability_to_vault_unlocked(&new_tree.root, password)?;
        let capability_epoch = old_conversion.capability_epoch.saturating_add(1);
        let new_conversion = PrivateConversion {
            plaintext_root: old_conversion.plaintext_root.clone(),
            ciphertext_root: new_tree.root.cid.clone(),
            ciphertext_manifest: new_tree
                .blocks
                .iter()
                .map(|block| block.cid.clone())
                .collect(),
            destination_namespace: old_conversion.destination_namespace.clone(),
            recipients: old_conversion.recipients.clone(),
            capability_epoch,
            created_at: now_secs(),
        };
        self.record_private_conversion_unlocked(new_conversion)?;
        self.append_security_event_unlocked(
            "rotated_private_capability",
            &Cid(new_tree.root.cid.clone()),
            &format!(
                "rotated {ciphertext_root} -> {} (capability epoch {capability_epoch})",
                new_tree.root.cid
            ),
        )?;

        Ok(RotationResult {
            old_ciphertext_root: ciphertext_root.to_string(),
            new_ciphertext_root: new_tree.root.cid.clone(),
            capability_epoch,
            block_count: new_tree.blocks.len(),
            new_read_capability: PrivateCapability::from_capability(&new_tree.root.to_read_only()),
        })
    }

    /// Build an exact ciphertext-only replication plan. This requires no vault
    /// key and is therefore safe in locked-vault relay-only mode.
    pub fn build_private_replication_plan(
        &self,
        ciphertext_root: &str,
        destination_namespace: &str,
    ) -> Result<EgressPlan> {
        let conversion = self
            .private_conversions()?
            .into_iter()
            .find(|entry| entry.ciphertext_root == ciphertext_root)
            .ok_or_else(|| Error::Encryption(format!("unknown private root {ciphertext_root}")))?;
        self.private_replication_plan_from_conversion(&conversion, destination_namespace)
    }

    fn private_replication_plan_from_conversion(
        &self,
        conversion: &PrivateConversion,
        destination_namespace: &str,
    ) -> Result<EgressPlan> {
        if destination_namespace.is_empty() {
            return Err(Error::SecurityPolicy(
                "private destination namespace must not be empty".to_string(),
            ));
        }
        if conversion.destination_namespace != destination_namespace {
            return Err(Error::SecurityPolicy(format!(
                "private ciphertext replication is not authorized for destination namespace {destination_namespace}"
            )));
        }
        if conversion.ciphertext_manifest.is_empty()
            || !conversion
                .ciphertext_manifest
                .iter()
                .any(|cid| cid == &conversion.ciphertext_root)
        {
            return Err(Error::SecurityPolicy(
                "private ciphertext manifest is missing its root".to_string(),
            ));
        }
        let manifest = conversion
            .ciphertext_manifest
            .iter()
            .cloned()
            .map(Cid)
            .collect::<Vec<_>>();
        let mut byte_size = 0;
        for cid in &manifest {
            let parsed = cid
                .0
                .parse::<cid::Cid>()
                .map_err(|error| Error::Encryption(format!("invalid ciphertext CID: {error}")))?;
            let bytes = self.read_block(cid)?;
            crate::car::verify_block(&parsed, &bytes)?;
            byte_size += bytes.len() as u64;
        }
        Ok(EgressPlan {
            root: Cid(conversion.ciphertext_root.clone()),
            resolved_name: None,
            operation: EgressOperation::PrivateEncryptedReplicate,
            backend: "private-swarm".to_string(),
            backend_target: destination_namespace.to_string(),
            network_posture: "private-swarm-no-public-dht".to_string(),
            manifest_digest: manifest_digest(&manifest),
            lock_registry_digest: registry_digest(&self.lock_registry()?)?,
            block_count: manifest.len(),
            manifest,
            byte_size,
            decoded_node_kinds: vec!["encrypted_private".to_string()],
            file_paths: Vec::new(),
            media_types: vec!["application/vnd.concierge.encrypted-block".to_string()],
            sensitivity_warnings: Vec::new(),
            known_public_receipts: self
                .publish_receipts()?
                .into_iter()
                .filter(|receipt| receipt.root == conversion.ciphertext_root)
                .count(),
            blocking_locks: Vec::new(),
        })
    }

    /// Produce a ciphertext-only CAR for a pre-reviewed private replication.
    /// This is allowed while the capability vault is locked.
    pub fn export_reviewed_private_car(
        &self,
        reviewed: &EgressPlan,
    ) -> Result<(Vec<u8>, EgressReceipt)> {
        let _policy_lock = self.policy_lock()?;
        let current =
            self.build_private_replication_plan(&reviewed.root.0, &reviewed.backend_target)?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "private ciphertext manifest or destination changed".to_string(),
            ));
        }
        self.check_private_replication_posture(&current)?;
        let mut blocks = Vec::with_capacity(current.manifest.len());
        for cid in &current.manifest {
            blocks.push((
                cid.0.parse::<cid::Cid>().map_err(|error| {
                    Error::Encryption(format!("invalid ciphertext CID: {error}"))
                })?,
                self.read_block(cid)?,
            ));
        }
        let root = current
            .root
            .0
            .parse::<cid::Cid>()
            .map_err(|error| Error::Encryption(format!("invalid ciphertext root: {error}")))?;
        let car = crate::car::build_car(&root, &blocks)?;
        let receipt = EgressReceipt {
            operation: EgressOperation::PrivateEncryptedReplicate,
            root: current.root.0.clone(),
            manifest_digest: current.manifest_digest.clone(),
            backend: current.backend.clone(),
            backend_target: current.backend_target.clone(),
            network_posture: current.network_posture.clone(),
            block_count: current.block_count,
            byte_size: current.byte_size,
            created_at: now_secs(),
        };
        self.append_private_receipt_unlocked(&receipt)?;
        self.append_security_event_unlocked(
            "private_ciphertext_replicated",
            &current.root,
            &format!("to namespace {}", current.backend_target),
        )?;
        Ok((car, receipt))
    }

    pub fn private_conversions(&self) -> Result<Vec<PrivateConversion>> {
        let path = self.private_index_path()?;
        if !path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect private index: {error}")))?
        {
            return Ok(Vec::new());
        }
        validate_private_file(&path)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("read private index: {error}")))?;
        let index: PrivateIndex = serde_json::from_str(&text)
            .map_err(|error| Error::Encryption(format!("parse private index: {error}")))?;
        if index.version != PRIVATE_INDEX_VERSION {
            return Err(Error::Encryption(format!(
                "unsupported private index version {}",
                index.version
            )));
        }
        Ok(index.entries)
    }

    pub fn private_copy_of(&self, plaintext_root: &str) -> Result<Option<String>> {
        Ok(self
            .private_conversions()?
            .into_iter()
            .find(|entry| entry.plaintext_root == plaintext_root)
            .map(|entry| entry.ciphertext_root))
    }

    fn record_private_conversion_unlocked(&self, conversion: PrivateConversion) -> Result<()> {
        let mut entries = self.private_conversions()?;
        entries.retain(|entry| entry.plaintext_root != conversion.plaintext_root);
        entries.push(conversion);
        let index = PrivateIndex {
            version: PRIVATE_INDEX_VERSION,
            entries,
        };
        ensure_private_dir(&self.security_dir()?)?;
        let bytes = serde_json::to_vec_pretty(&index)
            .map_err(|error| Error::Io(format!("serialize private index: {error}")))?;
        atomic_private_write(&self.private_index_path()?, &bytes)
    }

    pub fn private_egress_receipts(&self) -> Result<Vec<EgressReceipt>> {
        let path = self.private_receipts_path()?;
        if !path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect private receipts: {error}")))?
        {
            return Ok(Vec::new());
        }
        validate_private_file(&path)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| Error::Io(format!("read private receipts: {error}")))?;
        let trail: PrivateReceiptTrail = serde_json::from_str(&text)
            .map_err(|error| Error::Encryption(format!("parse private receipts: {error}")))?;
        if trail.version != PRIVATE_RECEIPT_VERSION {
            return Err(Error::Encryption(format!(
                "unsupported private receipt version {}",
                trail.version
            )));
        }
        Ok(trail.receipts)
    }

    fn append_private_receipt_unlocked(&self, receipt: &EgressReceipt) -> Result<()> {
        let mut receipts = self.private_egress_receipts()?;
        receipts.push(receipt.clone());
        let trail = PrivateReceiptTrail {
            version: PRIVATE_RECEIPT_VERSION,
            receipts,
        };
        let bytes = serde_json::to_vec_pretty(&trail)
            .map_err(|error| Error::Io(format!("serialize private receipt: {error}")))?;
        atomic_private_write(&self.private_receipts_path()?, &bytes)
    }

    /// Decrypt a private graph with the local sealed vault.
    pub fn read_encrypted_private(
        &self,
        ciphertext_root: &str,
        password: &str,
    ) -> Result<RecoveredPrivate> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let cap = self.find_capability_unlocked(ciphertext_root, password)?;
        self.read_private_with_capability(&PrivateCapability::from_capability(&cap.to_read_only()))
    }

    /// Decrypt a received share using its bearer capability. This never grants
    /// write authority because private shares issue read-only capabilities.
    pub fn read_private_with_capability(
        &self,
        capability: &PrivateCapability,
    ) -> Result<RecoveredPrivate> {
        let cap = capability.to_capability();
        let root_bytes = self.read_block(&Cid(cap.cid.clone()))?;
        let OpenNode::Dir(children) =
            open_node(&cap, &root_bytes).map_err(|error| Error::Encryption(error.to_string()))?
        else {
            return Err(Error::Encryption(
                "private root is not a directory".to_string(),
            ));
        };
        let by_name: HashMap<String, Capability> = children
            .into_iter()
            .map(|child| (child.name, child.cap))
            .collect();
        let manifest = self.read_private_file(&by_name, MANIFEST_CHILD)?;
        let manifest: ManifestDoc = serde_json::from_slice(&manifest)
            .map_err(|error| Error::Encryption(format!("manifest decode: {error}")))?;
        let mut blocks = Vec::with_capacity(manifest.cids.len());
        for cid in &manifest.cids {
            blocks.push((cid.clone(), self.read_private_file(&by_name, cid)?));
        }
        Ok(RecoveredPrivate {
            plaintext_root: manifest.root,
            blocks,
        })
    }

    fn read_private_file(
        &self,
        by_name: &HashMap<String, Capability>,
        name: &str,
    ) -> Result<Vec<u8>> {
        let cap = by_name
            .get(name)
            .ok_or_else(|| Error::Encryption(format!("private entry `{name}` missing")))?;
        let bytes = self.read_block(&Cid(cap.cid.clone()))?;
        match open_node(cap, &bytes).map_err(|error| Error::Encryption(error.to_string()))? {
            OpenNode::File(data) => Ok(data),
            OpenNode::Dir(_) => Err(Error::Encryption(format!("`{name}` is not a file"))),
        }
    }

    fn load_or_init_vault(&self) -> Result<VaultFile> {
        let path = self.capability_vault_path()?;
        if path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect capability vault: {error}")))?
        {
            validate_private_file(&path)?;
            let text = std::fs::read_to_string(&path)
                .map_err(|error| Error::Io(format!("read capability vault: {error}")))?;
            let file: VaultFile = serde_json::from_str(&text)
                .map_err(|error| Error::Encryption(format!("parse capability vault: {error}")))?;
            if file.version != CAPABILITY_VAULT_VERSION {
                return Err(Error::Encryption(format!(
                    "unsupported capability vault version {}",
                    file.version
                )));
            }
            Ok(file)
        } else {
            let mut salt = [0u8; 16];
            use rand_core::{OsRng, RngCore};
            OsRng.fill_bytes(&mut salt);
            Ok(VaultFile {
                version: CAPABILITY_VAULT_VERSION,
                salt: salt.to_vec(),
                sealed: Vec::new(),
            })
        }
    }

    fn save_vault_unlocked(&self, file: &VaultFile) -> Result<()> {
        ensure_private_dir(&self.security_dir()?)?;
        let bytes = serde_json::to_vec_pretty(file)
            .map_err(|error| Error::Io(format!("serialize capability vault: {error}")))?;
        atomic_private_write(&self.capability_vault_path()?, &bytes)
    }

    fn add_capability_to_vault_unlocked(&self, cap: &Capability, password: &str) -> Result<()> {
        let mut file = self.load_or_init_vault()?;
        let key = derive_vault_key(password, &file.salt)
            .map_err(|error| Error::Encryption(error.to_string()))?;
        let mut caps = if file.sealed.is_empty() {
            Vec::new()
        } else {
            key.open(&file.sealed)
                .map_err(|error| Error::Encryption(error.to_string()))?
        };
        caps.retain(|existing| existing.cid != cap.cid);
        caps.push(cap.clone());
        file.sealed = key
            .seal(&caps)
            .map_err(|error| Error::Encryption(error.to_string()))?;
        self.save_vault_unlocked(&file)
    }

    fn find_capability_unlocked(
        &self,
        ciphertext_root: &str,
        password: &str,
    ) -> Result<Capability> {
        let file = self.load_or_init_vault()?;
        if file.sealed.is_empty() {
            return Err(Error::Encryption("capability vault is empty".to_string()));
        }
        let key = derive_vault_key(password, &file.salt)
            .map_err(|error| Error::Encryption(error.to_string()))?;
        key.open(&file.sealed)
            .map_err(|error| Error::Encryption(error.to_string()))?
            .into_iter()
            .find(|cap| cap.cid == ciphertext_root)
            .ok_or_else(|| Error::Encryption(format!("no capability for {ciphertext_root}")))
    }
}

fn normalized_recipients(recipients: &[String]) -> Vec<String> {
    let mut normalized = recipients
        .iter()
        .map(|recipient| recipient.trim().to_string())
        .filter(|recipient| !recipient.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn validate_private_destination(destination_namespace: &str, recipients: &[String]) -> Result<()> {
    if destination_namespace.trim().is_empty() {
        return Err(Error::SecurityPolicy(
            "private destination namespace must not be empty".to_string(),
        ));
    }
    if normalized_recipients(recipients).is_empty() {
        return Err(Error::SecurityPolicy(
            "private sharing requires at least one reviewed recipient".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{CoreBinding, Node};
    use std::sync::{Arc, Barrier};

    fn store() -> (tempfile::TempDir, MemCli, Cid) {
        let dir = tempfile::TempDir::new().unwrap();
        let mem = MemCli::new(dir.path());
        let child = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"WETLANDS-CLASSIFIED","kind":"project"}"#.to_string(),
            })
            .unwrap();
        let root = mem.checkpoint("latest", &child, None).unwrap();
        mem.bind("latest", &root).unwrap();
        mem.set_password("pw").unwrap();
        (dir, mem, root)
    }

    fn reviewed(mem: &MemCli) -> PrivateSharePlan {
        let plan = mem
            .build_encrypt_and_share_plan(
                "latest",
                "team:wetlands",
                &["agent-recipient".to_string()],
            )
            .unwrap();
        mem.create_encrypt_and_share_private_grant(&plan, "pw")
            .unwrap();
        plan
    }

    #[test]
    fn conversion_uses_valid_ciphertext_cids_and_keeps_plaintext_locked() {
        let (_dir, mem, root) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        assert_ne!(result.conversion.ciphertext_root, root.0);
        assert!(result
            .conversion
            .ciphertext_root
            .parse::<cid::Cid>()
            .is_ok());
        assert!(mem.locks().unwrap().iter().any(|lock| lock.root == root.0));
        for cid in &mem.private_conversions().unwrap()[0].ciphertext_manifest {
            let bytes = mem.read_block(&Cid(cid.clone())).unwrap();
            assert!(!bytes
                .windows(b"WETLANDS-CLASSIFIED".len())
                .any(|window| window == b"WETLANDS-CLASSIFIED"));
        }
    }

    #[test]
    fn capability_key_rotation_relocks_the_graph_so_the_old_key_cannot_open_the_new_root() {
        let (_dir, mem, _) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        let old_root = result.conversion.ciphertext_root.clone();
        let old_read_cap = result.capability; // what a member (later revoked) holds

        // Rotate: re-encrypt under fresh keys at the next capability epoch.
        let rotation = mem.rotate_private_capability(&old_root, "pw").unwrap();
        assert_ne!(
            rotation.new_ciphertext_root, old_root,
            "rotation re-roots under fresh keys"
        );
        assert_eq!(
            rotation.capability_epoch, 1,
            "the capability epoch advanced"
        );

        // The NEW read capability recovers the original plaintext from the new root.
        let recovered = mem
            .read_private_with_capability(&rotation.new_read_capability)
            .unwrap();
        assert!(recovered.blocks.iter().any(|(_, b)| b
            .windows(b"WETLANDS-CLASSIFIED".len())
            .any(|w| w == b"WETLANDS-CLASSIFIED")));

        // Cryptographic cutoff: the OLD key cannot open the NEW root block.
        let new_root_bytes = mem
            .read_block(&Cid(rotation.new_ciphertext_root.clone()))
            .unwrap();
        assert!(
            matches!(
                concierge_crypto::open_node(&old_read_cap.to_capability(), &new_root_bytes),
                Err(concierge_crypto::CryptoError::CidMismatch)
            ),
            "a revoked holder's old key does not decrypt the rotated root",
        );

        // Honest limit: the old key still opens the OLD root (blocks already fetched).
        let old_recovered = mem.read_private_with_capability(&old_read_cap).unwrap();
        assert!(old_recovered.blocks.iter().any(|(_, b)| b
            .windows(b"WETLANDS-CLASSIFIED".len())
            .any(|w| w == b"WETLANDS-CLASSIFIED")));

        // The new generation is recorded at the advanced epoch.
        assert!(mem
            .private_conversions()
            .unwrap()
            .iter()
            .any(|c| c.ciphertext_root == rotation.new_ciphertext_root && c.capability_epoch == 1));
    }

    #[test]
    fn capability_handoff_reads_only_the_authorized_private_graph() {
        let (_dir, mem, _) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        let recovered = mem
            .read_private_with_capability(&result.capability)
            .unwrap();
        assert!(recovered.blocks.iter().any(|(_, bytes)| bytes
            .windows(b"WETLANDS-CLASSIFIED".len())
            .any(|window| window == b"WETLANDS-CLASSIFIED")));

        let wrong = PrivateCapability {
            ciphertext_root: result.capability.ciphertext_root.clone(),
            read_key: [7; 32],
        };
        assert!(mem.read_private_with_capability(&wrong).is_err());
    }

    #[test]
    fn encrypt_for_pin_blinds_the_record_yet_the_owner_keeps_the_key() {
        let (_dir, mem, root) = store();
        let (ciphertext_root, car) = mem.encrypt_subgraph_for_pin(&root, "pw").unwrap();

        // The thing pinned is a fresh ciphertext root, not the plaintext record.
        assert_ne!(ciphertext_root.0, root.0);
        assert!(ciphertext_root.0.parse::<cid::Cid>().is_ok());
        assert!(!car.is_empty());

        // What the pinning service would receive carries no plaintext.
        assert!(
            !car.windows(b"WETLANDS-CLASSIFIED".len())
                .any(|window| window == b"WETLANDS-CLASSIFIED"),
            "the exported ciphertext CAR must not leak plaintext",
        );

        // The owner kept the read capability locally, so the blind-pinned ciphertext
        // is still fully recoverable here.
        let cap = mem.find_capability_unlocked(&ciphertext_root.0, "pw").unwrap();
        let recovered = mem
            .read_private_with_capability(&PrivateCapability::from_capability(&cap.to_read_only()))
            .unwrap();
        assert!(recovered.blocks.iter().any(|(_, bytes)| bytes
            .windows(b"WETLANDS-CLASSIFIED".len())
            .any(|window| window == b"WETLANDS-CLASSIFIED")));

        // The plaintext record is untouched; a wrong password cannot encrypt-for-pin.
        assert!(mem.read_block(&root).is_ok());
        assert!(mem.encrypt_subgraph_for_pin(&root, "wrong").is_err());
    }

    #[test]
    fn locked_vault_can_export_ciphertext_but_cannot_decrypt_without_password() {
        let (_dir, mem, _) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        let plan = mem
            .build_private_replication_plan(&result.conversion.ciphertext_root, "team:wetlands")
            .unwrap();
        let (car, receipt) = mem.export_reviewed_private_car(&plan).unwrap();
        assert!(!car.is_empty());
        assert_eq!(
            receipt.operation,
            EgressOperation::PrivateEncryptedReplicate
        );
        assert!(mem
            .read_encrypted_private(&result.conversion.ciphertext_root, "wrong")
            .is_err());
    }

    #[test]
    fn conversion_grant_is_one_shot_and_exact_destination() {
        let (_dir, mem, _) = store();
        let plan = reviewed(&mem);
        mem.convert_and_share_private(&plan, "pw").unwrap();
        assert!(mem.convert_and_share_private(&plan, "pw").is_err());

        let mut changed = mem
            .build_encrypt_and_share_plan(
                "latest",
                "team:wetlands",
                &["agent-recipient".to_string()],
            )
            .unwrap();
        mem.create_encrypt_and_share_private_grant(&changed, "pw")
            .unwrap();
        changed.destination_namespace = "team:other".to_string();
        assert!(mem.convert_and_share_private(&changed, "pw").is_err());
    }

    #[test]
    fn private_receipts_never_mark_ciphertext_known_public() {
        let (_dir, mem, _) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        assert!(mem.publish_receipts().unwrap().is_empty());
        assert_eq!(mem.private_egress_receipts().unwrap().len(), 1);
        assert_eq!(
            mem.build_private_replication_plan(&result.conversion.ciphertext_root, "team:wetlands")
                .unwrap()
                .known_public_receipts,
            0
        );
    }

    #[test]
    fn private_manifest_and_car_contain_no_plaintext_cids_or_source_bytes() {
        let (_dir, mem, root) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        let conversion = &mem.private_conversions().unwrap()[0];
        assert!(!conversion
            .ciphertext_manifest
            .iter()
            .any(|cid| cid == &root.0));
        let plan = mem
            .build_private_replication_plan(&result.conversion.ciphertext_root, "team:wetlands")
            .unwrap();
        let (car, _) = mem.export_reviewed_private_car(&plan).unwrap();
        assert!(!car
            .windows(b"WETLANDS-CLASSIFIED".len())
            .any(|window| window == b"WETLANDS-CLASSIFIED"));
        assert!(!car
            .windows(root.0.len())
            .any(|window| window == root.0.as_bytes()));
    }

    #[test]
    fn private_replication_is_exact_destination_only() {
        let (_dir, mem, _) = store();
        let result = mem
            .convert_and_share_private(&reviewed(&mem), "pw")
            .unwrap();
        assert!(matches!(
            mem.build_private_replication_plan(
                &result.conversion.ciphertext_root,
                "team:unreviewed"
            ),
            Err(Error::SecurityPolicy(_))
        ));
    }

    #[test]
    fn concurrent_conversions_do_not_lose_vault_or_index_entries() {
        let (_dir, mem, _) = store();
        let other = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"SECOND-PRIVATE-GRAPH","kind":"project"}"#.to_string(),
            })
            .unwrap();
        mem.bind("other", &other).unwrap();
        // Review both sources against one stable lock-policy snapshot. If one
        // conversion had to auto-lock its source, invalidating the other
        // already-reviewed plan would be the correct fail-closed behavior.
        mem.lock_subgraph(&other, "private conversion source")
            .unwrap();
        let plans = ["latest", "other"].map(|name| {
            let plan = mem
                .build_encrypt_and_share_plan(
                    name,
                    &format!("team:{name}"),
                    &["agent-recipient".to_string()],
                )
                .unwrap();
            mem.create_encrypt_and_share_private_grant(&plan, "pw")
                .unwrap();
            plan
        });
        let barrier = Arc::new(Barrier::new(2));
        let handles = plans.map(|plan| {
            let mem = mem.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                mem.convert_and_share_private(&plan, "pw").unwrap()
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(mem.private_conversions().unwrap().len(), 2);
        for result in results {
            mem.read_encrypted_private(&result.conversion.ciphertext_root, "pw")
                .unwrap();
        }
    }
}
