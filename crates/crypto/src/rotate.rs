//! Key rotation = revocation. Port of the Cryptree key-rotation operation: decrypt the
//! subtree with the current capabilities, then re-encrypt under fresh random
//! keys. The result is a new ciphertext graph with new CIDs; the old capability
//! no longer decrypts it, so a revoked holder loses future access. (Honest
//! limit, as the plan states: rotation cannot unsend blocks already fetched.)

use crate::capability::Capability;
use crate::error::{CryptoError, Result};
use crate::node::{build, open_node, EncryptedTree, OpenNode, Plain};

/// Decrypt an entire subtree back into plaintext, fetching blocks via `fetch`.
pub fn decrypt_to_plain(
    cap: &Capability,
    fetch: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Plain> {
    let bytes = fetch(&cap.cid).ok_or_else(|| CryptoError::BlockNotFound(cap.cid.clone()))?;
    match open_node(cap, &bytes)? {
        OpenNode::File(data) => Ok(Plain::File(data)),
        OpenNode::Dir(children) => {
            let mut out = Vec::with_capacity(children.len());
            for child in children {
                out.push((child.name.clone(), decrypt_to_plain(&child.cap, fetch)?));
            }
            Ok(Plain::Dir(out))
        }
    }
}

/// Rotate every key under `old_root`: decrypt, then rebuild with fresh keys.
/// Returns the new tree (new root capability + new blocks).
pub fn rotate_tree(
    old_root: &Capability,
    fetch: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<EncryptedTree> {
    let plain = decrypt_to_plain(old_root, fetch)?;
    build(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::open_node;

    fn sample() -> Plain {
        Plain::Dir(vec![(
            "secret.txt".to_string(),
            Plain::File(b"rotate me".to_vec()),
        )])
    }

    #[test]
    fn rotation_locks_out_the_old_capability_but_preserves_content() {
        let original = build(&sample()).unwrap();
        let map = original.block_map();
        let rotated = rotate_tree(&original.root, &|cid| map.get(cid).cloned()).unwrap();
        let new_map = rotated.block_map();

        // New root has a new cid and new key material.
        assert_ne!(original.root.cid, rotated.root.cid);

        // The OLD capability cannot open the NEW root block.
        if let Some(new_root_bytes) = new_map.get(&rotated.root.cid) {
            assert!(matches!(
                open_node(&original.root, new_root_bytes),
                Err(CryptoError::CidMismatch)
            ));
        }

        // The NEW capability decrypts identical content.
        let plain = decrypt_to_plain(&rotated.root, &|cid| new_map.get(cid).cloned()).unwrap();
        match plain {
            Plain::Dir(children) => match &children[0].1 {
                Plain::File(data) => assert_eq!(data, b"rotate me"),
                other => panic!("expected file, got {other:?}"),
            },
            other => panic!("expected dir, got {other:?}"),
        }
    }
}
