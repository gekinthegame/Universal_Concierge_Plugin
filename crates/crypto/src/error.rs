//! The crate's typed error model.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    /// AEAD authentication failed: wrong key or tampered ciphertext. This is the
    /// load-bearing "you do not hold the capability" signal.
    #[error("decryption failed (wrong key or corrupt ciphertext)")]
    Decrypt,

    /// A stored ciphertext block was too short or malformed to parse.
    #[error("malformed ciphertext block")]
    MalformedCipherText,

    /// Ciphertext bytes did not match the content address carried by the
    /// capability. Decryption must never accept substituted block bytes.
    #[error("ciphertext CID mismatch")]
    CidMismatch,

    /// A capability did not grant the requested access (e.g. write on a
    /// read-only capability).
    #[error("capability does not grant the requested access")]
    AccessDenied,

    /// A referenced block CID was not available to the reader.
    #[error("encrypted block not found: {0}")]
    BlockNotFound(String),

    /// (De)serialization of a plaintext node or capability failed.
    #[error("serialization error: {0}")]
    Serialize(String),

    /// The capability vault could not be derived, sealed, or opened.
    #[error("capability vault error: {0}")]
    Vault(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;
