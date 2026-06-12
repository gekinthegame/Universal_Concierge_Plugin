//! Phase E - Cryptree capability encryption (Decision 0011).
//!
//! A faithful port of the Wuala Cryptree design (see
//! `CRYPTREE_PORT_SPEC.md`) onto proven RustCrypto primitives. The one idea: a
//! graph of **symmetric keys** laid over the content-addressed block graph, where
//! every edge is `SymmetricLink::wrap(from, to)` = encrypt key `to` under key
//! `from`. Access control is reachability in that key graph; read and write are
//! separate key spines, so a read-only capability is simply one that omits the
//! write key.
//!
//! Private content is **encrypted before hashing**, so a block's CID identifies
//! ciphertext and copying the raw block reveals nothing without the capability —
//! the Phase E exit criterion. This crate is pure (no `mem`, no I/O); the core
//! layer stores the opaque blocks and persists capabilities in the vault.
//!
//! Cipher: **XChaCha20-Poly1305** (the `chacha20poly1305` crate) — a modern,
//! audited AEAD with a 24-byte random nonce, the secretbox-equivalent the spec
//! calls for. We are not byte-compatible with any existing implementation (this
//! is a self-contained graph), so the AEAD choice is ours; the *design* is
//! unchanged.
//!
//! The core integration wires this graph into the egress guard, reviewed
//! convert-and-share flow, locked capability vault, and private-swarm posture.
//! Signed mutable-head mechanics remain with the network plan's later shared
//! namespace work; Phase E shares read-only capabilities by default.

pub mod capability;
pub mod error;
pub mod link;
pub mod node;
pub mod rotate;
pub mod secretbox;
pub mod vault;

pub use capability::Capability;
pub use error::CryptoError;
pub use link::SymmetricLink;
pub use node::{
    build, cid_of, open_node, walk_decrypt, EncryptedBlock, EncryptedTree, OpenChild, OpenNode,
    Plain,
};
pub use rotate::{decrypt_to_plain, rotate_tree};
pub use secretbox::{CipherText, SymmetricKey};
pub use vault::{derive_vault_key, VaultKey};
