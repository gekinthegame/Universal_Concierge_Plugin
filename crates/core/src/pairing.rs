//! Phase N · Phase B — Secure Pairing.
//!
//! How a *new* device joins an existing network — only after explicit approval,
//! and receiving only the scopes granted (plan §Secure Enrollment and Pairing).
//! This module is the **protocol logic**: offers, proof-of-possession, the
//! short-authentication-string (SAS) confirmation, one-use consumption, and the
//! approval that issues a scoped grant. The encrypted channel itself is the
//! libp2p **Noise** transport (`crates/net`) — separate, and orthogonal to this
//! authentication logic.
//!
//! ## The flow
//! 1. An admin device mints a short-lived, **single-use** [`PairingOffer`] —
//!    rendezvous info + an ephemeral key + an unguessable id, **and no secrets**
//!    (no UserID/DeviceID secret, password, or plaintext capability; safety rule).
//! 2. The new device generates its own [`DeviceID`](crate::membership::DeviceId)
//!    locally and returns a [`PairingResponse`] **signing the handshake
//!    transcript** — proof it possesses its DeviceID key.
//! 3. Both sides derive the same [`confirmation_phrase`] from the transcript. A
//!    man-in-the-middle that swapped any key changes the transcript, so the two
//!    phrases differ and the humans **reject** — pairing fails closed.
//! 4. The user approves a name + scopes on the trusted admin device; [`approve`]
//!    issues a membership certificate + the exact capabilities approved.
//! 5. The offer is consumed atomically and cannot be replayed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::capability::{Capability, Namespace, Operation};
use crate::identity::{verify as verify_sig, AgentId, Identity};
use crate::membership::{MembershipCertificate, NetworkDescriptor, NetworkId, SubjectKind};

pub const PAIRING_OFFER_VERSION: u32 = 1;
pub const PAIRING_RESPONSE_VERSION: u32 = 1;

/// A single-use, short-lived enrollment offer. Public by design — it carries
/// **no** secret material, only rendezvous info and an ephemeral public key, so it
/// is safe to render as a QR code (plan §Pairing Safety Requirements).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    pub version: u32,
    /// Unguessable offer identifier (128 bits).
    pub offer_id: String,
    pub network_id: NetworkId,
    /// The admin device that minted the offer (for attribution).
    pub admin_device_id: String,
    /// An ephemeral public key bound into the handshake transcript (the SAS nonce;
    /// in the full transport this is the Noise ephemeral).
    pub admin_ephemeral: String,
    /// How to reach the admin (e.g. a multiaddr) — routing only.
    pub rendezvous: String,
    pub issued_at: u64,
    pub expires_at: u64,
    /// Signed by `admin_device_id`, so the new device knows the offer is genuine.
    pub signature: String,
}

impl PairingOffer {
    /// Mint and sign an offer. `ttl_secs` should be short (minutes). The ephemeral
    /// key is freshly generated; its secret is not retained here — the encrypted
    /// channel is the transport's concern, this binds the SAS.
    pub fn create(
        admin_device: &Identity,
        network_id: &NetworkId,
        rendezvous: &str,
        issued_at: u64,
        ttl_secs: u64,
    ) -> Self {
        let mut offer = Self {
            version: PAIRING_OFFER_VERSION,
            offer_id: random_hex(16),
            network_id: network_id.clone(),
            admin_device_id: admin_device.agent_id().0,
            admin_ephemeral: Identity::generate().agent_id().0,
            rendezvous: rendezvous.to_string(),
            issued_at,
            expires_at: issued_at.saturating_add(ttl_secs),
            signature: String::new(),
        };
        offer.signature = admin_device.sign(&offer.signing_bytes());
        offer
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("offer serializes")
    }

    /// Verify the offer is well-formed and signed by its claimed admin device.
    pub fn verify(&self) -> Result<(), PairingError> {
        if self.version != PAIRING_OFFER_VERSION {
            return Err(PairingError::Version);
        }
        if verify_sig(&AgentId(self.admin_device_id.clone()), &self.signing_bytes(), &self.signature)
            .map_err(PairingError::Malformed)?
        {
            Ok(())
        } else {
            Err(PairingError::BadOfferSignature)
        }
    }
}

/// The new device's reply: it proves possession of its DeviceID key by signing the
/// handshake transcript (plan step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResponse {
    pub version: u32,
    pub offer_id: String,
    /// The new device's freshly-generated DeviceID public key.
    pub device_id: String,
    pub device_ephemeral: String,
    /// `device_id`'s signature over the handshake transcript.
    pub signature: String,
}

impl PairingResponse {
    /// Build the response to `offer`: the new `device` signs the transcript with
    /// its DeviceID key. The device generates its keys locally (safety rule).
    pub fn create(offer: &PairingOffer, device: &Identity) -> Self {
        let device_ephemeral = Identity::generate().agent_id().0;
        let device_id = device.agent_id().0;
        let transcript = transcript_hash(offer, &device_id, &device_ephemeral);
        Self {
            version: PAIRING_RESPONSE_VERSION,
            offer_id: offer.offer_id.clone(),
            device_id,
            device_ephemeral,
            signature: device.sign(&transcript),
        }
    }
}

/// The handshake transcript hash: binds the offer and *both* parties' keys. A MITM
/// must substitute keys to intercept, which changes this hash — and thus the SAS.
fn transcript_hash(offer: &PairingOffer, device_id: &str, device_ephemeral: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(offer.offer_id.as_bytes());
    hasher.update(b"|");
    hasher.update(offer.admin_device_id.as_bytes());
    hasher.update(b"|");
    hasher.update(offer.admin_ephemeral.as_bytes());
    hasher.update(b"|");
    hasher.update(device_id.as_bytes());
    hasher.update(b"|");
    hasher.update(device_ephemeral.as_bytes());
    hasher.finalize().into()
}

/// The human-comparable **short authentication string**: a 6-digit code derived
/// from the handshake transcript. Both screens show it; the user pairs only if
/// they match (plan step 4 — fails closed on mismatch).
pub fn confirmation_phrase(offer: &PairingOffer, response: &PairingResponse) -> String {
    let h = transcript_hash(offer, &response.device_id, &response.device_ephemeral);
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{n:06}")
}

/// Verify a [`PairingResponse`] against its offer: the offer is genuine and
/// unexpired, the response matches it, and the device's transcript signature
/// proves possession of its DeviceID key. Does **not** consume the offer or grant
/// anything — approval is a separate, explicit user step.
pub fn verify_pairing_response(
    offer: &PairingOffer,
    response: &PairingResponse,
    now: u64,
) -> Result<(), PairingError> {
    offer.verify()?;
    if response.version != PAIRING_RESPONSE_VERSION {
        return Err(PairingError::Version);
    }
    if now >= offer.expires_at {
        return Err(PairingError::Expired);
    }
    if response.offer_id != offer.offer_id {
        return Err(PairingError::OfferMismatch);
    }
    let transcript = transcript_hash(offer, &response.device_id, &response.device_ephemeral);
    if verify_sig(&AgentId(response.device_id.clone()), &transcript, &response.signature)
        .map_err(PairingError::Malformed)?
    {
        Ok(())
    } else {
        Err(PairingError::BadProofOfPossession)
    }
}

/// What the new device receives on approval: a membership certificate plus exactly
/// the capabilities the user granted. Nothing broader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingGrant {
    pub descriptor: NetworkDescriptor,
    pub membership: MembershipCertificate,
    pub capabilities: Vec<Capability>,
}

/// Issue the approved grant. In Phase B the approving device holds the network's
/// root **UserID** (the single-user multi-device case: the founding device adds its
/// own second computer), so it signs the membership cert + capabilities directly as
/// a root — every grant chains to the descriptor. `scopes` is exactly what the user
/// approved; the new device gets no more.
pub fn approve(
    root_user: &Identity,
    descriptor: &NetworkDescriptor,
    response: &PairingResponse,
    scopes: &[(Namespace, Vec<Operation>)],
    now: u64,
    cert_ttl_secs: u64,
) -> PairingGrant {
    let membership = MembershipCertificate::issue(
        root_user,
        &descriptor.network_id,
        &response.device_id,
        SubjectKind::Device,
        now,
        cert_ttl_secs,
        descriptor.membership_epoch,
        scopes.iter().flat_map(|(_, ops)| ops.iter().map(op_name)).collect(),
    );
    let capabilities = scopes
        .iter()
        .map(|(namespace, operations)| {
            Capability::issue(
                root_user,
                namespace.clone(),
                &response.device_id,
                operations.clone(),
                now,
                cert_ttl_secs,
                descriptor.membership_epoch,
                false, // a paired device receives concrete scopes, not grant authority
            )
        })
        .collect();
    PairingGrant { descriptor: descriptor.clone(), membership, capabilities }
}

fn op_name(op: &Operation) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Why pairing failed. All variants fail closed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("unsupported pairing protocol version")]
    Version,
    #[error("the offer signature does not verify")]
    BadOfferSignature,
    #[error("the offer has expired")]
    Expired,
    #[error("the response does not match the offer")]
    OfferMismatch,
    #[error("proof-of-possession failed: the device did not sign the transcript")]
    BadProofOfPossession,
    #[error("the offer has already been used")]
    AlreadyConsumed,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

fn random_hex(n_bytes: usize) -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = vec![0u8; n_bytes];
    OsRng.fill_bytes(&mut bytes);
    use std::fmt::Write;
    let mut s = String::with_capacity(n_bytes * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

use std::path::PathBuf;

use crate::binding::{Cid, MemCli};
use crate::capability::verify_capability;
use crate::error::{Error, Result as CoreResult};
use crate::membership::{verify_membership, RevocationSet};

/// Default pairing-offer lifetime — short, per the safety rules.
pub const DEFAULT_OFFER_TTL_SECS: u64 = 10 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl MemCli {
    fn pairing_dir(&self) -> CoreResult<PathBuf> {
        Ok(self.security_dir()?.join("pairing"))
    }

    /// **Admin side.** Mint a single-use pairing offer for `network_id` and persist
    /// it as pending, so a later [`MemCli::complete_pairing`] can match + consume
    /// it. Signed by this install's DeviceID. Carries no secrets — safe as a QR.
    pub fn create_pairing_offer(&self, network_id: &NetworkId, rendezvous: &str) -> CoreResult<PairingOffer> {
        self.ensure_security_dir()?;
        let admin_device = self.identity()?;
        let offer = PairingOffer::create(&admin_device, network_id, rendezvous, now_secs(), DEFAULT_OFFER_TTL_SECS);
        let dir = self.pairing_dir()?.join("offers");
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(format!("create pairing dir: {e}")))?;
        write_json(&dir.join(format!("{}.json", offer.offer_id)), &offer)?;
        Ok(offer)
    }

    fn consumed_path(&self) -> CoreResult<PathBuf> {
        Ok(self.pairing_dir()?.join("consumed.json"))
    }

    fn consumed_offers(&self) -> CoreResult<std::collections::BTreeSet<String>> {
        match std::fs::read_to_string(self.consumed_path()?) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::Io(format!("parse consumed: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(Error::Io(format!("read consumed: {e}"))),
        }
    }

    /// **Admin side.** Verify a new device's response against the pending offer,
    /// then issue the approved scopes. **Atomically one-use**: the offer id is
    /// recorded consumed and the pending offer deleted, so a replay fails closed.
    /// Records a security event. The approving install must hold the network root
    /// UserID (the single-user multi-device case).
    pub fn complete_pairing(
        &self,
        response: &PairingResponse,
        scopes: &[(Namespace, Vec<Operation>)],
        cert_ttl_secs: u64,
    ) -> CoreResult<PairingGrant> {
        // One-use: refuse a replay before doing any work.
        if self.consumed_offers()?.contains(&response.offer_id) {
            return Err(Error::SecurityPolicy("pairing offer already used".to_string()));
        }
        let offer_path = self.pairing_dir()?.join("offers").join(format!("{}.json", response.offer_id));
        let offer: PairingOffer = match std::fs::read_to_string(&offer_path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::Io(format!("parse offer: {e}")))?,
            Err(_) => return Err(Error::SecurityPolicy("unknown or expired pairing offer".to_string())),
        };
        verify_pairing_response(&offer, response, now_secs())
            .map_err(|e| Error::SecurityPolicy(format!("pairing rejected: {e}")))?;

        let descriptor = self
            .networks()?
            .into_iter()
            .find(|n| n.network_id == offer.network_id)
            .ok_or_else(|| Error::SecurityPolicy("no such network on this device".to_string()))?;
        let root_user = self.user_identity()?;
        if !descriptor.is_root(&root_user.agent_id().into()) {
            return Err(Error::SecurityPolicy(
                "this device does not hold the network root key to approve pairing".to_string(),
            ));
        }
        let grant = approve(&root_user, &descriptor, response, scopes, now_secs(), cert_ttl_secs);

        // Consume atomically: record id, then drop the pending offer.
        let mut consumed = self.consumed_offers()?;
        consumed.insert(response.offer_id.clone());
        write_json(&self.consumed_path()?, &consumed)?;
        let _ = std::fs::remove_file(&offer_path);
        let _ = self.append_security_event_unlocked(
            "device_paired",
            &Cid(response.device_id.clone()),
            &format!("network={} scopes={}", offer.network_id.0, grant.capabilities.len()),
        );
        Ok(grant)
    }

    /// **New-device side.** Verify and persist an approved [`PairingGrant`]: the
    /// descriptor, this device's membership certificate, and the granted
    /// capabilities. After this the device is a verifiable member holding exactly
    /// the approved scopes.
    pub fn accept_pairing_grant(&self, grant: &PairingGrant) -> CoreResult<()> {
        self.ensure_security_dir()?;
        let now = now_secs();
        let revoked = RevocationSet::new();
        grant.descriptor.verify().map_err(|e| Error::SecurityPolicy(format!("bad descriptor: {e}")))?;
        verify_membership(&grant.membership, &grant.descriptor, now, &revoked)
            .map_err(|e| Error::SecurityPolicy(format!("bad membership: {e}")))?;
        let this_device = self.identity()?.agent_id().0;
        if grant.membership.subject_id != this_device {
            return Err(Error::SecurityPolicy("grant is for a different device".to_string()));
        }
        for cap in &grant.capabilities {
            verify_capability(cap, &[], &grant.descriptor, now, &revoked)
                .map_err(|e| Error::SecurityPolicy(format!("bad capability: {e}")))?;
        }
        let dir = self.security_dir()?.join("networks");
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(format!("create networks dir: {e}")))?;
        let id = &grant.descriptor.network_id.0;
        write_json(&dir.join(format!("{id}.descriptor.json")), &grant.descriptor)?;
        write_json(&dir.join(format!("{id}.device-cert.json")), &grant.membership)?;
        write_json(&dir.join(format!("{id}.capabilities.json")), &grant.capabilities)?;
        Ok(())
    }

    /// This device's granted capabilities for `network_id`, if any.
    pub fn device_capabilities(&self, network_id: &NetworkId) -> CoreResult<Vec<Capability>> {
        let path = self.security_dir()?.join("networks").join(format!("{}.capabilities.json", network_id.0));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::Io(format!("parse capabilities: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(Error::Io(format!("read capabilities: {e}"))),
        }
    }
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> CoreResult<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::Io(format!("serialize: {e}")))?;
    std::fs::write(path, text).map_err(|e| Error::Io(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{verify_capability, NamespaceScope};
    use crate::membership::{verify_membership, RevocationSet};

    const MIN: u64 = 60;

    /// (root user, descriptor, admin device) — the founding device holds the root.
    fn founder() -> (Identity, NetworkDescriptor, Identity) {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "home", 1000);
        let admin_device = Identity::generate();
        (root, descriptor, admin_device)
    }

    #[test]
    fn a_clean_pairing_produces_matching_confirmation_phrases() {
        let (_root, descriptor, admin) = founder();
        let offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        let new_device = Identity::generate();
        let response = PairingResponse::create(&offer, &new_device);

        assert!(verify_pairing_response(&offer, &response, 1000 + MIN).is_ok());
        // Both endpoints derive the SAS from the same transcript → identical phrase.
        let admin_view = confirmation_phrase(&offer, &response);
        let device_view = confirmation_phrase(&offer, &response);
        assert_eq!(admin_view, device_view);
        assert_eq!(admin_view.len(), 6, "a 6-digit human code");
    }

    #[test]
    fn a_man_in_the_middle_who_swaps_a_key_changes_the_confirmation_phrase() {
        let (_root, descriptor, admin) = founder();
        let offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        let honest_device = Identity::generate();
        let honest = PairingResponse::create(&offer, &honest_device);

        // A MITM substitutes its own device key on the wire.
        let mitm_device = Identity::generate();
        let mitm = PairingResponse::create(&offer, &mitm_device);

        // The admin (who sees the MITM's key) and the honest device (who knows its
        // own key) compute DIFFERENT phrases → the humans reject. Fails closed.
        assert_ne!(
            confirmation_phrase(&offer, &honest),
            confirmation_phrase(&offer, &mitm),
            "a swapped key must change the SAS"
        );
    }

    #[test]
    fn proof_of_possession_rejects_a_forged_device_signature() {
        let (_root, descriptor, admin) = founder();
        let offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        let device = Identity::generate();
        let mut response = PairingResponse::create(&offer, &device);
        // Claim a different device id than the one that signed.
        response.device_id = Identity::generate().agent_id().0;
        assert_eq!(
            verify_pairing_response(&offer, &response, 1000 + MIN),
            Err(PairingError::BadProofOfPossession),
        );
    }

    #[test]
    fn an_expired_offer_is_rejected() {
        let (_root, descriptor, admin) = founder();
        let offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        let device = Identity::generate();
        let response = PairingResponse::create(&offer, &device);
        assert_eq!(
            verify_pairing_response(&offer, &response, 1000 + 6 * MIN),
            Err(PairingError::Expired),
        );
    }

    #[test]
    fn a_tampered_offer_signature_is_rejected() {
        let (_root, descriptor, admin) = founder();
        let mut offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        offer.rendezvous = "/ip4/evil".to_string(); // tamper after signing
        assert_eq!(offer.verify(), Err(PairingError::BadOfferSignature));
    }

    #[test]
    fn two_devices_pair_end_to_end_and_the_offer_is_strictly_one_use() {
        // Device A: the founding device (holds the root UserID + the network).
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        let descriptor = mem_a.create_network("home").unwrap();

        // Device B: a fresh install with its own distinct DeviceID.
        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        assert_ne!(mem_a.identity().unwrap().agent_id().0, mem_b.identity().unwrap().agent_id().0);

        // A mints an offer; B responds proving possession of its key.
        let offer = mem_a.create_pairing_offer(&descriptor.network_id, "/ip4/127.0.0.1/tcp/4001").unwrap();
        let response = PairingResponse::create(&offer, &mem_b.identity().unwrap());

        // A approves read-only on one project; B accepts and is now a member.
        let project = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let grant = mem_a
            .complete_pairing(&response, &[(project.clone(), vec![Operation::SyncRead])], 30 * 24 * 3600)
            .unwrap();
        mem_b.accept_pairing_grant(&grant).unwrap();

        let cert = mem_b.device_membership(&descriptor.network_id).unwrap().expect("B holds membership");
        assert_eq!(cert.subject_id, mem_b.identity().unwrap().agent_id().0);
        assert!(verify_membership(&cert, &descriptor, now_secs(), &RevocationSet::new()).is_ok());
        let caps = mem_b.device_capabilities(&descriptor.network_id).unwrap();
        assert_eq!(caps.len(), 1);
        assert!(caps[0].authorizes(Operation::SyncRead, &project));

        // One-use: replaying the same response fails closed.
        assert!(
            mem_a.complete_pairing(&response, &[(project, vec![Operation::SyncRead])], 30 * 24 * 3600).is_err(),
            "a consumed offer cannot be reused",
        );
    }

    #[test]
    fn the_exit_criterion_a_paired_device_gets_exactly_the_approved_scopes() {
        // The Phase B exit criterion: a new device joins only after approval and can
        // access only the exact scopes granted.
        let (root, descriptor, admin) = founder();
        let offer = PairingOffer::create(&admin, &descriptor.network_id, "/ip4/127.0.0.1", 1000, 5 * MIN);
        let new_device = Identity::generate();
        let response = PairingResponse::create(&offer, &new_device);
        verify_pairing_response(&offer, &response, 1000 + MIN).expect("clean handshake");

        // The user approves read-only access to ONE project namespace.
        let project = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let grant = approve(
            &root,
            &descriptor,
            &response,
            &[(project.clone(), vec![Operation::SyncRead])],
            2000,
            30 * 24 * 3600,
        );

        let now = 3000;
        let revoked = RevocationSet::new();
        // The new device is a verifiable member…
        assert!(verify_membership(&grant.membership, &descriptor, now, &revoked).is_ok());
        assert_eq!(grant.membership.subject_id, new_device.agent_id().0);
        // …holding exactly the granted capability…
        assert_eq!(grant.capabilities.len(), 1);
        let cap = &grant.capabilities[0];
        assert!(verify_capability(cap, &[], &descriptor, now, &revoked).is_ok());
        assert!(cap.authorizes(Operation::SyncRead, &project), "granted: read on atlas");
        // …and NOTHING more: not write, not another namespace.
        assert!(!cap.authorizes(Operation::SyncWrite, &project), "no write was granted");
        let other = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("secret".into()));
        assert!(!cap.authorizes(Operation::SyncRead, &other), "no access to other namespaces");
        // A paired device cannot re-delegate.
        assert!(!cap.delegable, "a paired device holds concrete scopes, not grant authority");
    }
}
