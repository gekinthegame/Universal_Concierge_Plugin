//! Phase N · Phase D — Signed Heads and Block Synchronization.
//!
//! Where two authorized devices actually **converge**. This module is the protocol
//! *logic* — signed head advertisement, head reconciliation (exchange-heads-then-
//! converge, after OrbitDB `src/sync.js` but signed + capability-scoped, never open
//! pubsub), and bounded, CID-verified block import. The libp2p request-response
//! wire handlers live in `crates/net` and are wired in Phase F; here a peer is just
//! a `fetch` closure, so the whole reconciliation is unit-testable offline.
//!
//! ## Invariants (plan §Synchronization Protocol / §Signed Head Advertisement)
//! - A [`HeadRecord`] is signed and only valid if its signer holds
//!   [`Operation::SyncWrite`] on the namespace (the embedded capability chains to a
//!   network root). Stale-epoch, wrong-network, revoked, or forged records reject.
//! - `heads[]` is a **set** — concurrent valid heads are expected and preserved
//!   (real merge is Phase E); reconciliation never drops a peer's head.
//! - Every received block is **CID-verified before it is durably imported**
//!   ([`MemCli::store_verified_raw_block`]); a tampered block cannot enter the store.
//! - Transfer is **bounded** (max block count + max bytes) and **deduplicated**
//!   (already-present CIDs are never refetched) — so a hostile peer cannot exhaust
//!   resources or smuggle a wrong-CID block past the content-addressing check.
//! - The set of CIDs to converge is an authorized **manifest** (for an
//!   encrypted-private namespace this is the ciphertext manifest from
//!   [`crate::private_sync`], so only ciphertext is ever exchanged).

use serde::{Deserialize, Serialize};

use crate::binding::{Cid, MemCli};
use crate::capability::{verify_capability, Capability, Namespace, Operation};
use crate::error::{Error, Result as CoreResult};
use crate::identity::{verify as verify_sig, AgentId, Identity};
use crate::membership::{NetworkDescriptor, NetworkId, RevocationSet};

pub const HEAD_RECORD_VERSION: u32 = 1;

/// Conservative default transfer bounds. A caller may tighten them per request.
pub const DEFAULT_MAX_BLOCKS: usize = 100_000;
pub const DEFAULT_MAX_BYTES: u64 = 1 << 30; // 1 GiB

/// A signed advertisement of a namespace's current head set (plan §Signed Head
/// Advertisement). The signer's `sync_write` capability is embedded so a receiver
/// can verify authority from material in the record alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRecord {
    pub version: u32,
    pub network_id: NetworkId,
    /// Canonical namespace string (`network:{id}:…`).
    pub namespace: String,
    /// The current heads — a **set** (sorted, deduped); concurrency is expected.
    pub heads: Vec<String>,
    /// Per-signer monotonic sequence; does **not** establish a global order.
    pub sequence: u64,
    pub membership_epoch: u64,
    pub capability_epoch: u64,
    pub signer_id: String,
    /// The signer's capability proving `sync_write` on `namespace` (the
    /// "signer_certificate"); chains to a network root.
    pub signer_capability: Capability,
    pub timestamp: u64,
    pub signature: String,
}

impl HeadRecord {
    /// Build and sign a head record. `signer_capability` must grant `sync_write` on
    /// `namespace` (checked at verify time, not here).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        signer: &Identity,
        network_id: &NetworkId,
        namespace: &Namespace,
        mut heads: Vec<String>,
        sequence: u64,
        membership_epoch: u64,
        capability_epoch: u64,
        signer_capability: Capability,
        timestamp: u64,
    ) -> Self {
        heads.sort();
        heads.dedup();
        let mut record = Self {
            version: HEAD_RECORD_VERSION,
            network_id: network_id.clone(),
            namespace: namespace.canonical(),
            heads,
            sequence,
            membership_epoch,
            capability_epoch,
            signer_id: signer.agent_id().0,
            signer_capability,
            timestamp,
            signature: String::new(),
        };
        record.signature = signer.sign(&record.signing_bytes());
        record
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.signature = String::new();
        serde_json::to_vec(&clone).expect("head record serializes")
    }
}

/// Why a head record was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("unsupported head record version")]
    Version,
    #[error("head record is for a different network")]
    WrongNetwork,
    #[error("the signer's capability is invalid: {0}")]
    Capability(#[from] crate::capability::CapabilityError),
    #[error("the signer's capability does not grant sync_write on this namespace")]
    NotAWriter,
    #[error("the signer's capability belongs to a different identity than the signer")]
    SignerMismatch,
    #[error("head record signature does not verify")]
    BadSignature,
    #[error("heads are not a canonical (sorted, deduped) set")]
    NonCanonicalHeads,
    #[error("transfer exceeded its bound: {0}")]
    LimitExceeded(&'static str),
    #[error("malformed identity material: {0}")]
    Malformed(String),
}

/// Verify a [`HeadRecord`]: the signer holds `sync_write` on the namespace (their
/// embedded capability chains to a root, is unexpired, in-epoch, unrevoked), the
/// record is correctly signed for this network, and `heads` is canonical. The peer
/// gets to advertise only what it is authorized to write.
pub fn verify_head_record(
    record: &HeadRecord,
    descriptor: &NetworkDescriptor,
    now: u64,
    revoked: &RevocationSet,
) -> Result<(), SyncError> {
    if record.version != HEAD_RECORD_VERSION {
        return Err(SyncError::Version);
    }
    if record.network_id != descriptor.network_id {
        return Err(SyncError::WrongNetwork);
    }
    let namespace = Namespace::parse(&record.namespace)
        .ok_or_else(|| SyncError::Malformed("namespace is not canonical".to_string()))?;
    // The signer's authority: a valid capability granting sync_write here.
    verify_capability(&record.signer_capability, &[], descriptor, now, revoked)?;
    if record.signer_capability.subject_id != record.signer_id {
        return Err(SyncError::SignerMismatch);
    }
    if !record.signer_capability.authorizes(Operation::SyncWrite, &namespace) {
        return Err(SyncError::NotAWriter);
    }
    // Heads must be a canonical set (so the record can't hide a fork by reordering).
    let mut canonical = record.heads.clone();
    canonical.sort();
    canonical.dedup();
    if canonical != record.heads {
        return Err(SyncError::NonCanonicalHeads);
    }
    if !verify_sig(&AgentId(record.signer_id.clone()), &record.signing_bytes(), &record.signature)
        .map_err(SyncError::Malformed)?
    {
        return Err(SyncError::BadSignature);
    }
    Ok(())
}

/// The result of reconciling local heads against a peer's advertised heads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// The union of both head sets — concurrent heads are **preserved**, never
    /// dropped (real merge into one head is Phase E).
    pub converged_heads: Vec<String>,
    /// Peer heads we do not have locally yet — the roots to pull from.
    pub missing_heads: Vec<String>,
}

/// Exchange-heads-then-converge (the head-level step). `local_has` answers whether
/// a CID is already present locally. The block pull then walks down from
/// `missing_heads` via [`MemCli::pull_blocks`].
pub fn reconcile(
    local_heads: &[String],
    remote: &HeadRecord,
    local_has: impl Fn(&str) -> bool,
) -> Reconciliation {
    let mut converged: Vec<String> = local_heads.iter().chain(remote.heads.iter()).cloned().collect();
    converged.sort();
    converged.dedup();
    let missing_heads = remote.heads.iter().filter(|h| !local_has(h)).cloned().collect();
    Reconciliation { converged_heads: converged, missing_heads }
}

/// Bounds on a block transfer (plan §Request-Response Requirements).
#[derive(Debug, Clone, Copy)]
pub struct SyncLimits {
    pub max_blocks: usize,
    pub max_bytes: u64,
}

impl Default for SyncLimits {
    fn default() -> Self {
        Self { max_blocks: DEFAULT_MAX_BLOCKS, max_bytes: DEFAULT_MAX_BYTES }
    }
}

/// What a pull moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub imported: usize,
    pub bytes: u64,
    pub already_present: usize,
}

impl MemCli {
    /// Whether a block is already in the local store.
    pub fn has_block(&self, cid: &str) -> bool {
        self.read_block(&Cid(cid.to_string())).is_ok()
    }

    /// The raw bytes of a local block, or `None` if absent — the serve side of the
    /// Phase F sync protocol. For encrypted-private namespaces these are inert
    /// ciphertext; the head/manifest layer (Phase C/D) governs which CIDs a peer
    /// ever learns to ask for.
    pub fn block_bytes(&self, cid: &str) -> Option<Vec<u8>> {
        self.read_block(&Cid(cid.to_string())).ok()
    }

    /// Import one block received from a peer, **verifying its CID before durable
    /// commit** (a wrong-CID block is rejected). Idempotent. The public single-block
    /// import the async sync driver uses (the bulk path is [`MemCli::pull_blocks`]).
    pub fn import_verified_block(&self, cid: &str, bytes: &[u8]) -> CoreResult<()> {
        self.store_verified_raw_block(&Cid(cid.to_string()), bytes)
    }

    /// Which CIDs of `manifest` are missing locally — the bounded fetch list.
    pub fn missing_blocks(&self, manifest: &[String]) -> Vec<String> {
        manifest.iter().filter(|cid| !self.has_block(cid)).cloned().collect()
    }

    /// Pull and durably import the missing blocks of `manifest` from a peer,
    /// **verifying every block's CID before import** and staying within `limits`.
    /// `fetch(cid)` returns the peer's bytes for a CID, or `None` if unavailable.
    /// Already-present blocks are skipped (dedup); a wrong-CID block aborts the
    /// pull (content-addressing); the import itself is atomic per block.
    pub fn pull_blocks(
        &self,
        manifest: &[String],
        fetch: impl Fn(&str) -> Option<Vec<u8>>,
        limits: SyncLimits,
    ) -> CoreResult<PullReport> {
        let mut report = PullReport::default();
        for cid in manifest {
            if self.has_block(cid) {
                report.already_present += 1;
                continue;
            }
            if report.imported >= limits.max_blocks {
                return Err(Error::SecurityPolicy(format!(
                    "sync refused: block count exceeded {}",
                    limits.max_blocks
                )));
            }
            let Some(bytes) = fetch(cid) else {
                // The peer cannot serve this CID — skip it (a later sync may).
                continue;
            };
            if report.bytes.saturating_add(bytes.len() as u64) > limits.max_bytes {
                return Err(Error::SecurityPolicy(format!(
                    "sync refused: byte budget exceeded {}",
                    limits.max_bytes
                )));
            }
            // CID-verify + dedup-on-disk happen inside store_verified_raw_block; a
            // tampered block fails here and never enters the store.
            self.store_verified_raw_block(&Cid(cid.clone()), &bytes)?;
            report.imported += 1;
            report.bytes += bytes.len() as u64;
        }
        Ok(report)
    }

    /// Convenience: pull every block reachable from `roots` from a peer, decoding
    /// links as blocks arrive (for plaintext namespaces). Encrypted-private
    /// namespaces pass their explicit ciphertext manifest to [`MemCli::pull_blocks`]
    /// instead, since ciphertext links are opaque.
    pub fn pull_reachable(
        &self,
        roots: &[String],
        fetch: impl Fn(&str) -> Option<Vec<u8>>,
        limits: SyncLimits,
    ) -> CoreResult<PullReport> {
        let mut report = PullReport::default();
        let mut queue: Vec<String> = roots.to_vec();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(cid) = queue.pop() {
            if !seen.insert(cid.clone()) {
                continue;
            }
            if self.has_block(&cid) {
                report.already_present += 1;
            } else {
                if report.imported >= limits.max_blocks {
                    return Err(Error::SecurityPolicy(format!(
                        "sync refused: block count exceeded {}",
                        limits.max_blocks
                    )));
                }
                let Some(bytes) = fetch(&cid) else { continue };
                if report.bytes.saturating_add(bytes.len() as u64) > limits.max_bytes {
                    return Err(Error::SecurityPolicy(format!(
                        "sync refused: byte budget exceeded {}",
                        limits.max_bytes
                    )));
                }
                self.store_verified_raw_block(&Cid(cid.clone()), &bytes)?;
                report.imported += 1;
                report.bytes += bytes.len() as u64;
            }
            // Now that the block is present, follow its IMMEDIATE links (not a
            // recursive walk — children may not be fetched yet).
            if let Ok(links) = self.block_links(&Cid(cid.clone())) {
                for link in links {
                    if !seen.contains(&link.0) {
                        queue.push(link.0);
                    }
                }
            }
        }
        Ok(report)
    }
}

/// A durable record of one completed namespace sync (plan §Synchronization Protocol,
/// "publish a new signed head record only after durable local commit"). Local
/// bookkeeping — what we pulled, from whom, and the head we converged on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncReceipt {
    pub network_id: String,
    pub namespace: String,
    pub peer: String,
    pub blocks_imported: usize,
    pub bytes: u64,
    /// The head set this device converged on after the sync.
    pub heads: Vec<String>,
    pub at: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_key(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

fn write_json<T: serde::Serialize>(path: &std::path::Path, value: &T) -> CoreResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(format!("create dir: {e}")))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::Io(format!("serialize: {e}")))?;
    std::fs::write(path, text).map_err(|e| Error::Io(format!("write {}: {e}", path.display())))
}

impl MemCli {
    fn head_path(&self, network_id: &NetworkId, namespace_canonical: &str) -> CoreResult<std::path::PathBuf> {
        Ok(self
            .security_dir()?
            .join("networks")
            .join(format!("{}.head-{}.json", network_id.0, safe_key(namespace_canonical))))
    }

    fn local_heads_path(&self, network_id: &NetworkId, namespace_canonical: &str) -> CoreResult<std::path::PathBuf> {
        Ok(self
            .security_dir()?
            .join("networks")
            .join(format!("{}.localheads-{}.json", network_id.0, safe_key(namespace_canonical))))
    }

    /// **Sign and store** a head advertisement for `namespace` (a writer's act).
    /// `signer_capability` must grant `sync_write` (verified by receivers). The
    /// per-signer sequence auto-increments from any prior stored head. Returns the
    /// signed record (the serve side hands it out on `GetHeads`).
    pub fn publish_head(
        &self,
        descriptor: &NetworkDescriptor,
        namespace: &Namespace,
        heads: Vec<String>,
        signer_capability: Capability,
    ) -> CoreResult<HeadRecord> {
        self.ensure_security_dir()?;
        let signer = self.identity()?;
        let canonical = namespace.canonical();
        let sequence = self
            .stored_head(&descriptor.network_id, &canonical)?
            .map(|h| h.sequence.saturating_add(1))
            .unwrap_or(0);
        let record = HeadRecord::create(
            &signer,
            &descriptor.network_id,
            namespace,
            heads.clone(),
            sequence,
            descriptor.membership_epoch,
            0,
            signer_capability,
            now_secs(),
        );
        write_json(&self.head_path(&descriptor.network_id, &canonical)?, &record)?;
        // A writer's own current heads are also its local heads.
        self.set_local_heads(&descriptor.network_id, &canonical, &heads)?;
        Ok(record)
    }

    /// The signed head record this device serves for a namespace, if any.
    pub fn stored_head(&self, network_id: &NetworkId, namespace_canonical: &str) -> CoreResult<Option<HeadRecord>> {
        match std::fs::read_to_string(self.head_path(network_id, namespace_canonical)?) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| Error::Io(format!("parse head: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(format!("read head: {e}"))),
        }
    }

    /// The heads this device currently considers current for a namespace (its view
    /// of convergence) — unsigned local state, distinct from a signed advertisement.
    pub fn local_heads(&self, network_id: &NetworkId, namespace_canonical: &str) -> Vec<String> {
        match self.local_heads_path(network_id, namespace_canonical) {
            Ok(path) => match std::fs::read_to_string(path) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    /// Record this device's converged head view for a namespace.
    pub fn set_local_heads(&self, network_id: &NetworkId, namespace_canonical: &str, heads: &[String]) -> CoreResult<()> {
        self.ensure_security_dir()?;
        let mut sorted = heads.to_vec();
        sorted.sort();
        sorted.dedup();
        write_json(&self.local_heads_path(network_id, namespace_canonical)?, &sorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{CoreBinding, Node};
    use crate::capability::NamespaceScope;
    use crate::membership::NetworkDescriptor;

    const DAY: u64 = 24 * 3600;

    fn writer_cap(root: &Identity, ns: &Namespace, subject: &str, epoch: u64) -> Capability {
        Capability::issue(
            root, ns.clone(), subject,
            vec![Operation::SyncRead, Operation::SyncWrite], 1000, DAY, epoch, false,
        )
    }

    #[test]
    fn a_head_record_from_a_writer_verifies_but_a_reader_cannot_advertise() {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let writer = Identity::generate();

        // A writer (sync_write) signs a valid head record.
        let cap = writer_cap(&root, &ns, &writer.agent_id().0, descriptor.membership_epoch);
        let record = HeadRecord::create(
            &writer, &descriptor.network_id, &ns, vec!["bafyHEAD".into()],
            1, descriptor.membership_epoch, 0, cap, 2000,
        );
        assert!(verify_head_record(&record, &descriptor, 2000, &RevocationSet::new()).is_ok());

        // A read-only holder cannot advertise heads.
        let reader = Identity::generate();
        let read_cap = Capability::issue(
            &root, ns.clone(), &reader.agent_id().0, vec![Operation::SyncRead], 1000, DAY, descriptor.membership_epoch, false,
        );
        let forged = HeadRecord::create(
            &reader, &descriptor.network_id, &ns, vec!["bafyHEAD".into()],
            1, descriptor.membership_epoch, 0, read_cap, 2000,
        );
        assert_eq!(
            verify_head_record(&forged, &descriptor, 2000, &RevocationSet::new()),
            Err(SyncError::NotAWriter),
        );
    }

    #[test]
    fn a_tampered_head_record_is_rejected() {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let writer = Identity::generate();
        let cap = writer_cap(&root, &ns, &writer.agent_id().0, descriptor.membership_epoch);
        let mut record = HeadRecord::create(
            &writer, &descriptor.network_id, &ns, vec!["bafyHEAD".into()],
            1, descriptor.membership_epoch, 0, cap, 2000,
        );
        record.heads.push("bafyINJECTED".into()); // add a head after signing
        assert!(verify_head_record(&record, &descriptor, 2000, &RevocationSet::new()).is_err());
    }

    #[test]
    fn reconcile_preserves_concurrent_heads_and_lists_what_is_missing() {
        let root = Identity::generate();
        let descriptor = NetworkDescriptor::create(&root, "team", 1000);
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let writer = Identity::generate();
        let cap = writer_cap(&root, &ns, &writer.agent_id().0, descriptor.membership_epoch);
        let remote = HeadRecord::create(
            &writer, &descriptor.network_id, &ns, vec!["A".into(), "B".into()],
            1, descriptor.membership_epoch, 0, cap, 2000,
        );
        // Local has A and a concurrent head C; remote has A and B.
        let recon = reconcile(&["A".to_string(), "C".to_string()], &remote, |h| h == "A");
        assert_eq!(recon.converged_heads, ["A", "B", "C"], "union preserves all heads");
        assert_eq!(recon.missing_heads, ["B"], "only B must be pulled");
    }

    #[test]
    fn two_devices_converge_by_pulling_only_missing_verified_blocks() {
        // The Phase D exit criterion (at the logic level): an authorized device
        // pulls only the missing, CID-verified blocks and converges on the head.
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        // A builds a small graph: a checkpoint over a node.
        let child = mem_a.put_node(&Node { kind: "memory".into(), fields_json: r#"{"text":"shared fact","kind":"reference"}"#.into() }).unwrap();
        let head = mem_a.checkpoint("latest", &child, None).unwrap();
        let manifest: Vec<String> = mem_a.walk(&head).unwrap().into_iter().map(|c| c.0).collect();
        assert!(manifest.len() >= 2);

        // B starts empty and pulls reachable blocks from A (A is the fetch source).
        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        assert_eq!(mem_b.missing_blocks(&manifest).len(), manifest.len(), "B is missing everything");

        let fetch = |cid: &str| mem_a.read_block(&Cid(cid.to_string())).ok();
        let report = mem_b.pull_reachable(&[head.0.clone()], fetch, SyncLimits::default()).unwrap();
        assert_eq!(report.imported, manifest.len(), "pulled exactly the graph");

        // Converged: B now has every block and can walk A's head as its own.
        assert!(mem_b.missing_blocks(&manifest).is_empty(), "B has the full graph");
        let b_walk: Vec<String> = mem_b.walk(&head).unwrap().into_iter().map(|c| c.0).collect();
        assert_eq!(b_walk.len(), manifest.len(), "B reconstructs the same subgraph");

        // Re-pulling is a no-op (dedup): nothing new imported.
        let again = mem_b.pull_reachable(&[head.0.clone()], |cid| mem_a.read_block(&Cid(cid.to_string())).ok(), SyncLimits::default()).unwrap();
        assert_eq!(again.imported, 0, "second sync imports nothing");
        assert_eq!(again.already_present, manifest.len());
    }

    #[test]
    fn authorized_ciphertext_sync_converges_without_revealing_plaintext() {
        // The full exit criterion: an authorized device pulls only the missing,
        // verified **ciphertext** blocks of a namespace and converges — and the
        // bytes it receives are inert ciphertext, not the plaintext secret. (Heavy:
        // the conversion runs scrypt password derivation.)
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        let child = mem_a.put_node(&Node { kind: "memory".into(), fields_json: r#"{"text":"WETLANDS-CLASSIFIED","kind":"project"}"#.into() }).unwrap();
        let root = mem_a.checkpoint("latest", &child, None).unwrap();
        mem_a.bind("latest", &root).unwrap();
        mem_a.set_password("pw").unwrap();
        let descriptor = mem_a.create_network("team").unwrap();
        let ns = Namespace::new(descriptor.network_id.clone(), NamespaceScope::Project("atlas".into()));
        let recipient = Identity::generate();
        let plan = mem_a.build_encrypt_and_share_plan("latest", &ns.canonical(), &[recipient.agent_id().0]).unwrap();
        mem_a.create_encrypt_and_share_private_grant(&plan, "pw").unwrap();
        let conversion = mem_a.convert_and_share_private(&plan, "pw").unwrap();
        let manifest = mem_a.private_conversions().unwrap()[0].ciphertext_manifest.clone();

        // B is a distinct device; it pulls the ciphertext manifest from A.
        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        let report = mem_b.pull_blocks(&manifest, |cid| mem_a.read_block(&Cid(cid.to_string())).ok(), SyncLimits::default()).unwrap();
        assert_eq!(report.imported, manifest.len(), "pulled the whole ciphertext graph");
        assert!(mem_b.missing_blocks(&manifest).is_empty(), "B converged on the ciphertext head");
        // Inert ciphertext only — the plaintext secret never reached B.
        for cid in &manifest {
            let bytes = mem_b.read_block(&Cid(cid.clone())).unwrap();
            assert!(!bytes.windows(b"WETLANDS-CLASSIFIED".len()).any(|w| w == b"WETLANDS-CLASSIFIED"), "no plaintext crossed the wire");
        }
        assert_eq!(conversion.conversion.ciphertext_root, manifest.iter().find(|c| **c == conversion.conversion.ciphertext_root).cloned().unwrap());
    }

    #[test]
    fn a_tampered_block_never_enters_the_store() {
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        let cid = mem_a.put_node(&Node { kind: "memory".into(), fields_json: r#"{"text":"honest","kind":"reference"}"#.into() }).unwrap();

        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        // A hostile peer serves corrupted bytes for the CID.
        let tampered = |_cid: &str| Some(b"not the real block".to_vec());
        let result = mem_b.pull_blocks(&[cid.0.clone()], tampered, SyncLimits::default());
        assert!(result.is_err(), "a wrong-CID block is rejected by content-addressing");
        assert!(!mem_b.has_block(&cid.0), "nothing was imported");
    }

    #[test]
    fn the_block_budget_is_enforced() {
        let dir_a = tempfile::tempdir().unwrap();
        let mem_a = MemCli::new(dir_a.path());
        let c1 = mem_a.put_node(&Node { kind: "memory".into(), fields_json: r#"{"text":"one","kind":"reference"}"#.into() }).unwrap();
        let c2 = mem_a.put_node(&Node { kind: "memory".into(), fields_json: r#"{"text":"two","kind":"reference"}"#.into() }).unwrap();

        let dir_b = tempfile::tempdir().unwrap();
        let mem_b = MemCli::new(dir_b.path());
        let fetch = |cid: &str| mem_a.read_block(&Cid(cid.to_string())).ok();
        let tight = SyncLimits { max_blocks: 1, max_bytes: DEFAULT_MAX_BYTES };
        let result = mem_b.pull_blocks(&[c1.0.clone(), c2.0.clone()], fetch, tight);
        assert!(result.is_err(), "pulling 2 blocks under a 1-block budget is refused");
    }
}
