//! Phase N · Phase A/B — Actor (ActorID) certificates.
//!
//! The third identity tier (plan §ActorID): a **harness or agent** running on a
//! device. An actor gets its own `did:key` and a **device-signed certificate** that
//! binds it to its hosting device and grants it permissions **narrower than the
//! device's** (least privilege, plan §Agent defaults). Contributions are then
//! attributable to the actual actor *and* the hosting device, and stopping or
//! replacing one agent never touches the DeviceID.
//!
//! Trust chain: root user → **device** (a [`MembershipCertificate`]) → **actor**
//! (this [`ActorCertificate`], signed by the device). [`verify_actor_certificate`]
//! checks the device is a valid member, the cert is signed by that device, it is
//! unexpired/in-epoch/unrevoked, and — crucially — the actor's operations are a
//! **subset of the device's** (an agent cannot widen its own authority).

use serde::{Deserialize, Serialize};

use crate::capability::{Namespace, Operation};
use crate::identity::{verify as verify_sig, AgentId, Identity};
use crate::membership::{
    verify_membership, MembershipCertificate, NetworkDescriptor, NetworkId, RevocationSet,
    SubjectKind,
};

pub const ACTOR_CERT_VERSION: u32 = 1;

/// A device-signed certificate enrolling an actor with scoped, expiry-bounded,
/// least-privilege operations on one namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorCertificate {
    pub version: u32,
    pub network_id: NetworkId,
    pub actor_id: String,
    /// The hosting device — must itself be a valid network member.
    pub device_id: String,
    pub namespace: String,
    pub operations: Vec<Operation>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub membership_epoch: u64,
    /// Signed by `device_id`.
    pub signature: String,
}

impl ActorCertificate {
    /// Issue and sign an actor certificate. `device` is the hosting device's
    /// identity; the granted `operations` must be a subset of what the device holds
    /// (enforced at verify time, not here).
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        device: &Identity,
        network_id: &NetworkId,
        actor_id: &str,
        namespace: &Namespace,
        operations: Vec<Operation>,
        issued_at: u64,
        ttl_secs: u64,
        membership_epoch: u64,
    ) -> Self {
        let mut cert = Self {
            version: ACTOR_CERT_VERSION,
            network_id: network_id.clone(),
            actor_id: actor_id.to_string(),
            device_id: device.agent_id().0,
            namespace: namespace.canonical(),
            operations,
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
            membership_epoch,
            signature: String::new(),
        };
        cert.signature = device.sign(&cert.signing_bytes());
        cert
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("actor cert serializes")
    }
}

/// Why an actor certificate was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorError {
    #[error("unsupported actor certificate version")]
    Version,
    #[error("actor certificate is for a different network")]
    WrongNetwork,
    #[error("the hosting device is not a valid network member")]
    DeviceNotMember,
    #[error("the presented device membership is not for a device")]
    NotADevice,
    #[error("the actor certificate's device does not match the presented device membership")]
    DeviceMismatch,
    #[error("actor certificate signature does not verify (not signed by the hosting device)")]
    BadSignature,
    #[error("actor certificate has expired")]
    Expired,
    #[error("actor certificate is not yet valid")]
    NotYetValid,
    #[error("actor certificate is from a stale membership epoch")]
    StaleEpoch,
    #[error("the actor has been revoked")]
    ActorRevoked,
    #[error("the actor was granted operations the hosting device does not hold")]
    PrivilegeEscalation,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

fn op_name(op: &Operation) -> String {
    serde_json::to_value(op).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

/// **The actor trust rule.** Accept `actor_cert` iff its hosting device is a valid
/// member (`device_membership` verifies and is a *device* cert for the same id), the
/// cert is signed by that device, it is unexpired/in-epoch, the actor is unrevoked,
/// and the actor's operations are a **subset of the device's** granted operations
/// (least privilege — an agent cannot widen its own authority).
pub fn verify_actor_certificate(
    actor_cert: &ActorCertificate,
    device_membership: &MembershipCertificate,
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
) -> Result<(), ActorError> {
    if actor_cert.version != ACTOR_CERT_VERSION {
        return Err(ActorError::Version);
    }
    if actor_cert.network_id != descriptor.network_id {
        return Err(ActorError::WrongNetwork);
    }
    // The hosting device must be a valid member, and a *device*.
    verify_membership(device_membership, descriptor, now, revoked)
        .map_err(|_| ActorError::DeviceNotMember)?;
    if device_membership.subject_kind != SubjectKind::Device {
        return Err(ActorError::NotADevice);
    }
    if device_membership.subject_id != actor_cert.device_id {
        return Err(ActorError::DeviceMismatch);
    }
    // The cert must be signed by that hosting device.
    if !verify_sig(&AgentId(actor_cert.device_id.clone()), &actor_cert.signing_bytes(), &actor_cert.signature)
        .map_err(ActorError::Malformed)?
    {
        return Err(ActorError::BadSignature);
    }
    if now < actor_cert.issued_at {
        return Err(ActorError::NotYetValid);
    }
    if now >= actor_cert.expires_at {
        return Err(ActorError::Expired);
    }
    if actor_cert.membership_epoch != descriptor.membership_epoch {
        return Err(ActorError::StaleEpoch);
    }
    if revoked.is_revoked(&actor_cert.actor_id) {
        return Err(ActorError::ActorRevoked);
    }
    // Least privilege: the actor's operations ⊆ the device's granted operations.
    if !actor_cert
        .operations
        .iter()
        .all(|op| device_membership.capabilities.contains(&op_name(op)))
    {
        return Err(ActorError::PrivilegeEscalation);
    }
    Ok(())
}

use std::path::PathBuf;

use crate::binding::MemCli;
use crate::error::{Error, Result as CoreResult};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MemCli {
    fn actors_path(&self, network_id: &NetworkId) -> CoreResult<PathBuf> {
        Ok(self
            .ensure_security_dir()?
            .join("networks")
            .join(format!("{}.actors.json", network_id.0)))
    }

    /// **Enroll an actor** hosted by this device: issue a device-signed
    /// [`ActorCertificate`] for `actor_id` with `operations` (defaulting to
    /// least-privilege read-only via [`Operation::agent_defaults`] if empty), scoped
    /// to `namespace`, and persist it. The granted operations must be a subset of
    /// this device's own — verified when the cert is later checked.
    pub fn enroll_actor(
        &self,
        network_id: &NetworkId,
        actor_id: &str,
        namespace: &Namespace,
        operations: Vec<Operation>,
    ) -> CoreResult<ActorCertificate> {
        let descriptor = self.network_descriptor(network_id)?;
        let device = self.identity()?;
        // The actor cannot exceed this device: intersect the requested ops (or the
        // least-privilege default) with what the device itself holds.
        let device_membership = self
            .device_membership(network_id)?
            .ok_or_else(|| Error::SecurityPolicy("this device is not a member of the network".to_string()))?;
        let held: std::collections::BTreeSet<String> =
            device_membership.capabilities.iter().cloned().collect();
        let candidate = if operations.is_empty() { Operation::agent_defaults() } else { operations };
        let operations: Vec<Operation> =
            candidate.into_iter().filter(|op| held.contains(&op_name(op))).collect();
        let cert = ActorCertificate::issue(
            &device,
            network_id,
            actor_id,
            namespace,
            operations,
            now_secs(),
            crate::membership::DEFAULT_CERT_TTL_SECS,
            descriptor.membership_epoch,
        );
        let mut all = self.actor_certificates(network_id)?;
        all.retain(|c| !(c.actor_id == cert.actor_id && c.namespace == cert.namespace));
        all.push(cert.clone());
        let text = serde_json::to_string_pretty(&all).map_err(|e| Error::Io(format!("serialize actors: {e}")))?;
        std::fs::write(self.actors_path(network_id)?, text).map_err(|e| Error::Io(format!("write actors: {e}")))?;
        Ok(cert)
    }

    /// The actor certificates this device has issued for a network.
    pub fn actor_certificates(&self, network_id: &NetworkId) -> CoreResult<Vec<ActorCertificate>> {
        match std::fs::read_to_string(self.actors_path(network_id)?) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::Io(format!("parse actors: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(format!("read actors: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::NamespaceScope;

    const DAY: u64 = 24 * 3600;

    /// (root, descriptor, namespace, device identity, device membership with the
    /// given op names).
    fn device_member(caps: &[&str]) -> (NetworkDescriptor, Namespace, Identity, MembershipCertificate) {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let device = Identity::generate();
        let membership = MembershipCertificate::issue(
            &root, &descriptor.network_id, &device.agent_id().0, SubjectKind::Device,
            1000, DAY, 0, caps.iter().map(|s| s.to_string()).collect(),
        );
        (descriptor, ns, device, membership)
    }

    #[test]
    fn a_device_enrolls_an_actor_with_a_subset_of_its_own_operations() {
        let (descriptor, ns, device, membership) = device_member(&["sync_read", "sync_write", "message_receive"]);
        let actor = Identity::generate();
        let cert = ActorCertificate::issue(
            &device, &descriptor.network_id, &actor.agent_id().0, &ns,
            vec![Operation::SyncRead, Operation::MessageReceive], 1000, DAY, 0,
        );
        assert!(verify_actor_certificate(&cert, &membership, &descriptor, 2000, &RevocationSet::new()).is_ok());
    }

    #[test]
    fn an_actor_cannot_be_granted_more_than_its_device_holds() {
        // The device holds read-only; enrolling the actor with write is escalation.
        let (descriptor, ns, device, membership) = device_member(&["sync_read"]);
        let actor = Identity::generate();
        let cert = ActorCertificate::issue(
            &device, &descriptor.network_id, &actor.agent_id().0, &ns,
            vec![Operation::SyncWrite], 1000, DAY, 0,
        );
        assert_eq!(
            verify_actor_certificate(&cert, &membership, &descriptor, 2000, &RevocationSet::new()),
            Err(ActorError::PrivilegeEscalation),
        );
    }

    #[test]
    fn an_actor_cert_from_a_non_member_device_is_rejected() {
        let (descriptor, ns, _device, _membership) = device_member(&["sync_read"]);
        // A device that is NOT a member of this network (no valid membership cert).
        let outsider_device = Identity::generate();
        let outsider_membership = MembershipCertificate::issue(
            &Identity::generate(), &descriptor.network_id, &outsider_device.agent_id().0,
            SubjectKind::Device, 1000, DAY, 0, vec!["sync_read".into()],
        ); // issued by a non-root → not a valid member
        let actor = Identity::generate();
        let cert = ActorCertificate::issue(&outsider_device, &descriptor.network_id, &actor.agent_id().0, &ns, vec![Operation::SyncRead], 1000, DAY, 0);
        assert_eq!(
            verify_actor_certificate(&cert, &outsider_membership, &descriptor, 2000, &RevocationSet::new()),
            Err(ActorError::DeviceNotMember),
        );
    }

    #[test]
    fn a_revoked_actor_is_rejected_and_tampering_breaks_the_signature() {
        let (descriptor, ns, device, membership) = device_member(&["sync_read"]);
        let actor = Identity::generate();
        let cert = ActorCertificate::issue(&device, &descriptor.network_id, &actor.agent_id().0, &ns, vec![Operation::SyncRead], 1000, DAY, 0);

        let mut revoked = RevocationSet::new();
        revoked.revoke(&actor.agent_id().0);
        assert_eq!(verify_actor_certificate(&cert, &membership, &descriptor, 2000, &revoked), Err(ActorError::ActorRevoked));

        let mut tampered = cert.clone();
        tampered.operations.push(Operation::SyncWrite);
        assert_eq!(
            verify_actor_certificate(&tampered, &membership, &descriptor, 2000, &RevocationSet::new()),
            Err(ActorError::BadSignature),
        );
    }

    #[test]
    fn enroll_actor_persists_a_least_privilege_default_when_no_ops_given() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let descriptor = mem.create_network("team").unwrap();
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let actor = Identity::generate();

        let cert = mem.enroll_actor(&descriptor.network_id, &actor.agent_id().0, &ns, vec![]).unwrap();
        // Default = read-only least privilege, never write/admin/publish.
        assert!(cert.operations.contains(&Operation::SyncRead));
        assert!(!cert.operations.contains(&Operation::SyncWrite));

        // Persisted + verifies against this device's (founder) membership.
        let stored = mem.actor_certificates(&descriptor.network_id).unwrap();
        assert_eq!(stored.len(), 1);
        let device_membership = mem.device_membership(&descriptor.network_id).unwrap().unwrap();
        assert!(verify_actor_certificate(&stored[0], &device_membership, &descriptor, now_secs(), &RevocationSet::new()).is_ok());
    }
}
