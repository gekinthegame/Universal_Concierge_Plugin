//! Phase N · Phase C — capability-gated private sync.
//!
//! The Decision 0011 encryption already exists in [`crate::private`]: plaintext is
//! reviewed and converted into a **capability-encrypted** ciphertext graph, the
//! ciphertext-only replication plan needs no vault key (so a locked-vault device
//! can relay it), and no private-sync path can emit the plaintext source. This
//! module is the **bridge** to the Phase N membership/capability model: it decides
//! *who* may receive a namespace's ciphertext.
//!
//! Rule (plan §Egress Rules + §Capability and Permission Model): a member may
//! receive a namespace's ciphertext manifest **iff** they present a verified
//! [`Capability`] granting [`Operation::SyncRead`] on that namespace — chained to a
//! root of the [`NetworkDescriptor`], unexpired, in-epoch, unrevoked. Possession of
//! ciphertext without the capability key reveals nothing (the blocks are inert
//! ciphertext); this gate simply stops the node from *handing it out* to anyone who
//! is not authorized, and ties replication authority to the same scoped model
//! everything else uses.
//!
//! Two properties carried over from [`crate::private`] and re-asserted here:
//! - **Ciphertext only** — the manifest comes from a `PrivateConversion`'s
//!   ciphertext manifest; the plaintext source CID never appears.
//! - **No vault key needed** — building the plan is safe while the capability vault
//!   is locked (relay-only); decrypting/converting still requires the password.

use crate::binding::MemCli;
use crate::capability::{verify_capability, Capability, Namespace, Operation};
use crate::egress::EgressPlan;
use crate::error::{Error, Result as CoreResult};
use crate::membership::{NetworkDescriptor, RevocationSet};

/// Why a capability-gated private-sync request was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivateSyncError {
    #[error("capability is invalid: {0}")]
    Capability(#[from] crate::capability::CapabilityError),
    #[error("capability does not grant sync_read on this namespace")]
    NotAuthorized,
}

/// The pure authorization gate: may the holder of `capability` (verified via
/// `chain` against `descriptor`) **read** `namespace`? Read authority is explicit —
/// a write-only or differently-scoped capability is refused.
pub fn authorize_namespace_read(
    capability: &Capability,
    chain: &[Capability],
    namespace: &Namespace,
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
) -> Result<(), PrivateSyncError> {
    verify_capability(capability, chain, descriptor, now, revoked)?;
    if capability.authorizes(Operation::SyncRead, namespace) {
        Ok(())
    } else {
        Err(PrivateSyncError::NotAuthorized)
    }
}

impl MemCli {
    /// Serve the ciphertext-only replication plan for `ciphertext_root` to a member
    /// who presents a valid `sync_read` capability on the conversion's destination
    /// namespace. Refuses (without revealing graph existence beyond the manifest the
    /// member is entitled to) if the capability is invalid, revoked, or unscoped for
    /// this namespace. The returned plan contains **ciphertext only** and requires
    /// **no vault key** (relay-safe while locked).
    #[allow(clippy::too_many_arguments)]
    pub fn private_manifest_for_member(
        &self,
        ciphertext_root: &str,
        recipient_capability: &Capability,
        chain: &[Capability],
        descriptor: &NetworkDescriptor,
        now: u64,
        revoked: &RevocationSet,
    ) -> CoreResult<EgressPlan> {
        let conversion = self
            .private_conversions()?
            .into_iter()
            .find(|c| c.ciphertext_root == ciphertext_root)
            .ok_or_else(|| Error::Encryption(format!("unknown private root {ciphertext_root}")))?;
        let namespace = Namespace::parse(&conversion.destination_namespace).ok_or_else(|| {
            Error::SecurityPolicy(
                "conversion destination is not a network namespace (network:{id}:…)".to_string(),
            )
        })?;
        if namespace.network_id != descriptor.network_id {
            return Err(Error::SecurityPolicy(
                "presented capability is for a different network than the ciphertext".to_string(),
            ));
        }
        authorize_namespace_read(recipient_capability, chain, &namespace, descriptor, now, revoked)
            .map_err(|e| Error::SecurityPolicy(format!("private sync refused: {e}")))?;
        // Authorized: reuse the existing ciphertext-only builder (no vault key).
        self.build_private_replication_plan(ciphertext_root, &conversion.destination_namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{CoreBinding, Node};
    use crate::capability::{Capability, NamespaceScope};
    use crate::identity::Identity;

    const DAY: u64 = 24 * 3600;

    /// A store with a converted private graph whose destination is a real network
    /// namespace. Returns (mem, descriptor, namespace, ciphertext_root, recipient).
    fn converted_in_namespace() -> (
        tempfile::TempDir,
        MemCli,
        NetworkDescriptor,
        Namespace,
        String,
        Identity,
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        let mem = MemCli::new(dir.path());
        // Plaintext to protect.
        let child = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"WETLANDS-CLASSIFIED","kind":"project"}"#.to_string(),
            })
            .unwrap();
        let root = mem.checkpoint("latest", &child, None).unwrap();
        mem.bind("latest", &root).unwrap();
        mem.set_password("pw").unwrap();

        // Found a network; the destination is one of its namespaces.
        let descriptor = mem.create_network("research-team").unwrap();
        let namespace = Namespace::new(
            descriptor.network_id.clone(),
            NamespaceScope::Project("atlas".into()),
        );
        let recipient = Identity::generate();

        let plan = mem
            .build_encrypt_and_share_plan("latest", &namespace.canonical(), &[recipient.agent_id().0])
            .unwrap();
        mem.create_encrypt_and_share_private_grant(&plan, "pw").unwrap();
        let result = mem.convert_and_share_private(&plan, "pw").unwrap();
        (dir, mem, descriptor, namespace, result.conversion.ciphertext_root, recipient)
    }

    fn root_user(mem: &MemCli) -> Identity {
        mem.user_identity().unwrap()
    }

    fn read_cap(mem: &MemCli, ns: &Namespace, subject: &str, epoch: u64) -> Capability {
        Capability::issue(&root_user(mem), ns.clone(), subject, vec![Operation::SyncRead], 1000, DAY, epoch, false)
    }

    #[test]
    fn an_authorized_member_receives_a_ciphertext_only_manifest() {
        let (_d, mem, descriptor, namespace, ct_root, recipient) = converted_in_namespace();
        let cap = read_cap(&mem, &namespace, &recipient.agent_id().0, descriptor.membership_epoch);

        let plan = mem
            .private_manifest_for_member(&ct_root, &cap, &[], &descriptor, 2000, &RevocationSet::new())
            .expect("authorized member gets the manifest");

        // Ciphertext only: the encrypted-private kind, and the plaintext source CID
        // is nowhere in the served manifest.
        assert!(plan.decoded_node_kinds.iter().any(|k| k == "encrypted_private"));
        assert!(plan.manifest.iter().any(|c| c.0 == ct_root), "the ciphertext root is present");
        let plaintext_root = mem.private_conversions().unwrap()[0].plaintext_root.clone();
        assert!(!plan.manifest.iter().any(|c| c.0 == plaintext_root), "no plaintext source CID leaks");
        // And every served block is inert ciphertext, not the secret bytes.
        for cid in &plan.manifest {
            let bytes = mem.read_block(cid).unwrap();
            assert!(!bytes.windows(b"WETLANDS-CLASSIFIED".len()).any(|w| w == b"WETLANDS-CLASSIFIED"));
        }
    }

    #[test]
    fn a_member_without_a_sync_read_capability_is_refused() {
        let (_d, mem, descriptor, namespace, ct_root, recipient) = converted_in_namespace();
        // A capability that grants WRITE but not READ (read/write are separate).
        let write_only = Capability::issue(
            &root_user(&mem), namespace, &recipient.agent_id().0,
            vec![Operation::SyncWrite], 1000, DAY, descriptor.membership_epoch, false,
        );
        assert!(
            mem.private_manifest_for_member(&ct_root, &write_only, &[], &descriptor, 2000, &RevocationSet::new()).is_err(),
            "write authority does not grant read",
        );
    }

    #[test]
    fn a_capability_for_a_different_namespace_does_not_unlock_this_one() {
        let (_d, mem, descriptor, _ns, ct_root, recipient) = converted_in_namespace();
        let other = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("secret".into()));
        let cap = read_cap(&mem, &other, &recipient.agent_id().0, descriptor.membership_epoch);
        assert!(
            mem.private_manifest_for_member(&ct_root, &cap, &[], &descriptor, 2000, &RevocationSet::new()).is_err(),
            "a read cap on another namespace must not serve this one",
        );
    }

    #[test]
    fn a_revoked_member_is_cut_off_from_the_manifest() {
        let (_d, mem, descriptor, namespace, ct_root, recipient) = converted_in_namespace();
        let cap = read_cap(&mem, &namespace, &recipient.agent_id().0, descriptor.membership_epoch);
        let mut revoked = RevocationSet::new();
        revoked.revoke(&recipient.agent_id().0);
        assert!(
            mem.private_manifest_for_member(&ct_root, &cap, &[], &descriptor, 2000, &revoked).is_err(),
            "a revoked member can no longer pull the namespace",
        );
    }

    #[test]
    fn the_pure_gate_requires_explicit_sync_read() {
        let (_d, mem, descriptor, namespace, _ct, recipient) = converted_in_namespace();
        let cap = read_cap(&mem, &namespace, &recipient.agent_id().0, descriptor.membership_epoch);
        assert!(authorize_namespace_read(&cap, &[], &namespace, &descriptor, 2000, &RevocationSet::new()).is_ok());
        let other = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Personal);
        assert_eq!(
            authorize_namespace_read(&cap, &[], &other, &descriptor, 2000, &RevocationSet::new()),
            Err(PrivateSyncError::NotAuthorized),
        );
    }
}
