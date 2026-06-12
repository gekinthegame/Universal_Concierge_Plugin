//! The Cryptree edge: a link from one symmetric key to another, where the
//! target key is encrypted under the source key. Port of the Cryptree
//! `SymmetricLink` primitive — the whole access mechanism in one tiny type.

use crate::error::Result;
use crate::secretbox::{CipherText, SymmetricKey};

/// `to` encrypted under `from`. Holding `from` recovers `to`; without `from` it
/// is opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymmetricLink(CipherText);

impl SymmetricLink {
    /// Encrypt key `to` under key `from`.
    pub fn wrap(from: &SymmetricKey, to: &SymmetricKey) -> Self {
        Self(from.seal(&to.to_bytes()))
    }

    /// Recover the wrapped key using `from`.
    pub fn unwrap(&self, from: &SymmetricKey) -> Result<SymmetricKey> {
        let bytes = from.open(&self.0)?;
        let array: [u8; crate::secretbox::KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| crate::error::CryptoError::MalformedCipherText)?;
        Ok(SymmetricKey::from_bytes(array))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_bytes()
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        Ok(Self(CipherText::from_bytes(raw)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CryptoError;

    #[test]
    fn wrap_then_unwrap_recovers_the_target_key() {
        let parent = SymmetricKey::random();
        let child = SymmetricKey::random();
        let link = SymmetricLink::wrap(&parent, &child);
        assert_eq!(link.unwrap(&parent).unwrap().to_bytes(), child.to_bytes());
    }

    #[test]
    fn unwrap_with_the_wrong_source_key_fails() {
        let parent = SymmetricKey::random();
        let child = SymmetricKey::random();
        let link = SymmetricLink::wrap(&parent, &child);
        let attacker = SymmetricKey::random();
        assert!(matches!(link.unwrap(&attacker), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn link_bytes_roundtrip() {
        let parent = SymmetricKey::random();
        let child = SymmetricKey::random();
        let link = SymmetricLink::wrap(&parent, &child);
        let restored = SymmetricLink::from_bytes(&link.to_bytes()).unwrap();
        assert_eq!(
            restored.unwrap(&parent).unwrap().to_bytes(),
            child.to_bytes()
        );
    }
}
