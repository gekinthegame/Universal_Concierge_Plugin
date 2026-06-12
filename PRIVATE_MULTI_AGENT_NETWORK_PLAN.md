# Private Multi-Agent Network and Shared Memory Sync Plan

## Objective

Allow one person to operate a private network of Concierge-enabled computers,
harnesses, and agents that can communicate and converge on shared memory without
copying one identity secret between installations.

The intended experience is:

1. Install Concierge on computer A and create a private network.
2. Install Concierge on computer B and pair it from A with a QR code or one-time
   enrollment code.
3. Approve the device and choose which graph scopes it may read or write.
4. Both Data Platters discover authorized peers, exchange missing
   content-addressed blocks, and converge on the same authorized graph.
5. Add agents with narrower capabilities so they can collaborate without gaining
   access to every project or the ability to publish data publicly.

This plan builds on:

- Phase 4 CAR import/export and CID verification;
- Phase 5.5 stable identities and signed shares;
- Phase 5.7 content-addressed messaging and libp2p transport;
- Decision 0010 immutable supersession;
- Decision 0011 capability-encrypted private content;
- Decision 0012 explicit public/private room posture; and
- `DATA_PLATTER_PRIVACY_LOCK_PLAN.md` publication guards.

It does not replace those designs. It supplies the missing multi-device
ownership, enrollment, synchronization, merge, and revocation protocol.

## Current-State Gap

The current architecture gives every installation one stable AgentID and can
sign content, exchange messages, import/export CAR files, and connect over
libp2p. That is not yet sufficient to claim that multiple computers belong to
the same user or that their graphs automatically merge.

The missing pieces are:

- a root identity that can prove distinct installations belong to the same user;
- private-network membership and scoped authorization;
- secure device and agent enrollment;
- signed graph-head advertisements;
- request-response transfer of missing blocks;
- deterministic convergence of concurrent graph heads;
- multi-parent merge checkpoints;
- encrypted private-subgraph capabilities;
- device and agent revocation with key rotation;
- automatic discovery, offline synchronization, and sync-state UX.

## Non-Negotiable Design Rules

1. **Never copy one AgentID private key between devices.** Every installation has
   a distinct key so it can be independently audited and revoked.
2. **Private network membership does not imply access to every graph.** Access is
   granted per namespace/subgraph with explicit capabilities.
3. **Private synchronization is not public publication.** Syncing authorized
   encrypted blocks to a private peer must never authorize Kubo publication,
   public-room attachment, or public DHT announcement.
4. **Encrypt private content before it leaves a device.** Noise protects the
   connection, not stored or relayed blocks.
5. **All received data is untrusted until verified.** Verify membership,
   capability, signature, CID, size limits, and schema before import.
6. **Merging never mutates or discards immutable history.** Convergence is a
   union of verified blocks plus explicit merge records.
7. **Preserve concurrent heads until a valid merge resolves them.** Do not use
   last-writer-wins for graph content.
8. **The most restrictive applicable future-egress policy wins.** A shared merge
   cannot silently turn private content public, and a device-local lock remains
   authoritative on its device without becoming synchronized merge state.
9. **Agents receive least privilege.** A harness agent should only read, write,
   message, or publish within explicitly granted scopes.
10. **Revocation is prospective, not magical deletion.** It can block future
    writes and future key epochs, but it cannot erase plaintext already
    decrypted by a removed device.
11. **No secret-bearing CLI arguments.** Pairing secrets, passwords, root keys,
    and capabilities must not enter shell history, process lists, logs, DAG
    nodes, or CAR exports.
12. **Administrative authority is not social rank.** Membership and capability
    administration are necessary security functions, not reputation,
    gamification, or status hierarchy.
13. **Local publication locks never synchronize.** Each device retains its own
    local safety overlay; shared namespace posture is separate signed state.
14. **Known-public exposure never becomes private again.** Restrictive merge
    policy may block future egress, but it cannot erase historical exposure.

## Identity Hierarchy

Use separate identities for ownership, networks, installations, and actors.

### UserID

`UserID` is the person's long-lived root ownership identity.

- Generated once when the user creates their first private network.
- Stored in the OS keystore or an encrypted Concierge identity vault.
- Used only to create networks, authorize administrative devices, recover
  membership, and sign high-value identity statements.
- Not used for routine messages, graph writes, or libp2p transport.
- Never copied as plaintext to another device.

For recovery, the user may explicitly transfer an encrypted recovery package or
use a future threshold-recovery design. Pairing a routine device must not
transfer the raw UserID private key.

### NetworkID

`NetworkID` identifies one private collaboration mesh. A user may create more
than one network, such as `personal`, `family`, or `research-team`.

A signed network descriptor contains:

```text
NetworkDescriptor {
  version
  network_id
  name
  created_at
  root_user_ids[]
  policy_digest
  current_membership_epoch
  signature
}
```

The descriptor is public metadata within the network. It must not contain
capability keys, passwords, or private graph roots.

### DeviceID

Every installation has a unique `DeviceID` and private key.

- The existing persisted install-level AgentID becomes the initial DeviceID
  identity during migration; do not break existing stores merely to rename it.
- A device signs transport/session statements and records which installation
  submitted an operation.
- A device can be revoked without rotating every other device's key.
- libp2p PeerID remains bound to the installation key unless a later decision
  deliberately separates transport identity.

### ActorID

`ActorID` identifies the human-facing client, harness, or agent that creates a
contribution on a device.

- A device can host multiple actors.
- Each agent/harness receives its own actor key or a device-signed constrained
  actor certificate.
- Contributions are attributable to the actual actor and hosting device.
- Actor permissions can be narrower than device permissions.
- Stopping or replacing one agent does not require replacing the DeviceID.

This resolves the ambiguity in the current term `AgentID`: existing
install-level AgentIDs remain valid as device identities, while new actor
identity is added explicitly rather than pretending one installation is one
agent forever.

> **Progress (2026-06-11): ActorID certificates DONE.** `crates/core/src/actor.rs` —
> `ActorCertificate` (device-signed, scoped, expiry-bounded) enrolls a harness/agent
> hosted by a device. `verify_actor_certificate(actor_cert, device_membership,
> descriptor, now, revoked)` verifies the **chain root→device→actor**: the hosting
> device is a valid member *and* a device, the cert is signed by that device, it is
> unexpired/in-epoch/unrevoked, and the actor's operations are a **subset of the
> device's** (an agent cannot widen its own authority). `MemCli::{enroll_actor
> (intersects the requested / least-privilege-default ops with the device's own
> holdings → cannot escalate), actor_certificates}`. **CLI:** `actor <list | enroll
> <actor-id> --namespace NS [--ops a,b]>`. Tests: 5.

### Membership Certificates

Authorization is proven by signed, short-lived, versioned certificates:

```text
MembershipCertificate {
  version
  network_id
  subject_id
  subject_kind          # device | actor | user
  issuer_id
  issued_at
  expires_at
  membership_epoch
  capabilities[]
  constraints
  signature
}
```

Certificates must be:

- scoped rather than globally permissive;
- expiry-bounded;
- linked to the issuing authority;
- checked against current revocation and epoch state;
- renewed without changing the subject identity; and
- excluded from public graph exports unless deliberately redacted for proof.

Routine contributions are signed by the ActorID or DeviceID that made them, not
by the UserID root.

### Resolution and Rotation Without a Directory

There is **no global identity directory** (this is the deliberate divergence from
atproto's `did:plc` — see DECISIONS.md 0018). Identity is resolved *within a
network* by verifiable certificate chains, not by looking anyone up in a registry.

- **Trust rule.** An identity is accepted iff its `MembershipCertificate` chains
  back to a `root_user_id` in the signed `NetworkDescriptor` and is unexpired,
  in-epoch, and unrevoked. "Who is this?" is answered from material you already
  hold (descriptor + certs received at pairing/sync), never from a remote lookup.
- **No first-contact lookup.** New identities enter only via authenticated pairing
  (see Pairing Flow), which transfers the device/actor public key + cert directly,
  peer-to-peer. There is no moment where a node must ask a third party to resolve a
  key.

**Identifier format — `did:key` for devices and actors.** DeviceID and ActorID use
`did:key` (the identifier *is* the public key; fully self-contained, no resolver).
Port atproto's `packages/did` did:key path and `packages/crypto` (k256/p256,
multibase encoding); ignore its did:plc/did:web resolvers. Because `did:key` is
immutable, **rotation of a device/actor = revoke + re-enroll a new identifier**
(consistent with revoking one device without rotating the others).

**UserID rotation/recovery — an in-network signed log.** The root UserID must
rotate or recover *without changing who the user is*, which `did:key` alone cannot
express. Keep UserID as a stable identifier carrying its own **append-only,
recovery-key-signed rotation log**, propagated through the network DAG and folded
into membership-epoch state. This is the `did:plc` operation-log *idea* without the
central service, and the concrete form of the "encrypted recovery package / future
threshold-recovery" note under UserID above. (Lands with Phase A identity + Phase G
rotation.)

## Capability and Permission Model

Authorization is expressed as explicit capabilities. Suggested operations:

```text
discover
sync_read
sync_write
message_receive
message_send
checkpoint_create
merge_propose
merge_accept
member_invite
capability_grant
member_revoke
request_publication_review
```

Every capability includes:

- target network;
- target namespace, room, or subgraph;
- allowed operations;
- optional path/node-kind constraints;
- issuance and expiry;
- current key/membership epoch;
- delegability constraints; and
- issuer signature.

Read and write authority must remain separate. No durable network capability,
including `request_publication_review`, authorizes public publication.
`request_publication_review` may only open the local Data Platter review flow;
the exact local password-authorized, one-shot public-publish grant remains the
only public-publication authority. Public publication is never implied by
`sync_read`, `sync_write`, room membership, merge authority, device
administration, or UserID ownership.

Agent defaults:

- no access to unrelated namespaces;
- read-only unless writing is required;
- no member administration;
- no capability delegation;
- no public publication;
- messaging limited by room participation policy; and
- explicit user approval before expanding scope.

## Secure Enrollment and Pairing

### Pairing Flow

1. An already-authorized administrative device creates a short-lived,
   single-use enrollment offer.
2. The offer is shown as a QR code and optional human-entered code. It contains
   only rendezvous information, an ephemeral public key, expiry, and an
   unguessable offer identifier.
3. The new device connects through an authenticated Noise session and proves
   possession of its newly generated DeviceID key.
4. Both screens display a human-verifiable confirmation phrase or numeric code
   derived from the handshake transcript.
5. The user approves the new device name, requested scopes, and permissions on
   an existing trusted device.
6. The administrative device issues a scoped membership certificate and
   transfers only the encrypted capabilities needed for the approved scopes.
7. The enrollment offer is consumed atomically and cannot be replayed.
8. Both devices record a security event and begin authorized synchronization.

### Pairing Safety Requirements

- Pairing offers expire quickly and are one-use.
- A pairing code alone is insufficient without the authenticated transcript and
  explicit approval.
- The new device generates its own keys locally.
- No UserID root secret, existing DeviceID secret, password, or plaintext
  capability is encoded in a QR code.
- A relay may forward enrollment traffic but cannot decrypt or alter it.
- Pairing fails closed when confirmation codes differ.
- Repeated failures are rate-limited and logged without secret material.
- Enrollment is recoverable after interrupted transfer without accidentally
  issuing duplicate broad certificates.

### Inviting Another Person

A private network may later include multiple UserIDs. Inviting another person
must issue capabilities to their UserID or devices without transferring the
creator's UserID secret. This is distinct from adding another device owned by
the same person.

## Shared Graph and Namespace Model

Do not treat an entire store as one universally shared graph. Introduce
authorized synchronization namespaces.

Examples:

```text
network:{network_id}:personal
network:{network_id}:project:{project_id}
network:{network_id}:room:{room_id}
network:{network_id}:agent:{actor_id}
```

A namespace defines:

- authorized roots and reachable encrypted subgraphs;
- current set of graph heads;
- capability and encryption epoch;
- signed shared posture such as `encrypted-private` or `signed-open`;
- conflict and merge policy;
- retention and replication policy; and
- which devices or actors may advertise writes.

Local-only nodes remain outside shared namespaces unless the user deliberately
converts them into a new capability-encrypted graph through the password-
authorized Data Platter flow. A shared namespace must not automatically traverse
into unrelated local graph roots. Device-local publication locks are never part
of namespace state and never synchronize.

## Signed Head Advertisement

Each writer advertises signed namespace state rather than claiming one mutable
global `latest` value:

```text
HeadRecord {
  version
  network_id
  namespace_id
  heads[]
  previous_record
  sequence
  membership_epoch
  capability_epoch
  signer_id
  signer_certificate
  timestamp
  signature
}
```

Rules:

- `heads[]` is a set because concurrent valid heads are expected.
- The signer must have `sync_write` for the namespace.
- Sequence numbers are per signer and cannot establish a global order.
- A new record must not silently omit an unresolved valid concurrent head.
- Stale, replayed, revoked, wrong-network, or unauthorized records are rejected.
- Head records reveal as little private metadata as practical and travel only
  through authorized channels for private namespaces.

## Synchronization Protocol

### Reconciliation

For each authorized namespace:

1. Discover and authenticate a peer.
2. Exchange supported protocol versions, membership epoch, capability proofs,
   and signed head summaries.
3. Compare known heads and compact block manifests.
4. Request only missing CIDs through a bounded request-response protocol.
5. Transfer small blocks directly and large ranges as bounded CAR streams.
6. Verify each received block's CID before staging it.
7. Verify decoded schema, signatures, capability scope, encryption posture, and
   resource limits.
8. Atomically import accepted blocks into the local store.
9. Preserve all valid concurrent heads.
10. Create or accept an explicit merge checkpoint when required.
11. Publish a new signed head record only after durable local commit.

The result is content-addressed set union with deduplication, not blind database
replication.

### Request-Response Requirements

Add dedicated protocols for:

- signed head exchange;
- CID existence checks;
- bounded block fetch;
- bounded CAR-range/subgraph fetch;
- capability/key-envelope delivery;
- revocation and epoch update fetch; and
- sync acknowledgement/status.

Every protocol must have:

- request and response byte limits;
- item-count and traversal-depth limits;
- timeouts and cancellation;
- concurrency and rate limits;
- no arbitrary filesystem path access;
- no unauthenticated broad graph traversal; and
- deterministic errors that do not leak unauthorized graph existence.

### Offline and Store-and-Forward

- An authorized always-on device or explicitly configured private relay may hold
  encrypted blocks and signed head records for offline peers.
- Relays receive only ciphertext and minimum routing metadata.
- Returning devices reconcile from signed heads and missing CIDs.
- A relay is not a membership authority and cannot grant access.
- Retention limits and deletion policy must be visible to the user.

## Graph Merge Semantics

### Immutable Union

Blocks with valid CIDs do not conflict at the storage layer. Synchronization
unions verified blocks and deduplicates identical CIDs.

Conflict exists at mutable interpretation points:

- competing namespace heads;
- names such as `latest`;
- concurrent checkpoints;
- superseding facts;
- shared namespace posture; and
- capability/membership changes.

### Multi-Parent Merge Checkpoint

The current checkpoint model has one optional parent. Add a versioned
multi-parent merge form:

```text
MergeCheckpoint {
  version
  namespace_id
  label
  root
  parents[]
  merge_policy
  conflicts[]
  author
  created_at
  signature
}
```

Compatibility rules:

- Existing single-parent checkpoints remain valid.
- A normal checkpoint may continue using one parent.
- A merge checkpoint must contain at least two distinct valid parent heads.
- Parent order is canonicalized before hashing.
- A merge record references, rather than rewrites, both histories.

### Deterministic Merge Rules

1. If one head is an ancestor of another, fast-forward to the descendant.
2. If heads are concurrent, retain both until an authorized merge checkpoint is
   accepted.
3. Immutable nodes are unioned by CID.
4. Facts evolve through Decision 0010 `supersedes` links; unresolved concurrent
   facts remain visible rather than silently overwritten.
5. Messages retain Phase 5.7's `(Lamport time, then ActorID/AgentID)` display
   ordering; display order does not erase forks.
6. Signed mutable-name records use explicit version/sequence rules and expose
   unresolved competing bindings.
7. Shared namespace posture uses the most restrictive valid signed state.
8. Public and private namespace boundaries never merge implicitly.
9. An automatic merge may only perform rules proven lossless and
   deterministic. Semantic conflicts require user or authorized-agent review.

Device-local publication locks do not merge. After graph convergence, every
device computes effective future-egress policy from its local lock overlay plus
the merged shared namespace posture. Known-public exposure is tracked
independently and remains permanent; a restrictive merge can block future
publication but cannot relabel already exposed CIDs as private.

### Convergence Property

Given the same valid block set, membership/revocation state, signed head records,
and accepted merge records, all authorized peers must compute the same visible
heads and conflict set regardless of arrival order.

## Privacy and Publication Integration

Private-network sync must integrate with the Data Platter lock and publication
guard rather than bypass it.

### Required Separation

| Operation | Meaning | Public publication authority |
|---|---|---|
| `sync-private` | Replicate capability-encrypted data to an authorized member | Never |
| `message-private` | Exchange encrypted room/message blocks | Never |
| `export-private` | Export encrypted blocks plus explicitly selected capability envelope | Never |
| `publish-public` | Intentionally expose reviewed plaintext/public blocks | Explicit one-shot authorization |

### Egress Rules

- Private sync builds a scoped egress plan and sends only namespace-authorized,
  capability-encrypted blocks.
- Local policy-locked plaintext must not sync merely because a peer is trusted.
- Local lock records never enter a namespace, head record, CAR, sync record, or
  merge. Each device enforces its own local overlay.
- Moving locked/local-only plaintext into a shared namespace requires the
  password-authorized `Convert to encrypted private and share` flow. That flow
  creates a new ciphertext graph and leaves the original plaintext locked.
- Public publication still passes the exact manifest review and one-shot
  publication grant defined in `DATA_PLATTER_PRIVACY_LOCK_PLAN.md`.
- No membership certificate or durable capability can authorize public
  publication. A network member may only request that the local user open the
  publication-review flow.
- Private CIDs are not announced to the public DHT.
- Private swarm membership is defense in depth, not a substitute for content
  encryption.
- A merge that includes encrypted-private shared input remains
  encrypted-private until a separate explicit declassification and publication
  review. On each device, any applicable local lock independently continues to
  block future egress.
- Known-public exposure remains visible after merge, locking, or encryption and
  is never erased by the most-restrictive-future-egress rule.

### Locked Capability Vault

After restart, a device with a locked capability vault fails closed:

- It may authenticate as its DeviceID and relay or retain already authorized
  ciphertext and signed head records without decrypting them.
- It may not decrypt private content, create private writes, unwrap or issue
  capabilities, perform semantic merges, or convert plaintext.
- Pre-approved ciphertext replication may continue only when authorization can
  be verified without exposing vault secrets.
- Operations requiring private read/write keys wait for local vault unlock.
- Vault unlock never grants public-publication authority.

## Revocation and Key Rotation

### Revoking a Device or Actor

1. An authorized administrator signs a revocation record.
2. Membership epoch advances.
3. Peers reject future writes, head records, pairing requests, and capability
   requests from the revoked subject.
4. Affected namespace capability epochs advance.
5. Remaining members receive new encrypted read/write key envelopes.
6. New blocks use the new epoch keys.
7. The Data Platter shows incomplete rotation or offline members clearly.

### Honest Limits

- Revocation cannot make a device forget blocks or keys it already received.
- Previously decrypted plaintext may have been copied outside Concierge.
- Full historical re-encryption is expensive and creates new ciphertext CIDs.
- The default revocation goal is to stop future access and future authorized
  writes.
- High-sensitivity namespaces may offer deliberate historical re-encryption and
  re-rooting as a separate operation.

## Transport and Discovery

Existing relay v2 and DCUtR support should be retained. Complete the private mesh
transport with:

- AutoNAT for reachability assessment;
- rendezvous or another authenticated discovery mechanism;
- direct connection upgrades where possible;
- relay reservation management and bounded relay usage;
- request-response graph/CAR synchronization;
- store-and-forward for offline peers;
- private-swarm PSK mode with forced private networking where appropriate;
- connection allowlists;
- libp2p resource manager limits;
- protocol-level rate limits and quotas;
- connection and sync telemetry that excludes private content; and
- optional QUIC/WebRTC/WebTransport only after the core protocol is stable.

Internet reachability tests must cover two peers behind separate consumer NATs,
not only localhost or one LAN.

### Tiered authentication regime (Decision 0024, cyber `identity.md`)

Identity stays one keypair per actor; the *authentication discipline* varies with
the trust boundary crossed. The same `send(target, message)` call selects a tier
by locality:

- **Tier A — capability handle (intra-process):** Cryptree capability possession
  is authorization (Decision 0011); zero per-message crypto.
- **Tier B — symmetric MAC (inter-process on a host):** one DH at connect → a
  machine-pinned (Secure-Enclave-bound) session key, per-message MAC. New.
- **Tier C — signature (inter-host/WAN):** Ed25519 today, post-quantum later;
  only tier-C-authenticated records persist publicly. Streams amortize by signing
  a Merkle root over a batch.

This regime is the mechanism the Phase I "Trust Thermometer" surfaces. Sequence
honestly: Tier A + Ed25519 Tier C exist today; Tier B Enclave MAC and post-quantum
Tier C are deferred hardening (Phase G/I), and no UI may imply crypto not shipped.

## Data Platter Experience

### Private Network Map

Add a network view showing:

- UserID and private networks;
- devices and their online/offline/revoked state;
- actors/agents hosted by each device;
- granted namespace scopes and operations;
- last successful sync and pending block count;
- current heads, forks, merge checkpoints, and unresolved conflicts;
- capability and membership epoch health;
- relay/direct connection status; and
- precise private, locked, encrypted, and known-public states.

The network map must distinguish device-local publication locks from signed
shared namespace posture. A local lock may appear only on the device that owns
it; known-public exposure must remain visible everywhere that verified exposure
evidence is available.

### Pairing Experience

1. Select `Add device`.
2. Choose which network and graph scopes to offer.
3. Display a short-lived QR/code and expiry.
4. On the new device, scan or enter the code.
5. Compare the confirmation phrase on both devices.
6. Review requested device name and capabilities.
7. Approve or reject.
8. Show initial sync progress and exact authorized namespaces.

### Agent Enrollment

The user can add an agent from the device view, select a harness, and grant
specific namespaces and operations. The UI must make it difficult to grant an
agent whole-store access accidentally and must never include public publication
by default.

### Conflict and Merge Experience

- Concurrent heads are visible, not hidden as errors.
- The graph highlights common ancestry and unique branches.
- Lossless automatic merges are labeled as automatic.
- Semantic conflicts show the competing signed records and provenance.
- The user can accept a merge proposal or ask an authorized agent to propose
  one.
- Shared-posture conflicts explain why the most restrictive shared posture won;
  device-local locks are displayed separately and never presented as merged
  network state.

### Revocation Experience

Revoking a device or actor must show:

- affected namespaces;
- future access that will stop;
- required key rotations;
- offline devices still awaiting the new epoch; and
- the honest warning that previously decrypted copies cannot be recalled.

## CLI Surface

Suggested commands:

```text
concierge-plugin user init
concierge-plugin network create <name>
concierge-plugin network list
concierge-plugin network pair
concierge-plugin network join
concierge-plugin network members
concierge-plugin network grant <subject> <scope> <operations>
concierge-plugin network revoke <subject>
concierge-plugin network rotate <scope>
concierge-plugin actor create <name>
concierge-plugin actor grant <actor> <scope> <operations>
concierge-plugin sync status
concierge-plugin sync now [scope]
concierge-plugin merge status [scope]
concierge-plugin merge propose <scope> <heads...>
concierge-plugin merge accept <merge-cid>
```

Safety requirements:

- No password, capability key, recovery secret, or pairing secret in command
  arguments.
- High-value grants, revocation, recovery, and public publication require an
  interactive/local approval surface.
- `network grant` and `actor grant` cannot issue public-publication authority;
  they may grant only `request_publication_review`.
- Machine-readable status output is available without exposing secrets.

## Protocol and Storage Records

Add versioned schemas for:

- `UserIdentityDescriptor`;
- `NetworkDescriptor`;
- `MembershipCertificate`;
- `ActorCertificate`;
- `RevocationRecord`;
- `CapabilityGrant`;
- `CapabilityEpochRecord`;
- `HeadRecord`;
- `MergeCheckpoint`;
- `SyncReceipt`; and
- `SecurityEvent`.

Secret material remains outside the ordinary DAG:

```text
.concierge/security/
  user-identity/
  device-identity/
  actor-identities/
  networks/
  capability-vault/
  revocations/
  pairing-offers/
  security-events.jsonl
```

Requirements:

- owner-only permissions;
- encrypted capability vault;
- OS-keystore integration where available;
- atomic writes and durable rotation;
- symlink and ownership checks;
- explicit version migration;
- no secrets in logs or crash reports; and
- backup/recovery flow tested before release.

## Implementation Sequence

> **Reference implementations (vendored; port patterns, do not add as deps):**
> - **atproto** (`~/Downloads/atproto-main`, DECISIONS.md 0018) — `packages/identity`
>   +`did` for Phase A (key/rotation shape, *not* did:plc), `packages/sync`+`xrpc`
>   for Phase D, `packages/pds` store-and-forward for Phase F, `packages/lexicon` for
>   schema validation. TypeScript + federation-shaped — borrow data structures and
>   verification, not the centralized directory/relay topology.
> - **OrbitDB** (`~/Downloads/orbitdb-main`, DECISIONS.md 0021) — `src/oplog` for the
>   Message Entry/clock/conflict-resolution (already adopted) and **Phase E** merge
>   (`log.js`/`heads.js`: union merge + concurrent-head preservation), `src/sync.js`
>   for **Phase D** exchange-heads-then-converge, `src/access-controllers` for room
>   write-authorization, `src/databases/events.js` for the thread/feed primitive.
>   Replace its open-pubsub sync with capability-scoped, signed, encrypted sync.
> - **feed-generator** (`~/Downloads/feed-generator-main`, DECISIONS.md 0019) — the
>   skeleton/hydration + pluggable-algo + indexer→disposable-SQLite pattern for the
>   social feed.
>
> **Messenger functional milestone (DECISIONS.md 0017):** the read-only Messenger
> becomes send/receive as a byproduct of **Phase B** (`message-private` capability +
> room membership), **Phase D** (block sync moves message blocks), **Phase E**
> (message-ordering merge), and **Phase F** (offline store-and-forward). Its
> two-user/one-room acceptance demo is a Phase N gate — see 0017. **Messages are
> text-only (DECISIONS.md 0020):** no file/media upload through the message path —
> files are shared by CID over verified block sync, which also keeps untrusted media
> parsers off the message ingest path.
>
> **Social feed panel (DECISIONS.md 0019):** the Messenger also gets a feed panel.
> Port the atproto **feed-generator** pattern (`~/Downloads/feed-generator-main`):
> index the mesh sync stream into the disposable SQLite cache (0014.4), pluggable
> `algos` emit a **skeleton of CIDs**, hydrate from IPLD on demand. v1 = a "rooms"
> algo + a "collaboration ideas" algo, scoped by membership/room capability (no
> global feed registry, no did:plc/JWT). Global feed first, per-room later. The
> feed is **opt-in with a one-switch local toggle**: off = indexer/algos/feed
> participation all stop and the local Data Platter still works fully. Feed may
> **display** media but offers **no download** (DECISIONS.md 0020 — a UX guardrail,
> not a hard boundary). Rides on Phase B + Phase D.

### Phase A - Identity Hierarchy and Membership

- Define UserID, NetworkID, DeviceID, and ActorID semantics.
- Preserve existing install AgentIDs as DeviceIDs during migration.
- Add versioned network descriptors and membership/actor certificates.
- Add certificate-chain, expiry, scope, and revocation verification.
- Add encrypted identity/capability vault foundations.

Exit criteria:

- two installations have distinct device keys and can prove membership in the
  same private network without sharing a private identity key.

> **Progress (2026-06-10): Phase A node-side DONE (pure identity/crypto, no transport).**
> `crates/core/src/membership.rs`:
> - **Identity hierarchy** — `UserId` / `DeviceId` / `ActorId` (all hex Ed25519
>   public keys; the identifier *is* the key, no directory — Decision 0018) + a
>   derived `NetworkId` = `sha256(root_user : name : created_at)`. The existing
>   install AgentID *is* the DeviceID (migration rule). `Identity::generate()` added
>   for in-memory actor/ephemeral keys.
> - **Signed records** — `NetworkDescriptor` (founder-signed; carries no secrets) +
>   `MembershipCertificate` (issuer-signed, expiry-bounded, scoped capabilities) +
>   `RevocationRecord`. Canonical signing = the struct with an empty signature,
>   serialized in declaration order.
> - **The trust rule** — `verify_membership(cert, descriptor, now, revoked)`: accept
>   iff the descriptor self-verifies, the cert's network matches, the **issuer is a
>   root user** of the descriptor (Phase A: delegated chains are Phase B), the
>   signature verifies, it is unexpired/not-yet-invalid, **in the current membership
>   epoch**, and the subject is unrevoked. Deterministic errors that don't leak other
>   identities. Tampering capabilities/name breaks the signature.
> - **Vault + persistence** — `MemCli::{user_identity, create_network, networks,
>   device_membership}` under `<store>/security/` (`user-identity/`, `networks/`),
>   owner-only key files (vault *encryption* is Phase C). The UserID is created
>   distinct from the DeviceID and never copied.
> - **CLI** — `user init`, `network <create <name>|list|show [id]>`. Live-verified:
>   UserID ≠ DeviceID; founding a network signs a descriptor + self-issues this
>   device's membership cert; `network show` re-verifies both from disk.
> - **Tests: 11** including the exit criterion (two distinct device keys prove one
>   network with no shared secret), outsider-issuer rejection, capability-tamper,
>   expiry/not-yet-valid, stale-epoch, revocation (+ signed record), wrong-network,
>   and the persistence round-trip.
>
> **Remaining for full Phase A (later):** delegated issuer chains (root → admin device
> → actor) land with the Phase B capability model; `did:key` multibase *spelling*;
> the UserID rotation/recovery log (Phase G); vault encryption-at-rest (Phase C).

### Phase B - Secure Pairing and Scoped Capabilities

- Implement one-use pairing offers and authenticated transcript confirmation.
- Add explicit device/actor capability grant flow.
- Add namespace definitions and default least-privilege agent policies.
- Connect private sync authorization to the Data Platter privacy guard.

Exit criteria:

- a new device can join only after approval and can access only the exact scopes
  granted during pairing.

> **Progress (2026-06-10): Phase B node-side DONE (protocol + crypto; the libp2p
> Noise channel is the existing transport, wired in Phase F).** Two modules:
>
> **Scoped capabilities** — `crates/core/src/capability.rs`:
> - `Operation` (the 12 ops; **no public-publish op exists** — the strongest is
>   `RequestPublicationReview`, which only *opens* local review; Decision 0026).
>   `Operation::agent_defaults()` = least-privilege read-only.
> - `Namespace` = `NetworkId` + `NamespaceScope {All, Personal, Project, Room,
>   Agent}` with `canonical()` + `covers()` (the scope-containment that stops
>   widening). Read/write are separate operations — read never implies write.
> - `Capability` (signed, scoped, expiry-bounded, `delegable`). `verify_capability`
>   is the **delegation trust rule**: validly signed, in-network, unexpired,
>   in-epoch, subject+issuer unrevoked, and the issuer authorized — a **root user**
>   *or* a delegable parent (verified recursively) whose authority is a superset
>   (same-or-narrower namespace, ⊇ operations, no later expiry — "can't grant more
>   than you hold"). Tests: 9 (delegation subset, scope/privilege escalation,
>   non-delegable, broken chain, mid-chain revocation, least-privilege defaults).
>
> **Secure pairing** — `crates/core/src/pairing.rs`:
> - `PairingOffer` (one-use, short-lived, admin-signed, **no secrets** — QR-safe) →
>   `PairingResponse` (new device signs the handshake transcript = proof-of-possession
>   of its DeviceID key) → `confirmation_phrase` (6-digit SAS from the transcript;
>   a MITM that swaps any key changes it → humans reject, fails closed) →
>   `approve` (issues a membership cert + exactly the approved capabilities, signed
>   by the network root — the single-user multi-device case).
> - One-use is enforced + persisted: `MemCli::{create_pairing_offer, complete_pairing
>   (atomic consume + security event), accept_pairing_grant, device_capabilities}`.
>   A replayed offer fails closed.
> - Tests: 7 incl. the exit criterion (a paired device gets *exactly* the approved
>   scopes, nothing more) and a two-device end-to-end one-use integration test.
> - **CLI:** `network <pair | respond <offer.json> | approve <response.json>
>   --namespace NS --ops a,b | accept <grant.json>>` — live-verified two-store loop.
>
> **Remaining for full Phase B (later):** multi-USER invites (issue to another
> person's UserID — needs delegated membership-cert issuance, not just root); actor
> enrollment UI; connecting sync authorization to the Data Platter guard lands with
> Phase C/D when sync exists; rate-limited repeated-failure logging.

### Phase C - Capability-Encrypted Private Foundations

- Implement Decision 0011 encrypted private subgraphs and capability envelopes.
- Add the password-authorized `Convert to encrypted private and share` flow.
- Ensure private sync manifests can contain ciphertext only.
- Define and enforce locked-vault relay-only versus decrypt/write behavior.
- Prevent private CID public-DHT announcements and prepare private-swarm mode.

Exit criteria:

- locked plaintext can be deliberately converted into a new capability-encrypted
  graph, and no private-sync path can emit its plaintext source graph.

> **Progress (2026-06-10): Phase C node-side DONE.** The encryption foundations
> *already existed* (`crates/core/src/private.rs` + `crates/crypto`, Decision 0011):
> `convert_and_share_private` (password-authorized, one-shot grant, plaintext stays
> locked), `build_private_replication_plan` (ciphertext-only, posture
> `private-swarm-no-public-dht`, needs **no vault key** → locked-vault relay-safe),
> and tests already proving *plaintext-locked-after-convert*, *ciphertext-only
> manifest/CAR*, and *locked-vault-exports-but-cannot-decrypt*. The exit criterion
> was met by that layer.
>
> The Phase N work was the **bridge to the membership/capability model** —
> `crates/core/src/private_sync.rs`: a member may receive a namespace's ciphertext
> **iff** they present a verified [`Capability`] granting `sync_read` on that
> namespace (chained to a root, unexpired, in-epoch, unrevoked).
> `authorize_namespace_read(cap, chain, namespace, descriptor, now, revoked)` is the
> pure gate; `MemCli::private_manifest_for_member(...)` finds the conversion, parses
> its destination into a real `Namespace` (added `Namespace::parse`, the inverse of
> `canonical()`), authorizes the recipient, then returns the existing ciphertext-only
> plan. So replication authority now flows from the same scoped model as everything
> else, and a revoked/unscoped member is cut off — while possession of the inert
> ciphertext alone still reveals nothing. Tests: 5 (authorized→ciphertext-only;
> write-only refused; wrong-namespace refused; revoked cut off; the pure gate). The
> gate is the library API the **Phase D** block-sync transport calls; no user CLI yet.
>
> **Remaining (later):** private-swarm PSK wiring already exists in `node.rs` (Phase 8
> sidekick); capability-epoch key rotation is Phase G; the Data-Platter network UX is
> Phase H.

### Phase D - Signed Heads and Block Synchronization

*Reference: OrbitDB `src/sync.js` (exchange-heads-then-converge) and atproto
`packages/sync`+`xrpc` — see DECISIONS.md 0021/0018. Add signing, capability
scoping, and encryption on top; do not adopt OrbitDB's open pubsub.*

- Implement signed namespace head records.
- Add authenticated request-response head, block, and bounded CAR protocols.
- Stage, verify, and atomically import incoming blocks.
- Add deduplication, retry, progress, resource limits, and sync receipts.
- Reject synchronization of plaintext for encrypted-private namespaces.

Exit criteria:

- two authorized devices exchange only missing verified ciphertext blocks and
  converge on the same non-conflicting namespace head.

> **Progress (2026-06-10): Phase D node-side DONE (protocol logic; the libp2p
> request-response wire handlers are Phase F).** `crates/core/src/sync.rs`:
> - **`HeadRecord`** — signed namespace head advertisement (heads as a sorted/deduped
>   **set**; concurrent heads expected). `verify_head_record` enforces the rule that
>   the signer holds `sync_write` on the namespace: the signer's capability is
>   **embedded** (the "signer_certificate") and must verify (chain to a root,
>   unexpired, in-epoch, unrevoked), its subject must equal the signer, and it must
>   `authorize(SyncWrite, namespace)`. A reader cannot advertise; a tampered record
>   (e.g. a head added after signing) is rejected.
> - **`reconcile(local_heads, remote, local_has)`** — exchange-heads-then-converge
>   (after OrbitDB `src/sync.js`, but signed + capability-scoped): the converged set
>   is the **union** (concurrent heads preserved, never dropped — real merge is
>   Phase E), and `missing_heads` is what to pull.
> - **Bounded, verified block import** — `MemCli::{has_block, missing_blocks,
>   pull_blocks, pull_reachable}`. Every block is **CID-verified before durable
>   import** (`store_verified_raw_block`), already-present blocks are **deduped**
>   (never refetched), and the transfer is bounded by `SyncLimits {max_blocks,
>   max_bytes}`. A wrong-CID block aborts and never enters the store. Added
>   `MemCli::block_links` (immediate, non-recursive links — `walk` can't be used
>   mid-pull since it recurses into not-yet-fetched children).
> - The CID set to converge is an authorized **manifest**; for an encrypted-private
>   namespace that is the ciphertext manifest from Phase C `private_sync`, so only
>   ciphertext is ever exchanged.
> - Tests: 7 incl. the exit criterion twice — a plaintext two-device convergence
>   (pull only missing, verified; re-sync is a no-op) **and** an end-to-end
>   **ciphertext** convergence (B pulls the ciphertext manifest and converges; every
>   received byte is inert ciphertext, no plaintext crosses the wire) — plus
>   writer-only head authority, tampered-record/tampered-block rejection, and the
>   block budget.
>
> **Remaining for full Phase D (Phase F wiring):** the libp2p request-response
> behaviour in `crates/net` (CID-existence check, bounded block/CAR fetch, head
> exchange) calling these gates; signed `SyncReceipt` persistence; per-peer rate
> limits/timeouts at the transport. The `network sync now/status` CLI lands with that.

### Phase E - Concurrent Graph Merge

*Reference: OrbitDB `src/oplog/log.js`+`heads.js`+`conflict-resolution.js` — the
append-only Merkle-DAG log with union merge, concurrent-head preservation, and the
deterministic (Lamport, then AgentID) sort — see DECISIONS.md 0021. Never
last-writer-wins.*

- Add multi-parent merge checkpoints while retaining old checkpoint support.
- Implement ancestry detection, fast-forward, concurrent-head preservation, and
  deterministic conflict calculation.
- Add merge proposal/acceptance authority and Data Platter conflict views.
- Apply supersession, message ordering, mutable-name, and shared-posture merge
  rules.
- Keep local lock overlays outside merge state and preserve known-public
  exposure independently.

Exit criteria:

- two devices can write while disconnected, reconnect, preserve both histories,
  and converge after an explicit valid merge without synchronizing local locks
  or erasing known-public exposure.

> **Progress (2026-06-10): Phase E node-side DONE — the A–E convergence core is
> complete.** `crates/core/src/merge.rs`:
> - **Ancestry + classification** — `MemCli::{checkpoint_parent, is_ancestor (bounded
>   parent-chain walk), classify_heads}` → `HeadRelation {Equal, AAncestorOfB,
>   BAncestorOfA, Concurrent}`.
> - **Resolution (rules 1–2)** — `MemCli::merge_heads(heads) -> MergeOutcome`: collapse
>   any head that is an ancestor of another (**fast-forward**), retain the rest as
>   concurrent tips (**preserve both, never last-writer-wins**). Deterministic
>   (sorted) and order-independent → every peer computes the same outcome.
> - **`MergeCheckpoint`** — signed, **multi-parent** (≥2 distinct, sorted), references
>   both histories without rewriting them. `verify` requires the author hold
>   `merge_accept` on the namespace (embedded capability chains to a root) + a valid
>   signature; a reader or a single-parent "merge" is rejected. Existing single-parent
>   checkpoints are untouched.
> - **`MemCli::apply_merge`** — verifies the merge, then records it as a real
>   content-addressed node **linking both parents** as provenance (via
>   `put_node_derived`). The merged-head CID is **deterministic in the merge's
>   fields**, so two devices that accept the same merge compute the **same** head and
>   converge; the head walks into both branches.
> - Tests: 4 incl. the exit criterion — two devices write disconnected, sync both
>   branches (Phase D), see the same concurrent head set, apply one authorized merge,
>   and converge on an identical merged head reaching both histories — **and a
>   device-local lock on one branch does not synchronize or merge** to the other
>   device. Plus fast-forward/concurrent classification, order-independent
>   determinism, and the ≥2-parents / merge_accept authority checks.
>
> **Release gate:** A–E now pass, so *automatic shared-memory convergence* is
> demonstrable. **Remaining (later sub-phases):** richer semantic-conflict surfacing +
> Data Platter conflict views (Phase E UX / Phase H); supersession/message-ordering/
> mutable-name merge refinements; the libp2p request-response wiring (Phase F).

### Phase F - Discovery, NAT, Relay, and Offline Sync

- Add AutoNAT and authenticated rendezvous/discovery.
- Complete request-response sync over direct and relayed paths.
- Add encrypted store-and-forward and retention limits.
- Test separate-NAT, relay-only, intermittent, and partition-healing scenarios.

Exit criteria:

- authorized devices behind separate home routers reconnect and converge without
  manual address exchange.

> **Progress (2026-06-11): Phase F core transport DONE — the A–E protocol logic now
> moves over real libp2p.** `crates/net/src/lib.rs` (the existing Phase 5.7
> `ConciergeNode`, which already had relay v2 + DCUtR + identify for NAT traversal):
> - **Request-response block/head sync** (`/concierge/sync/1.0.0`, CBOR codec):
>   `SyncRequest {GetBlock(cid), GetHeads(namespace)}` / `SyncResponse
>   {Block(Option), Heads(Option)}`. Added the `request_response` behaviour to the
>   composed `Behaviour`, command/event wiring (`ConciergeNode::{request_block,
>   request_heads}` → `NodeEvent::{BlockReceived, HeadsReceived}`), and a
>   request-id→request map so replies are labeled. A miss returns `None` (a
>   deterministic "not here" that reveals nothing else). The serve side is a
>   `SyncProvider` closure the application supplies; `store_provider(Arc<MemCli>)`
>   serves blocks straight from the content-addressed store. The transport never
>   reads the store itself and never CID-verifies — the **application verifies on
>   import** (Phase D `pull_blocks`).
> - **AutoNAT** behaviour added for reachability assessment → `NodeEvent::NatStatus
>   {public|private|unknown}` (the `private` signal to reserve a relay).
> - Tests (over the real libp2p localhost harness): a peer fetches an exact block
>   over request-response; an unknown CID returns a clean negative; and **two real
>   stores converge over libp2p** — A serves a real block, B fetches it over the
>   wire, CID-verifies, and imports it (15 net tests, all green).
>
> **The separate-NAT exit criterion is real-internet integration, by design** (the
> plan's own §Transport note: "tests must cover two peers behind separate consumer
> NATs, not only localhost"). The *mechanism* is now all present — relay v2 + DCUtR +
> AutoNAT + request-response sync — but proving two-home-router convergence needs a
> manual/integration deployment, not an in-sandbox unit test.
>
> **Update (2026-06-11): the sync loop is now DRIVEN end-to-end.**
> `concierge_net::sync_from_peer(node, events, peer, mem, descriptor, namespace,
> revoked, timeout)` runs the whole Phase D loop over the live connection:
> exchange-heads → `verify_head_record` → `reconcile` → fetch only the missing,
> **CID-verified** blocks (walking the graph down via `block_links` as they arrive)
> → adopt the converged heads → return a `SyncReceipt`. Serve side: `store_provider`
> now also serves the signed head record (`GetHeads`), and core maintains it —
> `MemCli::{publish_head (writer, sync_write-signed), stored_head, local_heads,
> set_local_heads, import_verified_block}`. **CLI:** `sync <status | now --peer
> <multiaddr> --namespace NS>`. **Test:** two real `MemCli` stores converge in one
> `sync_from_peer` call over libp2p (pulls exactly the missing graph; a second sync
> is a no-op) — 16 net tests green.
>
> **Update (2026-06-11): store-and-forward + rendezvous DONE.**
> - **Store-and-forward** — the sync protocol gained `PutBlock(cid, bytes)`;
>   `relay_provider(mem)` *accepts* pushed blocks (CID-verified before storage,
>   bounded by `MAX_PUSH_BLOCK_BYTES`, stored as inert ciphertext). A writer pushes
>   its blocks to an always-on relay and may go offline; another peer pulls them from
>   the relay (`ConciergeNode::push_block` → `NodeEvent::BlockStored`). Tested: a relay
>   holds a block for an offline peer who then fetches it.
> - **Rendezvous discovery** — added the libp2p `rendezvous` client + an opt-in
>   `rendezvous_point` server to the node. `register_rendezvous` / `discover_rendezvous`
>   → `NodeEvent::{RendezvousRegistered, RendezvousDiscovered}`. Tested: A registers at
>   a point, B discovers A there **with no manually-exchanged A↔B address** — the last
>   clause of this exit criterion, at the protocol level. 18 net tests green.
>
> **Remaining for full Phase F:** per-peer request rate-limits/timeouts (partly covered
> by `connection_limits` + the push size cap; finer limits fall under the Security
> Review Gate). The exit criterion's **two-separate-consumer-NAT** convergence is still
> real-internet integration, not an in-sandbox test — but every mechanism it needs
> (relay v2 + DCUtR + AutoNAT + rendezvous + the driven sync loop) is now present.

### Phase G - Revocation, Recovery, and Hardening

- Add signed revocation records and membership/capability epochs.
- Implement future-access cutoff and namespace key rotation.
- Add encrypted identity recovery and device replacement flow.
- Complete protocol fuzzing, abuse limits, security audit, and operational
  documentation.

Exit criteria:

- a revoked device cannot submit accepted writes or obtain new namespace epochs,
  and remaining devices continue syncing after rotation.

> **Progress (2026-06-11): Phase G access-control revocation DONE.**
> `crates/core/src/revocation.rs`. The verifiers already threaded a `RevocationSet`
> + `membership_epoch`; this makes revocation **real, signed, and propagable**:
> - `NetworkDescriptor::advance_epoch(root)` — bumps the membership epoch and
>   re-signs, so **every prior cert/capability is now stale** (the network-wide
>   future-access cutoff).
> - `MemCli::revoke(network_id, subject_id)` — root-only: advances the epoch, signs a
>   [`RevocationRecord`] at the new epoch, persists both the advanced descriptor and
>   the record to `<store>/security/networks/<id>.revocations.json`, logs a security
>   event. Prospective only (the honest-limits note: it cannot recall data a removed
>   device already holds).
> - `MemCli::revocation_set(network_id)` — builds the `RevocationSet` every verifier
>   consults from the ledger, honoring **only root-signed records** (a forged
>   revocation cannot cut off a legitimate member).
> - `MemCli::grant_capability(...)` — the grant/re-issue flow at the current epoch;
>   refuses a revoked subject. Remaining members are re-granted here after a rotation.
> - **CLI:** `network grant <subject> --namespace NS --ops a,b` (prints a
>   `PairingGrant` the member installs with `network accept`) and `network revoke
>   <subject>`. Live-verified: grant → revoke (epoch 0→1) → a grant to the revoked
>   subject is refused.
> - Tests: 3 incl. the **exit criterion** — revoke B (epoch advances), B's old
>   head/capability are rejected and B cannot obtain any valid capability at the new
>   epoch, while C is re-granted at the new epoch and keeps signing valid heads —
>   plus forged-revocation-ignored and root-only-can-revoke.
>
> **Update (2026-06-11): capability-key rotation DONE.** `crates/core/src/private.rs`
> — `MemCli::rotate_private_capability(ciphertext_root, password)`: unlocks the vault,
> calls `concierge_crypto::rotate_tree` (the Cryptree key-rotation operation — decrypt with the
> owner's current key, rebuild under **fresh keys**), stores the new ciphertext
> blocks, seals the new key, and records a new `PrivateConversion` at the **next
> `capability_epoch`** (new field). A revoked holder's old read key gets a
> `CidMismatch` on the rotated root — proven cryptographically in the test — while the
> new key recovers identical plaintext. **Honest limit upheld:** the *old* ciphertext
> blocks a removed device already fetched are not unsent; rotation protects the
> re-rooted graph going forward. Surface: GUI `/api/network/rotate` mutation (password
> in the loopback body, never the URL — same pattern as convert-private). Tests: 1
> core (the relock + old-key cutoff + epoch advance) + 1 gui guard.
>
> **Update (2026-06-11): UserID recovery / device replacement DONE.**
> `crates/core/src/recovery.rs` — the `did:plc` operation-log *idea* with **no central
> service**. A UserID carries an **append-only, recovery-key-signed rotation log**:
> `UserIdentityOp {Genesis, Rotate, Recover}` chained by `prev` = the prior op's hash;
> `verify_and_resolve` replays it to the **current active key**. The **stable id** (the
> genesis key) and the recovery key are invariant across the chain — so the active key
> can rotate (device replacement, signed by the current active key) or be recovered
> (signed by the offline recovery key when the active key is lost) **without changing
> who the user is**. A superseded key can no longer rotate; an unauthorized signer,
> broken chain, tampered op, or changed invariant is rejected. `MemCli::{establish_user_recovery
> (generates + stores the recovery key), rotate_user_key (installs a new active
> `user.key`), recover_user_key, user_identity_log}`; added `Identity::save`. **CLI:**
> `user <recovery | rotate | recover | show>`. Tests: 9 (8 log-replay incl. recover/
> superseded-key/tamper/invariant + 1 MemCli establish→rotate→recover integration).
>
> **Update (2026-06-11): rotated-root recognition DONE.** `verify_membership_with_logs`
> and `verify_capability_with_logs` accept a `&[UserIdentityLog]`; the root check now
> uses `NetworkDescriptor::resolved_root_keys(logs)` = the genesis `root_user_ids` plus
> each root's **current active key** resolved from its rotation log. So a
> recovered/rotated root that signs with a new key is still recognized as a root
> (verified by a test: a cert/cap signed by the rotated key is rejected without the log,
> accepted with it). The no-logs variants are unchanged (`verify_membership` = `…_with_logs(…, &[])`).
>
> **Remaining for full Phase G:** protocol fuzzing / abuse-limit hardening / external
> security audit (the plan's Security Review Gate — by design not an in-sandbox unit test).

### Phase H - Complete Data Platter Network UX

- Add network/device/actor map, pairing, scope grants, sync health, conflicts,
  merge review, and revocation.
- Clearly separate private sync from public publication.
- Add actionable diagnostics for offline, stale, revoked, unauthorized, and
  incomplete-rotation states.

Exit criteria:

- a non-technical user can pair a second computer, understand what it can access,
  observe convergence, and revoke it without using the CLI.

> **Progress (2026-06-11): Phase H network map DONE (the read/admin surface).**
> `crates/gui/src/lib.rs` + `index.html` — a new **Network** tab in the Data Platter:
> - **`/api/network`** read endpoint (`network_json`): the identity hierarchy (root
>   UserID vs this DeviceID — visibly distinct), each network with its
>   `descriptor_valid` / `is_root` / `membership_epoch`, this device's membership
>   validity + granted **capabilities and whether each still verifies** (so a stale
>   post-rotation cap shows as stale), and the **revoked** subjects. Computed from the
>   Phase A–G verifiers (`verify_membership` / `verify_capability` / `revocation_set`).
> - **Mutations** (CSRF-gated, no CLI): `/api/network/create` (found a network) and
>   `/api/network/revoke` (revoke a subject → epoch advances), both returning the
>   refreshed map. The eyebrow text states the invariant — private sync is
>   capability-scoped and **never** authorizes public publication.
> - **UI:** the Network tab renders per-network cards (root/verified/epoch pills,
>   capability rows colored by validity, a revoked list, and a root-only Revoke
>   control) + a Create-network form.
> - Tests: the map surfaces membership/capabilities/epoch and a revocation advances
>   the epoch + lists the subject (gui 36 tests green). The HTML/JS itself is
>   `include_str!`-baked (needs a release rebuild + restart to view) and is not
>   unit-tested; the endpoint logic is.
>
> **Remaining for full Phase H:** the **pairing wizard** UX (QR display + the
> offer→response→approve→accept flow in-GUI — today pairing is the CLI `network
> pair`); **live sync health** (last-sync / pending-block count / current heads,
> forks, merge checkpoints, unresolved conflicts) and the **merge/conflict review**
> views — these depend on the Phase F sync daemon reporting state; and the
> actor/agent enrollment surface.

### Phase I - Social Legibility Layer (Decision 0024)

Post-core UX/ranking layer that makes the user's *own* trust and *own* graph
legible to them. Depends on the network core (A–G) and the Phase 8 librarian's
tri-kernel (gravity/density). **Strictly local/personal — no token economy, no
karma, no global people-ranking (excluded by Decisions 0012 + 0022 + 0024).**

- **Trust Thermometer (Messenger):** label each message by the authentication tier
  it crossed — *Local* (A) / *Device* (B) / *Global Signed* (C) — surfacing the
  tiered regime above. Honest auth-strength only; the badge must never imply
  crypto not yet shipped. Cheapest first slice: render over today's Tier A
  capabilities + Ed25519 Tier C, before Tier B/PQ land.
- **Personal social-gravity lens (Data Platter):** weight tri-kernel gravity by
  the user's own follow graph (`social.rs`, Decision 0007) so nodes that people
  the user follows link to cluster/brighten toward center. A personal relevance
  lens computed over the user's own follows — never a global score that ranks
  people.
- **Structural-importance feed ranking (Messenger):** order messages by
  graph-gravity / load-bearing-ness (how many decisions/files a message ties
  together) — the structural math from Decision 0022. Framed as importance, never
  "reputation" or "hottest".
- **Explicitly excluded:** self-minting / "karma" / proof-of-contribution
  (`cyber-master/rewards.md`'s $CYB mint/burn/lock economy). Reopening it would be
  a separate decision against 0012 + 0022, not a Phase I feature.

Exit criteria:

- a message's trust tier is visible and matches the authentication actually used;
- the Data Platter can visibly cluster nodes by the user's follow graph without
  emitting or storing any global reputation score;
- feed ranking reflects graph-gravity and contains no popularity/karma signal.

> **Progress (2026-06-11): Phase I social legibility DONE (the three local signals).**
> `crates/core/src/legibility.rs` — strictly local/personal, **no** global
> reputation/karma/vote/token (the module header states the 0012+0022+0024 exclusion):
> - **Trust thermometer** — `TrustTier {Local, GlobalSigned, Unverified}` +
>   `message_trust_tier(envelope, this_agent)`: your own message is `Local` (Tier A
>   capability/possession), another identity's **Ed25519-verified** message is
>   `GlobalSigned` (Tier C), a tampered/unsigned one is `Unverified`. **Tier B
>   (Device/Enclave-MAC) is deliberately absent** — not shipped, so the badge can
>   never claim it. The tier always matches the authentication actually used.
> - **Structural importance** — `structural_importance(envelope)` = how many CIDs a
>   message ties together (the Decision 0022 gravity intuition), framed as
>   load-bearing-ness, never popularity/engagement.
> - **Personal social-gravity lens** — `social_gravity_factor(author, follows)`: a
>   `>1.0` multiplier that brightens nodes from people **you** follow (Decision 0007
>   follow graph). A lens over the user's *own* follows; emits/stores **no** global
>   score about anyone.
> - **Surfaced** in the Messenger (`thread_json` adds `trust_tier`/`trust_label`/
>   `importance`/`followed` per message; `index.html` renders a trust badge +
>   follow + ties-N pills, colored by tier — `Unverified` in vermilion).
> - Tests: 5 core (own→Local, other-verified→GlobalSigned, tampered→Unverified, the
>   never-claims-an-unshipped-tier honesty invariant, importance counts links, the
>   lens is purely local) + the gui thread test asserts the surfaced fields. All green.
>
> **Remaining for full Phase I:** weighting the Data Platter **graph** gravity by the
> follow lens (the function exists; wiring it into the graph layout/brightness is a
> deeper GUI change) and ordering the message feed by structural importance (the
> signal is surfaced per-message; a sort toggle is the remaining UX). Tier B (Enclave
> MAC) + post-quantum Tier C remain deferred hardening (Phase G), and the badge will
> only show them once their crypto ships.

## Required Test Coverage

### Identity and Enrollment

- Same UserID/network, distinct DeviceIDs, no copied device secret.
- Existing install AgentID migrates without identity loss.
- Actor contributions verify through the correct device/network certificate
  chain.
- Expired, wrong-network, wrong-scope, tampered, and revoked certificates fail.
- Pairing offer expiry, replay, race, and confirmation mismatch fail safely.
- Pairing never transfers or logs a root/private device key.

### Authorization and Privacy

- Device and actor can read/write only granted namespaces.
- Read-only capability cannot write.
- Sync capability cannot publish publicly.
- `request_publication_review` cannot publish without an exact local
  password-authorized one-shot grant.
- Agent cannot widen or delegate its own capabilities.
- Locked plaintext and unrelated local roots never enter a sync manifest.
- Converting locked plaintext creates new ciphertext CIDs and leaves the source
  graph locked/local-only.
- Private sync transfers ciphertext only.
- Public DHT never receives private namespace CIDs.
- Local lock records never synchronize or merge.
- Most-restrictive shared namespace posture wins during merge.
- Known-public exposure remains permanent after restrictive merge, locking, or
  encryption.
- A locked vault may relay authorized ciphertext but cannot decrypt, write,
  issue capabilities, or perform semantic merges.

### Synchronization

- Missing blocks transfer; existing blocks deduplicate.
- Tampered, wrong-CID, oversized, malformed, and unauthorized blocks reject
  before durable import.
- Interrupted sync resumes without corrupting the store.
- Arrival order does not change visible heads or conflict set.
- Sync cannot traverse outside the authorized namespace.
- Request, response, traversal, concurrency, and storage limits hold under
  hostile inputs.

### Merge and Convergence

- Ancestor head fast-forwards.
- Concurrent offline writes preserve both heads.
- Multi-parent merge checkpoint references all merged heads canonically.
- Existing single-parent checkpoints remain readable.
- Superseding facts retain complete history.
- Concurrent mutable-name bindings remain visible until resolved.
- All peers converge after receiving the same valid merge record.
- Public/private graph boundaries never merge silently.
- Local lock differences do not alter the shared merge result.
- Known-public evidence is never erased by merge.

### Revocation and Rotation

- Revoked device/actor writes and head records are rejected.
- Revoked subject cannot obtain a new capability epoch.
- Remaining devices receive and use rotated keys.
- Offline remaining device can catch up after rotation without restoring revoked
  access.
- UI and CLI state the limits of revocation accurately.

### Transport and Resilience

- Direct LAN synchronization.
- Two peers behind separate consumer NATs.
- Relay-only connection.
- Peer offline during writes, then reconnects.
- Multi-day partition healing.
- Store-and-forward relay cannot decrypt private blocks.
- Unauthorized and resource-exhaustion attempts are bounded.

### Data Platter

- Pairing scopes shown before approval match issued capabilities.
- Network map accurately shows devices, actors, heads, sync lag, and revocation.
- Conflict and merge views preserve provenance.
- Private sync and public publish controls are visually and behaviorally
  distinct.
- Revocation shows pending rotation and honest historical-access warning.

### Social Legibility (Phase I)

- A message's displayed trust tier (Local/Device/Global Signed) matches the
  authentication discipline actually used; the badge never claims a tier whose
  crypto is not shipped.
- The social-gravity lens is computed from the user's own follow graph and
  produces no persisted or transmitted global reputation/karma score.
- Feed ranking is a pure function of graph-gravity (decisions/files tied
  together); it contains no popularity, vote, or stake signal.
- No $CYB-style mint/burn/lock, self-minting, or karma artifact exists anywhere
  in records, GUI, or wire (guards Decisions 0012 + 0022 + 0024).

## Security Review Gate

Before describing the feature as a secure private multi-agent network, complete:

- threat-model review;
- identity and certificate protocol review;
- cryptographic design review;
- parser/request-response fuzzing;
- malicious-peer and resource-exhaustion tests;
- secret-handling and log audit;
- recovery and revocation tabletop exercises; and
- external security review before broad deployment.

## Release Gate

Do not claim automatic shared-memory convergence until Phases A through E pass.

Do not call synchronized content cryptographically private until Phases A
through D pass.

Do not claim reliable real-internet private networking until Phase F passes.

The release demonstration must show:

1. Computer A creates a private network.
2. Computer B pairs through explicit approval and receives limited scopes.
3. Both devices retain distinct keys and verifiable identities.
4. An authorized namespace synchronizes by missing CID and deduplicates blocks.
5. Both devices write while disconnected, reconnect, preserve both branches, and
   converge through a multi-parent merge.
6. An agent can collaborate only within its granted subgraph.
7. Locked/local-only and unrelated data never leave the source device.
8. Private synchronized blocks are ciphertext and unreadable without a
   capability.
9. Revoking computer B blocks future writes and new key epochs.
10. A local publication lock remains local and is not synchronized or merged.
11. Known-public exposure remains visible after a restrictive merge.
12. No durable network capability can publish publicly; public publication still
    requires the exact local password-authorized one-shot flow.

## Explicitly Deferred

- Global human-readable identity registry.
- Reputation/karma/token economies and global social-rank scores — permanently
  excluded, not merely deferred (Decisions 0012, 0022, 0024). This is distinct
  from Phase I's allowed signals: a *trust-tier* badge (authentication strength,
  not reputation) and *structural-importance* ordering (graph-gravity, not
  popularity), both computed locally with no global people-ranking.
- Automatic semantic conflict resolution that can discard a valid branch.
- Claiming deletion of copies already decrypted by another device.
- Full historical re-encryption on every routine revocation.
- Anonymous public-square Sybil resistance.
- Post-quantum identity migration before the core protocol is stable.

These may be addressed by later decisions, but none are prerequisites for the
private multi-agent network described here.
