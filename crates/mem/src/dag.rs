//! Graph traversal over the content-addressed DAG. `reachable_from` returns the
//! transitive closure of blocks a root depends on — used by sync (and GC later).
//!
//! It must follow BOTH structural links (typed fields like `Checkpoint.root`,
//! `Skill.supersedes`) AND associative `Record.edges`. We get that for free by
//! decoding to generic IPLD and collecting every `Ipld::Link`: both kinds
//! encode as DAG-CBOR tag-42 links, so there is no per-variant list to drift.

use crate::blockstore::Blockstore;
use crate::cid::Cid;
use ipld_core::ipld::Ipld;
use std::collections::HashSet;

/// Every CID link inside one block, structural or associative alike.
pub(crate) fn block_links<B: Blockstore + ?Sized>(
    store: &B,
    cid: &Cid,
) -> anyhow::Result<Vec<Cid>> {
    let bytes = store.get(cid)?;
    let ipld: Ipld = serde_ipld_dagcbor::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("dag-cbor decode of {cid} failed: {e}"))?;
    let mut links = Vec::new();
    collect_links(&ipld, &mut links);
    Ok(links)
}

fn collect_links(ipld: &Ipld, out: &mut Vec<Cid>) {
    match ipld {
        Ipld::Link(cid) => out.push(*cid),
        Ipld::Map(m) => {
            for v in m.values() {
                collect_links(v, out);
            }
        }
        Ipld::List(l) => {
            for v in l {
                collect_links(v, out);
            }
        }
        _ => {}
    }
}

/// Every *present* block CID reachable from `root` (including `root`),
/// deduplicated.
///
/// A block GC pruned on purpose is a frontier, not corruption: when a link
/// resolves to a tombstoned (intentionally deleted) block, we print the receipt
/// and stop there, excluding it from the closure — so `share` ships exactly the
/// blocks that still exist instead of crashing on a deleted ancestor. A block
/// that is missing WITHOUT a tombstone is still a loud error: that would make
/// sync ship a broken DAG. Content addressing keeps the graph acyclic; the
/// `visited` set collapses diamonds and guards the walk regardless.
pub fn reachable_from<B: Blockstore + ?Sized>(store: &B, root: &Cid) -> anyhow::Result<Vec<Cid>> {
    let mut visited: HashSet<Cid> = HashSet::new();
    let mut order: Vec<Cid> = Vec::new();
    let mut stack = vec![*root];
    while let Some(cid) = stack.pop() {
        if !visited.insert(cid) {
            continue;
        }
        match block_links(store, &cid) {
            Ok(links) => {
                order.push(cid);
                for link in links {
                    if !visited.contains(&link) {
                        stack.push(link);
                    }
                }
            }
            // The block isn't readable. If GC pruned it, that's a receipt of
            // truth — trace it and treat it as a frontier. Otherwise it's a
            // genuine hole in the DAG: propagate the error.
            Err(e) => match store.tombstone(&cid) {
                Some(t) => crate::trace::pruned(&cid, &t),
                None => return Err(e),
            },
        }
    }
    crate::trace::walk("reachable(root)", order.len());
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockstore::LocalBlocks;
    use crate::node::{Checkpoint, Edge, EdgeRel, Memory, MemoryKind, Node, Record, Skill, Source};
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    fn blocks() -> (TempDir, LocalBlocks) {
        let dir = TempDir::new().unwrap();
        let bs = LocalBlocks::new(dir.path().join("blocks"));
        (dir, bs)
    }

    fn mem(text: &str) -> Node {
        Node::Memory(Memory {
            text: text.into(),
            kind: MemoryKind::Project,
        })
    }

    /// Store a record built directly (so we can set source/edges) and return its CID.
    fn put(bs: &LocalBlocks, body: Node, source: Source, edges: Vec<Edge>) -> Cid {
        let rec = Record {
            schema_version: crate::node::CURRENT_SCHEMA_VERSION,
            created_at: 0,
            source,
            edges,
            body,
        };
        let bytes = crate::node::encode(&rec).unwrap();
        bs.put(&bytes).unwrap()
    }

    fn refs(to: Cid) -> Vec<Edge> {
        vec![Edge {
            rel: EdgeRel::References,
            to,
        }]
    }

    fn set(cids: Vec<Cid>) -> BTreeSet<Cid> {
        cids.into_iter().collect()
    }

    #[test]
    fn leaf_reaches_only_itself() {
        let (_d, bs) = blocks();
        let a = put(&bs, mem("a"), Source::User, vec![]);
        assert_eq!(reachable_from(&bs, &a).unwrap(), vec![a]);
    }

    #[test]
    fn follows_structural_links() {
        let (_d, bs) = blocks();
        let root = put(&bs, mem("root"), Source::User, vec![]);
        let cp = put(
            &bs,
            Node::Checkpoint(Checkpoint {
                label: "c".into(),
                root,
                parent: None,
            }),
            Source::System,
            vec![],
        );
        assert_eq!(set(reachable_from(&bs, &cp).unwrap()), set(vec![cp, root]));
    }

    #[test]
    fn follows_skill_supersedes_structural_link() {
        let (_d, bs) = blocks();
        let previous = put(
            &bs,
            Node::Skill(Skill {
                name: "review".into(),
                body: "old".into(),
                supersedes: None,
            }),
            Source::User,
            vec![],
        );
        let current = put(
            &bs,
            Node::Skill(Skill {
                name: "review".into(),
                body: "new".into(),
                supersedes: Some(previous),
            }),
            Source::User,
            vec![],
        );
        assert_eq!(
            set(reachable_from(&bs, &current).unwrap()),
            set(vec![current, previous])
        );
    }

    #[test]
    fn follows_associative_edges() {
        let (_d, bs) = blocks();
        let target = put(&bs, mem("target"), Source::User, vec![]);
        let src = put(
            &bs,
            mem("src"),
            Source::User,
            vec![Edge {
                rel: EdgeRel::Produced,
                to: target,
            }],
        );
        assert_eq!(
            set(reachable_from(&bs, &src).unwrap()),
            set(vec![src, target])
        );
    }

    #[test]
    fn follows_source_provenance_links() {
        let (_d, bs) = blocks();
        let origin = put(&bs, mem("origin"), Source::User, vec![]);
        let derived = put(
            &bs,
            mem("derived"),
            Source::Derived { from: vec![origin] },
            vec![],
        );
        assert_eq!(
            set(reachable_from(&bs, &derived).unwrap()),
            set(vec![derived, origin])
        );
    }

    #[test]
    fn diamond_is_deduplicated() {
        let (_d, bs) = blocks();
        let a = put(&bs, mem("a"), Source::User, vec![]);
        let b = put(&bs, mem("b"), Source::User, refs(a));
        let c = put(&bs, mem("c"), Source::User, refs(a));
        let d = put(
            &bs,
            mem("d"),
            Source::User,
            vec![
                Edge {
                    rel: EdgeRel::References,
                    to: b,
                },
                Edge {
                    rel: EdgeRel::References,
                    to: c,
                },
            ],
        );
        let reachable = reachable_from(&bs, &d).unwrap();
        assert_eq!(reachable.len(), 4, "A reached via two paths appears once");
        assert_eq!(set(reachable), set(vec![a, b, c, d]));
    }

    #[test]
    fn missing_root_errors() {
        let (_d, bs) = blocks();
        let missing = crate::cid::compute(b"never stored");
        assert!(reachable_from(&bs, &missing).is_err());
    }

    #[test]
    fn missing_linked_block_errors() {
        // Root is present but links to a block that was never stored: a loud
        // error, not a silently truncated closure.
        let (_d, bs) = blocks();
        let phantom = crate::cid::compute(b"unstored block");
        let src = put(&bs, mem("src"), Source::User, refs(phantom));
        assert!(reachable_from(&bs, &src).is_err());
    }
}
