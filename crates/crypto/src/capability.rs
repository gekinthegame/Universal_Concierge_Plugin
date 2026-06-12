//! A capability: the unit you grant. Port of the Cryptree `RelativeCapability` /
//! `AbsoluteCapability` primitives, with **read and write as separate keys**. A read-only
//! capability is exactly one whose `write_key` is `None` — the absence of the
//! key *is* the absence of the permission, so no enforcement code is needed.

use serde::{Deserialize, Serialize};

use crate::secretbox::{SymmetricKey, KEY_LEN};

/// A fully-resolved capability to one encrypted node: where it is (`cid`), the
/// key that decrypts it (`read_key`), and — only for read+write grants — the
/// `write_key` that unwraps the node's signer.
#[derive(Clone, Debug)]
pub struct Capability {
    pub cid: String,
    pub read_key: SymmetricKey,
    pub write_key: Option<SymmetricKey>,
}

impl Capability {
    pub fn read_only(cid: String, read_key: SymmetricKey) -> Self {
        Self {
            cid,
            read_key,
            write_key: None,
        }
    }

    pub fn read_write(cid: String, read_key: SymmetricKey, write_key: SymmetricKey) -> Self {
        Self {
            cid,
            read_key,
            write_key: Some(write_key),
        }
    }

    /// Whether this capability can author updates to the node.
    pub fn is_writable(&self) -> bool {
        self.write_key.is_some()
    }

    /// Derive a read-only capability from this one (drop the write key). This is
    /// how you delegate read access without granting write.
    pub fn to_read_only(&self) -> Self {
        Self {
            cid: self.cid.clone(),
            read_key: self.read_key.clone(),
            write_key: None,
        }
    }

    /// Serializable form for the vault (raw key bytes, no `SymmetricKey` Drop).
    pub fn to_wire(&self) -> CapabilityWire {
        CapabilityWire {
            cid: self.cid.clone(),
            read_key: self.read_key.to_bytes(),
            write_key: self.write_key.as_ref().map(SymmetricKey::to_bytes),
        }
    }

    pub fn from_wire(wire: &CapabilityWire) -> Self {
        Self {
            cid: wire.cid.clone(),
            read_key: SymmetricKey::from_bytes(wire.read_key),
            write_key: wire.write_key.map(SymmetricKey::from_bytes),
        }
    }
}

/// The at-rest encoding of a capability. Lives only inside the sealed vault.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityWire {
    pub cid: String,
    pub read_key: [u8; KEY_LEN],
    pub write_key: Option<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for CapabilityWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityWire")
            .field("cid", &self.cid)
            .field("read_key", &"[redacted]")
            .field("write_key", &self.write_key.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl Drop for CapabilityWire {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.read_key.zeroize();
        self.write_key.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_capability_has_no_write_key() {
        let cap = Capability::read_only("bafyabc".to_string(), SymmetricKey::random());
        assert!(!cap.is_writable());
    }

    #[test]
    fn to_read_only_drops_the_write_key_but_keeps_read() {
        let read = SymmetricKey::random();
        let write = SymmetricKey::random();
        let rw = Capability::read_write("bafyabc".to_string(), read.clone(), write);
        assert!(rw.is_writable());
        let ro = rw.to_read_only();
        assert!(!ro.is_writable());
        assert_eq!(ro.read_key.to_bytes(), read.to_bytes());
    }

    #[test]
    fn wire_roundtrip_preserves_keys() {
        let rw = Capability::read_write(
            "bafyabc".to_string(),
            SymmetricKey::random(),
            SymmetricKey::random(),
        );
        let restored = Capability::from_wire(&rw.to_wire());
        assert_eq!(restored.read_key.to_bytes(), rw.read_key.to_bytes());
        assert_eq!(
            restored.write_key.unwrap().to_bytes(),
            rw.write_key.unwrap().to_bytes()
        );
    }
}
