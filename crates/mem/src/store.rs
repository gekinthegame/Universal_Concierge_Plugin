//! The store wires the blockstore and the name index together, and exposes the
//! two functions that carry the whole system: `put_node` and `get_node`. Also
//! the `share` method, which publishes a subgraph to any `Backend`.

use crate::backend::Backend;
use crate::blockstore::{Blockstore, LocalBlocks};
use crate::cid::Cid;
use crate::gc::{GcReport, RetentionPolicy};
use crate::names::NameIndex;
use crate::node::{Edge, Node, Record, Source};
use crate::tombstones::{Tombstone, Tombstones};
use std::time::{SystemTime, UNIX_EPOCH};

/// The result of a tombstone-aware lookup: the record if it's present, or its
/// death certificate if GC pruned it. A pruned block is a fact, not an error.
pub enum Lookup {
    Present(Box<Record>),
    Pruned(Tombstone),
}

pub trait Clock {
    fn now_unix(&self) -> anyhow::Result<u64>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> anyhow::Result<u64> {
        Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
    }
}

pub struct Store<B: Blockstore, C: Clock = SystemClock> {
    blocks: B,
    names: NameIndex,
    clock: C,
}

impl<B: Blockstore> Store<B, SystemClock> {
    pub fn new(blocks: B, names: NameIndex) -> Self {
        Self {
            blocks,
            names,
            clock: SystemClock,
        }
    }
}

impl<B: Blockstore, C: Clock> Store<B, C> {
    pub fn with_clock(blocks: B, names: NameIndex, clock: C) -> Self {
        Self {
            blocks,
            names,
            clock,
        }
    }

    /// Wrap a node in a Record envelope (schema_version, created_at, source),
    /// encode DAG-CBOR, put the block. The river, forward.
    pub fn put_node(&self, node: Node, source: Source) -> anyhow::Result<Cid> {
        self.put_node_with_edges(node, source, Vec::new())
    }

    /// Like `put_node`, but stamps an **explicit** `created_at` instead of the
    /// wall clock. For **derived, rebuildable** nodes (e.g. the calendar month/year
    /// manifests) whose timestamp should be a deterministic function of their
    /// content — so re-deriving them yields the *same* CID (idempotency) rather
    /// than a new one each second.
    pub fn put_node_at(
        &self,
        node: Node,
        source: Source,
        created_at: u64,
    ) -> anyhow::Result<Cid> {
        self.put_node_with_edges_at(node, source, Vec::new(), created_at)
    }

    /// Like `put_node`, but with associative graph edges on the record.
    pub(crate) fn put_node_with_edges(
        &self,
        node: Node,
        source: Source,
        edges: Vec<Edge>,
    ) -> anyhow::Result<Cid> {
        let created_at = self.clock.now_unix()?;
        self.put_node_with_edges_at(node, source, edges, created_at)
    }

    fn put_node_with_edges_at(
        &self,
        node: Node,
        source: Source,
        edges: Vec<Edge>,
        created_at: u64,
    ) -> anyhow::Result<Cid> {
        let kind = node.kind();
        let record = Record {
            schema_version: crate::node::CURRENT_SCHEMA_VERSION,
            created_at,
            source,
            edges,
            body: node,
        };
        let bytes = crate::node::encode(&record)?;
        let cid = self.blocks.put(&bytes)?;
        crate::trace::put(kind, &cid, bytes.len());
        Ok(cid)
    }

    /// Get a block and decode it into a Record. The river, backward.
    pub fn get_node(&self, cid: &Cid) -> anyhow::Result<Record> {
        let bytes = self.blocks.get(cid)?;
        crate::node::decode(&bytes)
    }

    /// Tombstone-aware lookup: the record if present, or its death certificate
    /// if GC pruned it. Only a genuinely missing (untombstoned) block errors.
    pub fn lookup(&self, cid: &Cid) -> anyhow::Result<Lookup> {
        match self.blocks.get(cid) {
            Ok(bytes) => Ok(Lookup::Present(Box::new(crate::node::decode(&bytes)?))),
            Err(e) => match self.blocks.tombstone(cid) {
                Some(t) => Ok(Lookup::Pruned(t)),
                None => Err(e),
            },
        }
    }

    /// Whether a block is present on the store.
    pub fn has_block(&self, cid: &Cid) -> anyhow::Result<bool> {
        self.blocks.has(cid)
    }

    /// The present blocks reachable from `root` (pruned frontiers excluded).
    pub fn reachable(&self, root: &Cid) -> anyhow::Result<Vec<Cid>> {
        crate::dag::reachable_from(&self.blocks, root)
    }

    /// The only mutable operation: point a name at a root CID.
    pub fn bind(&mut self, name: &str, root: Cid) -> anyhow::Result<()> {
        self.names.bind(name, root)?;
        crate::trace::bind(name, &root);
        Ok(())
    }

    pub fn resolve(&self, name: &str) -> anyhow::Result<Cid> {
        self.names.resolve(name)
    }

    /// All current name → CID bindings (for `mem ls`).
    pub fn names(&self) -> impl Iterator<Item = (&str, &Cid)> {
        self.names.iter()
    }

    /// Publish the subgraph rooted at `root` to a network backend (a selective
    /// share, not a whole-store backup). The backend owns the push strategy;
    /// this just hands it our blockstore and the root.
    pub fn share(&self, remote: &dyn Backend, root: &Cid) -> anyhow::Result<()> {
        remote.push(&self.blocks, root)
    }
}

impl<C: Clock> Store<LocalBlocks, C> {
    /// Run a retention/GC pass: trim the auto-checkpoint chain to the policy's
    /// newest-N and sweep orphans, recording a tombstone for every deletion.
    /// The clock stamps each tombstone's time of death.
    pub fn gc(&self, policy: &RetentionPolicy) -> anyhow::Result<GcReport> {
        let mut tombstones = self.blocks.load_tombstones()?;
        let now = self.clock.now_unix()?;
        crate::gc::collect(&self.blocks, &self.names, &mut tombstones, policy, now)
    }

    /// Load this store's tombstone ledger (the GC death certificates).
    pub fn tombstones(&self) -> anyhow::Result<Tombstones> {
        self.blocks.load_tombstones()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendManifest;
    use crate::blockstore::LocalBlocks;
    use crate::config::Config;
    use crate::node::{Checkpoint, EdgeRel, Memory, MemoryKind};
    use tempfile::TempDir;

    /// A backend that records what `push` was handed, so we can assert `share`
    /// delegated correctly and passed exactly the reachable subgraph.
    struct RecordingBackend {
        seen: std::cell::RefCell<Option<(Cid, Vec<Cid>)>>,
    }
    impl Backend for RecordingBackend {
        fn manifest() -> BackendManifest {
            BackendManifest {
                name: "recording",
                label: "test",
                requires: vec![],
            }
        }
        fn from_config(_cfg: &Config) -> anyhow::Result<Self> {
            Ok(RecordingBackend {
                seen: std::cell::RefCell::new(None),
            })
        }
        fn push(&self, local: &dyn Blockstore, root: &Cid) -> anyhow::Result<()> {
            let cids = crate::dag::reachable_from(local, root)?;
            *self.seen.borrow_mut() = Some((*root, cids));
            Ok(())
        }
        fn get_block(&self, _cid: &Cid) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("not needed for this test")
        }
    }

    /// Deterministic clock so `created_at` is assertable.
    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn now_unix(&self) -> anyhow::Result<u64> {
            Ok(self.0)
        }
    }

    fn store(now: u64) -> (TempDir, Store<LocalBlocks, FixedClock>) {
        let dir = TempDir::new().unwrap();
        let blocks = LocalBlocks::new(dir.path().join("blocks"));
        let names = NameIndex::load(dir.path().join("names.json")).unwrap();
        (dir, Store::with_clock(blocks, names, FixedClock(now)))
    }

    fn memory(text: &str) -> Node {
        Node::Memory(Memory {
            text: text.into(),
            kind: MemoryKind::Project,
        })
    }

    #[test]
    fn share_publishes_exactly_the_reachable_subgraph() {
        let (_d, s) = store(0);
        let leaf = s.put_node(memory("leaf"), Source::User).unwrap();
        let cp = s
            .put_node(
                Node::Checkpoint(Checkpoint {
                    label: "c".into(),
                    root: leaf,
                    parent: None,
                }),
                Source::System,
            )
            .unwrap();

        let backend = RecordingBackend {
            seen: std::cell::RefCell::new(None),
        };
        s.share(&backend, &cp).unwrap();

        let (root, cids) = backend.seen.into_inner().unwrap();
        assert_eq!(root, cp, "share passes the chosen root to push");
        assert!(cids.contains(&cp) && cids.contains(&leaf));
        assert_eq!(cids.len(), 2, "checkpoint + its leaf, nothing else");
    }

    #[test]
    fn put_then_get_round_trips_the_body() {
        let (_d, s) = store(100);
        let cid = s.put_node(memory("hello"), Source::User).unwrap();
        match s.get_node(&cid).unwrap().body {
            Node::Memory(m) => assert_eq!(m.text, "hello"),
            other => panic!("expected Memory, got {other:?}"),
        }
    }

    #[test]
    fn put_node_stamps_the_envelope() {
        let (_d, s) = store(1234);
        let cid = s.put_node(memory("x"), Source::System).unwrap();
        let rec = s.get_node(&cid).unwrap();
        assert_eq!(rec.created_at, 1234, "created_at comes from the clock");
        assert_eq!(rec.schema_version, crate::node::CURRENT_SCHEMA_VERSION);
        assert!(matches!(rec.source, Source::System));
        assert!(rec.edges.is_empty());
    }

    #[test]
    fn put_is_content_addressed() {
        let (_d, s) = store(7);
        let a = s.put_node(memory("same"), Source::User).unwrap();
        let b = s.put_node(memory("same"), Source::User).unwrap();
        assert_eq!(a, b, "identical record bytes → identical CID");
    }

    #[test]
    fn put_with_edges_preserves_them() {
        let (_d, s) = store(0);
        let target = s.put_node(memory("target"), Source::User).unwrap();
        let edges = vec![Edge {
            rel: EdgeRel::Produced,
            to: target,
        }];
        let cid = s
            .put_node_with_edges(memory("src"), Source::User, edges)
            .unwrap();
        let rec = s.get_node(&cid).unwrap();
        assert_eq!(rec.edges.len(), 1);
        assert_eq!(rec.edges[0].rel, EdgeRel::Produced);
        assert_eq!(rec.edges[0].to, target);
    }

    #[test]
    fn bind_and_resolve_through_store() {
        let (_d, mut s) = store(0);
        let cid = s.put_node(memory("root"), Source::User).unwrap();
        s.bind("current-project", cid).unwrap();
        assert_eq!(s.resolve("current-project").unwrap(), cid);
    }

    #[test]
    fn checkpoint_can_be_bound_resolved_and_read_back() {
        let (_d, mut s) = store(42);
        let root = s.put_node(memory("project root"), Source::User).unwrap();
        let checkpoint = Node::Checkpoint(Checkpoint {
            label: "phase-2.2".into(),
            root,
            parent: None,
        });

        let checkpoint_cid = s.put_node(checkpoint, Source::System).unwrap();
        s.bind("current-project", checkpoint_cid).unwrap();

        let resolved = s.resolve("current-project").unwrap();
        assert_eq!(resolved, checkpoint_cid);

        let rec = s.get_node(&resolved).unwrap();
        match rec.body {
            Node::Checkpoint(cp) => {
                assert_eq!(cp.label, "phase-2.2");
                assert_eq!(cp.root, root);
                assert_eq!(cp.parent, None);
            }
            other => panic!("expected Checkpoint, got {other:?}"),
        }
    }

    #[test]
    fn resolve_unknown_name_errors() {
        let (_d, s) = store(0);
        assert!(s.resolve("nope").is_err());
    }
}
