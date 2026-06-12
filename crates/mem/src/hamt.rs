//! A hand-rolled IPLD HAMT (sharded, content-addressed map) for the Phase A.5
//! day tier. Ported from the reference implementations
//! [`go-hamt-ipld`](https://github.com/filecoin-project/go-hamt-ipld) and
//! [`fvm_ipld_hamt`](https://github.com/filecoin-project/ref-fvm) (both MIT;
//! design ported, not depended on — see DECISIONS.md 0014, 0011/0018/0021 pattern).
//!
//! The HAMT hashes each key (SHA-256) and consumes `BIT_WIDTH` bits of the digest
//! per tree level to index a node's `2^BIT_WIDTH` slots. Each occupied slot is a
//! *bucket* of key-sorted entries; when a bucket overflows `BUCKET_SIZE`, it is
//! pushed down into a child node. Because buckets stay key-sorted and splits are
//! a pure function of the key hashes, a given set of entries always produces the
//! same root CID regardless of insertion order (CHAMP canonical form) — which is
//! exactly what content-addressed dedup wants.
//!
//! Block layout (per node), DAG-CBOR: `[bitfield, [pointer, ...]]` where
//! `bitfield` is a minimal big-endian byte string of the 256-bit occupancy map and
//! each `pointer` is the tagged form `{"0": <link cid>}` or `{"1": [[key, val], ...]}`.
//! Keys are byte strings; values are any DAG-CBOR-serializable `V` (for the day
//! tier, `V` is a record [`Cid`] link).
//!
//! Scope: insert + lookup + iterate, with bucket split. Deletion (and the CHAMP
//! collapse-on-delete canonicalization) is not implemented — the calendar tiers
//! are insert-only; retention happens by sealing/GC at the block level, not by
//! removing individual entries. Add it here if a delete path is ever needed.

use crate::blockstore::Blockstore;
use crate::cid::Cid;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bits of the key hash consumed per level → `2^BIT_WIDTH` slots per node.
const BIT_WIDTH: u32 = 8;
/// Max entries in a bucket before it is pushed down into a child node.
const BUCKET_SIZE: usize = 3;

type HashedKey = [u8; 32];

fn hash_key(key: &[u8]) -> HashedKey {
    let mut hasher = Sha256::new();
    hasher.update(key);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// HashBits — read the next `n` bits of a hashed key as an integer index.
// ---------------------------------------------------------------------------

struct HashBits<'a> {
    digest: &'a HashedKey,
    consumed: u32,
}

impl<'a> HashBits<'a> {
    fn new(digest: &'a HashedKey) -> Self {
        Self::at(digest, 0)
    }

    fn at(digest: &'a HashedKey, consumed: u32) -> Self {
        Self { digest, consumed }
    }

    /// The next `BIT_WIDTH` bits as a slot index (0..256). Errors if the digest
    /// is exhausted (tree too deep / pathological collisions).
    fn next(&mut self) -> Result<u8> {
        let i = BIT_WIDTH;
        let remaining = self.digest.len() as u32 * 8 - self.consumed;
        if remaining == 0 {
            anyhow::bail!("hamt: key hash exhausted (tree too deep)");
        }
        Ok(self.next_bits(i.min(remaining)))
    }

    fn next_bits(&mut self, i: u32) -> u8 {
        let cur_byte = (self.consumed / 8) as usize;
        let left = 8 - (self.consumed % 8);
        let byte = self.digest[cur_byte];
        let mask = |n: u32| -> u8 { ((1u16 << n) - 1) as u8 };
        if i == left {
            self.consumed += i;
            mask(i) & byte
        } else if i < left {
            let a = byte & mask(left);
            let b = a & !mask(left - i);
            self.consumed += i;
            b >> (left - i)
        } else {
            let mut out = mask(left) & byte;
            out <<= i - left;
            self.consumed += left;
            out + self.next_bits(i - left)
        }
    }
}

// ---------------------------------------------------------------------------
// Bitfield — 256-bit occupancy map, serialized as a minimal big-endian byte string.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Bitfield([u64; 4]);

impl Bitfield {
    fn test_bit(&self, idx: u8) -> bool {
        self.0[(idx / 64) as usize] & (1 << (idx % 64)) != 0
    }

    fn set_bit(&mut self, idx: u8) {
        self.0[(idx / 64) as usize] |= 1 << (idx % 64);
    }

    /// Compacted position of slot `idx` in the pointers array: how many occupied
    /// slots precede it (popcount of set bits below `idx`).
    fn index_for(&self, idx: u8) -> usize {
        let mut count = 0;
        for bit in 0..idx {
            if self.test_bit(bit) {
                count += 1;
            }
        }
        count
    }
}

impl Serialize for Bitfield {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        // 32 bytes big-endian (word[3] is most significant), leading zeros stripped.
        let mut buf = [0u8; 32];
        buf[0..8].copy_from_slice(&self.0[3].to_be_bytes());
        buf[8..16].copy_from_slice(&self.0[2].to_be_bytes());
        buf[16..24].copy_from_slice(&self.0[1].to_be_bytes());
        buf[24..32].copy_from_slice(&self.0[0].to_be_bytes());
        let start = buf.iter().position(|&b| b != 0).unwrap_or(buf.len());
        serde_bytes::Serialize::serialize(&buf[start..], s)
    }
}

impl<'de> Deserialize<'de> for Bitfield {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let bytes: Vec<u8> = serde_bytes::deserialize(d)?;
        if bytes.len() > 32 {
            return Err(serde::de::Error::custom("hamt bitfield exceeds 32 bytes"));
        }
        let mut buf = [0u8; 32];
        buf[32 - bytes.len()..].copy_from_slice(&bytes);
        let word = |r: std::ops::Range<usize>| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&buf[r]);
            u64::from_be_bytes(w)
        };
        Ok(Bitfield([
            word(24..32),
            word(16..24),
            word(8..16),
            word(0..8),
        ]))
    }
}

// ---------------------------------------------------------------------------
// KeyValuePair / Pointer / Node
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeyValuePair<V>(#[serde(with = "serde_bytes")] Vec<u8>, V);

/// On-disk pointer form: tagged (Filecoin V0 variant) so serde can (de)serialize
/// it without IPLD type-peeking. `{"0": cid}` = link to child node; `{"1": [...]}`
/// = inline bucket of entries.
#[derive(Debug, Serialize, Deserialize)]
enum PointerRepr<V> {
    #[serde(rename = "0")]
    Link(Cid),
    #[serde(rename = "1")]
    Bucket(Vec<KeyValuePair<V>>),
}

/// In-memory pointer. `Dirty` is a not-yet-persisted child held in memory; it is
/// flushed into a `Link` before the node is serialized.
#[derive(Debug)]
enum Pointer<V> {
    Bucket(Vec<KeyValuePair<V>>),
    Link(Cid),
    Dirty(Box<Node<V>>),
}

#[derive(Debug)]
struct Node<V> {
    bitfield: Bitfield,
    pointers: Vec<Pointer<V>>,
}

// Manual impl: an empty node is independent of `V`, so don't require `V: Default`.
impl<V> Default for Node<V> {
    fn default() -> Self {
        Node {
            bitfield: Bitfield::default(),
            pointers: Vec::new(),
        }
    }
}

impl<V> Serialize for Node<V>
where
    V: Serialize + Clone,
{
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let reprs: std::result::Result<Vec<PointerRepr<V>>, S::Error> = self
            .pointers
            .iter()
            .map(|p| match p {
                Pointer::Bucket(vals) => Ok(PointerRepr::Bucket(vals.clone())),
                Pointer::Link(cid) => Ok(PointerRepr::Link(*cid)),
                Pointer::Dirty(_) => Err(serde::ser::Error::custom(
                    "hamt: cannot serialize a dirty (unflushed) pointer",
                )),
            })
            .collect();
        (&self.bitfield, &reprs?).serialize(s)
    }
}

impl<'de, V> Deserialize<'de> for Node<V>
where
    V: Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let (bitfield, reprs): (Bitfield, Vec<PointerRepr<V>>) = Deserialize::deserialize(d)?;
        let pointers = reprs
            .into_iter()
            .map(|r| match r {
                PointerRepr::Link(cid) => Pointer::Link(cid),
                PointerRepr::Bucket(vals) => Pointer::Bucket(vals),
            })
            .collect();
        Ok(Node { bitfield, pointers })
    }
}

impl<V> Node<V>
where
    V: Serialize + serde::de::DeserializeOwned + Clone,
{
    fn load<B: Blockstore>(store: &B, cid: &Cid) -> Result<Self> {
        let bytes = store.get(cid).with_context(|| format!("hamt: load node {cid}"))?;
        serde_ipld_dagcbor::from_slice(&bytes).with_context(|| format!("hamt: decode node {cid}"))
    }

    fn get<B: Blockstore>(&self, store: &B, hb: &mut HashBits, key: &[u8]) -> Result<Option<V>> {
        let idx = hb.next()?;
        if !self.bitfield.test_bit(idx) {
            return Ok(None);
        }
        match &self.pointers[self.bitfield.index_for(idx)] {
            Pointer::Bucket(vals) => Ok(vals.iter().find(|kv| kv.0 == key).map(|kv| kv.1.clone())),
            Pointer::Dirty(node) => node.get(store, hb, key),
            Pointer::Link(cid) => Node::load(store, cid)?.get(store, hb, key),
        }
    }

    /// Insert or overwrite. Returns whether the tree was modified.
    fn set<B: Blockstore>(
        &mut self,
        store: &B,
        hb: &mut HashBits,
        key: &[u8],
        value: V,
    ) -> Result<()> {
        let idx = hb.next()?;
        if !self.bitfield.test_bit(idx) {
            let pos = self.bitfield.index_for(idx);
            self.bitfield.set_bit(idx);
            self.pointers
                .insert(pos, Pointer::Bucket(vec![KeyValuePair(key.to_vec(), value)]));
            return Ok(());
        }
        let pos = self.bitfield.index_for(idx);
        let child = &mut self.pointers[pos];
        match child {
            Pointer::Link(cid) => {
                let cid = *cid;
                let mut node = Node::load(store, &cid)?;
                node.set(store, hb, key, value)?;
                *child = Pointer::Dirty(Box::new(node));
                Ok(())
            }
            Pointer::Dirty(node) => node.set(store, hb, key, value),
            Pointer::Bucket(vals) => {
                if let Some(existing) = vals.iter_mut().find(|kv| kv.0 == key) {
                    existing.1 = value;
                    return Ok(());
                }
                if vals.len() < BUCKET_SIZE {
                    // Insert in key-sorted order (canonical form).
                    let at = vals.iter().position(|kv| kv.0.as_slice() > key).unwrap_or(vals.len());
                    vals.insert(at, KeyValuePair(key.to_vec(), value));
                    return Ok(());
                }
                // Overflow → push the bucket (plus the new entry) down one level.
                let consumed = hb.consumed;
                let kvs = std::mem::take(vals);
                let mut sub = Node::<V>::default();
                sub.set(store, hb, key, value)?;
                for KeyValuePair(k, v) in kvs {
                    let h = hash_key(&k);
                    sub.set(store, &mut HashBits::at(&h, consumed), &k, v)?;
                }
                *child = Pointer::Dirty(Box::new(sub));
                Ok(())
            }
        }
    }

    /// Persist dirty children bottom-up, replacing each with a `Link`.
    fn flush<B: Blockstore>(&mut self, store: &B) -> Result<()> {
        for p in &mut self.pointers {
            if let Pointer::Dirty(node) = p {
                node.flush(store)?;
                let bytes = serde_ipld_dagcbor::to_vec(node.as_ref())
                    .context("hamt: encode node for flush")?;
                let cid = store.put(&bytes).context("hamt: put node")?;
                *p = Pointer::Link(cid);
            }
        }
        Ok(())
    }

    /// Visit every (key, value) entry in key-hash order.
    fn for_each<B: Blockstore>(
        &self,
        store: &B,
        f: &mut dyn FnMut(&[u8], &V) -> Result<()>,
    ) -> Result<()> {
        for p in &self.pointers {
            match p {
                Pointer::Bucket(vals) => {
                    for kv in vals {
                        f(&kv.0, &kv.1)?;
                    }
                }
                Pointer::Dirty(node) => node.for_each(store, f)?,
                Pointer::Link(cid) => Node::load(store, cid)?.for_each(store, f)?,
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Hamt — the public wrapper over a blockstore.
// ---------------------------------------------------------------------------

/// A sharded, content-addressed map backed by a [`Blockstore`]. Mutations stay in
/// memory until [`Hamt::flush`], which persists the tree and returns the root CID.
pub struct Hamt<'a, B, V> {
    store: &'a B,
    root: Node<V>,
}

impl<'a, B, V> Hamt<'a, B, V>
where
    B: Blockstore,
    V: Serialize + serde::de::DeserializeOwned + Clone,
{
    /// A new, empty HAMT.
    pub fn new(store: &'a B) -> Self {
        Self {
            store,
            root: Node::default(),
        }
    }

    /// Load an existing HAMT by its root CID.
    pub fn load(store: &'a B, root: &Cid) -> Result<Self> {
        Ok(Self {
            store,
            root: Node::load(store, root)?,
        })
    }

    /// Insert or overwrite `key → value`. Persisted on [`Self::flush`].
    pub fn set(&mut self, key: &[u8], value: V) -> Result<()> {
        let h = hash_key(key);
        let mut hb = HashBits::new(&h);
        self.root.set(self.store, &mut hb, key, value)
    }

    /// Look up `key`.
    pub fn get(&self, key: &[u8]) -> Result<Option<V>> {
        let h = hash_key(key);
        let mut hb = HashBits::new(&h);
        self.root.get(self.store, &mut hb, key)
    }

    /// Visit every entry. Order is by key hash, not insertion.
    pub fn for_each(&self, mut f: impl FnMut(&[u8], &V) -> Result<()>) -> Result<()> {
        self.root.for_each(self.store, &mut f)
    }

    /// Persist all in-memory changes and return the root CID. The CID is a pure
    /// function of the entry set (canonical form), so identical content dedups.
    pub fn flush(&mut self) -> Result<Cid> {
        self.root.flush(self.store)?;
        let bytes = serde_ipld_dagcbor::to_vec(&self.root).context("hamt: encode root")?;
        self.store.put(&bytes).context("hamt: put root")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockstore::LocalBlocks;
    use tempfile::TempDir;

    fn store() -> (TempDir, LocalBlocks) {
        let dir = TempDir::new().unwrap();
        let blocks = LocalBlocks::new(dir.path().join("blocks"));
        (dir, blocks)
    }

    #[test]
    fn set_get_roundtrip_in_memory() {
        let (_d, blocks) = store();
        let mut h: Hamt<_, String> = Hamt::new(&blocks);
        h.set(b"alpha", "a".into()).unwrap();
        h.set(b"beta", "b".into()).unwrap();
        assert_eq!(h.get(b"alpha").unwrap().as_deref(), Some("a"));
        assert_eq!(h.get(b"beta").unwrap().as_deref(), Some("b"));
        assert_eq!(h.get(b"missing").unwrap(), None);
    }

    #[test]
    fn survives_flush_and_reload() {
        let (_d, blocks) = store();
        let root = {
            let mut h: Hamt<_, u64> = Hamt::new(&blocks);
            for i in 0..200u64 {
                h.set(format!("key-{i}").as_bytes(), i).unwrap();
            }
            h.flush().unwrap()
        };
        let h: Hamt<_, u64> = Hamt::load(&blocks, &root).unwrap();
        for i in 0..200u64 {
            assert_eq!(h.get(format!("key-{i}").as_bytes()).unwrap(), Some(i));
        }
        assert_eq!(h.get(b"nope").unwrap(), None);
    }

    #[test]
    fn overwrite_replaces_value() {
        let (_d, blocks) = store();
        let mut h: Hamt<_, u64> = Hamt::new(&blocks);
        h.set(b"k", 1).unwrap();
        h.set(b"k", 2).unwrap();
        assert_eq!(h.get(b"k").unwrap(), Some(2));
    }

    #[test]
    fn root_cid_is_canonical_regardless_of_insert_order() {
        let (_d, blocks) = store();
        let n = 300u64;

        let mut a: Hamt<_, u64> = Hamt::new(&blocks);
        for i in 0..n {
            a.set(format!("key-{i}").as_bytes(), i).unwrap();
        }
        let cid_a = a.flush().unwrap();

        // Insert the same entries in reverse order.
        let mut b: Hamt<_, u64> = Hamt::new(&blocks);
        for i in (0..n).rev() {
            b.set(format!("key-{i}").as_bytes(), i).unwrap();
        }
        let cid_b = b.flush().unwrap();

        assert_eq!(cid_a, cid_b, "same entry set must yield the same root CID");
    }

    #[test]
    fn for_each_visits_every_entry() {
        let (_d, blocks) = store();
        let mut h: Hamt<_, u64> = Hamt::new(&blocks);
        for i in 0..50u64 {
            h.set(format!("k{i}").as_bytes(), i).unwrap();
        }
        let mut seen = std::collections::BTreeSet::new();
        h.for_each(|_k, v| {
            seen.insert(*v);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 50);
        assert!(seen.contains(&0) && seen.contains(&49));
    }
}
