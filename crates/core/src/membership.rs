//! Phase N · Phase A — Identity Hierarchy and Membership.
//!
//! The first layer of the private multi-agent network (`PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`).
//! This module is **pure identity/crypto — no transport** (that is later sub-phases).
//! It answers exactly one question: *can two installations prove they belong to the
//! same private network without sharing a private key?*
//!
//! ## Identity hierarchy (plan §Identity Hierarchy)
//! Four distinct Ed25519 keypairs, never one secret copied between devices
//! (design rule 1):
//! - **[`UserId`]** — the person's long-lived root ownership key. Creates networks
//!   and authorizes members. Not used for routine writes/transport.
//! - **[`DeviceId`]** — one per installation. The existing install AgentID *is* the
//!   DeviceID (migration rule: don't break stores to rename it).
//! - **[`ActorId`]** — a harness/agent on a device; least-privilege, narrower than
//!   the device.
//! - **[`NetworkId`]** — identifies one mesh; derived deterministically from the
//!   founding user + name + time so it is unique and verifiable.
//!
//! The identifier *is* the public key (self-contained, no directory lookup — the
//! deliberate divergence from a `did:plc`-style registry, Decision 0018). We carry
//! the raw hex public key here; the `did:key` multibase *spelling* is a
//! presentation refinement that does not change the trust rule.
//!
//! ## Trust rule (plan §Resolution Without a Directory)
//! An identity is accepted iff its [`MembershipCertificate`] chains back to a
//! `root_user_id` in the signed [`NetworkDescriptor`] and is unexpired, in-epoch,
//! and unrevoked. "Who is this?" is answered entirely from material you already
//! hold — never a remote lookup.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{verify as verify_sig, AgentId, Identity};

pub const NETWORK_DESCRIPTOR_VERSION: u32 = 1;
pub const MEMBERSHIP_CERT_VERSION: u32 = 1;
pub const REVOCATION_RECORD_VERSION: u32 = 1;

macro_rules! hex_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<AgentId> for $name {
            fn from(a: AgentId) -> Self {
                $name(a.0)
            }
        }
    };
}

hex_id!(
    /// Root ownership identity (hex Ed25519 public key).
    UserId
);
hex_id!(
    /// Per-installation identity — equals the install's existing AgentID.
    DeviceId
);
hex_id!(
    /// A harness/agent actor hosted on a device.
    ActorId
);

/// Identifies one private collaboration mesh. Derived, not random, so it is stable
/// and independently recomputable from the descriptor's founding fields.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetworkId(pub String);

impl NetworkId {
    /// `sha256(root_user_id : name : created_at)` — deterministic and unique per
    /// (founder, name, creation time).
    fn derive(root_user_id: &str, name: &str, created_at: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(root_user_id.as_bytes());
        hasher.update(b":");
        hasher.update(name.as_bytes());
        hasher.update(b":");
        hasher.update(created_at.to_string().as_bytes());
        NetworkId(hex(&hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a certificate is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    User,
    Device,
    Actor,
}

/// Why a membership check failed. Errors are deterministic and never leak which
/// *other* identities exist (plan §Request-Response Requirements).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MembershipError {
    #[error("unsupported {what} version {found} (this build understands {expected})")]
    Version { what: &'static str, found: u32, expected: u32 },
    #[error("signature does not verify for {what}")]
    BadSignature { what: &'static str },
    #[error("certificate network does not match the descriptor")]
    WrongNetwork,
    #[error("issuer is not a root user of this network")]
    UntrustedIssuer,
    #[error("certificate has expired")]
    Expired,
    #[error("certificate is not yet valid")]
    NotYetValid,
    #[error("certificate is from a stale membership epoch")]
    StaleEpoch,
    #[error("subject has been revoked")]
    Revoked,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

/// Signed, public-within-the-network metadata describing one mesh. Contains **no**
/// capability keys, passwords, or private graph roots (plan §NetworkID).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDescriptor {
    pub version: u32,
    pub network_id: NetworkId,
    pub name: String,
    pub created_at: u64,
    pub root_user_ids: Vec<UserId>,
    /// Hash of the network policy (empty default in Phase A; policy lands later).
    pub policy_digest: String,
    pub membership_epoch: u64,
    /// Signed by one of `root_user_ids`.
    pub signature: String,
}

impl NetworkDescriptor {
    /// Create and sign a new network founded by `root` (a UserID identity).
    pub fn create(root: &Identity, name: &str, created_at: u64) -> Self {
        let root_user: UserId = root.agent_id().into();
        let network_id = NetworkId::derive(root_user.as_str(), name, created_at);
        let mut descriptor = Self {
            version: NETWORK_DESCRIPTOR_VERSION,
            network_id,
            name: name.to_string(),
            created_at,
            root_user_ids: vec![root_user],
            policy_digest: String::new(),
            membership_epoch: 0,
            signature: String::new(),
        };
        descriptor.signature = root.sign(&descriptor.signing_bytes());
        descriptor
    }

    /// Canonical bytes for signing/verifying: the descriptor with an empty
    /// signature, serialized in declaration order (deterministic).
    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("descriptor serializes")
    }

    /// Verify the descriptor is well-formed and signed by one of its declared root
    /// users. (A founder self-signs; additional roots are added by an existing
    /// root in later sub-phases.)
    pub fn verify(&self) -> Result<(), MembershipError> {
        if self.version != NETWORK_DESCRIPTOR_VERSION {
            return Err(MembershipError::Version {
                what: "network descriptor",
                found: self.version,
                expected: NETWORK_DESCRIPTOR_VERSION,
            });
        }
        let msg = self.signing_bytes();
        let signed_by_a_root = self.root_user_ids.iter().any(|user| {
            verify_sig(&AgentId(user.0.clone()), &msg, &self.signature).unwrap_or(false)
        });
        if signed_by_a_root {
            Ok(())
        } else {
            Err(MembershipError::BadSignature { what: "network descriptor" })
        }
    }

    pub fn is_root(&self, user: &UserId) -> bool {
        self.root_user_ids.contains(user)
    }

    /// The set of currently-authorized **root signing keys**: every `root_user_id`
    /// (the stable genesis key) plus, for any root that has rotated or been
    /// recovered, its **current active key** resolved from its rotation log (Phase G
    /// recovery). So a recovered root that now signs with a new key is still
    /// recognized as a root. With no logs this is exactly `root_user_ids`.
    pub fn resolved_root_keys(
        &self,
        root_logs: &[crate::recovery::UserIdentityLog],
    ) -> std::collections::BTreeSet<String> {
        let mut keys: std::collections::BTreeSet<String> =
            self.root_user_ids.iter().map(|u| u.0.clone()).collect();
        for log in root_logs {
            if let Ok(resolved) = crate::recovery::verify_and_resolve(log) {
                if self.root_user_ids.iter().any(|u| u.0 == resolved.user_id) {
                    keys.insert(resolved.active_key);
                }
            }
        }
        keys
    }

    /// Advance the membership epoch and re-sign, founded by `root`. A revocation
    /// bumps the epoch so **every prior certificate and capability becomes stale**
    /// (verifiers reject a mismatched epoch) — the network-wide future-access
    /// cutoff. Must be re-signed by a root user.
    pub fn advance_epoch(&mut self, root: &Identity) {
        self.membership_epoch = self.membership_epoch.saturating_add(1);
        self.signature = root.sign(&self.signing_bytes());
    }
}

/// A scoped, expiry-bounded, signed proof of membership (plan §Membership
/// Certificates). Routine contributions are signed by the subject; *this* proves
/// the subject is authorized at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipCertificate {
    pub version: u32,
    pub network_id: NetworkId,
    pub subject_id: String,
    pub subject_kind: SubjectKind,
    pub issuer_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub membership_epoch: u64,
    pub capabilities: Vec<String>,
    pub constraints: String,
    pub signature: String,
}

impl MembershipCertificate {
    /// Issue and sign a certificate. In Phase A the issuer is a root user (the only
    /// authority that chains to the descriptor); delegated issuance is later.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        issuer: &Identity,
        network_id: &NetworkId,
        subject_id: &str,
        subject_kind: SubjectKind,
        issued_at: u64,
        ttl_secs: u64,
        membership_epoch: u64,
        capabilities: Vec<String>,
    ) -> Self {
        let mut cert = Self {
            version: MEMBERSHIP_CERT_VERSION,
            network_id: network_id.clone(),
            subject_id: subject_id.to_string(),
            subject_kind,
            issuer_id: issuer.agent_id().0,
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
            membership_epoch,
            capabilities,
            constraints: String::new(),
            signature: String::new(),
        };
        cert.signature = issuer.sign(&cert.signing_bytes());
        cert
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("certificate serializes")
    }
}

/// The set of subjects whose membership has been revoked (plan §Revocation). In
/// Phase A this is the verifier-side input; signed [`RevocationRecord`]s and epoch
/// advancement are issued in Phase G.
#[derive(Debug, Clone, Default)]
pub struct RevocationSet {
    revoked: std::collections::BTreeSet<String>,
}

impl RevocationSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revoke(&mut self, subject_id: &str) {
        self.revoked.insert(subject_id.to_string());
    }

    pub fn is_revoked(&self, subject_id: &str) -> bool {
        self.revoked.contains(subject_id)
    }
}

/// A signed revocation of a subject's membership (plan §Revoking a Device or Actor).
/// Phase A defines the record + verification; epoch rotation/key re-issue is Phase G.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationRecord {
    pub version: u32,
    pub network_id: NetworkId,
    pub subject_id: String,
    pub membership_epoch: u64,
    pub issuer_id: String,
    pub revoked_at: u64,
    pub signature: String,
}

impl RevocationRecord {
    pub fn issue(
        issuer: &Identity,
        network_id: &NetworkId,
        subject_id: &str,
        membership_epoch: u64,
        revoked_at: u64,
    ) -> Self {
        let mut record = Self {
            version: REVOCATION_RECORD_VERSION,
            network_id: network_id.clone(),
            subject_id: subject_id.to_string(),
            membership_epoch,
            issuer_id: issuer.agent_id().0,
            revoked_at,
            signature: String::new(),
        };
        record.signature = issuer.sign(&record.signing_bytes());
        record
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("revocation serializes")
    }

    /// A revocation is valid iff signed by a root user of the descriptor it names.
    pub fn verify(&self, descriptor: &NetworkDescriptor) -> Result<(), MembershipError> {
        if self.version != REVOCATION_RECORD_VERSION {
            return Err(MembershipError::Version {
                what: "revocation record",
                found: self.version,
                expected: REVOCATION_RECORD_VERSION,
            });
        }
        if self.network_id != descriptor.network_id {
            return Err(MembershipError::WrongNetwork);
        }
        let issuer = UserId(self.issuer_id.clone());
        if !descriptor.is_root(&issuer) {
            return Err(MembershipError::UntrustedIssuer);
        }
        if verify_sig(&AgentId(self.issuer_id.clone()), &self.signing_bytes(), &self.signature)
            .unwrap_or(false)
        {
            Ok(())
        } else {
            Err(MembershipError::BadSignature { what: "revocation record" })
        }
    }
}

/// **The trust rule.** Accept `cert` iff it chains to a root of `descriptor`, is
/// validly signed, unexpired, in the current membership epoch, and unrevoked.
/// `now` is unix seconds. This is the single gate every received identity passes
/// before it is treated as a network member.
pub fn verify_membership(
    cert: &MembershipCertificate,
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
) -> Result<(), MembershipError> {
    verify_membership_with_logs(cert, descriptor, now, revoked, &[])
}

/// As [`verify_membership`], but recognizes a root that has **rotated/recovered**
/// its key (Phase G): the issuer may be any current active root key resolved from
/// `root_logs`, not only the original genesis key. Pass `&[]` for the default.
pub fn verify_membership_with_logs(
    cert: &MembershipCertificate,
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
    root_logs: &[crate::recovery::UserIdentityLog],
) -> Result<(), MembershipError> {
    if cert.version != MEMBERSHIP_CERT_VERSION {
        return Err(MembershipError::Version {
            what: "membership certificate",
            found: cert.version,
            expected: MEMBERSHIP_CERT_VERSION,
        });
    }
    // The descriptor must itself be valid before we trust anything it vouches for.
    descriptor.verify()?;
    if cert.network_id != descriptor.network_id {
        return Err(MembershipError::WrongNetwork);
    }
    // The issuer must be a root user (the only authority that chains to the
    // descriptor) — its genesis key, or its current active key after a rotation.
    if !descriptor.resolved_root_keys(root_logs).contains(&cert.issuer_id) {
        return Err(MembershipError::UntrustedIssuer);
    }
    if !verify_sig(&AgentId(cert.issuer_id.clone()), &cert.signing_bytes(), &cert.signature)
        .map_err(MembershipError::Malformed)?
    {
        return Err(MembershipError::BadSignature { what: "membership certificate" });
    }
    if now < cert.issued_at {
        return Err(MembershipError::NotYetValid);
    }
    if now >= cert.expires_at {
        return Err(MembershipError::Expired);
    }
    if cert.membership_epoch != descriptor.membership_epoch {
        return Err(MembershipError::StaleEpoch);
    }
    if revoked.is_revoked(&cert.subject_id) {
        return Err(MembershipError::Revoked);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The default membership-certificate lifetime: certs are short-lived and renewed
/// (plan §Membership Certificates). 30 days is the Phase A default.
pub const DEFAULT_CERT_TTL_SECS: u64 = 30 * 24 * 3600;

use std::path::PathBuf;

use crate::binding::MemCli;
use crate::error::{Error, Result as CoreResult};

impl MemCli {
    /// The root **UserID** for this install, created on first use under
    /// `security/user-identity/`. Distinct from the install DeviceID
    /// (`identity()`) — design rule 1: never conflate or copy the root key.
    pub fn user_identity(&self) -> CoreResult<Identity> {
        let path = self.ensure_security_dir()?.join("user-identity").join("user.key");
        Identity::load_or_create(&path).map_err(Error::Io)
    }

    fn networks_dir(&self) -> CoreResult<PathBuf> {
        Ok(self.security_dir()?.join("networks"))
    }

    /// Found a new private network owned by this install's UserID, persist its
    /// signed descriptor, and self-issue this **device's** membership certificate
    /// (the founding device is a member). Returns the descriptor.
    pub fn create_network(&self, name: &str) -> CoreResult<NetworkDescriptor> {
        self.ensure_security_dir()?;
        let user = self.user_identity()?;
        let descriptor = NetworkDescriptor::create(&user, name, now_secs());
        let dir = self.networks_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(format!("create networks dir: {e}")))?;
        let desc_path = dir.join(format!("{}.descriptor.json", descriptor.network_id.0));
        write_json(&desc_path, &descriptor)?;

        // Self-issue this device's membership certificate.
        let device = self.identity()?;
        let cert = MembershipCertificate::issue(
            &user,
            &descriptor.network_id,
            &device.agent_id().0,
            SubjectKind::Device,
            now_secs(),
            DEFAULT_CERT_TTL_SECS,
            descriptor.membership_epoch,
            vec!["sync_read".to_string(), "sync_write".to_string()],
        );
        write_json(&dir.join(format!("{}.device-cert.json", descriptor.network_id.0)), &cert)?;
        Ok(descriptor)
    }

    /// Every private network this install has founded or joined (its persisted
    /// descriptors).
    pub fn networks(&self) -> CoreResult<Vec<NetworkDescriptor>> {
        let dir = self.networks_dir()?;
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(Error::Io(format!("read networks dir: {e}"))),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.to_string_lossy().ends_with(".descriptor.json") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(descriptor) = serde_json::from_str::<NetworkDescriptor>(&text) {
                        out.push(descriptor);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// This device's membership certificate for `network_id`, if it holds one.
    pub fn device_membership(&self, network_id: &NetworkId) -> CoreResult<Option<MembershipCertificate>> {
        let path = self.networks_dir()?.join(format!("{}.device-cert.json", network_id.0));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| Error::Io(format!("parse device cert: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(format!("read device cert: {e}"))),
        }
    }
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> CoreResult<()> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| Error::Io(format!("serialize: {e}")))?;
    std::fs::write(path, text).map_err(|e| Error::Io(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3600;

    /// A founded network plus a device joined to it. Returns (root user, descriptor,
    /// device identity, device's membership cert).
    fn founded_network() -> (Identity, NetworkDescriptor, Identity, MembershipCertificate) {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "research-team", 1000);
        let device = Identity::generate();
        let cert = MembershipCertificate::issue(
            &root,
            &descriptor.network_id,
            &device.agent_id().0,
            SubjectKind::Device,
            1000,
            24 * HOUR,
            0,
            vec!["sync_read".to_string()],
        );
        (root, descriptor, device, cert)
    }

    #[test]
    fn the_exit_criterion_two_distinct_devices_prove_one_network() {
        // The Phase A exit criterion: two installations with DISTINCT device keys
        // prove membership in the same network without sharing any private key.
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "personal", 1000);
        let device_a = Identity::generate();
        let device_b = Identity::generate();
        assert_ne!(device_a.agent_id(), device_b.agent_id(), "distinct device keys");

        let cert_a = MembershipCertificate::issue(
            &root, &descriptor.network_id, &device_a.agent_id().0, SubjectKind::Device, 1000, 24 * HOUR, 0, vec![],
        );
        let cert_b = MembershipCertificate::issue(
            &root, &descriptor.network_id, &device_b.agent_id().0, SubjectKind::Device, 1000, 24 * HOUR, 0, vec![],
        );
        let revoked = RevocationSet::new();
        // Each proves membership from material it holds — no UserID secret, no
        // peer device secret needed.
        assert!(verify_membership(&cert_a, &descriptor, 2000, &revoked).is_ok());
        assert!(verify_membership(&cert_b, &descriptor, 2000, &revoked).is_ok());
    }

    #[test]
    fn network_id_is_deterministic_and_descriptor_self_verifies() {
        let root = Identity::generate();
        let d1 = NetworkDescriptor::create(&root, "team", 42);
        let d2 = NetworkDescriptor::create(&root, "team", 42);
        assert_eq!(d1.network_id, d2.network_id, "derived id is deterministic");
        let d3 = NetworkDescriptor::create(&root, "team", 43);
        assert_ne!(d1.network_id, d3.network_id, "time disambiguates");
        assert!(d1.verify().is_ok(), "a founder-signed descriptor self-verifies");
    }

    #[test]
    fn a_forged_descriptor_signature_is_rejected() {
        let (_root, mut descriptor, _device, _cert) = founded_network();
        // Tamper: rename the network after signing.
        descriptor.name = "evil-rename".to_string();
        assert_eq!(descriptor.verify(), Err(MembershipError::BadSignature { what: "network descriptor" }));
    }

    #[test]
    fn a_certificate_from_an_outsider_does_not_chain_to_the_network() {
        let (_root, descriptor, device, _cert) = founded_network();
        // An outsider (not a root user) issues a cert for the same device.
        let outsider = Identity::generate();
        let forged = MembershipCertificate::issue(
            &outsider, &descriptor.network_id, &device.agent_id().0, SubjectKind::Device, 1000, 24 * HOUR, 0, vec![],
        );
        assert_eq!(
            verify_membership(&forged, &descriptor, 2000, &RevocationSet::new()),
            Err(MembershipError::UntrustedIssuer),
            "only a root user's certificate chains to the network"
        );
    }

    #[test]
    fn tampered_capabilities_break_the_certificate_signature() {
        let (_root, descriptor, _device, mut cert) = founded_network();
        cert.capabilities.push("sync_write".to_string()); // privilege escalation attempt
        assert_eq!(
            verify_membership(&cert, &descriptor, 2000, &RevocationSet::new()),
            Err(MembershipError::BadSignature { what: "membership certificate" }),
        );
    }

    #[test]
    fn expiry_and_not_yet_valid_are_enforced() {
        let (_root, descriptor, _device, cert) = founded_network();
        let revoked = RevocationSet::new();
        // issued_at = 1000, expires_at = 1000 + 24h.
        assert_eq!(verify_membership(&cert, &descriptor, 999, &revoked), Err(MembershipError::NotYetValid));
        assert!(verify_membership(&cert, &descriptor, 1000 + HOUR, &revoked).is_ok());
        assert_eq!(verify_membership(&cert, &descriptor, 1000 + 24 * HOUR, &revoked), Err(MembershipError::Expired));
    }

    #[test]
    fn a_stale_epoch_certificate_is_rejected() {
        let (root, mut descriptor, _device, cert) = founded_network();
        // A revocation elsewhere advanced the network epoch; the old cert is stale.
        descriptor.membership_epoch = 1;
        descriptor.signature = root.sign(&descriptor.signing_bytes()); // re-sign the advanced descriptor
        assert_eq!(
            verify_membership(&cert, &descriptor, 2000, &RevocationSet::new()),
            Err(MembershipError::StaleEpoch),
        );
    }

    #[test]
    fn a_revoked_subject_is_rejected_and_the_record_verifies() {
        let (root, descriptor, device, cert) = founded_network();
        let mut revoked = RevocationSet::new();
        revoked.revoke(&device.agent_id().0);
        assert_eq!(
            verify_membership(&cert, &descriptor, 2000, &revoked),
            Err(MembershipError::Revoked),
        );
        // The signed revocation record itself chains to a root user.
        let record = RevocationRecord::issue(&root, &descriptor.network_id, &device.agent_id().0, 1, 1500);
        assert!(record.verify(&descriptor).is_ok());
        let outsider = Identity::generate();
        let forged = RevocationRecord::issue(&outsider, &descriptor.network_id, &device.agent_id().0, 1, 1500);
        assert_eq!(forged.verify(&descriptor), Err(MembershipError::UntrustedIssuer));
    }

    #[test]
    fn create_network_persists_a_verifiable_descriptor_and_device_cert() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let descriptor = mem.create_network("home").unwrap();

        // The UserID (root) is distinct from this install's DeviceID — never copied.
        let user = mem.user_identity().unwrap();
        let device = mem.identity().unwrap();
        assert_ne!(user.agent_id().0, device.agent_id().0, "root UserID ≠ DeviceID");
        assert!(descriptor.is_root(&user.agent_id().into()), "the founder is a root user");

        // Reload from disk and verify the device's membership chains to the network.
        let networks = mem.networks().unwrap();
        assert_eq!(networks.len(), 1);
        let cert = mem.device_membership(&descriptor.network_id).unwrap().expect("device cert");
        assert_eq!(cert.subject_id, device.agent_id().0);
        assert!(verify_membership(&cert, &descriptor, now_secs(), &RevocationSet::new()).is_ok());
    }

    #[test]
    fn a_certificate_for_another_network_is_rejected() {
        let (root, descriptor, device, _cert) = founded_network();
        let other = NetworkDescriptor::create(&root, "other-net", 9999);
        let cert_for_other = MembershipCertificate::issue(
            &root, &other.network_id, &device.agent_id().0, SubjectKind::Device, 1000, 24 * HOUR, 0, vec![],
        );
        assert_eq!(
            verify_membership(&cert_for_other, &descriptor, 2000, &RevocationSet::new()),
            Err(MembershipError::WrongNetwork),
        );
    }
}
