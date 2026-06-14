use super::*;

/// The Phase N · Phase D sync protocol, spoken over libp2p request-response.
pub const SYNC_PROTOCOL: &str = "/concierge/sync/1.0.0";

/// A request on the [`SYNC_PROTOCOL`]: fetch one block by CID, or a namespace's
/// signed head record. Bounded, authenticated, point-to-point — never a broad
/// graph traversal (plan §Request-Response Requirements).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncRequest {
    /// Fetch the raw bytes of one content-addressed block.
    GetBlock(String),
    /// Fetch the signed [`HeadRecord`](concierge_core::HeadRecord) JSON for a
    /// canonical namespace.
    GetHeads(String),
    /// **Store-and-forward push**: hand a content-addressed block to a relay so it
    /// can hold it for an offline peer. The relay CID-verifies before storing and
    /// stores only **inert ciphertext** (it cannot decrypt). `(cid, bytes)`.
    PutBlock(String, Vec<u8>),
    /// **Direct message**: hand an opaque, app-signed message-envelope to a peer
    /// over this concierge-only point-to-point protocol — *not* a public gossipsub
    /// topic anyone could read or inject into. The transport never inspects or
    /// verifies the payload; the receiving application verifies the envelope's
    /// signature. The bytes are an opaque envelope (e.g. signed JSON).
    Deliver(Vec<u8>),
}

/// The response to a [`SyncRequest`]. `None` means "I don't have it / I won't serve
/// it" — a deterministic answer that does not reveal what else exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncResponse {
    Block(Option<Vec<u8>>),
    Heads(Option<Vec<u8>>),
    /// Whether a pushed block was accepted (CID verified, within limits).
    Stored(bool),
    /// Whether the receiving application accepted or durably queued a direct
    /// message after authenticating and applying its consent policy.
    Delivered(bool),
}

/// The serve side: answers inbound [`SyncRequest`]s from the local store. The
/// application supplies this (it holds the `MemCli` + the capability checks); the
/// transport just moves the bytes. A node with no provider serves nothing.
pub type SyncProvider = Arc<dyn Fn(SyncRequest) -> SyncResponse + Send + Sync>;

/// The largest single block a store-and-forward relay will accept on a push.
pub const MAX_PUSH_BLOCK_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_PENDING_DIRECT_MESSAGES: usize = 1024;

pub(super) fn noop_provider() -> SyncProvider {
    Arc::new(|request| match request {
        SyncRequest::GetBlock(_) => SyncResponse::Block(None),
        SyncRequest::GetHeads(_) => SyncResponse::Heads(None),
        SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// Build a serve-side [`SyncProvider`] backed by a real local store: it answers
/// `GetBlock` from the content-addressed block store. (Head serving is left to the
/// application, which maintains the signed [`HeadRecord`](concierge_core::HeadRecord)
/// for each namespace it writes.) Sensitive content is inert ciphertext at rest, and
/// which CIDs a peer learns to ask for is governed by the Phase C/D head/manifest
/// gating — so serving a known CID's bytes is safe defense-in-depth.
pub fn store_provider(mem: Arc<concierge_core::MemCli>) -> SyncProvider {
    Arc::new(move |request| match request {
        SyncRequest::GetBlock(cid) => SyncResponse::Block(mem.block_bytes(&cid)),
        SyncRequest::GetHeads(namespace) => {
            // Serve the signed head record this device maintains for the namespace.
            let head = concierge_core::Namespace::parse(&namespace)
                .and_then(|ns| mem.stored_head(&ns.network_id, &namespace).ok().flatten())
                .and_then(|record| serde_json::to_vec(&record).ok());
            SyncResponse::Heads(head)
        }
        // A plain store does not accept pushes (use `relay_provider` for that).
        SyncRequest::PutBlock(_, _) => SyncResponse::Stored(false),
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// A **store-and-forward relay** provider: like [`store_provider`] but it also
/// *accepts* pushed blocks, holding them for offline peers. A pushed block is
/// **CID-verified before storage** (a wrong-CID block is refused) and bounded by
/// [`MAX_PUSH_BLOCK_BYTES`]. The relay stores only inert ciphertext — it cannot
/// decrypt and is not a membership authority (plan §Offline and Store-and-Forward).
pub fn relay_provider(mem: Arc<concierge_core::MemCli>) -> SyncProvider {
    Arc::new(move |request| match request {
        SyncRequest::GetBlock(cid) => SyncResponse::Block(mem.block_bytes(&cid)),
        SyncRequest::GetHeads(namespace) => {
            let head = concierge_core::Namespace::parse(&namespace)
                .and_then(|ns| mem.stored_head(&ns.network_id, &namespace).ok().flatten())
                .and_then(|record| serde_json::to_vec(&record).ok());
            SyncResponse::Heads(head)
        }
        SyncRequest::PutBlock(cid, bytes) => {
            if bytes.len() > MAX_PUSH_BLOCK_BYTES {
                return SyncResponse::Stored(false);
            }
            // import_verified_block CID-verifies; a forged cid/bytes pair is rejected.
            SyncResponse::Stored(mem.import_verified_block(&cid, &bytes).is_ok())
        }
        // Direct messages are intercepted in `run()` before the provider is
        // consulted; this arm only satisfies exhaustiveness.
        SyncRequest::Deliver(_) => SyncResponse::Delivered(false),
    })
}

/// What a pending outbound request was asking for, so its response can be labeled.
#[derive(Debug)]
pub(super) enum Pending {
    Block {
        cid: String,
        reply: Option<oneshot::Sender<Option<Vec<u8>>>>,
    },
    Heads {
        namespace: String,
        reply: Option<oneshot::Sender<Option<Vec<u8>>>>,
    },
    Push(String),
}

/// **The end-to-end sync driver (Phase F)**: pull a namespace from `peer` to
/// convergence over the live connection. It runs the full Phase D loop —
/// exchange-heads → verify → reconcile → fetch only the missing, CID-verified
/// blocks (walking the graph down as they arrive) → adopt the converged heads —
/// and returns a [`SyncReceipt`]. Bounded by `overall_timeout`. Requests are serial
/// (one in flight) for simple response correlation; a hostile/absent peer just
/// times out. The caller supplies the verified `descriptor` + the current
/// `revoked` set so a stale, unauthorized, or revoked head is rejected.
#[allow(clippy::too_many_arguments)]
pub async fn sync_from_peer(
    node: &ConciergeNode,
    _events: &mut mpsc::UnboundedReceiver<NodeEvent>,
    peer: PeerId,
    mem: &concierge_core::MemCli,
    descriptor: &concierge_core::NetworkDescriptor,
    namespace: &concierge_core::Namespace,
    revoked: &concierge_core::RevocationSet,
    overall_timeout: std::time::Duration,
) -> Result<concierge_core::SyncReceipt, String> {
    use concierge_core::{reconcile, verify_head_record, HeadRecord};
    use tokio::time::Instant;

    let deadline = Instant::now() + overall_timeout;
    let canonical = namespace.canonical();

    // 1. Exchange heads — fetch and verify the peer's signed advertisement.
    let head_bytes = fetch_head(node, peer, &canonical, deadline)
        .await
        .ok_or_else(|| "peer did not return a head record".to_string())?;
    let remote: HeadRecord =
        serde_json::from_slice(&head_bytes).map_err(|e| format!("malformed head record: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    verify_head_record(&remote, descriptor, now, revoked)
        .map_err(|e| format!("head record rejected: {e}"))?;

    // 2. Reconcile against our local view.
    let local = mem.local_heads(&descriptor.network_id, &canonical);
    let recon = reconcile(&local, &remote, |h| mem.has_block(h));

    // 3. Fetch only the missing blocks, walking the graph down as they arrive.
    let mut imported = 0usize;
    let mut bytes_total = 0u64;
    let mut queue = recon.missing_heads.clone();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(cid) = queue.pop() {
        if !seen.insert(cid.clone()) {
            continue;
        }
        if !mem.has_block(&cid) {
            let bytes = fetch_block(node, peer, &cid, deadline)
                .await
                .ok_or_else(|| format!("could not fetch block {cid}"))?;
            // CID-verified before durable import; a tampered block aborts the sync.
            mem.import_verified_block(&cid, &bytes)
                .map_err(|e| format!("import {cid}: {e}"))?;
            imported += 1;
            bytes_total += bytes.len() as u64;
        }
        // Follow the block's immediate links (present now that it is imported).
        if let Ok(links) = mem.block_links(&concierge_core::Cid(cid.clone())) {
            for link in links {
                if !seen.contains(&link.0) {
                    queue.push(link.0);
                }
            }
        }
    }

    // 4. Durable commit done — adopt the converged head set.
    mem.set_local_heads(&descriptor.network_id, &canonical, &recon.converged_heads)
        .map_err(|error| format!("commit converged heads: {error}"))?;
    Ok(concierge_core::SyncReceipt {
        network_id: descriptor.network_id.0.clone(),
        namespace: canonical,
        peer: peer.to_string(),
        blocks_imported: imported,
        bytes: bytes_total,
        heads: recon.converged_heads,
        at: now,
    })
}

/// Request a namespace's head record from `peer`, retrying until the connection is
/// up or the deadline passes.
async fn fetch_head(
    node: &ConciergeNode,
    peer: PeerId,
    namespace: &str,
    deadline: tokio::time::Instant,
) -> Option<Vec<u8>> {
    use tokio::time::Instant;
    while Instant::now() < deadline {
        let slice = (deadline - Instant::now()).min(std::time::Duration::from_millis(700));
        match tokio::time::timeout(slice, node.request_heads_response(peer, namespace)).await {
            Ok(Ok(Some(bytes))) => return Some(bytes),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await
            }
        }
    }
    None
}

/// Request one block from `peer`, retrying until served or the deadline passes.
async fn fetch_block(
    node: &ConciergeNode,
    peer: PeerId,
    cid: &str,
    deadline: tokio::time::Instant,
) -> Option<Vec<u8>> {
    use tokio::time::Instant;
    while Instant::now() < deadline {
        let slice = (deadline - Instant::now()).min(std::time::Duration::from_millis(700));
        match tokio::time::timeout(slice, node.request_block_response(peer, cid)).await {
            Ok(Ok(Some(bytes))) => return Some(bytes),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await
            }
        }
    }
    None
}
