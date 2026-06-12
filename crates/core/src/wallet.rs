//! Wallet attestation + settings (Pillar C, Decision 0033). The browser (Brave/
//! Opera) custodies keys and signs; **we hold no keys**. This module only *verifies*
//! a signature (so a linked address is cryptographically tied to the AgentID) and
//! holds the on-device wallet preferences. The Concierge owns the wallet *UX*; the
//! browser owns the wallet.

use serde::{Deserialize, Serialize};

/// A verified link: "this external address attests to controlling this AgentID".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletLink {
    /// Lowercased `0x…` (EVM) or base58 (future: Solana) address.
    pub address: String,
    /// `"evm"` for now.
    pub chain: String,
    /// The AgentID the wallet signed.
    pub agent_id: String,
    /// The signature (hex for EVM).
    pub signature: String,
    pub linked_at: u64,
}

/// On-device wallet preferences — what the Concierge wallet UX remembers. None of
/// this is secret (no keys), but it gates the future agent-propose tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletSettings {
    /// May the host AI *propose* transactions? Off by default (Brave still confirms).
    pub agent_access: bool,
    /// Per-transaction spend cap the agent may propose, as a human string (e.g. "0.05").
    pub spend_cap: String,
    /// Addresses the agent is allowed to send to (empty = none).
    pub allowlist: Vec<String>,
    /// Preferred EVM chain id (hex, e.g. "0x1" = Ethereum mainnet).
    pub preferred_chain: String,
}

impl Default for WalletSettings {
    fn default() -> Self {
        Self {
            agent_access: false,
            spend_cap: String::new(),
            allowlist: Vec::new(),
            preferred_chain: String::new(),
        }
    }
}

/// The persisted wallet state (`<store>/wallet.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WalletState {
    #[serde(default)]
    pub links: Vec<WalletLink>,
    #[serde(default)]
    pub settings: WalletSettings,
}

/// The exact message a wallet signs to link itself to an AgentID. Both the GUI
/// (which asks the browser wallet to sign) and the verifier must use this string.
pub fn link_message(agent_id: &str) -> String {
    format!("Link this wallet to my Concierge identity (AgentID): {agent_id}")
}

// ── EVM `personal_sign` (EIP-191) recovery ───────────────────────────────────

/// Recover the signer address from an EVM `personal_sign` over `message`. Returns
/// the lowercased `0x…` address. This is how we verify a `WalletLink`: the browser
/// wallet signs the AgentID, and we confirm the recovered address matches.
pub fn recover_eth_personal_sign(message: &str, signature_hex: &str) -> Result<String, String> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    use sha3::{Digest, Keccak256};

    let raw = hex_decode(signature_hex.trim().trim_start_matches("0x"))?;
    if raw.len() != 65 {
        return Err(format!("signature must be 65 bytes, got {}", raw.len()));
    }
    let v = raw[64];
    let rec = if v >= 27 { v - 27 } else { v };
    let recovery_id = RecoveryId::from_byte(rec).ok_or("invalid recovery id")?;
    let signature = Signature::from_slice(&raw[..64]).map_err(|e| format!("invalid signature: {e}"))?;

    // EIP-191 personal-sign prefix, then Keccak-256.
    let prefixed = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
    let digest = Keccak256::new_with_prefix(prefixed.as_bytes());
    let key = VerifyingKey::recover_from_digest(digest, &signature, recovery_id)
        .map_err(|e| format!("could not recover signer: {e}"))?;

    // address = last 20 bytes of Keccak-256(uncompressed pubkey without the 0x04 tag).
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    Ok(format!("0x{}", hex_encode(&hash[12..])))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("hex must have an even length".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("bad hex: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{SigningKey, RecoveryId, Signature};
    use sha3::{Digest, Keccak256};

    fn address_of(key: &SigningKey) -> String {
        let vk = key.verifying_key();
        let point = vk.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        format!("0x{}", hex_encode(&hash[12..]))
    }

    #[test]
    fn recovers_the_signer_of_a_personal_sign() {
        // Self-consistent: sign the EIP-191 digest of a message, then recover it.
        let key = SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let message = "deadbeefAGENTID";
        let prefixed = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
        let digest = Keccak256::new_with_prefix(prefixed.as_bytes());
        let (sig, recid): (Signature, RecoveryId) =
            key.sign_digest_recoverable(digest).unwrap();
        let mut raw = sig.to_bytes().to_vec();
        raw.push(recid.to_byte() + 27);
        let recovered = recover_eth_personal_sign(message, &hex_encode(&raw)).unwrap();
        assert_eq!(recovered, address_of(&key));
    }

    #[test]
    fn rejects_a_malformed_signature() {
        assert!(recover_eth_personal_sign("hi", "0x1234").is_err());
    }
}
