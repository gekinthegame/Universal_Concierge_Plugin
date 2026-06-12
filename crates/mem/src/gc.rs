//! Phase 5.3 — retention / garbage collection.
//!
//! Two jobs, both safe by construction over the immutable DAG:
//!
//! 1. **Trim the auto-checkpoint chain** to the newest N. Auto-checkpoints
//!    chain via `Checkpoint.parent`, so left alone they accrete one tiny block
//!    per turn forever. GC walks that chain from the well-known head (`latest`)
//!    and deletes the `Checkpoint` blocks past N. Their root `Conversation`s
//!    stay reachable through the conversation `parent` chain, so trimming frees
//!    only the checkpoint nodes themselves — never conversation history.
//!
//! 2. **Sweep orphans** — blocks no live name, kept checkpoint, or `Decision`
//!    can reach (abandoned `mem put`s, heads rebound away, blocks of deleted
//!    names). This is git-gc's mark-and-sweep: delete only the unreachable.
//!
//! Every deletion is recorded as a [`Tombstone`] — a receipt of truth. A later
//! walk that crosses a pruned link reports the receipt and stops there, instead
//! of treating the (intentional) absence as corruption.
//!
//! GC never deletes a block reachable from a kept root, so the kept closure is
//! always complete: the only links it leaves dangling are checkpoint `parent`
//! pointers into the trimmed tail, which resolve to tombstones, not holes.

use crate::blockstore::{Blockstore, LocalBlocks};
use crate::cid::Cid;
use crate::names::NameIndex;
use crate::node::Node;
use crate::tombstones::{Tombstone, Tombstones};
use std::collections::HashSet;

/// What to retain. Derived from `[checkpoint]` config.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Keep this many newest auto-checkpoints in the chain; trim the rest.
    pub keep_checkpoints: usize,
    /// The well-known name pointing at the chain head (default `latest`).
    pub checkpoint_name: String,
}

/// The outcome of one GC pass.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub scanned: usize,
    pub kept: usize,
    pub pruned_checkpoints: Vec<Cid>,
    pub pruned_orphans: Vec<Cid>,
}

impl GcReport {
    pub fn pruned_total(&self) -> usize {
        self.pruned_checkpoints.len() + self.pruned_orphans.len()
    }
}

/// Run one GC pass: trim the checkpoint chain, sweep orphans, record tombstones.
///
/// `now` stamps each tombstone's time of death (injectable for deterministic
/// tests). Deletes blocks from `blocks` and persists the death certificates to
/// `tombstones` in a single write.
pub fn collect(
    blocks: &LocalBlocks,
    names: &NameIndex,
    tombstones: &mut Tombstones,
    policy: &RetentionPolicy,
    now: u64,
) -> anyhow::Result<GcReport> {
    let on_disk = blocks.iter_cids()?;

    // Named roots: every explicit binding survives, along with its closure.
    let named: HashSet<Cid> = names.iter().map(|(_, cid)| *cid).collect();

    // Enumerate the auto-checkpoint chain from the configured head, newest
    // first. The newest `keep` survive; the tail beyond them is trimmed —
    // unless a checkpoint is also bound by an explicit name (then it stays).
    let chain = checkpoint_chain(blocks, names, &policy.checkpoint_name)?;
    let keep_n = policy.keep_checkpoints;
    let surviving_tip = chain.get(keep_n.saturating_sub(1)).copied();
    let trim: HashSet<Cid> = chain
        .iter()
        .skip(keep_n)
        .copied()
        .filter(|cid| !named.contains(cid))
        .collect();

    // Decisions are kept wherever they live — a settled call is never garbage.
    // Finding them means decoding each block. Only *records* decode as a `Node`;
    // HAMT internal nodes, blobs, and other structural blocks don't — and a
    // non-record can never be a Decision, so skip a block that fails to decode
    // rather than aborting GC on any store that contains them (it always does).
    let mut decisions: Vec<Cid> = Vec::new();
    for cid in &on_disk {
        let bytes = blocks.get(cid)?;
        if let Ok(record) = crate::node::decode(&bytes) {
            if matches!(record.body, Node::Decision(_)) {
                decisions.push(*cid);
            }
        }
    }

    // Mark: the reachable closure of every root, refusing to cross into the
    // trimmed tail (so old checkpoints actually become collectable).
    let roots = named
        .iter()
        .copied()
        .chain(chain.iter().take(keep_n).copied())
        .chain(decisions.iter().copied());
    let keep = mark(blocks, roots, &trim)?;

    // Sweep: everything on disk that isn't kept. A trimmed checkpoint points
    // forward to the surviving tip; a plain orphan just records its death.
    let mut report = GcReport {
        scanned: on_disk.len(),
        kept: keep.len(),
        ..Default::default()
    };
    let mut deaths: Vec<(Cid, Tombstone)> = Vec::new();
    for cid in &on_disk {
        if keep.contains(cid) {
            continue;
        }
        let tombstone = if trim.contains(cid) {
            report.pruned_checkpoints.push(*cid);
            Tombstone {
                pruned_at: now,
                reason: format!("auto-checkpoint trimmed (keep={keep_n})"),
                superseded_by: surviving_tip,
            }
        } else {
            report.pruned_orphans.push(*cid);
            Tombstone {
                pruned_at: now,
                reason: "orphan".to_string(),
                superseded_by: None,
            }
        };
        deaths.push((*cid, tombstone));
    }

    // Persist receipts before deleting any block. If the process dies after the
    // ledger write, lookup still prefers the present block; if it dies after a
    // delete, the tombstone already explains the absence.
    tombstones.record(deaths.iter().cloned())?;
    for (cid, tombstone) in &deaths {
        blocks.remove(cid)?;
        crate::trace::pruned(cid, tombstone);
    }
    crate::trace::walk("gc kept(roots)", report.kept);
    Ok(report)
}

/// Walk the `Checkpoint.parent` chain from the head bound to `name`, newest
/// first. Stops at the first non-checkpoint (or already-pruned) link. An empty
/// vec means the name isn't bound or doesn't point at a checkpoint.
fn checkpoint_chain(
    blocks: &LocalBlocks,
    names: &NameIndex,
    name: &str,
) -> anyhow::Result<Vec<Cid>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = names.resolve(name).ok();
    while let Some(cid) = cursor {
        if !seen.insert(cid) || !blocks.has(&cid)? {
            break;
        }
        let record = blocks
            .get(&cid)
            .and_then(|bytes| crate::node::decode(&bytes))?;
        match record.body {
            Node::Checkpoint(checkpoint) => {
                chain.push(cid);
                cursor = checkpoint.parent;
            }
            _ => break,
        }
    }
    Ok(chain)
}

/// Mark-phase reachability: the closure of `roots`, never entering `cut`.
///
/// `cut` is the trimmed checkpoint tail. A link into it (e.g. the surviving
/// tip's `parent`) is simply not followed, so those blocks fall out of the keep
/// set and become collectable; everything else reachable is retained. A link to
/// an already-pruned block (from a prior pass) is a tombstoned frontier and is
/// likewise skipped, not an error.
fn mark(
    blocks: &LocalBlocks,
    roots: impl IntoIterator<Item = Cid>,
    cut: &HashSet<Cid>,
) -> anyhow::Result<HashSet<Cid>> {
    let mut keep: HashSet<Cid> = HashSet::new();
    let mut stack: Vec<Cid> = roots.into_iter().filter(|c| !cut.contains(c)).collect();
    while let Some(cid) = stack.pop() {
        if cut.contains(&cid) || !keep.insert(cid) {
            continue;
        }
        match crate::dag::block_links(blocks, &cid) {
            Ok(links) => {
                for link in links {
                    if !keep.contains(&link) && !cut.contains(&link) {
                        stack.push(link);
                    }
                }
            }
            // A kept root/link whose block is gone: fine only if GC pruned it
            // before (a tombstone). Otherwise it's a real hole — surface it.
            Err(e) => {
                keep.remove(&cid);
                if blocks.tombstone(&cid).is_none() {
                    return Err(e);
                }
            }
        }
    }
    Ok(keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Checkpoint, Conversation, Decision, Memory, MemoryKind, Source};
    use crate::store::{Clock, Store};
    use crate::tombstones::Tombstones;
    use tempfile::TempDir;

    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn now_unix(&self) -> anyhow::Result<u64> {
            Ok(self.0)
        }
    }

    /// A store backed by a real temp dir so GC can scan/delete files, with a
    /// fixed clock so checkpoint CIDs and tombstone times are deterministic.
    fn store(now: u64) -> (TempDir, Store<LocalBlocks, FixedClock>) {
        let dir = TempDir::new().unwrap();
        let blocks = LocalBlocks::new(dir.path().join("blocks"));
        let names = NameIndex::load(dir.path().join("names.json")).unwrap();
        (dir, Store::with_clock(blocks, names, FixedClock(now)))
    }

    fn policy(keep: usize) -> RetentionPolicy {
        RetentionPolicy {
            keep_checkpoints: keep,
            checkpoint_name: "latest".to_string(),
        }
    }

    fn memory(text: &str) -> Node {
        Node::Memory(Memory {
            text: text.into(),
            kind: MemoryKind::Project,
        })
    }

    /// Build a chain of N auto-checkpoints, each over its own conversation head
    /// chained to the prior one, rebinding `latest` each step — exactly what the
    /// session's auto-checkpoint observer produces. Returns CIDs newest-first.
    fn build_chain(store: &mut Store<LocalBlocks, FixedClock>, n: usize) -> Vec<Cid> {
        let mut checkpoints = Vec::new();
        let mut head: Option<Cid> = None;
        let mut last_checkpoint: Option<Cid> = None;
        for i in 0..n {
            let turn = store
                .put_node(memory(&format!("turn {i}")), Source::User)
                .unwrap();
            let convo = store
                .put_node(
                    Node::Conversation(Conversation {
                        turns: vec![turn],
                        parent: head,
                    }),
                    Source::System,
                )
                .unwrap();
            store.bind("conversation", convo).unwrap();
            head = Some(convo);
            let cp = store
                .put_node(
                    Node::Checkpoint(Checkpoint {
                        label: "auto".into(),
                        root: convo,
                        parent: last_checkpoint,
                    }),
                    Source::System,
                )
                .unwrap();
            store.bind("latest", cp).unwrap();
            last_checkpoint = Some(cp);
            checkpoints.push(cp);
        }
        checkpoints.reverse(); // newest first
        checkpoints
    }

    #[test]
    fn trims_old_checkpoints_keeping_newest_n_and_tombstones_them() {
        let (_d, mut s) = store(1000);
        let chain = build_chain(&mut s, 5); // newest first: [c0,c1,c2,c3,c4]

        let report = s.gc(&policy(2)).unwrap();

        // c2,c3,c4 trimmed; c0,c1 kept.
        assert_eq!(report.pruned_checkpoints.len(), 3);
        for trimmed in &chain[2..] {
            assert!(!s.has_block(trimmed).unwrap(), "old checkpoint deleted");
        }
        for kept in &chain[..2] {
            assert!(s.has_block(kept).unwrap(), "newest N checkpoints survive");
        }

        // The tombstone is a receipt pointing forward to the surviving tip (c1,
        // the oldest kept), with a time of death.
        let tombstones = s.tombstones().unwrap();
        let receipt = tombstones.get(&chain[4]).unwrap();
        assert_eq!(receipt.pruned_at, 1000);
        assert_eq!(receipt.superseded_by, Some(chain[1]));
        assert!(receipt.reason.contains("keep=2"));
    }

    #[test]
    fn trimming_preserves_conversation_history() {
        // Even after trimming old checkpoints, every conversation turn stays
        // reachable from `latest` (via the conversation parent chain) — so a
        // resume from the kept tip still sees full history.
        let (_d, mut s) = store(7);
        let chain = build_chain(&mut s, 4);
        s.gc(&policy(1)).unwrap();

        // The single kept checkpoint reaches every surviving block, with no
        // hole (the tombstoned parent frontier is tolerated, not an error).
        let reachable = s.reachable(&chain[0]).unwrap();
        // 1 checkpoint + 4 conversations + 4 turns = 9 present blocks.
        assert_eq!(reachable.len(), 9, "full conversation history retained");
    }

    #[test]
    fn gc_tolerates_non_node_blocks_like_hamt_internal_nodes() {
        // Regression: GC scanned EVERY on-disk block as a `Node` to find Decisions
        // and aborted (`dag-cbor version probe failed`) on the first that didn't
        // decode — HAMT internal nodes, blobs, index blocks, which every real store
        // has. A non-record can't be a Decision, so GC must skip it, not abort.
        let (dir, mut s) = store(5);
        let named = s.put_node(memory("kept by name"), Source::User).unwrap();
        s.bind("keep", named).unwrap();

        // Inject a raw, non-`Node` block into the same blocks dir — a stand-in for a
        // HAMT internal node: storable, content-addressed, but not a record.
        let raw_blocks = LocalBlocks::new(dir.path().join("blocks"));
        let raw = raw_blocks
            .put(b"not a node, just structural bytes")
            .unwrap();

        // GC completes (previously this returned Err and aborted), keeps the named
        // record, and sweeps the unreferenced non-Node block as an orphan.
        let report = s.gc(&policy(10)).unwrap();
        assert!(s.has_block(&named).unwrap(), "named record kept");
        assert!(!s.has_block(&raw).unwrap(), "non-Node orphan reclaimed");
        assert!(report.pruned_orphans.contains(&raw));
    }

    #[test]
    fn sweeps_orphans_but_keeps_named_and_decisions() {
        let (_d, mut s) = store(5);

        // An orphan: put but never bound, nothing links to it.
        let orphan = s.put_node(memory("abandoned"), Source::User).unwrap();
        // A named memory: bound, so kept.
        let named = s.put_node(memory("kept by name"), Source::User).unwrap();
        s.bind("current-project", named).unwrap();
        // A Decision: kept wherever it is, even unbound.
        let decision = s
            .put_node(
                Node::Decision(Decision {
                    question: "trim?".into(),
                    choice: "keep last N".into(),
                    rationale: "immutability".into(),
                }),
                Source::System,
            )
            .unwrap();

        let report = s.gc(&policy(10)).unwrap();

        assert!(!s.has_block(&orphan).unwrap(), "orphan swept");
        assert!(s.has_block(&named).unwrap(), "named block kept");
        assert!(s.has_block(&decision).unwrap(), "decision kept");
        assert_eq!(report.pruned_orphans, vec![orphan]);

        let receipt = s.tombstones().unwrap();
        let t = receipt.get(&orphan).unwrap();
        assert_eq!(t.reason, "orphan");
        assert_eq!(t.superseded_by, None);
    }

    #[test]
    fn failed_tombstone_write_does_not_delete_blocks() {
        let (dir, s) = store(5);
        let orphan = s.put_node(memory("abandoned"), Source::User).unwrap();
        let blocks = LocalBlocks::new(dir.path().join("blocks"));
        let names = NameIndex::load(dir.path().join("names.json")).unwrap();

        let blocker = dir.path().join("not-a-directory");
        let mut tombstones = Tombstones::load(blocker.join("tombstones.json")).unwrap();
        std::fs::write(&blocker, "x").unwrap();

        assert!(
            collect(&blocks, &names, &mut tombstones, &policy(10), 5).is_err(),
            "ledger write must fail before any block deletion"
        );
        assert!(
            s.has_block(&orphan).unwrap(),
            "GC must not delete without a durable tombstone receipt"
        );
    }

    #[test]
    fn keeps_everything_when_chain_is_within_budget() {
        let (_d, mut s) = store(0);
        let chain = build_chain(&mut s, 3);
        let report = s.gc(&policy(10)).unwrap();
        assert_eq!(report.pruned_checkpoints.len(), 0);
        for cp in &chain {
            assert!(s.has_block(cp).unwrap());
        }
    }

    #[test]
    fn gc_is_idempotent() {
        let (_d, mut s) = store(3);
        build_chain(&mut s, 5);
        let first = s.gc(&policy(2)).unwrap();
        assert!(first.pruned_total() > 0);
        let second = s.gc(&policy(2)).unwrap();
        assert_eq!(
            second.pruned_total(),
            0,
            "a second pass finds nothing new to prune"
        );
    }

    #[test]
    fn empty_store_is_a_noop() {
        let (_d, s) = store(0);
        let report = s.gc(&policy(10)).unwrap();
        assert_eq!(report.scanned, 0);
        assert_eq!(report.pruned_total(), 0);
    }

    /// A named binding pointing at an old checkpoint protects it from the trim.
    #[test]
    fn an_explicitly_named_old_checkpoint_survives_trimming() {
        let (_d, mut s) = store(9);
        let chain = build_chain(&mut s, 5);
        s.bind("milestone", chain[4]).unwrap(); // pin the oldest

        let report = s.gc(&policy(2)).unwrap();
        assert!(
            s.has_block(&chain[4]).unwrap(),
            "named old checkpoint is not trimmed"
        );
        assert!(!report.pruned_checkpoints.contains(&chain[4]));
    }
}
