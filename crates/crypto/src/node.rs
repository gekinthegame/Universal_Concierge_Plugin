//! The encrypted Cryptree node. Port of the core of the Cryptree `CryptreeNode`.
//!
//! A node's plaintext is sealed under its **read key**, so the stored block is
//! ciphertext and its content address hashes ciphertext. A directory's sealed
//! plaintext holds, per child: the child's read key (recoverable by anyone who
//! can read the directory) and — wrapped under the directory's **write key** — a
//! link to the child's write key. So a read-only directory capability yields
//! read-only child capabilities; only a read+write capability propagates write.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::capability::Capability;
use crate::error::{CryptoError, Result};
use crate::link::SymmetricLink;
use crate::secretbox::{CipherText, SymmetricKey, KEY_LEN};

/// A plaintext subtree the caller wants encrypted.
#[derive(Clone, Debug)]
pub enum Plain {
    /// A directory of named children.
    Dir(Vec<(String, Plain)>),
    /// A leaf file's bytes.
    File(Vec<u8>),
}

/// One stored, encrypted block: its ciphertext content address and bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedBlock {
    pub cid: String,
    pub bytes: Vec<u8>,
}

/// The result of encrypting a subtree: the root capability plus every block.
#[derive(Debug)]
pub struct EncryptedTree {
    pub root: Capability,
    pub blocks: Vec<EncryptedBlock>,
}

impl EncryptedTree {
    /// A cid -> block-bytes view, e.g. to feed a reader.
    pub fn block_map(&self) -> HashMap<String, Vec<u8>> {
        self.blocks
            .iter()
            .map(|block| (block.cid.clone(), block.bytes.clone()))
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
enum StoredPlain {
    Dir { children: Vec<StoredChild> },
    File { data: Vec<u8> },
}

#[derive(Serialize, Deserialize)]
struct StoredChild {
    name: String,
    read_key: [u8; KEY_LEN],
    cid: String,
    /// The child's write key wrapped under *this directory's* write key. Absent
    /// from read-only graphs; unreadable without the directory write key.
    write_link: Option<Vec<u8>>,
}

/// The content address of an encrypted block: sha-256 over the ciphertext bytes,
/// so the address reveals nothing about the plaintext.
pub fn cid_of(block_bytes: &[u8]) -> String {
    const RAW_CODEC: u64 = 0x55;
    const SHA2_256: u64 = 0x12;
    let digest = Sha256::digest(block_bytes);
    let multihash = multihash::Multihash::<64>::wrap(SHA2_256, digest.as_slice())
        .expect("sha2-256 digest always fits the configured multihash");
    cid::Cid::new_v1(RAW_CODEC, multihash).to_string()
}

/// Encrypt `plain` into a fresh Cryptree. Every node gets new random read and
/// write keys; the returned root capability is read+write.
pub fn build(plain: &Plain) -> Result<EncryptedTree> {
    let mut blocks = Vec::new();
    let root = build_node(plain, &mut blocks)?;
    Ok(EncryptedTree { root, blocks })
}

fn build_node(plain: &Plain, blocks: &mut Vec<EncryptedBlock>) -> Result<Capability> {
    let read_key = SymmetricKey::random();
    let write_key = SymmetricKey::random();
    let stored = match plain {
        Plain::File(data) => StoredPlain::File { data: data.clone() },
        Plain::Dir(children) => {
            let mut stored_children = Vec::with_capacity(children.len());
            for (name, child_plain) in children {
                let child = build_node(child_plain, blocks)?;
                let write_link = child
                    .write_key
                    .as_ref()
                    .map(|child_write| SymmetricLink::wrap(&write_key, child_write).to_bytes());
                stored_children.push(StoredChild {
                    name: name.clone(),
                    read_key: child.read_key.to_bytes(),
                    cid: child.cid.clone(),
                    write_link,
                });
            }
            StoredPlain::Dir {
                children: stored_children,
            }
        }
    };
    let plaintext =
        serde_json::to_vec(&stored).map_err(|error| CryptoError::Serialize(error.to_string()))?;
    let ciphertext = read_key.seal(&plaintext);
    let bytes = ciphertext.to_bytes();
    let cid = cid_of(&bytes);
    blocks.push(EncryptedBlock {
        cid: cid.clone(),
        bytes,
    });
    Ok(Capability::read_write(cid, read_key, write_key))
}

/// One decrypted child reference, with a capability scoped to its parent's
/// access (read-only parent => read-only child).
#[derive(Clone, Debug)]
pub struct OpenChild {
    pub name: String,
    pub cap: Capability,
}

/// A decrypted node.
#[derive(Clone, Debug)]
pub enum OpenNode {
    Dir(Vec<OpenChild>),
    File(Vec<u8>),
}

/// Decrypt one block with `cap`. Fails with [`CryptoError::Decrypt`] if the
/// capability's read key does not match the block — which is exactly what stops
/// a child capability from reading a parent or a sibling.
pub fn open_node(cap: &Capability, block_bytes: &[u8]) -> Result<OpenNode> {
    if cid_of(block_bytes) != cap.cid {
        return Err(CryptoError::CidMismatch);
    }
    let ciphertext = CipherText::from_bytes(block_bytes)?;
    let plaintext = cap.read_key.open(&ciphertext)?;
    let stored: StoredPlain = serde_json::from_slice(&plaintext)
        .map_err(|error| CryptoError::Serialize(error.to_string()))?;
    match stored {
        StoredPlain::File { data } => Ok(OpenNode::File(data)),
        StoredPlain::Dir { children } => {
            let mut open = Vec::with_capacity(children.len());
            for child in children {
                let read_key = SymmetricKey::from_bytes(child.read_key);
                // Recover the child write key only if we hold this directory's
                // write key (read-only holders cannot).
                let write_key = match (&cap.write_key, &child.write_link) {
                    (Some(dir_write), Some(link_bytes)) => {
                        Some(SymmetricLink::from_bytes(link_bytes)?.unwrap(dir_write)?)
                    }
                    _ => None,
                };
                open.push(OpenChild {
                    name: child.name,
                    cap: Capability {
                        cid: child.cid,
                        read_key,
                        write_key,
                    },
                });
            }
            Ok(OpenNode::Dir(open))
        }
    }
}

/// Read an entire subtree from `root`, fetching blocks via `fetch`. Returns each
/// node's cid and decrypted form. Read access flows strictly downward.
pub fn walk_decrypt(
    root: &Capability,
    fetch: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<Vec<(String, OpenNode)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(cap) = stack.pop() {
        let bytes = fetch(&cap.cid).ok_or_else(|| CryptoError::BlockNotFound(cap.cid.clone()))?;
        let node = open_node(&cap, &bytes)?;
        if let OpenNode::Dir(children) = &node {
            for child in children {
                stack.push(child.cap.clone());
            }
        }
        out.push((cap.cid.clone(), node));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Plain {
        Plain::Dir(vec![
            (
                "secret.txt".to_string(),
                Plain::File(b"WETLANDS-CLASSIFIED".to_vec()),
            ),
            (
                "notes.txt".to_string(),
                Plain::File(b"sibling content".to_vec()),
            ),
        ])
    }

    #[test]
    fn exit_criterion_blocks_are_ciphertext_and_unreadable_without_the_capability() {
        let tree = build(&sample()).unwrap();
        assert!(tree.root.cid.parse::<cid::Cid>().is_ok());
        // No stored block contains the plaintext marker.
        for block in &tree.blocks {
            assert!(!block
                .bytes
                .windows(b"WETLANDS-CLASSIFIED".len())
                .any(|window| window == b"WETLANDS-CLASSIFIED"));
        }
        // A random capability over the same cid cannot read it.
        let root_bytes = tree.block_map()[&tree.root.cid].clone();
        let impostor = Capability::read_only(tree.root.cid.clone(), SymmetricKey::random());
        assert!(matches!(
            open_node(&impostor, &root_bytes),
            Err(CryptoError::Decrypt)
        ));
    }

    #[test]
    fn substituted_ciphertext_is_rejected_even_when_the_read_key_can_decrypt_it() {
        let tree = build(&sample()).unwrap();
        let replacement = tree.root.read_key.seal(br#"{"File":{"data":[1,2,3]}}"#);
        assert!(matches!(
            open_node(&tree.root.to_read_only(), &replacement.to_bytes()),
            Err(CryptoError::CidMismatch)
        ));
    }

    #[test]
    fn root_capability_reaches_every_descendant() {
        let tree = build(&sample()).unwrap();
        let map = tree.block_map();
        let nodes = walk_decrypt(&tree.root, |cid| map.get(cid).cloned()).unwrap();
        let files: Vec<Vec<u8>> = nodes
            .iter()
            .filter_map(|(_, node)| match node {
                OpenNode::File(data) => Some(data.clone()),
                OpenNode::Dir(_) => None,
            })
            .collect();
        assert!(files.contains(&b"WETLANDS-CLASSIFIED".to_vec()));
        assert!(files.contains(&b"sibling content".to_vec()));
    }

    #[test]
    fn a_child_capability_cannot_read_its_parent_or_siblings() {
        let tree = build(&sample()).unwrap();
        let map = tree.block_map();
        let OpenNode::Dir(children) = open_node(&tree.root, &map[&tree.root.cid]).unwrap() else {
            panic!("root is a directory");
        };
        let secret = children.iter().find(|c| c.name == "secret.txt").unwrap();
        let sibling = children.iter().find(|c| c.name == "notes.txt").unwrap();

        // The child capability opens its own file...
        assert!(matches!(
            open_node(&secret.cap, &map[&secret.cap.cid]).unwrap(),
            OpenNode::File(_)
        ));
        // ...but cannot read the parent directory block...
        assert!(matches!(
            open_node(&secret.cap, &map[&tree.root.cid]),
            Err(CryptoError::CidMismatch)
        ));
        // ...nor the sibling's block.
        assert!(matches!(
            open_node(&secret.cap, &map[&sibling.cap.cid]),
            Err(CryptoError::CidMismatch)
        ));
    }

    #[test]
    fn read_only_directory_yields_read_only_children() {
        let tree = build(&sample()).unwrap();
        let map = tree.block_map();
        // Read+write root => writable children.
        let OpenNode::Dir(rw_children) = open_node(&tree.root, &map[&tree.root.cid]).unwrap()
        else {
            panic!("dir");
        };
        assert!(rw_children.iter().all(|c| c.cap.is_writable()));

        // Read-only root => children are read-only too (no write keys recovered).
        let read_only_root = tree.root.to_read_only();
        let OpenNode::Dir(ro_children) = open_node(&read_only_root, &map[&tree.root.cid]).unwrap()
        else {
            panic!("dir");
        };
        assert!(ro_children.iter().all(|c| !c.cap.is_writable()));
        // Read access is preserved for the read-only holder.
        assert!(matches!(
            open_node(&ro_children[0].cap, &map[&ro_children[0].cap.cid]).unwrap(),
            OpenNode::File(_)
        ));
    }
}
