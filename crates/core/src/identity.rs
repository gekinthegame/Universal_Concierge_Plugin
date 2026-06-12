//! Phase 5.5 — Social identity (AgentID).
//!
//! Every install holds a stable **AgentID**: an Ed25519 keypair generated once,
//! persisted *outside the DAG* (`.concierge/identity.key`), and reused on every
//! start — so a node that shuts down and comes back up tomorrow is the *same*
//! social participant (Decision 0007). The public key is the AgentID; the secret
//! never leaves the keystore.
//!
//! Sharing is **signed**: a share signs the root CID with the AgentID, so a
//! recipient verifies *who* shared it (authenticity) on top of *what* the CID
//! proves (integrity). Verification rejects tampered, wrong-signer, and
//! unknown-signer shares.
//!
//! Crypto suite per Decision 0011: **Ed25519** via `ed25519-dalek`.

use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

/// A stable social identity: the hex-encoded Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(pub String);

/// The local identity — wraps the secret signing key. Created once, then loaded.
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Load the keypair from `key_path`, or generate and persist a new one. The
    /// persistence is what makes the AgentID stable across restarts; the file is
    /// kept private (0600 on unix) and never enters the DAG.
    pub fn load_or_create(key_path: &Path) -> Result<Self, String> {
        if key_path.exists() {
            let hex =
                std::fs::read_to_string(key_path).map_err(|e| format!("read identity key: {e}"))?;
            let bytes = from_hex(hex.trim()).map_err(|e| format!("identity key hex: {e}"))?;
            let secret: [u8; 32] = bytes
                .try_into()
                .map_err(|_| "identity key must be 32 bytes".to_string())?;
            Ok(Self {
                signing: SigningKey::from_bytes(&secret),
            })
        } else {
            let signing = SigningKey::generate(&mut OsRng);
            if let Some(parent) = key_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("identity key dir: {e}"))?;
            }
            std::fs::write(key_path, to_hex(&signing.to_bytes()))
                .map_err(|e| format!("write identity key: {e}"))?;
            set_private_perms(key_path);
            Ok(Self { signing })
        }
    }

    /// Generate a fresh in-memory keypair with no file backing. Used for
    /// identities that are not the persisted install key — actor/agent keys
    /// (Phase N), ephemeral pairing keys, and tests. Persist via the security
    /// vault separately if the identity must survive a restart.
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Persist this identity's secret to `key_path` (owner-only), so a later
    /// [`Identity::load_or_create`] loads *this* key. Used by UserID rotation to
    /// install a new active key at the existing path.
    pub fn save(&self, key_path: &Path) -> Result<(), String> {
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("identity key dir: {e}"))?;
        }
        std::fs::write(key_path, to_hex(&self.signing.to_bytes()))
            .map_err(|e| format!("write identity key: {e}"))?;
        set_private_perms(key_path);
        Ok(())
    }

    /// The public AgentID for this install.
    pub fn agent_id(&self) -> AgentId {
        AgentId(to_hex(self.signing.verifying_key().as_bytes()))
    }

    /// Sign a message, returning a hex-encoded detached signature.
    pub fn sign(&self, msg: &[u8]) -> String {
        to_hex(&self.signing.sign(msg).to_bytes())
    }

    /// The 32-byte Ed25519 secret. Used to derive the libp2p PeerID for the
    /// transport, so the network identity and the AgentID share one key
    /// (Decision 0007). Stays in-process — never serialized.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }
}

/// Verify a detached signature: does `sig_hex` over `msg` come from `agent_id`?
/// Returns `Ok(false)` for a bad signature, and `Err` for malformed inputs
/// (so callers can distinguish "wrong signer" from "garbage").
pub fn verify(agent_id: &AgentId, msg: &[u8], sig_hex: &str) -> Result<bool, String> {
    let pk = from_hex(&agent_id.0).map_err(|e| format!("agent id hex: {e}"))?;
    let pk: [u8; 32] = pk
        .try_into()
        .map_err(|_| "agent id must be 32 bytes".to_string())?;
    let vk =
        VerifyingKey::from_bytes(&pk).map_err(|e| format!("agent id is not a valid key: {e}"))?;
    let sig = from_hex(sig_hex).map_err(|e| format!("signature hex: {e}"))?;
    let sig: [u8; 64] = sig
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    Ok(vk.verify(msg, &Signature::from_bytes(&sig)).is_ok())
}

#[cfg(unix)]
fn set_private_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_perms(_path: &Path) {}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_key(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "concierge-id-{tag}-{}-{nanos}.key",
            std::process::id()
        ))
    }

    #[test]
    fn agent_id_is_stable_across_reload() {
        let path = temp_key("stable");
        let first = Identity::load_or_create(&path).expect("create").agent_id();
        // "Restart": load the same persisted key again.
        let second = Identity::load_or_create(&path).expect("reload").agent_id();
        assert_eq!(first, second, "the AgentID must survive a restart");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let path = temp_key("sign");
        let id = Identity::load_or_create(&path).expect("create");
        let msg = b"bafyROOT";
        let sig = id.sign(msg);
        assert!(
            verify(&id.agent_id(), msg, &sig).expect("verify"),
            "a valid signature verifies"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tampered_message_fails_verification() {
        let path = temp_key("tamper");
        let id = Identity::load_or_create(&path).expect("create");
        let sig = id.sign(b"original");
        assert!(
            !verify(&id.agent_id(), b"tampered", &sig).expect("verify"),
            "a different message must not verify"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_signer_fails_verification() {
        let path_a = temp_key("signer-a");
        let path_b = temp_key("signer-b");
        let a = Identity::load_or_create(&path_a).expect("a");
        let b = Identity::load_or_create(&path_b).expect("b");
        let msg = b"shared root";
        let sig_from_b = b.sign(msg);
        // A's signature over the same message would verify; B's must not, against A.
        assert!(
            !verify(&a.agent_id(), msg, &sig_from_b).expect("verify"),
            "another agent's signature must not verify"
        );
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }
}
