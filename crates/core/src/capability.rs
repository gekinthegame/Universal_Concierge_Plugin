//! Phase N · Phase B — Scoped Capabilities.
//!
//! Membership ([`crate::membership`]) answers *"are you in the network?"*.
//! Capabilities answer *"what may you do, where?"* — the least-privilege
//! authorization layer (plan §Capability and Permission Model).
//!
//! A [`Capability`] is a signed, scoped, expiry-bounded grant of specific
//! [`Operation`]s on one [`Namespace`]. Capabilities **delegate**: a holder whose
//! grant is `delegable` and includes [`Operation::CapabilityGrant`] may issue
//! *sub-capabilities*, but only ones that are a **subset** of its own authority —
//! same-or-narrower namespace, a subset of its operations, and no later expiry
//! (the "you cannot grant more than you hold" rule). Every chain terminates at a
//! root user of the [`NetworkDescriptor`].
//!
//! ## Two invariants this module enforces structurally
//! 1. **Read and write authority are separate** — [`Operation::SyncRead`] never
//!    implies [`Operation::SyncWrite`]; each is granted explicitly.
//! 2. **No durable capability authorizes public publication.** There is no
//!    "publish" operation at all. The strongest is
//!    [`Operation::RequestPublicationReview`], which may only *open* the local Data
//!    Platter review flow — the password-authorized one-shot grant remains the sole
//!    public-publication authority (Decision 0026; plan §Privacy and Publication).

use serde::{Deserialize, Serialize};

use crate::identity::{verify as verify_sig, AgentId, Identity};
use crate::membership::{NetworkDescriptor, NetworkId, RevocationSet};

pub const CAPABILITY_VERSION: u32 = 1;

/// The operations a capability may grant (plan §Capability and Permission Model).
/// Deliberately contains **no** public-publish operation — public publication is
/// never a network capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Discover,
    SyncRead,
    SyncWrite,
    MessageReceive,
    MessageSend,
    CheckpointCreate,
    MergePropose,
    MergeAccept,
    MemberInvite,
    CapabilityGrant,
    MemberRevoke,
    /// May only *open* the local publication-review flow. Never authorizes the
    /// public publish itself (Decision 0026).
    RequestPublicationReview,
}

impl Operation {
    /// The least-privilege default operation set for a freshly enrolled agent
    /// (plan §Agent defaults): read-only, no write/admin/delegation/publication.
    /// Widening beyond this requires explicit user approval.
    pub fn agent_defaults() -> Vec<Operation> {
        vec![
            Operation::Discover,
            Operation::SyncRead,
            Operation::MessageReceive,
        ]
    }
}

/// Which slice of a network a capability targets (plan §Shared Graph and Namespace
/// Model). String form: `network:{network_id}:{scope}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamespaceScope {
    /// The whole network — only roots and top-level admins hold this.
    All,
    Personal,
    Project(String),
    Room(String),
    Agent(String),
}

/// A fully-qualified namespace: a network plus a scope within it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    pub network_id: NetworkId,
    pub scope: NamespaceScope,
}

impl Namespace {
    pub fn new(network_id: NetworkId, scope: NamespaceScope) -> Self {
        Self { network_id, scope }
    }

    /// Canonical wire/string form, e.g. `network:abcd…:project:atlas`.
    pub fn canonical(&self) -> String {
        let tail = match &self.scope {
            NamespaceScope::All => "*".to_string(),
            NamespaceScope::Personal => "personal".to_string(),
            NamespaceScope::Project(id) => format!("project:{id}"),
            NamespaceScope::Room(id) => format!("room:{id}"),
            NamespaceScope::Agent(id) => format!("agent:{id}"),
        };
        format!("network:{}:{}", self.network_id.0, tail)
    }

    /// Parse a canonical namespace string (`network:{id}:{scope}`) back into a
    /// [`Namespace`] — the inverse of [`Namespace::canonical`].
    pub fn parse(s: &str) -> Option<Namespace> {
        let rest = s.strip_prefix("network:")?;
        let (network_id, tail) = rest.split_once(':')?;
        let scope = match tail {
            "*" => NamespaceScope::All,
            "personal" => NamespaceScope::Personal,
            other => {
                let (kind, id) = other.split_once(':')?;
                match kind {
                    "project" => NamespaceScope::Project(id.to_string()),
                    "room" => NamespaceScope::Room(id.to_string()),
                    "agent" => NamespaceScope::Agent(id.to_string()),
                    _ => return None,
                }
            }
        };
        Some(Namespace::new(NetworkId(network_id.to_string()), scope))
    }

    /// Does this namespace authority **cover** `other`? `All` covers every scope in
    /// the same network; otherwise scopes must match exactly. Never crosses
    /// networks. This is what stops a delegated grant from widening scope.
    pub fn covers(&self, other: &Namespace) -> bool {
        if self.network_id != other.network_id {
            return false;
        }
        matches!(self.scope, NamespaceScope::All) || self.scope == other.scope
    }
}

/// A signed, scoped, expiry-bounded grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub version: u32,
    pub namespace: Namespace,
    pub subject_id: String,
    pub operations: Vec<Operation>,
    /// Optional path / node-kind constraints (free-form in Phase B).
    pub constraints: Vec<String>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub membership_epoch: u64,
    /// May the subject re-delegate a subset of this capability?
    pub delegable: bool,
    pub issuer_id: String,
    pub signature: String,
}

impl Capability {
    /// Issue and sign a capability. The issuer must be authorized to grant it —
    /// either a root user, or (verified separately via [`verify_capability`]) the
    /// holder of a delegable parent. Issuance itself does not check authority; the
    /// *verifier* does, so a forged grant simply fails verification.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        issuer: &Identity,
        namespace: Namespace,
        subject_id: &str,
        operations: Vec<Operation>,
        issued_at: u64,
        ttl_secs: u64,
        membership_epoch: u64,
        delegable: bool,
    ) -> Self {
        let mut cap = Self {
            version: CAPABILITY_VERSION,
            namespace,
            subject_id: subject_id.to_string(),
            operations,
            constraints: Vec::new(),
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
            membership_epoch,
            delegable,
            issuer_id: issuer.agent_id().0,
            signature: String::new(),
        };
        cap.signature = issuer.sign(&cap.signing_bytes());
        cap
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("capability serializes")
    }

    /// Does this capability *grant* `operation` on `namespace`? Authorization to
    /// **use** a capability still requires it to pass [`verify_capability`]; this is
    /// the scope/operation predicate only.
    pub fn authorizes(&self, operation: Operation, namespace: &Namespace) -> bool {
        self.namespace.covers(namespace) && self.operations.contains(&operation)
    }

    /// Whether `child` is a legal **subset** delegation of `self`: same-or-narrower
    /// namespace, a subset of operations, and no later expiry. (Does not check
    /// signatures — that is the verifier's job.)
    fn can_delegate(&self, child: &Capability) -> Result<(), CapabilityError> {
        if !self.delegable {
            return Err(CapabilityError::NotDelegable);
        }
        if !self.operations.contains(&Operation::CapabilityGrant) {
            return Err(CapabilityError::CannotGrant);
        }
        if !self.namespace.covers(&child.namespace) {
            return Err(CapabilityError::ScopeEscalation);
        }
        if !child
            .operations
            .iter()
            .all(|op| self.operations.contains(op))
        {
            return Err(CapabilityError::PrivilegeEscalation);
        }
        if child.expires_at > self.expires_at {
            return Err(CapabilityError::OutlivesGrant);
        }
        Ok(())
    }
}

/// Why a capability check failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error("unsupported capability version {found} (this build understands {expected})")]
    Version { found: u32, expected: u32 },
    #[error("capability signature does not verify")]
    BadSignature,
    #[error("capability targets a different network than the descriptor")]
    WrongNetwork,
    #[error("capability has expired")]
    Expired,
    #[error("capability is not yet valid")]
    NotYetValid,
    #[error("capability is from a stale membership epoch")]
    StaleEpoch,
    #[error("subject has been revoked")]
    SubjectRevoked,
    #[error("issuer has been revoked")]
    IssuerRevoked,
    #[error("delegation chain is broken: a parent does not issue its child")]
    BrokenChain,
    #[error("a non-root issuer presented no authorizing parent capability")]
    NoAuthorizingParent,
    #[error("the parent capability is not delegable")]
    NotDelegable,
    #[error("the parent capability lacks capability_grant")]
    CannotGrant,
    #[error("delegation widened the namespace scope")]
    ScopeEscalation,
    #[error("delegation granted operations the issuer does not hold")]
    PrivilegeEscalation,
    #[error("delegated capability outlives its grant")]
    OutlivesGrant,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

/// **The capability trust rule.** Accept `cap` iff it is validly signed, in-network,
/// unexpired, in-epoch, its subject and issuer are unrevoked, and the issuer was
/// *authorized to grant it* — either a root user of `descriptor`, or the holder of
/// a delegable parent in `chain` (verified recursively) whose authority is a
/// superset of `cap`. `chain` lists the issuer's authorizing capabilities from the
/// immediate parent upward; it is empty when `cap` was issued directly by a root.
pub fn verify_capability(
    cap: &Capability,
    chain: &[Capability],
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
) -> Result<(), CapabilityError> {
    verify_capability_with_logs(cap, chain, descriptor, now, revoked, &[])
}

/// As [`verify_capability`], but recognizes a root that has **rotated/recovered**
/// its key (Phase G) via `root_logs` — so a recovered root's grants still chain.
/// Pass `&[]` for the default.
pub fn verify_capability_with_logs(
    cap: &Capability,
    chain: &[Capability],
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
    root_logs: &[crate::recovery::UserIdentityLog],
) -> Result<(), CapabilityError> {
    if cap.version != CAPABILITY_VERSION {
        return Err(CapabilityError::Version {
            found: cap.version,
            expected: CAPABILITY_VERSION,
        });
    }
    if cap.namespace.network_id != descriptor.network_id {
        return Err(CapabilityError::WrongNetwork);
    }
    if !verify_sig(
        &AgentId(cap.issuer_id.clone()),
        &cap.signing_bytes(),
        &cap.signature,
    )
    .map_err(CapabilityError::Malformed)?
    {
        return Err(CapabilityError::BadSignature);
    }
    if now < cap.issued_at {
        return Err(CapabilityError::NotYetValid);
    }
    if now >= cap.expires_at {
        return Err(CapabilityError::Expired);
    }
    if cap.membership_epoch != descriptor.membership_epoch {
        return Err(CapabilityError::StaleEpoch);
    }
    if revoked.is_revoked(&cap.subject_id) {
        return Err(CapabilityError::SubjectRevoked);
    }
    if revoked.is_revoked(&cap.issuer_id) {
        return Err(CapabilityError::IssuerRevoked);
    }
    // A root user may grant anything in its network — chain terminates here. A
    // root that has rotated/recovered its key is recognized via `root_logs`.
    if descriptor
        .resolved_root_keys(root_logs)
        .contains(&cap.issuer_id)
    {
        return Ok(());
    }
    // Otherwise the issuer must hold a delegable parent that authorizes this grant.
    let (parent, rest) = chain
        .split_first()
        .ok_or(CapabilityError::NoAuthorizingParent)?;
    if parent.subject_id != cap.issuer_id {
        return Err(CapabilityError::BrokenChain);
    }
    parent.can_delegate(cap)?;
    // The parent must itself be valid (recurse up to a root).
    verify_capability_with_logs(parent, rest, descriptor, now, revoked, root_logs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::NetworkDescriptor;

    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;

    fn net() -> (Identity, NetworkDescriptor) {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        (root, descriptor)
    }

    fn ns(descriptor: &NetworkDescriptor, scope: NamespaceScope) -> Namespace {
        Namespace::new(descriptor.network_id.clone(), scope)
    }

    #[test]
    fn a_root_issued_capability_verifies_and_authorizes_only_its_scope() {
        let (root, descriptor) = net();
        let device = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        let cap = Capability::issue(
            &root,
            project.clone(),
            &device.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            DAY,
            0,
            false,
        );
        assert!(verify_capability(&cap, &[], &descriptor, 2000, &RevocationSet::new()).is_ok());
        // Authorizes read on its own namespace…
        assert!(cap.authorizes(Operation::SyncRead, &project));
        // …but not write (read/write are separate), nor another namespace.
        assert!(
            !cap.authorizes(Operation::SyncWrite, &project),
            "read never implies write"
        );
        let other = ns(&descriptor, NamespaceScope::Project("other".into()));
        assert!(
            !cap.authorizes(Operation::SyncRead, &other),
            "scope is exact"
        );
    }

    #[test]
    fn delegation_chain_admin_grants_a_subset_to_an_agent() {
        let (root, descriptor) = net();
        let admin = Identity::generate();
        let agent = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));

        // Root grants the admin device a delegable read+write+grant capability.
        let admin_cap = Capability::issue(
            &root,
            project.clone(),
            &admin.agent_id().0,
            vec![
                Operation::SyncRead,
                Operation::SyncWrite,
                Operation::CapabilityGrant,
            ],
            1000,
            DAY,
            0,
            true,
        );
        // Admin delegates a read-only subset to an agent.
        let agent_cap = Capability::issue(
            &admin,
            project.clone(),
            &agent.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            HOUR,
            0,
            false,
        );
        // The agent capability verifies through the admin parent up to the root.
        assert!(verify_capability(
            &agent_cap,
            std::slice::from_ref(&admin_cap),
            &descriptor,
            2000,
            &RevocationSet::new()
        )
        .is_ok());
        // The admin capability itself verifies directly (root-issued).
        assert!(
            verify_capability(&admin_cap, &[], &descriptor, 2000, &RevocationSet::new()).is_ok()
        );
    }

    #[test]
    fn an_agent_cannot_grant_more_than_it_holds() {
        let (root, descriptor) = net();
        let admin = Identity::generate();
        let attacker_subject = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));

        // Admin holds read-only + grant (but NOT write), delegable.
        let admin_cap = Capability::issue(
            &root,
            project.clone(),
            &admin.agent_id().0,
            vec![Operation::SyncRead, Operation::CapabilityGrant],
            1000,
            DAY,
            0,
            true,
        );
        // Admin tries to grant WRITE it does not hold → privilege escalation.
        let forged = Capability::issue(
            &admin,
            project.clone(),
            &attacker_subject.agent_id().0,
            vec![Operation::SyncWrite],
            1000,
            HOUR,
            0,
            false,
        );
        assert_eq!(
            verify_capability(
                &forged,
                &[admin_cap],
                &descriptor,
                2000,
                &RevocationSet::new()
            ),
            Err(CapabilityError::PrivilegeEscalation),
        );
    }

    #[test]
    fn a_non_delegable_holder_cannot_delegate() {
        let (root, descriptor) = net();
        let holder = Identity::generate();
        let agent = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        // Holder has grant op but is NOT delegable.
        let holder_cap = Capability::issue(
            &root,
            project.clone(),
            &holder.agent_id().0,
            vec![Operation::SyncRead, Operation::CapabilityGrant],
            1000,
            DAY,
            0,
            false,
        );
        let child = Capability::issue(
            &holder,
            project.clone(),
            &agent.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            HOUR,
            0,
            false,
        );
        assert_eq!(
            verify_capability(
                &child,
                &[holder_cap],
                &descriptor,
                2000,
                &RevocationSet::new()
            ),
            Err(CapabilityError::NotDelegable),
        );
    }

    #[test]
    fn delegation_cannot_widen_the_namespace() {
        let (root, descriptor) = net();
        let admin = Identity::generate();
        let agent = Identity::generate();
        // Admin holds grant on ONE project only.
        let admin_ns = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        let admin_cap = Capability::issue(
            &root,
            admin_ns,
            &admin.agent_id().0,
            vec![Operation::SyncRead, Operation::CapabilityGrant],
            1000,
            DAY,
            0,
            true,
        );
        // Admin tries to grant on the WHOLE network.
        let all = ns(&descriptor, NamespaceScope::All);
        let forged = Capability::issue(
            &admin,
            all,
            &agent.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            HOUR,
            0,
            false,
        );
        assert_eq!(
            verify_capability(
                &forged,
                &[admin_cap],
                &descriptor,
                2000,
                &RevocationSet::new()
            ),
            Err(CapabilityError::ScopeEscalation),
        );
    }

    #[test]
    fn an_unauthorized_issuer_with_no_parent_is_rejected() {
        let (_root, descriptor) = net();
        let outsider = Identity::generate();
        let agent = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        let cap = Capability::issue(
            &outsider,
            project,
            &agent.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            HOUR,
            0,
            false,
        );
        assert_eq!(
            verify_capability(&cap, &[], &descriptor, 2000, &RevocationSet::new()),
            Err(CapabilityError::NoAuthorizingParent),
        );
    }

    #[test]
    fn revoking_a_mid_chain_issuer_invalidates_everything_below_it() {
        let (root, descriptor) = net();
        let admin = Identity::generate();
        let agent = Identity::generate();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        let admin_cap = Capability::issue(
            &root,
            project.clone(),
            &admin.agent_id().0,
            vec![Operation::SyncRead, Operation::CapabilityGrant],
            1000,
            DAY,
            0,
            true,
        );
        let agent_cap = Capability::issue(
            &admin,
            project,
            &agent.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            HOUR,
            0,
            false,
        );
        let mut revoked = RevocationSet::new();
        revoked.revoke(&admin.agent_id().0);
        // The agent cap's issuer (admin) is revoked → the whole chain fails.
        assert_eq!(
            verify_capability(&agent_cap, &[admin_cap], &descriptor, 2000, &revoked),
            Err(CapabilityError::IssuerRevoked),
        );
    }

    #[test]
    fn agent_defaults_are_least_privilege_and_carry_no_publish_authority() {
        let defaults = Operation::agent_defaults();
        assert!(defaults.contains(&Operation::SyncRead));
        assert!(
            !defaults.contains(&Operation::SyncWrite),
            "agents are read-only by default"
        );
        assert!(
            !defaults.contains(&Operation::CapabilityGrant),
            "no delegation by default"
        );
        assert!(
            !defaults.contains(&Operation::MemberInvite),
            "no member admin by default"
        );
        // The invariant: no operation grants public publication. The strongest is a
        // *request* to open the local review flow — never the publish itself.
        assert!(!defaults.contains(&Operation::RequestPublicationReview));
    }

    #[test]
    fn namespace_canonical_form_round_trips_scope() {
        let (_root, descriptor) = net();
        let project = ns(&descriptor, NamespaceScope::Project("atlas".into()));
        assert!(project.canonical().ends_with(":project:atlas"));
        let all = ns(&descriptor, NamespaceScope::All);
        assert!(all.canonical().ends_with(":*"));
        assert!(
            all.covers(&project),
            "the whole-network scope covers a project"
        );
        assert!(
            !project.covers(&all),
            "a project does not cover the whole network"
        );
        // canonical ⇄ parse round-trips every scope.
        for scope in [
            NamespaceScope::All,
            NamespaceScope::Personal,
            NamespaceScope::Project("atlas".into()),
            NamespaceScope::Room("r1".into()),
            NamespaceScope::Agent("a1".into()),
        ] {
            let n = ns(&descriptor, scope);
            assert_eq!(Namespace::parse(&n.canonical()), Some(n));
        }
    }
}
