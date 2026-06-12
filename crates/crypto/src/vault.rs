//! The capability vault. Port of the Cryptree password-unlocked capability store:
//! the store password is stretched with **scrypt** into a vault key that seals
//! the set of capabilities at rest. The plan's rule (5): the password unlocks the
//! local capability vault — it does not directly encrypt every node.
//!
//! This crate is pure, so the vault here is a bytes-in/bytes-out primitive; the
//! core layer owns the on-disk file and the salt.

use crate::capability::{Capability, CapabilityWire};
use crate::error::{CryptoError, Result};
use crate::secretbox::{CipherText, SymmetricKey, KEY_LEN};
use zeroize::Zeroizing;

/// scrypt parameters for vault-key derivation (log_n=15, r=8, p=1) — the
/// Wuala Cryptree parameters, matching the store-password verifier in the core.
const VAULT_LOG_N: u8 = 15;
const VAULT_R: u32 = 8;
const VAULT_P: u32 = 1;

/// A key derived from the store password that seals/opens the capability vault.
pub struct VaultKey(SymmetricKey);

/// Derive the vault key from the password and a per-store random salt.
pub fn derive_vault_key(password: &str, salt: &[u8]) -> Result<VaultKey> {
    let params = scrypt::Params::new(VAULT_LOG_N, VAULT_R, VAULT_P, KEY_LEN)
        .map_err(|error| CryptoError::Vault(format!("scrypt params: {error}")))?;
    let mut derived = [0u8; KEY_LEN];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut derived)
        .map_err(|error| CryptoError::Vault(format!("scrypt derive: {error}")))?;
    Ok(VaultKey(SymmetricKey::from_bytes(derived)))
}

impl VaultKey {
    /// Seal a capability set into opaque vault bytes.
    pub fn seal(&self, capabilities: &[Capability]) -> Result<Vec<u8>> {
        let wire: Vec<CapabilityWire> = capabilities.iter().map(Capability::to_wire).collect();
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&wire).map_err(|error| CryptoError::Vault(error.to_string()))?,
        );
        Ok(self.0.seal(&plaintext).to_bytes())
    }

    /// Open vault bytes back into capabilities. Fails with
    /// [`CryptoError::Decrypt`] when the password (and thus the vault key) is
    /// wrong — the vault fails closed.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<Capability>> {
        let ciphertext = CipherText::from_bytes(sealed)?;
        let plaintext = Zeroizing::new(self.0.open(&ciphertext)?);
        let wire: Vec<CapabilityWire> = serde_json::from_slice(&plaintext)
            .map_err(|error| CryptoError::Vault(error.to_string()))?;
        Ok(wire.iter().map(Capability::from_wire).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Vec<Capability> {
        vec![
            Capability::read_write(
                "ctaaa".to_string(),
                SymmetricKey::random(),
                SymmetricKey::random(),
            ),
            Capability::read_only("ctbbb".to_string(), SymmetricKey::random()),
        ]
    }

    #[test]
    fn vault_seals_and_opens_with_the_right_password() {
        let salt = b"per-store-salt-16";
        let key = derive_vault_key("correct password", salt).unwrap();
        let original = caps();
        let sealed = key.seal(&original).unwrap();

        let reopened = key.open(&sealed).unwrap();
        assert_eq!(reopened.len(), 2);
        assert_eq!(
            reopened[0].read_key.to_bytes(),
            original[0].read_key.to_bytes()
        );
        assert!(reopened[0].is_writable());
        assert!(!reopened[1].is_writable());
    }

    #[test]
    fn vault_fails_closed_with_the_wrong_password() {
        let salt = b"per-store-salt-16";
        let sealed = derive_vault_key("correct password", salt)
            .unwrap()
            .seal(&caps())
            .unwrap();
        let wrong = derive_vault_key("wrong password", salt).unwrap();
        assert!(matches!(wrong.open(&sealed), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn vault_bytes_are_opaque() {
        let salt = b"per-store-salt-16";
        let sealed = derive_vault_key("pw", salt).unwrap().seal(&caps()).unwrap();
        // The cids inside are not visible in the sealed bytes.
        assert!(!sealed.windows(5).any(|window| window == b"ctaaa"));
    }
}
