//! Phase N · Phase E — Concurrent Graph Merge.
//!
//! The last piece of the convergence core (A–E). When two devices write while
//! disconnected, their namespace heads diverge. This module classifies that
//! divergence and resolves it **without ever discarding a valid branch** (after
//! OrbitDB's oplog: union + concurrent-head preservation, never last-writer-wins).
//!
//! ## Deterministic merge rules (plan §Deterministic Merge Rules)
//! 1. If one head is an **ancestor** of another, **fast-forward** to the descendant.
//! 2. If heads are **concurrent**, **retain both** until an authorized
//!    [`MergeCheckpoint`] is accepted.
//! 3. Immutable nodes union by CID (Phase D already does this).
//! 9. An automatic merge performs only lossless, deterministic steps; semantic
//!    conflicts are surfaced for review, not auto-resolved.
//!
//! A [`MergeCheckpoint`] is **multi-parent** (≥2 distinct heads), signed by an
//! author holding [`Operation::MergeAccept`], and **references** both histories
//! rather than rewriting them. Existing single-parent checkpoints are untouched.
//! Convergence property: given the same blocks + the same accepted merge, every
//! peer computes the same head, regardless of arrival order.
//!
//! Device-local publication locks are **not** part of any merge (they live in the
//! security overlay, never the DAG), and known-public exposure is tracked
//! independently — a merge can neither leak a lock nor erase exposure.

use serde::{Deserialize, Serialize};

use crate::binding::{cid_from_link, Cid, CidOrName, CoreBinding, MemCli, Node, Record};
use crate::capability::{verify_capability, Capability, Namespace, Operation};
use crate::error::{Error, Result as CoreResult};
use crate::identity::{verify as verify_sig, AgentId, Identity};
use crate::membership::{NetworkDescriptor, NetworkId, RevocationSet};

pub const MERGE_CHECKPOINT_VERSION: u32 = 1;
/// Bound on parent-chain traversal, so a malformed/cyclic chain can't hang a sync.
pub const MAX_ANCESTRY_DEPTH: usize = 100_000;

/// How two heads relate on the checkpoint parent-chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadRelation {
    /// The same head.
    Equal,
    /// `a` is an ancestor of `b` — fast-forward to `b`.
    AAncestorOfB,
    /// `b` is an ancestor of `a` — fast-forward to `a`.
    BAncestorOfA,
    /// Neither is an ancestor of the other — concurrent, needs a merge.
    Concurrent,
}

/// The result of resolving a set of heads (plan rules 1–2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// The surviving tip heads after collapsing ancestors — sorted, deduped. One
    /// head means the namespace has converged (a fast-forward); two or more means
    /// concurrent branches awaiting a [`MergeCheckpoint`].
    pub heads: Vec<String>,
    /// Whether any head was an ancestor that got fast-forwarded away.
    pub fast_forwarded: bool,
}

impl MergeOutcome {
    /// Has the namespace converged on a single head?
    pub fn converged(&self) -> bool {
        self.heads.len() <= 1
    }
}

impl MemCli {
    /// The single parent of a checkpoint, or `None` for a root checkpoint / a
    /// non-checkpoint node.
    pub fn checkpoint_parent(&self, cid: &Cid) -> CoreResult<Option<Cid>> {
        let Record::Live { body_json, .. } = self.get(&CidOrName::Cid(cid.clone()))? else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(&body_json)
            .map_err(|e| Error::Io(format!("parse checkpoint record: {e}")))?;
        let parent = &value["body"]["parent"];
        if parent.is_null() {
            Ok(None)
        } else {
            Ok(Some(cid_from_link(parent)?))
        }
    }

    /// Is `ancestor` reachable from `descendant` by following checkpoint parents?
    /// (`a == d` is not "ancestor" — see [`MemCli::classify_heads`].)
    pub fn is_ancestor(&self, ancestor: &Cid, descendant: &Cid) -> CoreResult<bool> {
        let mut current = self.checkpoint_parent(descendant)?;
        let mut depth = 0;
        while let Some(node) = current {
            if &node == ancestor {
                return Ok(true);
            }
            if depth >= MAX_ANCESTRY_DEPTH {
                break;
            }
            current = self.checkpoint_parent(&node)?;
            depth += 1;
        }
        Ok(false)
    }

    /// Classify two heads on the parent-chain.
    pub fn classify_heads(&self, a: &Cid, b: &Cid) -> CoreResult<HeadRelation> {
        if a == b {
            return Ok(HeadRelation::Equal);
        }
        if self.is_ancestor(a, b)? {
            return Ok(HeadRelation::AAncestorOfB);
        }
        if self.is_ancestor(b, a)? {
            return Ok(HeadRelation::BAncestorOfA);
        }
        Ok(HeadRelation::Concurrent)
    }

    /// Resolve a head set: collapse any head that is an ancestor of another
    /// (fast-forward, rule 1), keep the rest as concurrent tips (rule 2). The
    /// result is **deterministic** (sorted) and independent of input order, so
    /// every peer with the same blocks computes the same outcome.
    pub fn merge_heads(&self, heads: &[String]) -> CoreResult<MergeOutcome> {
        let mut unique: Vec<String> = heads.to_vec();
        unique.sort();
        unique.dedup();
        let cids: Vec<Cid> = unique.iter().cloned().map(Cid).collect();

        let mut tips = Vec::new();
        let mut fast_forwarded = false;
        for (i, candidate) in cids.iter().enumerate() {
            // Keep `candidate` only if it is not an ancestor of any other head.
            let mut is_ancestor_of_another = false;
            for (j, other) in cids.iter().enumerate() {
                if i != j && self.is_ancestor(candidate, other)? {
                    is_ancestor_of_another = true;
                    break;
                }
            }
            if is_ancestor_of_another {
                fast_forwarded = true;
            } else {
                tips.push(candidate.0.clone());
            }
        }
        tips.sort();
        tips.dedup();
        Ok(MergeOutcome {
            heads: tips,
            fast_forwarded,
        })
    }

    /// Record an accepted merge as a real, content-addressed node that **links both
    /// parents** as provenance (so the merged head walks into both histories). The
    /// CID is deterministic in the merge's fields, so two devices applying the same
    /// verified merge compute the **same** head and converge. Verifies the merge's
    /// authority first.
    pub fn apply_merge(
        &self,
        merge: &MergeCheckpoint,
        descriptor: &NetworkDescriptor,
        now: u64,
        revoked: &RevocationSet,
    ) -> CoreResult<Cid> {
        merge
            .verify(descriptor, now, revoked)
            .map_err(|e| Error::SecurityPolicy(format!("merge rejected: {e}")))?;
        // A deterministic body (no wall-clock / local state) so the merged head CID
        // is identical on every device that accepts this merge.
        let body = serde_json::json!({
            "question": format!("merge of {} heads in {}", merge.parents.len(), merge.namespace),
            "choice": merge.label,
            "rationale": merge.merge_policy,
        });
        let parents: Vec<Cid> = merge.parents.iter().cloned().map(Cid).collect();
        // Stamp the merge's own (deterministic, signed) timestamp, not the wall clock —
        // so every device that applies this merge derives the identical head CID.
        self.put_node_derived_at(
            &Node {
                kind: "decision".to_string(),
                fields_json: body.to_string(),
            },
            &parents,
            merge.created_at,
        )
    }
}

/// A signed, multi-parent merge record (plan §Multi-Parent Merge Checkpoint). It
/// references the concurrent heads it merges; it does not rewrite either history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCheckpoint {
    pub version: u32,
    pub network_id: NetworkId,
    pub namespace: String,
    pub label: String,
    /// The merged heads — **≥2 distinct, sorted** (parent order canonicalized
    /// before hashing so the record is reproducible).
    pub parents: Vec<String>,
    /// e.g. `"union-lossless"`. Only lossless/deterministic policies auto-apply.
    pub merge_policy: String,
    /// Semantic conflicts surfaced for review (never silently resolved).
    pub conflicts: Vec<String>,
    pub author_id: String,
    /// The author's capability proving `merge_accept` on the namespace.
    pub author_capability: Capability,
    pub created_at: u64,
    pub signature: String,
}

impl MergeCheckpoint {
    /// Build and sign a merge checkpoint over `parents` (deduped + sorted). The
    /// author must hold `merge_accept` on the namespace (checked at verify time).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        author: &Identity,
        network_id: &NetworkId,
        namespace: &Namespace,
        label: &str,
        parents: Vec<String>,
        merge_policy: &str,
        conflicts: Vec<String>,
        author_capability: Capability,
        created_at: u64,
    ) -> Self {
        let mut parents = parents;
        parents.sort();
        parents.dedup();
        let mut record = Self {
            version: MERGE_CHECKPOINT_VERSION,
            network_id: network_id.clone(),
            namespace: namespace.canonical(),
            label: label.to_string(),
            parents,
            merge_policy: merge_policy.to_string(),
            conflicts,
            author_id: author.agent_id().0,
            author_capability,
            created_at,
            signature: String::new(),
        };
        record.signature = author.sign(&record.signing_bytes());
        record
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("merge checkpoint serializes")
    }

    /// Verify the merge: ≥2 distinct sorted parents, the author holds `merge_accept`
    /// on the namespace (capability chains to a root, unexpired, in-epoch,
    /// unrevoked), and the signature is valid for this network.
    pub fn verify(
        &self,
        descriptor: &NetworkDescriptor,
        now: u64,
        revoked: &RevocationSet,
    ) -> Result<(), MergeError> {
        if self.version != MERGE_CHECKPOINT_VERSION {
            return Err(MergeError::Version);
        }
        if self.network_id != descriptor.network_id {
            return Err(MergeError::WrongNetwork);
        }
        if self.parents.len() < 2 {
            return Err(MergeError::TooFewParents);
        }
        let mut canonical = self.parents.clone();
        canonical.sort();
        canonical.dedup();
        if canonical != self.parents {
            return Err(MergeError::NonCanonicalParents);
        }
        let namespace = Namespace::parse(&self.namespace)
            .ok_or_else(|| MergeError::Malformed("namespace is not canonical".to_string()))?;
        verify_capability(&self.author_capability, &[], descriptor, now, revoked)?;
        if self.author_capability.subject_id != self.author_id {
            return Err(MergeError::AuthorMismatch);
        }
        if !self
            .author_capability
            .authorizes(Operation::MergeAccept, &namespace)
        {
            return Err(MergeError::NotAMerger);
        }
        if !verify_sig(
            &AgentId(self.author_id.clone()),
            &self.signing_bytes(),
            &self.signature,
        )
        .map_err(MergeError::Malformed)?
        {
            return Err(MergeError::BadSignature);
        }
        Ok(())
    }
}

/// Why a merge was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("unsupported merge checkpoint version")]
    Version,
    #[error("merge checkpoint is for a different network")]
    WrongNetwork,
    #[error("a merge checkpoint must reference at least two parents")]
    TooFewParents,
    #[error("parents are not a canonical (sorted, deduped) set")]
    NonCanonicalParents,
    #[error("the author's capability is invalid: {0}")]
    Capability(#[from] crate::capability::CapabilityError),
    #[error("the author's capability belongs to a different identity than the author")]
    AuthorMismatch,
    #[error("the author's capability does not grant merge_accept on this namespace")]
    NotAMerger,
    #[error("merge checkpoint signature does not verify")]
    BadSignature,
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::NamespaceScope;
    use crate::membership::NetworkDescriptor;

    const DAY: u64 = 24 * 3600;

    fn net() -> (Identity, NetworkDescriptor, Namespace) {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(
            descriptor.network_id.clone(),
            NamespaceScope::Project("atlas".into()),
        );
        (root, descriptor, ns)
    }

    fn merger_cap(root: &Identity, ns: &Namespace, subject: &str, epoch: u64) -> Capability {
        Capability::issue(
            root,
            ns.clone(),
            subject,
            vec![Operation::MergeAccept],
            1000,
            DAY,
            epoch,
            false,
        )
    }

    /// A base checkpoint and two concurrent children over it, in one store.
    fn diverged() -> (tempfile::TempDir, MemCli, Cid, Cid, Cid) {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let n0 = mem
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"base","kind":"reference"}"#.into(),
            })
            .unwrap();
        let base = mem.checkpoint("base", &n0, None).unwrap();
        // Two concurrent writes, each a checkpoint whose parent is `base`.
        let na = mem
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"branch-a","kind":"reference"}"#.into(),
            })
            .unwrap();
        let ca = mem.checkpoint("a", &na, Some(&base)).unwrap();
        let nb = mem
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"branch-b","kind":"reference"}"#.into(),
            })
            .unwrap();
        let cb = mem.checkpoint("b", &nb, Some(&base)).unwrap();
        (dir, mem, base, ca, cb)
    }

    #[test]
    fn an_ancestor_fast_forwards_and_concurrent_heads_are_preserved() {
        let (_d, mem, base, ca, cb) = diverged();
        // base is an ancestor of ca → fast-forward.
        assert_eq!(
            mem.classify_heads(&base, &ca).unwrap(),
            HeadRelation::AAncestorOfB
        );
        let ff = mem.merge_heads(&[base.0.clone(), ca.0.clone()]).unwrap();
        assert_eq!(
            ff.heads.as_slice(),
            std::slice::from_ref(&ca.0),
            "the descendant wins the fast-forward"
        );
        assert!(ff.converged() && ff.fast_forwarded);

        // ca and cb are concurrent → both preserved.
        assert_eq!(
            mem.classify_heads(&ca, &cb).unwrap(),
            HeadRelation::Concurrent
        );
        let mut both = [ca.0.clone(), cb.0.clone()];
        both.sort();
        let outcome = mem.merge_heads(&[ca.0.clone(), cb.0.clone()]).unwrap();
        assert_eq!(
            outcome.heads, both,
            "concurrent heads are retained, not dropped"
        );
        assert!(
            !outcome.converged(),
            "still two heads → needs a merge checkpoint"
        );
    }

    #[test]
    fn merge_resolution_is_deterministic_regardless_of_input_order() {
        let (_d, mem, base, ca, cb) = diverged();
        let one = mem
            .merge_heads(&[base.0.clone(), ca.0.clone(), cb.0.clone()])
            .unwrap();
        let two = mem
            .merge_heads(&[cb.0.clone(), base.0.clone(), ca.0.clone()])
            .unwrap();
        assert_eq!(one, two, "same heads in any order → same outcome");
        // base collapses (ancestor of both); ca and cb remain.
        assert_eq!(one.heads.len(), 2);
    }

    #[test]
    fn a_merge_checkpoint_needs_two_parents_and_merge_accept_authority() {
        let (root, descriptor, ns) = net();
        let author = Identity::generate();
        let cap = merger_cap(
            &root,
            &ns,
            &author.agent_id().0,
            descriptor.membership_epoch,
        );

        // Fewer than two parents is not a merge.
        let one_parent = MergeCheckpoint::create(
            &author,
            &descriptor.network_id,
            &ns,
            "m",
            vec!["A".into()],
            "union-lossless",
            vec![],
            cap.clone(),
            2000,
        );
        assert_eq!(
            one_parent.verify(&descriptor, 2000, &RevocationSet::new()),
            Err(MergeError::TooFewParents)
        );

        // A valid two-parent merge by a merge_accept holder verifies.
        let valid = MergeCheckpoint::create(
            &author,
            &descriptor.network_id,
            &ns,
            "m",
            vec!["A".into(), "B".into()],
            "union-lossless",
            vec![],
            cap,
            2000,
        );
        assert!(valid
            .verify(&descriptor, 2000, &RevocationSet::new())
            .is_ok());

        // A reader (no merge_accept) cannot author a merge.
        let reader = Identity::generate();
        let read_cap = Capability::issue(
            &root,
            ns.clone(),
            &reader.agent_id().0,
            vec![Operation::SyncRead],
            1000,
            DAY,
            descriptor.membership_epoch,
            false,
        );
        let forged = MergeCheckpoint::create(
            &reader,
            &descriptor.network_id,
            &ns,
            "m",
            vec!["A".into(), "B".into()],
            "union-lossless",
            vec![],
            read_cap,
            2000,
        );
        assert_eq!(
            forged.verify(&descriptor, 2000, &RevocationSet::new()),
            Err(MergeError::NotAMerger)
        );
    }

    #[test]
    fn two_devices_converge_after_an_explicit_merge_without_merging_local_locks() {
        // The Phase E exit criterion: two devices write while disconnected, sync
        // both branches, and converge after an explicit valid merge — and a
        // device-local lock is not part of the merge.
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(
            descriptor.network_id.clone(),
            NamespaceScope::Project("atlas".into()),
        );

        // Shared base, created once and synchronized before the devices disconnect.
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        let n0 = mem_a
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"base","kind":"reference"}"#.into(),
            })
            .unwrap();
        let base_a = mem_a.checkpoint("base", &n0, None).unwrap();
        use crate::sync::SyncLimits;
        mem_b
            .pull_reachable(
                std::slice::from_ref(&base_a.0),
                |cid| mem_a.read_block(&Cid(cid.to_string())).ok(),
                SyncLimits::default(),
            )
            .unwrap();
        let base_b = base_a.clone();

        // Disconnected concurrent writes.
        let na = mem_a
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"branch-a","kind":"reference"}"#.into(),
            })
            .unwrap();
        let ca = mem_a.checkpoint("a", &na, Some(&base_a)).unwrap();
        let nb = mem_b
            .put_node(&Node {
                kind: "memory".into(),
                fields_json: r#"{"text":"branch-b","kind":"reference"}"#.into(),
            })
            .unwrap();
        let cb = mem_b.checkpoint("b", &nb, Some(&base_b)).unwrap();

        // A device-local publication lock on A — must never enter merge state.
        mem_a.lock_subgraph(&ca, "local-only").ok();

        // Reconnect: each device pulls the other's branch (Phase D sync).
        mem_a
            .pull_reachable(
                std::slice::from_ref(&cb.0),
                |cid| mem_b.read_block(&Cid(cid.to_string())).ok(),
                SyncLimits::default(),
            )
            .unwrap();
        mem_b
            .pull_reachable(
                std::slice::from_ref(&ca.0),
                |cid| mem_a.read_block(&Cid(cid.to_string())).ok(),
                SyncLimits::default(),
            )
            .unwrap();

        // Both see the same concurrent head set.
        let heads_a = mem_a.merge_heads(&[ca.0.clone(), cb.0.clone()]).unwrap();
        let heads_b = mem_b.merge_heads(&[ca.0.clone(), cb.0.clone()]).unwrap();
        assert_eq!(heads_a, heads_b);
        assert!(
            !heads_a.converged(),
            "two concurrent branches before the merge"
        );

        // An authorized author creates one merge; both devices apply it.
        let author = Identity::generate();
        let cap = merger_cap(
            &root,
            &ns,
            &author.agent_id().0,
            descriptor.membership_epoch,
        );
        let merge = MergeCheckpoint::create(
            &author,
            &descriptor.network_id,
            &ns,
            "merge a+b",
            vec![ca.0.clone(), cb.0.clone()],
            "union-lossless",
            vec![],
            cap,
            3000,
        );
        let head_a = mem_a
            .apply_merge(&merge, &descriptor, 3000, &RevocationSet::new())
            .unwrap();
        let head_b = mem_b
            .apply_merge(&merge, &descriptor, 3000, &RevocationSet::new())
            .unwrap();

        // Converged: identical merged head on both, reaching BOTH branches.
        assert_eq!(
            head_a, head_b,
            "both devices converge on the same merged head"
        );
        let reachable: Vec<String> = mem_a
            .walk(&head_a)
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert!(
            reachable.contains(&ca.0) && reachable.contains(&cb.0),
            "the merge references both histories"
        );

        // The local lock did not become merge state. A holds a lock on its own
        // branch `ca`; B (which synced the `ca` *block*) has no lock on it — the
        // device-local overlay never synchronized or merged.
        assert!(
            mem_a.locks().unwrap().iter().any(|l| l.root == ca.0),
            "A holds its device-local lock"
        );
        assert!(mem_b.has_block(&ca.0), "B did sync the ca block");
        assert!(
            !mem_b.locks().unwrap().iter().any(|l| l.root == ca.0),
            "A's device-local lock did not synchronize or merge to B",
        );
    }
}
