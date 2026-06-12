//! Phase N · Phase G — Revocation, Recovery, and Key Rotation.
//!
//! The verifiers ([`verify_membership`](crate::membership::verify_membership),
//! [`verify_capability`](crate::capability::verify_capability),
//! [`verify_head_record`](crate::sync::verify_head_record), and the merge check)
//! already consult a [`RevocationSet`] and the descriptor's `membership_epoch`.
//! This module makes revocation **real and propagable**: a persisted, signed
//! revocation ledger, and the **epoch advance** that cuts off a revoked subject's
//! future access network-wide.
//!
//! ## Revoking a subject (plan §Revoking a Device or Actor)
//! 1. A root administrator signs a [`RevocationRecord`].
//! 2. The network **membership epoch advances** — every prior certificate and
//!    capability is now from a stale epoch and is rejected.
//! 3. The record is persisted to the ledger; peers that receive it add the subject
//!    to their [`RevocationSet`].
//! 4. Remaining members are re-issued certificates/capabilities at the new epoch
//!    (the grant flow) and keep syncing.
//!
//! ## Honest limits (plan §Honest Limits)
//! Revocation is **prospective**: it stops future accepted writes and future epoch
//! keys. It cannot make a removed device forget blocks or keys it already received,
//! and previously decrypted plaintext may have been copied outside Concierge.
//! Full historical re-encryption (a new ciphertext root) is a separate, deliberate
//! operation, not part of routine revocation.

use std::path::PathBuf;

use serde::Serialize;

use crate::binding::{Cid, MemCli};
use crate::capability::{Capability, Namespace, Operation};
use crate::error::{Error, Result as CoreResult};
use crate::membership::{
    MembershipCertificate, NetworkDescriptor, NetworkId, RevocationRecord, RevocationSet,
    SubjectKind, UserId, DEFAULT_CERT_TTL_SECS,
};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MemCli {
    fn descriptor_path(&self, network_id: &NetworkId) -> CoreResult<PathBuf> {
        Ok(self
            .security_dir()?
            .join("networks")
            .join(format!("{}.descriptor.json", network_id.0)))
    }

    fn revocations_path(&self, network_id: &NetworkId) -> CoreResult<PathBuf> {
        Ok(self
            .security_dir()?
            .join("networks")
            .join(format!("{}.revocations.json", network_id.0)))
    }

    /// Load one network's descriptor by id.
    pub fn network_descriptor(&self, network_id: &NetworkId) -> CoreResult<NetworkDescriptor> {
        self.networks()?
            .into_iter()
            .find(|n| &n.network_id == network_id)
            .ok_or_else(|| Error::SecurityPolicy(format!("no such network {}", network_id.0)))
    }

    /// The persisted signed revocation records for a network.
    pub fn revocation_ledger(&self, network_id: &NetworkId) -> CoreResult<Vec<RevocationRecord>> {
        match std::fs::read_to_string(self.revocations_path(network_id)?) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::Io(format!("parse revocations: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(format!("read revocations: {e}"))),
        }
    }

    /// Build the [`RevocationSet`] every verifier consults, from the persisted
    /// ledger — only records that **verify** against the current descriptor (signed
    /// by a root, right network) are honored, so a forged revocation cannot cut off
    /// a legitimate member.
    pub fn revocation_set(&self, network_id: &NetworkId) -> CoreResult<RevocationSet> {
        let descriptor = self.network_descriptor(network_id)?;
        let mut set = RevocationSet::new();
        for record in self.revocation_ledger(network_id)? {
            if record.verify(&descriptor).is_ok() {
                set.revoke(&record.subject_id);
            }
        }
        Ok(set)
    }

    /// **Revoke a subject.** Signs a [`RevocationRecord`] (this device must hold the
    /// network root key), **advances the membership epoch** (so every prior cert/cap
    /// is now stale), persists both the advanced descriptor and the record, and logs
    /// a security event. Returns the advanced descriptor. Prospective only — see the
    /// module's honest-limits note.
    pub fn revoke(&self, network_id: &NetworkId, subject_id: &str) -> CoreResult<NetworkDescriptor> {
        self.ensure_security_dir()?;
        let mut descriptor = self.network_descriptor(network_id)?;
        let root = self.user_identity()?;
        if !descriptor.is_root(&UserId(root.agent_id().0)) {
            return Err(Error::SecurityPolicy(
                "this device does not hold the network root key to revoke".to_string(),
            ));
        }
        // Advance the epoch first, so the record names the epoch it takes effect in.
        descriptor.advance_epoch(&root);
        let record = RevocationRecord::issue(
            &root,
            network_id,
            subject_id,
            descriptor.membership_epoch,
            now_secs(),
        );

        let mut ledger = self.revocation_ledger(network_id)?;
        ledger.push(record);
        write_json(&self.revocations_path(network_id)?, &ledger)?;
        write_json(&self.descriptor_path(network_id)?, &descriptor)?;
        let _ = self.append_security_event_unlocked(
            "subject_revoked",
            &Cid(subject_id.to_string()),
            &format!("network={} new_epoch={}", network_id.0, descriptor.membership_epoch),
        );
        Ok(descriptor)
    }

    /// Issue (or **re-issue after rotation**) a subject's membership certificate plus
    /// one capability at the network's **current** epoch — the grant flow that keeps
    /// remaining members syncing after a revocation. Root-signed; refuses a subject
    /// that is currently revoked.
    pub fn grant_capability(
        &self,
        network_id: &NetworkId,
        subject_id: &str,
        subject_kind: SubjectKind,
        namespace: &Namespace,
        operations: Vec<Operation>,
    ) -> CoreResult<(MembershipCertificate, Capability)> {
        let descriptor = self.network_descriptor(network_id)?;
        if self.revocation_set(network_id)?.is_revoked(subject_id) {
            return Err(Error::SecurityPolicy(
                "cannot grant to a revoked subject".to_string(),
            ));
        }
        let root = self.user_identity()?;
        if !descriptor.is_root(&UserId(root.agent_id().0)) {
            return Err(Error::SecurityPolicy(
                "this device does not hold the network root key to grant".to_string(),
            ));
        }
        let now = now_secs();
        let cert = MembershipCertificate::issue(
            &root,
            network_id,
            subject_id,
            subject_kind,
            now,
            DEFAULT_CERT_TTL_SECS,
            descriptor.membership_epoch,
            operations.iter().map(|op| op_name(op)).collect(),
        );
        let capability = Capability::issue(
            &root,
            namespace.clone(),
            subject_id,
            operations,
            now,
            DEFAULT_CERT_TTL_SECS,
            descriptor.membership_epoch,
            false,
        );
        Ok((cert, capability))
    }
}

fn op_name(op: &Operation) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(format!("create dir: {e}")))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::Io(format!("serialize: {e}")))?;
    std::fs::write(path, text).map_err(|e| Error::Io(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{verify_capability, NamespaceScope};
    use crate::identity::Identity;
    use crate::sync::{verify_head_record, HeadRecord};

    const DAY: u64 = 24 * 3600;

    #[test]
    fn the_exit_criterion_a_revoked_device_is_cut_off_while_others_keep_syncing() {
        // Founder device A holds the root + network.
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let descriptor0 = mem.create_network("research-team").unwrap();
        let ns = Namespace::new(descriptor0.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let root = mem.user_identity().unwrap();

        // Two member devices, each a writer at epoch 0.
        let device_b = Identity::generate();
        let device_c = Identity::generate();
        let writer = |id: &str, epoch: u64| {
            Capability::issue(&root, ns.clone(), id, vec![Operation::SyncRead, Operation::SyncWrite], 1000, DAY, epoch, false)
        };
        let cap_b0 = writer(&device_b.agent_id().0, 0);
        let cap_c0 = writer(&device_c.agent_id().0, 0);

        // Both can write (sign a head record) at epoch 0.
        let none = RevocationSet::new();
        let head_b0 = HeadRecord::create(&device_b, &descriptor0.network_id, &ns, vec!["bafyB".into()], 1, 0, 0, cap_b0.clone(), 1500);
        assert!(verify_head_record(&head_b0, &descriptor0, 2000, &none).is_ok());

        // --- Revoke B ---
        let descriptor1 = mem.revoke(&descriptor0.network_id, &device_b.agent_id().0).unwrap();
        assert_eq!(descriptor1.membership_epoch, 1, "the epoch advanced");
        let revoked = mem.revocation_set(&descriptor0.network_id).unwrap();
        assert!(revoked.is_revoked(&device_b.agent_id().0));

        // B is cut off: its old write fails (stale epoch / revoked), and a fresh
        // capability issued *to B* is refused — it cannot obtain a new epoch.
        assert!(verify_head_record(&head_b0, &descriptor1, 2000, &revoked).is_err(), "revoked device's old head rejected");
        assert!(verify_capability(&cap_b0, &[], &descriptor1, 2000, &revoked).is_err(), "B's epoch-0 cap is now stale");
        let cap_b1 = writer(&device_b.agent_id().0, 1); // someone tries to re-grant B at the new epoch
        assert!(verify_capability(&cap_b1, &[], &descriptor1, 2000, &revoked).is_err(), "a revoked subject cannot hold any valid capability");

        // C keeps syncing: re-issued at the new epoch, its writes verify again.
        // (The grant stamps real wall-clock issue time, so verify at "now".)
        assert!(verify_capability(&cap_c0, &[], &descriptor1, 2000, &revoked).is_err(), "C's old epoch-0 cap is also stale after rotation");
        let (_cert_c1, cap_c1) = mem
            .grant_capability(&descriptor0.network_id, &device_c.agent_id().0, SubjectKind::Device, &ns, vec![Operation::SyncRead, Operation::SyncWrite])
            .unwrap();
        let now = now_secs();
        assert!(verify_capability(&cap_c1, &[], &descriptor1, now, &revoked).is_ok(), "remaining member re-issued at the new epoch");
        let head_c1 = HeadRecord::create(&device_c, &descriptor1.network_id, &ns, vec!["bafyC".into()], 1, 1, 0, cap_c1, now);
        assert!(verify_head_record(&head_c1, &descriptor1, now, &revoked).is_ok(), "remaining device continues syncing after rotation");

        // The grant flow itself refuses a revoked subject.
        assert!(
            mem.grant_capability(&descriptor0.network_id, &device_b.agent_id().0, SubjectKind::Device, &ns, vec![Operation::SyncRead]).is_err(),
            "cannot grant to a revoked subject",
        );
    }

    #[test]
    fn a_forged_revocation_does_not_cut_off_a_member() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let descriptor = mem.create_network("team").unwrap();
        let victim = Identity::generate();

        // An outsider (not a root) forges a revocation record and we plant it in
        // the ledger; the verifier must ignore it.
        let outsider = Identity::generate();
        let forged = RevocationRecord::issue(&outsider, &descriptor.network_id, &victim.agent_id().0, 1, 1500);
        let ledger = vec![forged];
        write_json(&mem.revocations_path(&descriptor.network_id).unwrap(), &ledger).unwrap();

        let set = mem.revocation_set(&descriptor.network_id).unwrap();
        assert!(!set.is_revoked(&victim.agent_id().0), "a non-root-signed revocation is not honored");
    }

    #[test]
    fn only_the_root_holder_can_revoke() {
        // A device without the network's descriptor/root cannot revoke into it.
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let fake_network = NetworkId("deadbeef".to_string());
        assert!(mem.revoke(&fake_network, "someone").is_err(), "no such network → cannot revoke");
    }
}
