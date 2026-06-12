//! Phase N · Phase G — UserID Recovery and Device Replacement.
//!
//! A device/actor identity is a bare `did:key` — immutable, so "rotation" there is
//! just revoke-and-re-enroll ([[phase-n-pairing-capabilities]]). The **root UserID**
//! is different: it must be able to **rotate its active signing key** (you replaced
//! a computer) or be **recovered** (you lost the active key) *without changing who
//! the user is*. A bare key can't express that, so the UserID carries an
//! **append-only, recovery-key-signed rotation log** (plan §Resolution and Rotation
//! Without a Directory) — the `did:plc` operation-log *idea* with **no central
//! service**: identity is resolved by replaying material you already hold.
//!
//! ## The model
//! - The **stable identifier** (`user_id`) is the *genesis* active key — it never
//!   changes, even as the active key rotates.
//! - A separate, high-priority **recovery key** (held offline) is declared at
//!   genesis. It can authorize a rotation even when the current active key is lost
//!   or compromised — that is *recovery*.
//! - Each operation links `prev` = the hash of the prior op (an append-only chain),
//!   and is signed by an **authorized** key: the *current active key* (routine
//!   rotation) or the *recovery key* (recovery).
//! - [`verify_and_resolve`] replays the log and returns the **current** active key.
//!   The `user_id` and the recovery key are invariant across the whole chain — a
//!   forged op that tries to change them, point at the wrong `prev`, or sign with an
//!   unauthorized key is rejected.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{verify as verify_sig, AgentId, Identity};

pub const USER_IDENTITY_OP_VERSION: u32 = 1;

/// What an operation does (metadata; authority is determined by the signer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserOpKind {
    /// Establishes the identity: declares the stable id, the initial active key, and
    /// the recovery key. Self-signed by the initial key.
    Genesis,
    /// Routine key rotation, signed by the *current active* key (device replacement).
    Rotate,
    /// Rotation signed by the *recovery* key — used when the active key is lost.
    Recover,
}

/// One signed entry in a UserID's rotation log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentityOp {
    pub version: u32,
    pub kind: UserOpKind,
    /// The stable UserID identifier — the genesis active key. Invariant.
    pub user_id: String,
    /// Hash of the previous op (`None` only for genesis) — the append-only link.
    pub prev: Option<String>,
    /// The active signing key in effect *after* this op.
    pub active_key: String,
    /// The recovery key — set at genesis, carried unchanged. Invariant.
    pub recovery_key: String,
    pub created_at: u64,
    /// The key that signed this op (must be authorized at this point in the chain).
    pub signer: String,
    pub signature: String,
}

impl UserIdentityOp {
    /// Establish a new UserID. `initial` is the first active key (and the stable id);
    /// `recovery_key` is a separate, offline key that can later authorize recovery.
    pub fn genesis(initial: &Identity, recovery_key: &str, created_at: u64) -> Self {
        let id = initial.agent_id().0;
        let mut op = Self {
            version: USER_IDENTITY_OP_VERSION,
            kind: UserOpKind::Genesis,
            user_id: id.clone(),
            prev: None,
            active_key: id.clone(),
            recovery_key: recovery_key.to_string(),
            created_at,
            signer: id,
            signature: String::new(),
        };
        op.signature = initial.sign(&op.signing_bytes());
        op
    }

    /// Append a rotation to `new_active_key`, signed by `signer` — which must be the
    /// current active key (a [`UserOpKind::Rotate`]) or the recovery key (a
    /// [`UserOpKind::Recover`]); the verifier enforces that.
    pub fn rotate(prior: &UserIdentityOp, new_active_key: &str, signer: &Identity, created_at: u64) -> Self {
        let kind = if signer.agent_id().0 == prior.recovery_key {
            UserOpKind::Recover
        } else {
            UserOpKind::Rotate
        };
        let mut op = Self {
            version: USER_IDENTITY_OP_VERSION,
            kind,
            user_id: prior.user_id.clone(),
            prev: Some(prior.hash()),
            active_key: new_active_key.to_string(),
            recovery_key: prior.recovery_key.clone(),
            created_at,
            signer: signer.agent_id().0,
            signature: String::new(),
        };
        op.signature = signer.sign(&op.signing_bytes());
        op
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("op serializes")
    }

    /// The content id of this op (the full signed op) — what the next op's `prev`
    /// points at, binding the chain.
    pub fn hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("op serializes");
        let digest = Sha256::digest(&bytes);
        let mut s = String::with_capacity(digest.len() * 2);
        for b in digest {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// The UserID's full rotation log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserIdentityLog {
    pub ops: Vec<UserIdentityOp>,
}

/// The resolved current state of a UserID after replaying its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    /// The stable identifier (never changes).
    pub user_id: String,
    /// The key the user currently signs with.
    pub active_key: String,
    /// The recovery key.
    pub recovery_key: String,
}

/// Why a rotation log was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("the log is empty (no genesis)")]
    Empty,
    #[error("unsupported user-identity op version")]
    Version,
    #[error("the first op must be a self-signed genesis")]
    BadGenesis,
    #[error("an op's prev does not point at the previous op (broken chain)")]
    BrokenChain,
    #[error("the stable user_id or recovery key changed mid-chain")]
    InvariantChanged,
    #[error("an op was signed by a key not authorized at that point (not the active or recovery key)")]
    UnauthorizedSigner,
    #[error("an op's signature does not verify")]
    BadSignature,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

/// **Replay and verify** a rotation log, returning the current [`ResolvedIdentity`].
/// Genesis must be self-signed; every later op must chain by `prev`, keep the
/// `user_id`/recovery key invariant, be signed by the then-authorized active or
/// recovery key, and verify. The stable `user_id` is the answer to "who is this
/// user" no matter how many times the active key has rotated.
pub fn verify_and_resolve(log: &UserIdentityLog) -> Result<ResolvedIdentity, RecoveryError> {
    let mut ops = log.ops.iter();
    let genesis = ops.next().ok_or(RecoveryError::Empty)?;
    if genesis.version != USER_IDENTITY_OP_VERSION {
        return Err(RecoveryError::Version);
    }
    // Genesis: self-signed by the initial key, which is both the id and active key.
    if genesis.kind != UserOpKind::Genesis
        || genesis.prev.is_some()
        || genesis.signer != genesis.user_id
        || genesis.active_key != genesis.user_id
    {
        return Err(RecoveryError::BadGenesis);
    }
    if !verify_sig(&AgentId(genesis.signer.clone()), &genesis.signing_bytes(), &genesis.signature)
        .map_err(RecoveryError::Malformed)?
    {
        return Err(RecoveryError::BadSignature);
    }

    let user_id = genesis.user_id.clone();
    let recovery_key = genesis.recovery_key.clone();
    let mut active_key = genesis.active_key.clone();
    let mut prev_hash = genesis.hash();

    for op in ops {
        if op.version != USER_IDENTITY_OP_VERSION {
            return Err(RecoveryError::Version);
        }
        if op.prev.as_deref() != Some(prev_hash.as_str()) {
            return Err(RecoveryError::BrokenChain);
        }
        if op.user_id != user_id || op.recovery_key != recovery_key {
            return Err(RecoveryError::InvariantChanged);
        }
        // Authority: the current active key (rotation) or the recovery key (recovery).
        if op.signer != active_key && op.signer != recovery_key {
            return Err(RecoveryError::UnauthorizedSigner);
        }
        if !verify_sig(&AgentId(op.signer.clone()), &op.signing_bytes(), &op.signature)
            .map_err(RecoveryError::Malformed)?
        {
            return Err(RecoveryError::BadSignature);
        }
        active_key = op.active_key.clone();
        prev_hash = op.hash();
    }

    Ok(ResolvedIdentity { user_id, active_key, recovery_key })
}

use std::path::PathBuf;

use crate::binding::{Cid, MemCli};
use crate::error::{Error, Result as CoreResult};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MemCli {
    fn user_identity_dir(&self) -> CoreResult<PathBuf> {
        Ok(self.ensure_security_dir()?.join("user-identity"))
    }

    fn user_key_path(&self) -> CoreResult<PathBuf> {
        Ok(self.user_identity_dir()?.join("user.key"))
    }

    fn recovery_key_path(&self) -> CoreResult<PathBuf> {
        Ok(self.user_identity_dir()?.join("recovery.key"))
    }

    fn user_log_path(&self) -> CoreResult<PathBuf> {
        Ok(self.user_identity_dir()?.join("log.json"))
    }

    /// This UserID's rotation log, if recovery has been established.
    pub fn user_identity_log(&self) -> CoreResult<Option<UserIdentityLog>> {
        match std::fs::read_to_string(self.user_log_path()?) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| Error::Io(format!("parse user log: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(format!("read user log: {e}"))),
        }
    }

    fn write_user_log(&self, log: &UserIdentityLog) -> CoreResult<()> {
        let text = serde_json::to_string_pretty(log).map_err(|e| Error::Io(format!("serialize user log: {e}")))?;
        std::fs::write(self.user_log_path()?, text).map_err(|e| Error::Io(format!("write user log: {e}")))
    }

    /// **Establish recovery** for this install's UserID: generate an offline
    /// **recovery key** and write a self-signed genesis op. Idempotent — if a log
    /// already exists it is returned unchanged. Back up the recovery key: it is the
    /// only way to recover the identity if the active key is lost. Returns the
    /// resolved identity (the stable id + current active key + recovery key).
    pub fn establish_user_recovery(&self) -> CoreResult<ResolvedIdentity> {
        if let Some(log) = self.user_identity_log()? {
            return verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("existing user log invalid: {e}")));
        }
        let initial = self.user_identity()?; // the current active UserID key
        // A fresh, separate recovery key — persisted owner-only (ideally moved
        // offline / into an encrypted recovery package; see the plan).
        let recovery = Identity::generate();
        recovery.save(&self.recovery_key_path()?).map_err(Error::Io)?;
        let genesis = UserIdentityOp::genesis(&initial, &recovery.agent_id().0, now_secs());
        let log = UserIdentityLog { ops: vec![genesis] };
        self.write_user_log(&log)?;
        let _ = self.append_security_event_unlocked(
            "user_recovery_established",
            &Cid(initial.agent_id().0),
            "genesis user-identity op written; recovery key generated",
        );
        verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("{e}")))
    }

    /// **Rotate the active key** (device replacement): generate a new active key,
    /// append a rotation signed by the *current* active key, and install the new key
    /// as `user.key`. The stable UserID is unchanged. Requires recovery to have been
    /// established.
    pub fn rotate_user_key(&self) -> CoreResult<ResolvedIdentity> {
        let mut log = self
            .user_identity_log()?
            .ok_or_else(|| Error::SecurityPolicy("no user-identity log — run establish first".to_string()))?;
        let current = self.user_identity()?;
        let new_active = Identity::generate();
        let prior = log.ops.last().ok_or_else(|| Error::SecurityPolicy("empty user log".to_string()))?;
        let op = UserIdentityOp::rotate(prior, &new_active.agent_id().0, &current, now_secs());
        log.ops.push(op);
        verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("rotation invalid: {e}")))?;
        // Install the new active key, then commit the log.
        new_active.save(&self.user_key_path()?).map_err(Error::Io)?;
        self.write_user_log(&log)?;
        let _ = self.append_security_event_unlocked("user_key_rotated", &Cid(new_active.agent_id().0.clone()), "active key rotated");
        verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("{e}")))
    }

    /// **Recover the identity** using the offline recovery key (when the active key
    /// is lost or compromised): append a rotation signed by the recovery key to a new
    /// active key, and install it. The stable UserID survives. (Here the recovery key
    /// is read from `recovery.key`; in production it would be supplied from an offline
    /// backup.)
    pub fn recover_user_key(&self) -> CoreResult<ResolvedIdentity> {
        let mut log = self
            .user_identity_log()?
            .ok_or_else(|| Error::SecurityPolicy("no user-identity log — nothing to recover".to_string()))?;
        let recovery = Identity::load_or_create(&self.recovery_key_path()?).map_err(Error::Io)?;
        let new_active = Identity::generate();
        let prior = log.ops.last().ok_or_else(|| Error::SecurityPolicy("empty user log".to_string()))?;
        if prior.recovery_key != recovery.agent_id().0 {
            return Err(Error::SecurityPolicy("recovery key does not match this identity".to_string()));
        }
        let op = UserIdentityOp::rotate(prior, &new_active.agent_id().0, &recovery, now_secs());
        log.ops.push(op);
        verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("recovery invalid: {e}")))?;
        new_active.save(&self.user_key_path()?).map_err(Error::Io)?;
        self.write_user_log(&log)?;
        let _ = self.append_security_event_unlocked("user_identity_recovered", &Cid(new_active.agent_id().0.clone()), "active key recovered via recovery key");
        verify_and_resolve(&log).map_err(|e| Error::SecurityPolicy(format!("{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genesis(initial: &Identity, recovery: &Identity) -> UserIdentityLog {
        UserIdentityLog { ops: vec![UserIdentityOp::genesis(initial, &recovery.agent_id().0, 1000)] }
    }

    #[test]
    fn a_fresh_genesis_resolves_to_its_own_key_with_a_stable_id() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let log = genesis(&initial, &recovery);
        let resolved = verify_and_resolve(&log).unwrap();
        assert_eq!(resolved.user_id, initial.agent_id().0);
        assert_eq!(resolved.active_key, initial.agent_id().0);
        assert_eq!(resolved.recovery_key, recovery.agent_id().0);
    }

    #[test]
    fn rotating_the_active_key_keeps_the_same_user_id() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let new_device = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        // Routine rotation: the current active key signs in the new key.
        let op = UserIdentityOp::rotate(log.ops.last().unwrap(), &new_device.agent_id().0, &initial, 2000);
        assert_eq!(op.kind, UserOpKind::Rotate);
        log.ops.push(op);

        let resolved = verify_and_resolve(&log).unwrap();
        assert_eq!(resolved.user_id, initial.agent_id().0, "the identity is unchanged");
        assert_eq!(resolved.active_key, new_device.agent_id().0, "but the active key rotated");
    }

    #[test]
    fn recovery_works_when_the_active_key_is_lost() {
        // The active key is gone; the offline recovery key authorizes a brand-new
        // active key. Identity survives.
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let replacement = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        let op = UserIdentityOp::rotate(log.ops.last().unwrap(), &replacement.agent_id().0, &recovery, 3000);
        assert_eq!(op.kind, UserOpKind::Recover, "signed by the recovery key");
        log.ops.push(op);

        let resolved = verify_and_resolve(&log).unwrap();
        assert_eq!(resolved.user_id, initial.agent_id().0, "still the same user after recovery");
        assert_eq!(resolved.active_key, replacement.agent_id().0);
    }

    #[test]
    fn a_chain_of_rotations_resolves_to_the_latest_key() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let k2 = Identity::generate();
        let k3 = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        log.ops.push(UserIdentityOp::rotate(log.ops.last().unwrap(), &k2.agent_id().0, &initial, 2000));
        log.ops.push(UserIdentityOp::rotate(log.ops.last().unwrap(), &k3.agent_id().0, &k2, 3000));
        let resolved = verify_and_resolve(&log).unwrap();
        assert_eq!(resolved.active_key, k3.agent_id().0);
        assert_eq!(resolved.user_id, initial.agent_id().0);
    }

    #[test]
    fn a_stranger_cannot_rotate_the_identity() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let attacker = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        // Attacker (neither the active nor the recovery key) signs a rotation.
        let op = UserIdentityOp::rotate(log.ops.last().unwrap(), &attacker.agent_id().0, &attacker, 2000);
        log.ops.push(op);
        assert_eq!(verify_and_resolve(&log), Err(RecoveryError::UnauthorizedSigner));
    }

    #[test]
    fn a_superseded_key_can_no_longer_rotate() {
        // After rotating initial → k2, the OLD initial key is no longer authorized.
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let k2 = Identity::generate();
        let k3 = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        log.ops.push(UserIdentityOp::rotate(log.ops.last().unwrap(), &k2.agent_id().0, &initial, 2000));
        // `initial` is now superseded; it tries to rotate again → rejected.
        log.ops.push(UserIdentityOp::rotate(log.ops.last().unwrap(), &k3.agent_id().0, &initial, 3000));
        assert_eq!(verify_and_resolve(&log), Err(RecoveryError::UnauthorizedSigner));
    }

    #[test]
    fn a_tampered_op_or_broken_chain_is_rejected() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let k2 = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        let mut op = UserIdentityOp::rotate(log.ops.last().unwrap(), &k2.agent_id().0, &initial, 2000);
        op.active_key = Identity::generate().agent_id().0; // tamper after signing
        log.ops.push(op);
        assert_eq!(verify_and_resolve(&log), Err(RecoveryError::BadSignature));

        // Broken chain: an op whose prev points nowhere.
        let mut log2 = genesis(&initial, &recovery);
        let mut op2 = UserIdentityOp::rotate(log2.ops.last().unwrap(), &k2.agent_id().0, &initial, 2000);
        op2.prev = Some("deadbeef".to_string());
        op2.signature = initial.sign(&op2.signing_bytes()); // re-sign the tampered op
        log2.ops.push(op2);
        assert_eq!(verify_and_resolve(&log2), Err(RecoveryError::BrokenChain));
    }

    #[test]
    fn memcli_establish_rotate_and_recover_keep_a_stable_user_id_across_key_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());

        // Establish recovery — the genesis id is the device's current UserID key.
        let r0 = mem.establish_user_recovery().unwrap();
        let stable_id = r0.user_id.clone();
        assert_eq!(r0.active_key, stable_id, "genesis active = the id");
        // Idempotent.
        assert_eq!(mem.establish_user_recovery().unwrap(), r0);

        // Rotate the active key (device replacement): the id holds, the key changes,
        // and the on-disk user.key now *is* the new active key.
        let r1 = mem.rotate_user_key().unwrap();
        assert_eq!(r1.user_id, stable_id, "same user after rotation");
        assert_ne!(r1.active_key, r0.active_key, "active key rotated");
        assert_eq!(mem.user_identity().unwrap().agent_id().0, r1.active_key, "the installed key is the new active key");

        // Recover via the offline recovery key (active key 'lost'): id still holds.
        let r2 = mem.recover_user_key().unwrap();
        assert_eq!(r2.user_id, stable_id, "same user after recovery");
        assert_ne!(r2.active_key, r1.active_key, "a fresh active key was installed");
        assert_eq!(r2.recovery_key, r0.recovery_key, "recovery key invariant");

        // The persisted log replays to exactly the resolved state.
        let log = mem.user_identity_log().unwrap().unwrap();
        assert_eq!(verify_and_resolve(&log).unwrap(), r2);
        assert_eq!(log.ops.len(), 3, "genesis + rotate + recover");
    }

    #[test]
    fn a_rotated_root_is_still_recognized_as_a_network_root() {
        // The integration: after a root rotates its key, a membership cert + a
        // capability it signs with the NEW key are accepted *with* its rotation log,
        // but rejected without it (the genesis key is still all the descriptor lists).
        use crate::capability::{verify_capability, verify_capability_with_logs, Capability, Namespace, NamespaceScope, Operation};
        use crate::membership::{verify_membership, verify_membership_with_logs, MembershipCertificate, NetworkDescriptor, RevocationSet, SubjectKind};

        let initial = Identity::generate();
        let recovery = Identity::generate();
        let descriptor = NetworkDescriptor::create(&initial, "team", 1000); // root = initial's key
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));

        // The root rotates initial → new_active.
        let new_active = Identity::generate();
        let log = {
            let g = UserIdentityOp::genesis(&initial, &recovery.agent_id().0, 1000);
            let rot = UserIdentityOp::rotate(&g, &new_active.agent_id().0, &initial, 2000);
            UserIdentityLog { ops: vec![g, rot] }
        };

        // The root, now signing with new_active, issues a device cert + a capability.
        let device = Identity::generate();
        let cert = MembershipCertificate::issue(&new_active, &descriptor.network_id, &device.agent_id().0, SubjectKind::Device, 1000, 24 * 3600, 0, vec![]);
        let cap = Capability::issue(&new_active, ns, &device.agent_id().0, vec![Operation::SyncRead], 1000, 24 * 3600, 0, false);
        let now = 3000;
        let revoked = RevocationSet::new();

        // Without the rotation log, the new key is an unknown issuer.
        assert!(verify_membership(&cert, &descriptor, now, &revoked).is_err(), "new key not recognized without the log");
        assert!(verify_capability(&cap, &[], &descriptor, now, &revoked).is_err());

        // With the log, the rotated root key is recognized — the recovered owner can
        // still administer the network.
        assert!(verify_membership_with_logs(&cert, &descriptor, now, &revoked, &[log.clone()]).is_ok());
        assert!(verify_capability_with_logs(&cap, &[], &descriptor, now, &revoked, &[log]).is_ok());
    }

    #[test]
    fn the_user_id_cannot_be_changed_mid_chain() {
        let initial = Identity::generate();
        let recovery = Identity::generate();
        let k2 = Identity::generate();
        let mut log = genesis(&initial, &recovery);
        let mut op = UserIdentityOp::rotate(log.ops.last().unwrap(), &k2.agent_id().0, &initial, 2000);
        op.user_id = "someone-else".to_string();
        op.signature = initial.sign(&op.signing_bytes()); // re-sign so only the invariant check fires
        log.ops.push(op);
        assert_eq!(verify_and_resolve(&log), Err(RecoveryError::InvariantChanged));
    }
}
