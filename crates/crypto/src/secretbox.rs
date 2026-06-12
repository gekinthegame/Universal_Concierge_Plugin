//! The AEAD primitive: a 32-byte symmetric key and a {nonce, ciphertext}
//! envelope. Port of the Cryptree `TweetNaClKey` + `CipherText` primitives, on
//! XChaCha20-Poly1305 instead of NaCl secretbox.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};
use zeroize::Zeroize;

use crate::error::{CryptoError, Result};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// A symmetric read/write key. Zeroized on drop so spent key material does not
/// linger in memory.
#[derive(Clone)]
pub struct SymmetricKey([u8; KEY_LEN]);

impl SymmetricKey {
    /// A fresh random key from the OS CSPRNG.
    pub fn random() -> Self {
        let mut bytes = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Copy out the raw key bytes (e.g. to wrap into a child link or vault).
    pub fn to_bytes(&self) -> [u8; KEY_LEN] {
        self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    /// Encrypt `plaintext` under this key with a fresh random 24-byte nonce.
    pub(crate) fn seal(&self, plaintext: &[u8]) -> CipherText {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let bytes = self
            .cipher()
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            // XChaCha20-Poly1305 encryption only fails on absurd (4GiB+) inputs.
            .expect("AEAD encryption of an in-memory buffer");
        CipherText { nonce, bytes }
    }

    /// Decrypt, returning [`CryptoError::Decrypt`] when the key is wrong or the
    /// ciphertext was tampered with (AEAD authentication failure).
    pub fn open(&self, ciphertext: &CipherText) -> Result<Vec<u8>> {
        self.cipher()
            .decrypt(
                XNonce::from_slice(&ciphertext.nonce),
                ciphertext.bytes.as_slice(),
            )
            .map_err(|_| CryptoError::Decrypt)
    }
}

impl Drop for SymmetricKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for SymmetricKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        formatter.write_str("SymmetricKey(redacted)")
    }
}

/// A nonce + AEAD ciphertext (which includes the Poly1305 tag). The canonical
/// on-disk block form is `nonce || ciphertext` (see [`CipherText::to_bytes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CipherText {
    pub nonce: [u8; NONCE_LEN],
    pub bytes: Vec<u8>,
}

impl CipherText {
    /// Canonical block bytes: `nonce || ciphertext`. What a content address
    /// hashes over, so the CID identifies ciphertext.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(NONCE_LEN + self.bytes.len());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.bytes);
        out
    }

    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        if raw.len() < NONCE_LEN {
            return Err(CryptoError::MalformedCipherText);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&raw[..NONCE_LEN]);
        Ok(Self {
            nonce,
            bytes: raw[NONCE_LEN..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_succeeds_and_wrong_key_fails() {
        let key = SymmetricKey::random();
        let ciphertext = key.seal(b"the wetlands plan");
        assert_eq!(key.open(&ciphertext).unwrap(), b"the wetlands plan");

        let other = SymmetricKey::random();
        assert!(matches!(other.open(&ciphertext), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn ciphertext_does_not_contain_the_plaintext() {
        let key = SymmetricKey::random();
        let ciphertext = key.seal(b"SECRET-MARKER-12345");
        assert!(!ciphertext
            .to_bytes()
            .windows(b"SECRET-MARKER-12345".len())
            .any(|window| window == b"SECRET-MARKER-12345"));
    }

    #[test]
    fn fresh_nonce_per_seal_diversifies_ciphertext() {
        let key = SymmetricKey::random();
        let one = key.seal(b"same plaintext");
        let two = key.seal(b"same plaintext");
        assert_ne!(one.to_bytes(), two.to_bytes(), "nonce reuse would be a bug");
        assert_eq!(key.open(&one).unwrap(), key.open(&two).unwrap());
    }

    #[test]
    fn block_bytes_roundtrip_and_reject_truncation() {
        let key = SymmetricKey::random();
        let ciphertext = key.seal(b"x");
        let raw = ciphertext.to_bytes();
        assert_eq!(CipherText::from_bytes(&raw).unwrap(), ciphertext);
        assert!(matches!(
            CipherText::from_bytes(&raw[..NONCE_LEN - 1]),
            Err(CryptoError::MalformedCipherText)
        ));
    }

    #[test]
    fn tampering_with_ciphertext_is_detected() {
        let key = SymmetricKey::random();
        let mut ciphertext = key.seal(b"authentic");
        let last = ciphertext.bytes.len() - 1;
        ciphertext.bytes[last] ^= 0x01;
        assert!(matches!(key.open(&ciphertext), Err(CryptoError::Decrypt)));
    }
}
