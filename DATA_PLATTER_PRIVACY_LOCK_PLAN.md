# Data Platter Privacy Lock and Publication Guard Plan

## Objective

Protect users from accidentally exposing projects, files, prompts, responses, tool
results, checkpoints, or other reachable memory through Kubo, another publishing
backend, a public room, or an exported plaintext CAR.

The Data Platter will let a user select a node and mark its entire reachable
subgraph **Locked / Local-only**. A locked node must be unlocked from the Data
Platter with the store password before any public-sharing attempt can proceed.

This is a cross-cutting security phase, not only a GUI feature. The GUI presents
the control, but the core publication guard enforces it below every CLI, GUI, and
backend path.

## Non-Negotiable Safety Rules

1. **Personal-store roots are locked by default.** A manual lock button alone is
   insufficient because users will forget to lock sensitive nodes. Public-square
   rooms remain explicit signed-open exceptions, consistent with Decision 0012.
2. **Locking never mutates an IPLD node.** Nodes and CIDs are immutable. Lock state
   is a local policy overlay stored outside the DAG.
3. **A lock applies to the selected node and its entire reachable subgraph.**
   Node-only locks are unsafe because sharing a parent exports its descendants.
4. **Every public egress path must pass one core guard.** No CLI flag, GUI route,
   backend implementation, or adapter may bypass it.
5. **Unlocking does not publish.** It only creates a short-lived, scoped
   authorization. Publication remains a second explicit action.
6. **No permanent unlock state.** Unlock grants expire; public-publish and
   encrypt-and-share-private grants are one-shot.
7. **No password bypass.** Do not support `--force`, password CLI arguments,
   password environment variables, or stored plaintext passwords.
8. **Known-public data cannot be made private retroactively.** Locking a previously
   published node blocks future plugin-managed publication but cannot recall
   copies already fetched by others.
9. **Do not call plaintext policy-locked data cryptographically private.** The
   first implementation prevents accidental publication. True confidentiality
   requires Decision 0011 capability encryption.
10. **Local locks never become shared network policy.** A lock is a device-local
    safety decision and must never enter a DAG, CAR, sync record, namespace
    record, or merge.
11. **No durable capability authorizes public publication.** A device, actor,
    network membership, or shared namespace may request a publication review,
    but only an exact, local, password-authorized, one-shot public-publish grant
    may authorize public egress.

## Threat Model

### In Scope

- A user accidentally clicks Share or runs `concierge-plugin share`.
- A harness, GUI action, or future plugin route accidentally invokes publishing.
- A root selected for publication indirectly reaches locked files or nodes.
- A name is rebound between preview and publication.
- A backend publishes a broader graph than the user reviewed.
- A malicious webpage attempts to call the loopback GUI's unlock endpoints.
- Password guessing against the local unlock endpoint.

### Out of Scope for the Publication Lock

- A malicious process already running as the same OS user.
- A user bypassing Concierge and directly invoking Kubo, `mem`, or copying raw
  block files.
- Revoking content that was already published and copied.
- Protecting plaintext blocks from a compromised local machine.

Those require OS isolation, encrypted-at-rest content, and capability encryption.

## Privacy States

The GUI must display precise states rather than a single ambiguous padlock:

| State | Meaning | Public egress |
|---|---|---|
| `Locked / Local-only` | Local plaintext may exist; plugin-managed publication and plaintext export are blocked | Blocked |
| `Inherited lock` | Node is reachable from a locked root | Blocked |
| `View-unlocked` | Password accepted for temporary local inspection | Still blocked |
| `Publish-authorized` | Password accepted for this root and one public action | Allowed once, until expiry |
| `Encrypted private` | CID identifies ciphertext; access requires a capability | Capability share only |
| `Known public` | A local publish receipt exists | Already potentially irreversible |
| `Unknown exposure` | No receipt exists, but external/manual publication cannot be ruled out | Treat cautiously |

## Lock Semantics

### Local Policy Overlay

Store lock policy outside the DAG:

```text
.concierge/security/
  password.json
  locks.json
  guard.key
  unlock-grants/
  security-events.jsonl
```

- Directory permissions: owner-only.
- Files: owner read/write only.
- Writes: atomic temporary-file plus rename.
- Reject symlinks and unexpected ownership/permissions.
- Never place passwords, derived keys, unlock grants, or lock policy in a CAR,
  IPLD node, publish receipt, or shared message.
- Never synchronize lock records or resolve them through graph merge. Each
  device computes and enforces its own effective local locks.

Example lock record:

```json
{
  "root": "bafy...",
  "scope": "reachable_subgraph",
  "created_at": 1780700000,
  "label": "Project Apollo checkpoint",
  "reason": "user_locked"
}
```

### Effective Lock Calculation

An intended publication manifest is blocked when:

```text
walk(publication_root) intersects union(walk(each_locked_root))
```

This catches:

- directly sharing a locked node;
- sharing a parent that reaches a locked descendant;
- sharing a descendant covered by an inherited subgraph lock;
- sharing a newly-created root that links to older locked content.

The guard must return all blocking lock roots and the first relevant paths/nodes
so the GUI can focus them on the map.

### Local Lock Versus Shared Namespace Posture

Local lock policy and shared network privacy posture are deliberately separate:

- **Local publication lock** is a device-local policy overlay. It controls
  whether this installation may inspect plaintext, convert it to encrypted
  private form, export it, or publish it. It is never synchronized.
- **Shared namespace posture** is signed shared state such as
  `encrypted-private` or `signed-open`. Authorized peers may synchronize and
  merge that posture, but it cannot remove a local lock.
- **Known-public exposure** is historical evidence, not a permission. Once a CID
  is known public, that warning remains permanent even if future egress is
  locked.

For any local action, the effective future-egress policy is the most restrictive
combination of the local lock and shared namespace posture. Historical
known-public exposure is tracked independently and never becomes private again.

## Password and Unlock Design

Stay aligned with Decision 0011's password primitive: use **scrypt** with a random
salt and versioned parameters.

### Password Setup

- The first lock operation prompts the user to create and confirm a store
  password.
- Store only the salt, scrypt parameters, format version, and verifier.
- Never store or log the password.
- Zero password and derived-key buffers after use.
- Use constant-time verifier comparison.
- Apply rate limiting and exponential backoff after failed attempts.
- Password reset requires a deliberate local recovery flow and must never
  silently unlock existing policy.

### Unlock Grants

There are three distinct grants:

1. **View grant**
   - Created from the Data Platter after password verification.
   - Allows local record previews for a short session.
   - Never authorizes publication.

2. **Public-publish grant**
   - Created only from the Data Platter after password verification and manifest
     review.
   - Scoped to one exact root CID, one manifest digest, one backend, and one
     operation.
   - One-shot and short-lived.
   - Consumed atomically before the backend receives bytes.

3. **Encrypt-and-share-private grant**
   - Created only from the Data Platter after password verification and review
     of the destination private namespace and recipients.
   - Scoped to one exact plaintext root, one exact source manifest, one
     destination namespace, and one conversion operation.
   - One-shot and short-lived.
   - Creates a new capability-encrypted subgraph with new ciphertext CIDs.
   - Never authorizes public publication or synchronization of the original
     plaintext graph.

The CLI must not accept a password. If a locked target is sent to a public-share
command, it must return:

```text
Publication blocked: this root reaches locked data.
Open the Data Platter, review the manifest, and authorize one public publish.
```

If a locked target is sent to a private-share command, it must return:

```text
Private sharing blocked: this root is locked local plaintext.
Open the Data Platter and choose Convert to encrypted private and share.
```

## Core Egress and Publication Guard

Introduce one core policy service used by every egress path:

```text
build_egress_plan(root, operation, backend) -> EgressPlan
check_egress(plan, optional_grant) -> Allowed | Blocked
execute_approved_egress(plan, optional_grant, backend) -> EgressReceipt
```

`EgressPlan` includes:

- resolved root CID;
- operation (`public_publish`, `plaintext_car_export`, `public_room_attach`,
  `encrypt_and_share_private`, `private_encrypted_replicate`);
- exact reachable CID manifest;
- manifest digest;
- block count and byte size;
- decoded node kinds;
- file paths and media types where available;
- sensitivity warnings;
- backend and its network posture;
- intersecting explicit and inherited locks;
- known publication receipts.

### Mandatory Guard Placement

The guard must run:

- in `MemCli::share` before signing or backend dispatch;
- immediately before Kubo `dag/import`;
- before any optional pin-service upload;
- before plaintext CAR export leaves the process;
- before attaching a root/CID to a public room;
- before future network replication or MCP publishing;
- after resolving a name to its final CID.

Backend implementations receive an approved immutable `EgressPlan`, not an
unchecked root string.

`private_encrypted_replicate` does not use a public-publish grant. It is allowed
only when the exact manifest contains capability-encrypted blocks authorized for
the destination namespace and no local-only plaintext. A durable network
capability may authorize this private ciphertext replication, but it can never
authorize `public_publish`.

`encrypt_and_share_private` is a local transformation followed by private
ciphertext replication. The source plaintext manifest is reviewed and encrypted
locally but never sent. The guard authorizes egress only for the resulting
verified ciphertext manifest.

`EgressReceipt` records the operation type. Only `public_publish` creates a
`PublishReceipt` and known-public exposure evidence.

### Race and Consistency Protection

- Preview and publish use the same manifest digest.
- Recompute and recheck immediately before egress.
- Use a cross-process publication lock so lock changes and publication cannot
  race.
- If the root, name binding, reachable manifest, lock registry, backend, or grant
  changes, abort and require a new review.
- Locking wins over an in-progress publication that has not begun network egress.

## Safer Publishing Vocabulary

The current word `share` hides a dangerous distinction. Change the product
surface to make intent explicit:

- `pin-local`: local persistence only; must not imply privacy if Kubo is
  public-networked.
- `share-private`: capability-encrypted sharing from Decision 0011.
- `publish-public`: irreversible public-network publication.

Until capability encryption exists:

- `share` becomes an alias that refuses to proceed without an explicit public
  choice.
- Public Kubo publishing requires `publish-public` or an equivalent explicit GUI
  action.
- No target defaults to `latest`.
- No backend is treated as private unless its network posture is verified.

## Data Platter User Experience

### Node Interaction

Clicking a graph node opens a **Privacy and Publication** drawer:

- current precise privacy state;
- explicit or inherited lock source;
- affected reachable-node and file counts;
- known publish receipts;
- `Lock this subgraph` action;
- `Unlock for viewing` action;
- `Convert to encrypted private and share` action;
- `Review public publication` action;
- `Focus lock root` action for inherited locks.

### Visual Language

- Locked root: closed padlock and strong boundary ring.
- Inherited locked node: hatched/dimmed lock treatment.
- View-unlocked: open padlock with visible expiry countdown.
- Publish-authorized: temporary warning state with one-shot indicator.
- Known public: permanent public marker; never visually revert to private.
- Encrypted private: distinct key/capability marker, not the ordinary lock icon.

### Lock Flow

1. Select node.
2. Click `Lock this subgraph`.
3. If no store password exists, create and confirm one.
4. Show affected node/file count.
5. Confirm lock.
6. Graph immediately marks the root and inherited descendants.

Locking does not require entering the password after initial password setup.
Unlocking always requires it.

### Attempted Public Share of Locked Data

1. User invokes public share from GUI or CLI.
2. Core guard builds the exact manifest and finds lock intersections.
3. Publication is blocked before any bytes reach a backend.
4. GUI message states:

   ```text
   Public publication blocked.
   This root reaches 3 locked subgraphs containing 42 nodes and 8 files.
   Unlock and authorize publication from the Data Platter first.
   ```

5. `Show on map` focuses every blocking lock root.

### Public Authorization Flow

1. Select `Review public publication`.
2. Show exact node/file manifest, byte size, backend posture, sensitivity
   warnings, and known-public warning.
3. User enters password.
4. User checks an irreversible-publication acknowledgement.
5. User confirms `Authorize one public publish`.
6. A one-shot grant is created and immediately consumed by the requested
   publication.
7. A local security event and publish receipt are recorded.

Unlocking alone never triggers this flow.

## Sensitive Content Review

Before any public-publish grant is created, inspect decoded metadata and file
paths for likely secrets:

- `.env`, credentials, keys, certificates, wallet files;
- hidden files and excluded directories;
- common API-key/token patterns;
- private-key headers;
- unusually large blobs or unknown binary media;
- identity and security-policy files.

High-confidence sensitive findings block public publication by default. A later
override, if ever added, must require a second explicit security decision and
must not be a generic `--force`.

## True Cryptographic Privacy Follow-Up

The publication lock prevents accidental plugin-managed egress. It does not make
existing plaintext blocks confidential.

Implement Decision 0011 as the next security layer:

1. Encrypt private content before hashing so its CID identifies ciphertext.
2. Generate a random read key per private subgraph.
3. Wrap child keys under parent keys following the Cryptree model.
4. Store capabilities encrypted at rest.
5. Let the password unlock the local capability vault, not directly encrypt every
   node.
6. `share-private` hands over a capability, never a bare plaintext CID.
7. Never announce private plaintext CIDs to the public DHT.
8. Use a private swarm for sensitive/team replication where appropriate.

Changing an existing plaintext node to **Encrypted private** creates a new
encrypted subgraph and therefore new CIDs. The original plaintext graph must be
automatically locked, excluded from egress, and clearly labeled as a local
legacy copy. If it was already published, the UI must state that encryption
cannot revoke the published copy.

### Convert Locked Data to Encrypted Private

Locked/local-only plaintext may enter a shared private namespace only through an
explicit conversion:

1. Select the locked root in the Data Platter.
2. Choose `Convert to encrypted private and share`.
3. Review the exact source manifest, destination namespace, recipients, and
   capabilities.
4. Enter the store password.
5. Create and immediately consume an exact one-shot
   `Encrypt-and-share-private` grant.
6. Encrypt before hashing, producing a new ciphertext graph and new root CID.
7. Verify that the private sync manifest contains only ciphertext CIDs.
8. Keep the original plaintext graph locked/local-only.

This flow never unlocks the plaintext graph for ordinary network replication and
never creates public-publication authority.

### Locked Capability Vault Behavior

After restart, a locked capability vault must fail closed:

- The device may authenticate as its DeviceID and relay or retain already
  authorized ciphertext and signed head records without decrypting them.
- The device may not decrypt private content, create private graph writes,
  unwrap or issue capabilities, perform semantic merges, or convert plaintext.
- Pre-approved ciphertext replication may continue only if authorization can be
  verified without exposing vault secrets.
- Any operation requiring read/write keys waits for local vault unlock through
  the approved local UI or OS-keystore policy.
- Vault unlock never creates public-publication authority.

## Implementation Phases

### Phase A - Immediate Containment

- Disable implicit public publishing.
- Require an explicit public-publish operation.
- Remove any default-to-`latest` public path.
- Add a mandatory manifest preview and irreversible-publication warning.
- Document that standard Kubo is public-networked unless explicitly isolated.

Exit criteria:

- no ordinary plugin action can send store data to Kubo or another public
  backend without an explicit public-publish choice.

### Phase B - Lock Registry and Core Guard

- Add versioned lock registry and security-event trail outside the DAG.
- Add default locks for new personal-store checkpoint roots.
- Implement effective inherited-subgraph lock calculation.
- Implement `EgressPlan` and enforce it in all current publishing/export paths.
- Add typed `PublicationBlocked` errors with blocker details.

Exit criteria:

- every current egress path refuses manifests intersecting locked content.

### Phase C - Password and Scoped Grants

- Add password setup and scrypt verifier.
- Add view grants and one-shot public-publish grants.
- Add expiry, atomic consumption, rate limiting, constant-time comparison, and
  secret zeroization.
- Ensure CLI cannot unlock or bypass locks.

Exit criteria:

- only a password-authorized, exact, unexpired, one-shot plan can publish locked
  content.

### Phase D - Data Platter Controls

- Add Privacy and Publication drawer.
- Add lock, unlock, focus, manifest review, and public authorization flows.
- Add graph state styling and expiry countdowns.
- Add blocked-share navigation to every relevant lock root.
- Harden loopback mutation endpoints with POST-only routes, CSRF tokens, Origin
  and Host validation, no CORS, strict cookies, rate limits, and no secret logs.

Exit criteria:

- a user can lock a graph root, see inherited locks, and authorize exactly one
  reviewed public publication from the map.

### Phase E - Capability-Encrypted Private Sharing

- Implement encrypted private subgraphs and capability vault.
- Add `share-private`.
- Add the password-authorized `Convert to encrypted private and share` flow.
- Define and enforce locked-vault relay-only versus decrypt/write behavior.
- Add private-swarm support and prevent public DHT announcements for private
  content.
- Separate encrypted-private visuals from policy-lock visuals.

Exit criteria:

- private content remains unreadable even if its encrypted blocks are copied from
  public storage.

## Required Tests

### Core Guard

- Directly publishing a locked root is blocked.
- Publishing a parent that reaches a locked descendant is blocked.
- Publishing a descendant covered by an inherited lock is blocked.
- Publishing an unrelated unlocked root succeeds only with explicit public
  confirmation.
- Name rebinding between preview and publish invalidates authorization.
- Any manifest change invalidates authorization.
- Locking during preview prevents publication.
- Every backend and export path uses the same guard.
- No `--force`, environment variable, or direct backend call bypass exists.

### Password and Grants

- Correct password creates only the requested scoped grant.
- Incorrect passwords never create grants and trigger backoff.
- Passwords and derived keys never appear in logs, errors, DAG nodes, CARs, or
  receipts.
- View grants cannot publish.
- Publish grants are exact-root, exact-manifest, exact-backend, one-shot, and
  expiring.
- Reusing, modifying, or racing a grant fails.
- Restart clears ephemeral grants.

### GUI and Loopback Security

- Locked and inherited states render accurately.
- Clicking a blocker focuses the correct graph node.
- Share attempt displays exact blocker counts without exposing hidden body data.
- CSRF, invalid Origin, invalid Host, GET mutation, and cross-site requests fail.
- Password fields are never retained or autofilled into logs/state.
- Known-public nodes never display as private after locking.

### Cryptographic Privacy

- Encrypted private nodes produce ciphertext CIDs.
- Wrong or absent capabilities cannot decrypt.
- Capability sharing decrypts only the authorized subgraph.
- Plaintext CIDs never enter private-share manifests.
- Private content is never announced through public routing.
- Local lock records never enter sync records, DAG nodes, CARs, or merge state.
- A durable device/network capability cannot authorize public publication.
- Converting a locked root produces new ciphertext CIDs and leaves the original
  plaintext graph locked/local-only.
- Conversion never sends any source plaintext block; only the verified
  ciphertext manifest may replicate.
- Private encrypted replication creates an `EgressReceipt`, not a
  `PublishReceipt`, and does not mark ciphertext as known public.
- Only an executed `public_publish` operation creates known-public exposure
  evidence.
- A locked vault may relay authorized ciphertext but cannot decrypt, write,
  issue capabilities, or perform semantic merges.
- Known-public exposure remains visible after locking, encryption, or merge.

## Release Gate

Do not describe the plugin as safe for general project/file capture until Phases
A through D pass.

Do not describe data as **cryptographically private** until Phase E passes.

The release must demonstrate:

1. A personal checkpoint is locked by default.
2. A public Kubo share attempt is blocked before network egress.
3. The Data Platter focuses the blocking node.
4. A wrong password fails.
5. A correct password plus reviewed one-shot authorization allows only the exact
   requested publication.
6. A second publication attempt is blocked again.
7. A previously published node is clearly labeled irreversible/known-public.
