//! Sovereign naming substrate — **Layers 1 + 2** (petnames + signed contact cards).
//!
//! No registrar, no chain, no rent. Naming is local-first and trust-rooted: a
//! **petname** ([`crate::social`]) is your private, absolute name for an AgentID,
//! and a **[`ContactCard`]** is a self-asserted, self-authenticating profile a peer
//! signs with their own key. Petnames always win over asserted names — the
//! anti-spoofing rule (Zooko's Triangle: we take human-readable + secure, and give
//! up global-uniqueness on purpose).
//!
//! Identity is the [`AgentId`] (an Ed25519 public key). On the wire we encode it as
//! a W3C **`did:key`** (`did:key:z…`) so a card is self-describing and standards-
//! interoperable — a pure encoding of the existing key, no new dependency. This
//! identity model follows the guidance in `3mail-main` (DID + self-asserted profile
//! + consent-gated exchange), minus its Ceramic / Ethereum / SMTP / ENS-registry
//!   parts (Decisions 0012 / 0022 / 0024: no token economy, no global registry).

use serde::{Deserialize, Serialize};

use crate::identity::{self, AgentId, Identity};

// ── did:key (multicodec ed25519-pub + base58btc, multibase 'z') ──────────────

/// Multicodec prefix for an Ed25519 public key (`0xed`, varint-encoded `ed 01`).
const ED25519_PUB_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Encode an [`AgentId`] (hex Ed25519 pubkey) as a `did:key:z…`.
pub fn did_key_from_agent(agent: &AgentId) -> Result<String, String> {
    let pk = hex_to_bytes(&agent.0)?;
    if pk.len() != 32 {
        return Err("agent id must be a 32-byte ed25519 key".to_string());
    }
    let mut buf = ED25519_PUB_MULTICODEC.to_vec();
    buf.extend_from_slice(&pk);
    Ok(format!("did:key:z{}", base58btc_encode(&buf)))
}

/// Recover the [`AgentId`] from a `did:key:z…` (rejects non-ed25519 dids).
pub fn agent_id_from_did(did: &str) -> Result<AgentId, String> {
    let body = did
        .strip_prefix("did:key:z")
        .ok_or_else(|| "not a did:key:z… identifier".to_string())?;
    let buf = base58btc_decode(body)?;
    if buf.len() != 34 || buf[0] != ED25519_PUB_MULTICODEC[0] || buf[1] != ED25519_PUB_MULTICODEC[1]
    {
        return Err("not an ed25519 did:key".to_string());
    }
    Ok(AgentId(bytes_to_hex(&buf[2..])))
}

// ── Layer 2: the Contact Card ────────────────────────────────────────────────

/// A self-asserted, self-authenticating profile a peer publishes about themselves.
/// The `sig` verifies against the key in `did`, so a card is provably *from* that
/// AgentID — but the human `display_name` inside it is only a **hint** until the
/// receiver pins it with a petname (TOFU).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactCard {
    /// The author's identity as a `did:key:z…` (= their AgentID).
    pub did: String,
    /// Self-asserted display name (a hint until petnamed).
    pub display_name: String,
    /// Optional small avatar (data-uri or CID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    /// Optional stable IPNS of the author's Studio site (Layer-2 web naming).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_ipns: Option<String>,
    /// Rooms the author advertises.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<String>,
    pub updated_at: u64,
    /// Detached Ed25519 signature (hex) over the card's canonical bytes.
    #[serde(default)]
    pub sig: String,
}

impl ContactCard {
    /// A new unsigned card for `agent`. Call [`ContactCard::sign`] before sharing.
    pub fn new(agent: &AgentId, display_name: &str, updated_at: u64) -> Result<Self, String> {
        Ok(Self {
            did: did_key_from_agent(agent)?,
            display_name: display_name.to_string(),
            updated_at,
            ..Default::default()
        })
    }

    /// Canonical bytes the signature covers (everything but `sig`).
    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.sig = String::new();
        serde_json::to_vec(&clone).unwrap_or_default()
    }

    /// Sign with the author's identity (sets `sig`).
    pub fn sign(&mut self, id: &Identity) {
        self.sig = id.sign(&self.signing_bytes());
    }

    /// The author's AgentID (decoded from `did`).
    pub fn agent_id(&self) -> Result<AgentId, String> {
        agent_id_from_did(&self.did)
    }

    /// Is the signature valid for the key in `did`? (self-authenticating)
    pub fn verify(&self) -> bool {
        match self.agent_id() {
            Ok(aid) => identity::verify(&aid, &self.signing_bytes(), &self.sig).unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// A signed vouch: *I* (the introducer) assert this `subject` goes by `asserted_name`.
/// Lets names propagate through people you already trust — the registry-free path to
/// reach. Still petname-gated on the receiving side.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    /// did:key of the introducer.
    pub from: String,
    /// did:key of the person being named.
    pub subject_did: String,
    pub asserted_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_ipns: Option<String>,
    pub updated_at: u64,
    #[serde(default)]
    pub sig: String,
}

impl Introduction {
    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.sig = String::new();
        serde_json::to_vec(&clone).unwrap_or_default()
    }
    pub fn sign(&mut self, id: &Identity) {
        self.sig = id.sign(&self.signing_bytes());
    }
    /// Is this signed by the `from` introducer?
    pub fn verify(&self) -> bool {
        match agent_id_from_did(&self.from) {
            Ok(aid) => identity::verify(&aid, &self.signing_bytes(), &self.sig).unwrap_or(false),
            Err(_) => false,
        }
    }
    pub fn subject_agent(&self) -> Result<AgentId, String> {
        agent_id_from_did(&self.subject_did)
    }
}

// ── Resolution model ─────────────────────────────────────────────────────────

/// Where a resolved display name came from — drives how the UI trusts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameSource {
    /// A petname you set — pinned, absolute, trusted.
    Petname,
    /// A name an introducer you trust vouched for — a strong hint.
    Introduced,
    /// A self-asserted card name — a hint (TOFU), shown as unverified.
    Card,
    /// No name known; shows a shortened AgentID.
    Unknown,
}

/// A resolved display name + its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedName {
    pub text: String,
    pub source: NameSource,
    /// True only for [`NameSource::Petname`] — safe to treat as authoritative.
    pub verified: bool,
}

impl ResolvedName {
    pub fn new(text: impl Into<String>, source: NameSource) -> Self {
        Self {
            text: text.into(),
            source,
            verified: matches!(source, NameSource::Petname),
        }
    }
}

/// A short, stable label for an unknown AgentID (first 10 hex chars).
pub fn short_agent(agent_id: &str) -> String {
    let head: String = agent_id.chars().take(10).collect();
    format!("{head}…")
}

// ── tiny hex + base58btc (no extra deps) ─────────────────────────────────────

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58btc_encode(input: &[u8]) -> String {
    let zeros = input.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = vec![0];
    for &byte in input {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + digits.len());
    for _ in 0..zeros {
        out.push('1');
    }
    for &d in digits.iter().rev() {
        out.push(B58[d as usize] as char);
    }
    out
}

fn base58btc_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut bytes: Vec<u8> = vec![0];
    for ch in s.chars() {
        let val = B58
            .iter()
            .position(|&c| c == ch as u8)
            .ok_or_else(|| format!("invalid base58 character '{ch}'"))? as u32;
        let mut carry = val;
        for b in bytes.iter_mut() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = s.chars().take_while(|c| *c == '1').count();
    let mut out = vec![0u8; zeros];
    out.extend(bytes.iter().rev());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Identity {
        Identity::generate()
    }

    #[test]
    fn did_key_round_trips_with_the_agent_id() {
        let id = id();
        let aid = id.agent_id();
        let did = did_key_from_agent(&aid).expect("encode");
        assert!(did.starts_with("did:key:z"), "{did}");
        assert_eq!(agent_id_from_did(&did).expect("decode"), aid);
    }

    #[test]
    fn a_non_ed25519_did_is_rejected() {
        assert!(agent_id_from_did("did:web:example.com").is_err());
        assert!(agent_id_from_did("did:key:zNOTbase58!!").is_err());
    }

    #[test]
    fn a_signed_card_verifies_and_a_tampered_one_does_not() {
        let id = id();
        let mut card = ContactCard::new(&id.agent_id(), "Jason", 1700).expect("new");
        card.site_ipns = Some("k51example".into());
        card.sign(&id);
        assert!(card.verify(), "a freshly signed card verifies");
        assert_eq!(card.agent_id().unwrap(), id.agent_id());

        // tamper the asserted name → signature must fail
        let mut forged = card.clone();
        forged.display_name = "Mallory".into();
        assert!(!forged.verify(), "tampered display name must not verify");
    }

    #[test]
    fn a_card_signed_by_a_different_key_does_not_verify() {
        let alice = id();
        let mallory = id();
        let mut card = ContactCard::new(&alice.agent_id(), "Alice", 1).expect("new");
        card.sign(&mallory); // wrong signer
        assert!(!card.verify(), "card must be signed by the did's own key");
    }

    #[test]
    fn introductions_verify_against_the_introducer() {
        let alice = id();
        let bob_did = did_key_from_agent(&id().agent_id()).unwrap();
        let mut intro = Introduction {
            from: did_key_from_agent(&alice.agent_id()).unwrap(),
            subject_did: bob_did,
            asserted_name: "Bob".into(),
            updated_at: 5,
            ..Default::default()
        };
        intro.sign(&alice);
        assert!(intro.verify());
        let mut bad = intro.clone();
        bad.asserted_name = "Not Bob".into();
        assert!(!bad.verify());
    }

    #[test]
    fn petname_resolution_is_marked_verified() {
        assert!(ResolvedName::new("Mom", NameSource::Petname).verified);
        assert!(!ResolvedName::new("Jason", NameSource::Card).verified);
    }
}
