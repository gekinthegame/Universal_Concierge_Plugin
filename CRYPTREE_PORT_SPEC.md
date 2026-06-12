# Cryptree Port Spec — Decision 0011 Capability Encryption

## Source and provenance

Implements the **Wuala Cryptree** design — *Cryptree: A Folder Tree Structure
for Cryptographic File Systems* (Grolimund, Meisser, Schmid, Wattenhofer, 2006),
the cryptographic file-system design used by the Wuala storage system, which has
a long real-world track record including third-party penetration testing. We
port the **design and data shapes** onto proven RustCrypto primitives — our
stack is Rust + the `mem` content-addressing seam.

**"Already-proven tools" mandate.** Every cryptographic primitive below maps to
a battle-tested Rust crate or a primitive we already ship. We invent no crypto.

| Cryptree primitive | What it is | Proven Rust equivalent | Status in our tree |
|---|---|---|---|
| `TweetNaClKey` / `Salsa20Poly1305.secretbox` | NaCl secretbox = **XSalsa20-Poly1305**, 32-byte key, 24-byte nonce | `crypto_secretbox` (RustCrypto) **or** `dryoc` (pure-Rust libsodium) | add dep |
| `SigningKeyPair` / `PublicSigningKey` | Ed25519 | `ed25519-dalek` v2 | **already used** (identity, message sigs) |
| content hash → `Cid`/`Multihash` | SHA-256 multihash | `sha2` + `cid` | **already used** (CAR, blocks) |
| `CborObject` serialization | DAG-CBOR | `serde` + our existing JSON/CBOR block path via `mem` | adapt |
| scrypt vault password (`ScryptGenerator(15,8,1,32)`) | scrypt N=2^15,r=8,p=1,len=32 | `scrypt` crate | add dep (also needed by privacy-lock plan) |

Recommendation: **`crypto_secretbox`** (RustCrypto, same ecosystem as
`ed25519-dalek`/`sha2` we already pull) for the secretbox, **`scrypt`** for the
vault. If we want libsodium-exact byte compatibility later, `dryoc`
is the drop-in; for our own self-contained graph, RustCrypto is the lighter dep.

---

## The one core idea: a Cryptree is a graph of *keys*, parallel to the graph of *blocks*

Our DAG already links **blocks** by CID. A Cryptree overlays a second graph of
**symmetric keys**, where each edge is "key A encrypts key B." Holding one key
lets you walk to exactly the keys (and therefore the blocks) it transitively
unwraps — and no further. Access control *is* reachability in the key graph.

The entire edge primitive is the Cryptree `SymmetricLink` primitive (about 35
lines):

```
SymmetricLink::wrap(from: &Key, to: &Key) -> SymmetricLink   // encrypt `to` under `from`
SymmetricLink::unwrap(&self, from: &Key)  -> Key             // decrypt back to `to`
```

That's it. Every access relationship in the system is built from wrapping one
random key under another. Port this **first and verbatim** — it is the whole
mechanism.

---

## Primitives to port verbatim (small, self-contained)

### 1. `SymmetricKey` — secretbox wrapper

```rust
/// 32-byte XSalsa20-Poly1305 key. Wraps crypto_secretbox.
pub struct SymmetricKey([u8; 32]);

impl SymmetricKey {
    pub fn random() -> Self;                                   // OsRng, like our identity keys
    pub fn encrypt(&self, plain: &[u8], nonce: &[u8; 24]) -> Vec<u8>;  // secretbox
    pub fn decrypt(&self, cipher: &[u8], nonce: &[u8; 24]) -> Result<Vec<u8>, CryptoError>;
}
```

Cryptree detail to keep: **24-byte random nonce per encryption**, stored
alongside ciphertext (never derived, never reused).

### 2. `CipherText` — nonce + ciphertext envelope

Direct port of `CipherText.java`. Serializes as `{nonce, ciphertext}`.

```rust
pub struct CipherText { nonce: [u8; 24], bytes: Vec<u8> }

impl CipherText {
    pub fn build<T: Serialize>(from: &SymmetricKey, secret: &T) -> Self;     // nonce=random; encrypt(serialize(secret))
    pub fn decrypt<T: DeserializeOwned>(&self, from: &SymmetricKey) -> Result<T, CryptoError>;
}
```

### 3. `SymmetricLink` — the edge

```rust
pub struct SymmetricLink(CipherText);
impl SymmetricLink {
    pub fn wrap(from: &SymmetricKey, to: &SymmetricKey) -> Self;   // CipherText::build(from, to)
    pub fn unwrap(&self, from: &SymmetricKey) -> Result<SymmetricKey, CryptoError>;
}
```

These three types are ~120 lines of Rust and carry no implementation-specific baggage.

---

## The key-graph per node (the heart of `CryptreeNode.java`)

A node is a **directory** or a **file**. Each holds a small set of symmetric
keys, linked so that one *base read key* is the single capability you hand out.

### Directory keys: `{ base (read), parent (read), write }`
### File keys:      `{ base == parent (read), data, write }`

From `CryptreeNode.getParentKey/getDataKey/getWriterLink` (lines 264-295), the
links are:

```
                     ┌─────────────── rBaseKey (the read capability) ───────────────┐
                     │                          │                                   │
              encrypts child links       SymmetricLink → parentKey         SymmetricLink → signer (write key)
              (dir) OR data key (file)    (metadata + link to parent)       (only if this node can be written)
                     │                          │
            ChildrenLinks block          FileProperties + parent RelativeCapability
```

Concretely, three network-visible encrypted components per node (class doc,
lines 38-44), which we map onto **one or more `mem` blocks**:

| Cryptree component | Encrypted with | Contains | Our mapping |
|---|---|---|---|
| `fromBaseKey` block | `rBaseKey` | the `parentOrData` key, optional `SymmetricLinkToSigner` (write key), next-chunk link | a small CBOR block, body = `CipherText` |
| `childrenOrData` | dir: base key; file: data key | child `RelativeCapability` list **or** file bytes | block(s) of ciphertext |
| `fromParentKey` block | `parentKey` | `FileProperties` (name, size, ...) + optional link to parent's parent key | CBOR block, body = `CipherText` |

The crucial property (lines 268-276): **read access to a folder ≠ read access
to its parent.** `parentKey` is reachable *down* from a child you were granted,
but a child capability does not reach back *up* to siblings you weren't granted.
This is what makes "share one subfolder" safe.

---

## Read vs. write separation (the part our plans insist on)

Both plans require "read and write authority must remain separate." Cryptree does
this with **two independent key spines**:

- **Read spine:** `rBaseKey` → unwraps `parentKey`/`dataKey` → decrypt content.
- **Write spine:** a separate `wBaseKey` whose `SymmetricLink` unwraps the
  **Ed25519 signing key** for the node (`getSigner`, lines 288-295). Only a
  holder of `wBaseKey` can recover the signing key and produce a valid mutable
  update.

A **read-only capability** simply omits the `wBaseKeyLink` (see
`RelativeCapability.wBaseKeyLink: Optional`). Granting read-only is literally
"hand over the capability without the write-link." No enforcement code needed —
the absence of the key *is* the absence of the permission. This is exactly what
the privacy-lock and network plans want for `sync_read` vs `sync_write`.

---

## Capability = the unit you grant (`RelativeCapability` / `AbsoluteCapability`)

Port `RelativeCapability.java` (already read in full):

```rust
pub struct Capability {
    pub writer:   Option<PublicKeyHash>,   // present only at entry points / writer changes
    pub map_key:  [u8; 32],                // address of the node within the writer's space
    pub r_base_key: SymmetricKey,          // READ capability
    pub w_base_link: Option<SymmetricLink>,// WRITE capability (absent ⇒ read-only)
    // (anti-enumeration block-access-token (BAT) extension; see "deliberately deferred")
}
```

- **Absolute** = `{owner, writer, map_key, r_base_key, w_base_key?}` — a fully
  resolved, dialable capability (our `network:{id}:...` namespace root).
- **Relative** = the same minus `owner`/implicit-`writer` — how a parent stores
  links to children compactly. `relative.to_absolute(parent_absolute)` resolves
  it (lines 56-62) by unwrapping the child's keys under the parent's.

This `Capability` struct is the concrete shape of Decision 0011's "capability
envelope" and the network plan's `CapabilityGrant`. Delivering one to a peer =
the network plan's "capability/key-envelope delivery" request-response.

---

## Granting access (the share flow)

To grant someone read access to subtree rooted at node N: hand them
`Capability{ r_base_key: N.rBaseKey, w_base_link: None, map_key, writer }`. They
walk down via the child `SymmetricLink`s you already stored; they cannot walk up
(no parentKey link upward) and cannot write (no w_base_link).

To grant read+write: include `w_base_link` so they can unwrap the signing key.

For password/out-of-band delivery, the Cryptree design wraps the whole capability in
an `EncryptedCapability` (scrypt-derived key over the cap) — the "secret link"
feature. That maps directly onto the privacy-lock plan's
`Encrypt-and-share-private` grant and gives us a ready design for shareable
secret links later.

---

## Revocation = key rotation (`rotateAllKeys`, lines 706-850)

The network plan's Phase G ("revocation cannot magically delete; it stops future
access via new epoch keys") **is** `rotateAllKeys`:

1. Generate fresh `baseRead`, `baseWrite`, `mapKey`, and optionally a new signer
   (lines 645-647).
2. Re-encrypt the node under the new keys; re-wrap every child link under the new
   base (recurse down — lines 767-820).
3. Re-issue capabilities to the *remaining* members; the revoked member's old
   keys no longer unwrap the new graph.
4. New CIDs result (ciphertext changed) — honest limit, already in both plans:
   revocation can't recall blocks already fetched.

Note `rotateSigner` (line 655): you can rotate read keys without rotating the
write/signing identity, or rotate both. That maps to "capability epoch" vs
"membership epoch" in the network plan.

`initAndAuthoriseSigner` / `deAuthoriseSigner` (lines 666-704) show the
add/remove-writer dance against the owner's writer-data — our analog is the
`MembershipCertificate` + `RevocationRecord` issuance, signed by the parent
authority.

---

## How it lands on `mem` / IPLD

The Cryptree assumes a content-addressed block store with a per-writer **signed
mutable pointer** (CAS). We already have:

- **content-addressed blocks** via `mem` (CID = hash) — for Cryptree, the CID
  hashes the **ciphertext**, so the CID identifies an opaque encrypted blob
  (satisfies "private CIDs reveal nothing", "never announce plaintext").
- **Ed25519 signing** — reuse for the write-spine signing key and the signed
  head.
- **the signed-head CAS** — port the Cryptree signed-head pointer update
  (`original→updated` + per-writer `sequence`) wrapped in `SignedPointerUpdate`;
  this is the network plan's `HeadRecord` and the privacy-lock plan's
  cross-process publication lock. (Generalize the single `updated` head to a head
  **set** for our fork-tolerant merge — the one place we deliberately diverge from
  the Cryptree design's single-writer CAS.)

So a private subgraph is just: encrypted CBOR blocks in the normal store +
capabilities held outside the DAG (in the `.concierge/security/capability-vault/`
the plans already define) + a signed head pointer per writer.

---

## Deliberately deferred for v1 (honesty about scope)

Port the key-graph; **skip these advanced features until needed**:

- **BAT / block-access-tokens** — an anti-enumeration block-access-token (BAT)
  extension, a storage-auth layer. Useful later for "don't leak which CIDs exist"; not needed
  to prove capability encryption. Leave the `Option<Bat>` field as a reserved
  `None`.
- **Fragmentation + padding** (`FragmentedPaddedCipherText`, 4096-byte fragments,
  16/64-byte metadata padding) — size-hiding for large files. Our memory nodes
  are small; add padding later as a metadata-leak hardening pass.
- **Stream secret / next-chunk chaining** (`calculateNextMapKey`) — for large
  multi-chunk files. Our records are single-block; defer.
- **Link nodes for rename-safety** — only matters once we have a shared mutable
  directory namespace with sibling-name collisions. Defer with the merge work.

Keeping these out makes v1 a few hundred lines instead of 1400.

---

## Proposed Rust module + build order

New crate `crates/crypto` (no `mem` dependency — pure, unit-testable, like our
`messaging` core), then wire into `core`:

```
crates/crypto/src/
  secretbox.rs   // SymmetricKey + CipherText           (port: TweetNaClKey, CipherText)
  link.rs        // SymmetricLink                        (port: SymmetricLink)
  capability.rs  // Capability (rel/abs), read/write split (port: RelativeCapability, AbsoluteCapability)
  node.rs        // EncryptedNode: {from_base, children_or_data, from_parent} (port: CryptreeNode core)
  rotate.rs      // key rotation / revocation            (port: rotateAllKeys)
  vault.rs       // scrypt-locked capability vault        (port: EncryptedCapability + plans' vault)
```

**Build order (each step independently testable):**

1. `secretbox.rs` + `link.rs` — *test vector*: `wrap(A, B)` then `unwrap(A)` == B; wrong key fails; round-trip a known plaintext.
2. `capability.rs` — read-only cap cannot produce write-link; `to_absolute` resolves child under parent.
3. `node.rs` — build a 2-level dir→file tree; holder of root base key reaches file data; holder of file cap cannot reach a sibling.
4. `rotate.rs` — after rotation, old capability fails to unwrap new graph; remaining holder's re-issued cap succeeds.
5. `vault.rs` — scrypt password unlocks the cap vault; wrong password fails with backoff; no key material on disk in plaintext.
6. Wire `core`: encrypted-private node kind; CID = hash(ciphertext); `mem` stores opaque blocks; capabilities live in `.concierge/security/`.

**Cross-check tests against the plans** (both already enumerate them): encrypted
nodes produce ciphertext CIDs; wrong/absent capability cannot decrypt; capability
sharing decrypts only the authorized subtree; plaintext CIDs never enter a
private manifest.

---

## Bottom line

The expensive, dangerous part — designing the crypto — is already done and
audited. We are porting ~5 small, well-separated Java classes (`SymmetricLink`,
`CipherText`, `TweetNaClKey`, `RelativeCapability`, the core of `CryptreeNode`)
onto proven Rust crates (`crypto_secretbox`, `ed25519-dalek`, `sha2`, `scrypt`)
and the `mem`/CAR seams we've already shipped. No new cryptographic invention is
required for Decision 0011.
