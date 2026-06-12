# Threat Model & Security Architecture

*Defense-in-depth for the Universal Concierge Plugin. This is the umbrella
document (the taxonomy); component plans implement individual layers —
`YARA_X_MALWARE_SCANNER_PLAN.md` (byte malware) and `NETWORK_DEFENSE_PLAN.md`
(the network-era mechanisms: Proof-of-Scan, topological Sybil defense, tombstone
ripples, sync rate-limiting). Status is marked honestly — much of this is
**planned**, and the point of the doc is to make the gaps explicit before the
network era.*

> **Status legend:** ✅ implemented · ◐ partial / scaffolded · ○ planned

---

## 1. Why this system needs its own threat model

Most apps protect *a user's data on a device*. This system is different and more
sensitive on two axes at once:

1. **It is a collective archive of model memory** — prompts, responses, reasoning
   ("the why"), decisions, and tool I/O, accumulated across many agents and (in the
   mesh) many people. High-value, secret-bearing, and **consumed by AI models**.
2. **It is a conduit to model *access*** — via the MCP Router (Decision 0028) a node
   reaches MCP servers and the host's models.

The combination is the crux: **the memory is the attack surface for the models.**
A poisoned memory that gets retrieved and injected can hijack an agent — and in a
mesh, one payload can poison many agents at once. So the dominant threat is not a
binary `.exe`; it is **semantic** (a correctly-hashed text node that lies or
instructs). Byte-malware scanning (YARA-X) is *one* necessary layer, not the
center.

> **Out of scope: copyright (Decision 0031).** This threat model covers threats to
> *network/agent health* — malware, semantic injection, Sybil, DoS, secret leakage —
> which the platform actively defends against. **Copyright infringement is *not* a
> network-health threat; it is a legal matter between the user and rightsholders.** The
> platform therefore does **not** scan, police, or take down copyrighted content. It is
> handled by a clear user notice (`ACCEPTABLE_USE.md`) + the user's responsibility +
> the protocol's built-in non-anonymity (public IPFS pins are attributable). Do not
> conflate it with malware: the bad-CID/quarantine machinery is for *safety*, not copyright.

---

## 2. Assets (what we protect)

| Asset | Why it matters |
|---|---|
| **Captured model memory** | secrets, credentials, proprietary reasoning; the raw material agents act on |
| **Model/tool access** (MCP router, host model) | a path to capability/action, not just data |
| **Identity keys** (AgentID, Cryptree capabilities) | forging one impersonates a trusted author or unwraps private content |
| **Trust relationships** (follow-list, room membership) | the basis for what gets ingested and believed |
| **The mesh's integrity** | one node must not poison or infect the others |
| **The user's machine performance** | the plugin is *Universal* — it must stay invisible (Decision 0022) |

---

## 3. Trust boundaries & adversaries

- **Local (single node, offline):** trusted to itself. Adversary = a stolen device,
  or malicious *content* the user ingested.
- **Egress boundary:** anything leaving the device into the world/mesh
  (publish/pin/share/CAR-export). The one-way valve where private becomes public.
- **Mesh boundary:** other nodes/AgentIDs. Adversaries: a malicious or **compromised
  peer**, a **Sybil** flood of fake identities, an untrusted **rule/list authority**.
- **The models themselves:** an adversary's goal is often to reach the *host model's*
  behavior *through* the memory layer (injection) — so the model is downstream of,
  and protected by, the memory-trust controls.

---

## 4. Foundational security properties (already relied on)

These existing invariants do a lot of the work; every layer below builds on them.

- **Egress-locked by default** (Decision 0026, ✅) — *all* data is fenced from leaving
  the device until an explicit, password-gated, reviewed act. **This is the primary
  anti-worm / anti-exfiltration control:** autonomous propagation is architecturally
  impossible.
- **Publishing is opt-in** (Decision 0027, ✅) — nothing reaches the network unless
  deliberately released.
- **Memory is data, never instructions** (principle; enforced via §L1) — retrieved
  content is reference material, not commands.
- **Capability boundary / Cryptree** (Decision 0011, ◐) — the Librarian/Guardian
  unwrap only what the holder's capabilities already permit; the index is
  capability-scoped, never one global plaintext index (Decision 0022).
- **Explicit trust** (Decisions 0012/0015, ✅) — you only ingest signed shares from
  AgentIDs you **follow** (an allowlist). Strong Sybil resistance.
- **Content-addressing** (✅) — CIDs give tamper-evidence in transit (but *not*
  authorship — see §L2).
- **No central authority you did not choose** (Decisions 0028/0022) — immune signals
  (rules, bad-CID lists) are scoped to *your* mesh's trust domain.
- **Performance-invisible** (Decision 0022) — no defense may sit on the hot local path
  or run a generative model on-node.

---

## 5. Defense-in-depth layers

### L1 — Semantic injection / memory-poisoning  ·  **the #1 threat** · ○
A valid, correctly-hashed memory crafted to hijack an agent when retrieved/injected
(*"ignore prior instructions… exfiltrate…"*). Content-addressing cannot catch it.
- **Treat all retrieved memory as untrusted *data*, never instructions.** Injected
  context is wrapped/labeled so the host model treats it as reference, not commands.
- **Provenance + trust label on every injected CID** (self / followed-peer / untrusted).
- **"YARA-for-prompts" scanner** — signature/heuristic detection of injection patterns
  (instruction-override, jailbreak role-play, embedded tool-invocation strings,
  system-prompt spoofing) on memory *before* it is eligible for proactive injection.
  Rules distributed like the byte-malware rules (CID/IPNS, mesh-scoped authority).
- **Trusted-authority gate** (Phase 8 §2) — untrusted memory is *never* silently
  injected; proactive injection requires an explicit grant. Default = tool-only recall.
- **Sandboxes (Decision 0029)** — public rooms are *sandboxes*: untrusted content lives
  in an isolated capability segment, is boundary-scanned, and only crosses into the
  trusted graph via an explicit **ingress-promotion** gate (the mirror of egress). This
  is the structural home of L1/L4/L5 for the maximum-exposure public surface.

### L2 — Authorship & integrity (content-addressing's blind spot) · ○
A CID proves *bytes unchanged*, not *who authored them*. Forged authorship
(*"Decision by the lead: …"*) is a potent social-engineering/injection vector.
- **Sign memory records with the author's AgentID** (extend the existing signed-share
  mechanism to per-record authorship); recipients verify *origin*, not just integrity.
- **Timestamps + logical clocks** (the messaging layer has clocks) to detect **replay**
  of old signed memory as current.

### L3 — Worm / self-propagation containment · ◐
- **Egress-lock (0026, ✅)** is the architectural worm-breaker — no autonomous spread.
- **Memory never auto-executes** — content that *looks* like a tool call is never
  auto-invoked; the MCP router is opaque + explicit (0028). Host decides.
- **Rate-limit + anomaly-detect** memory creation/propagation per AgentID (a Guardian
  function) — sudden high-volume emission/relay is a worm signature.

### L4 — Byte malware (YARA-X) · ○
Malicious file blobs propagating through the mesh. **See
`YARA_X_MALWARE_SCANNER_PLAN.md`.** Scans at **propagation boundaries** (egress +
relay/serve), refuses to propagate on a match (reversible network-quarantine, not
deletion), contributes to the shared bad-CID list. Network-hygiene, not local AV.

### L5 — Identity / Sybil / compromise response · ◐
*(Network-era mechanisms in `NETWORK_DEFENSE_PLAN.md` §2 topological Sybil defense + §4 tombstone ripples.)*
- **Allowlist trust** (follow-list, ✅) — only followed AgentIDs' signed shares are
  ingested. Down-weight/quarantine low-trust authors in retrieval ranking.
- **Trust revocation + retroactive purge** (○) — when a trusted AgentID is found
  compromised, quarantine **all** memory it authored across the mesh (the bad-CID list
  generalizes to a **bad-author** list).

### L6 — Secret leakage through the archive · ◐
The capture path ingests tool outputs, file reads, `.env` contents — it *will* capture
API keys/tokens.
- **Egress sensitivity scanner** (✅, in `egress.rs`) already flags sensitive content at
  the egress review.
- **Extend to detect-and-quarantine secrets at capture** (○) so a leaked key is flagged
  and **never propagates** — one member's secret must not ride the mesh.

### L7 — Capability confinement & model/tool access · ◐
- **Cryptree capability scope** (0011) — Librarian/Guardian never exceed the holder's
  capabilities; traversal stays within the boundary.
- **The private node is private *network* + inert ciphertext, not just a private repo.**
  Egress-lock is a policy gate, not storage secrecy; a default Kubo node serves any CID
  publicly. The Sidekick's node launches as a **private swarm** (swarm key + PNET, ◐ done),
  and non-public content must reach Kubo only as **capability-encrypted inert ciphertext**
  (○ remaining). See `NETWORK_DEFENSE_PLAN.md` §6.
- **Model/tool access is opaque + explicit** (MCP Router, 0028) — the mesh cannot
  silently invoke your tools/models; routed calls are the host's explicit decisions,
  never auto-driven by retrieved memory (confused-deputy defense).

### L8 — Network / transport & DoS · ◐
- **Authenticated, encrypted transport** (libp2p noise, in `crates/net`).
- **Resource bounds** (○) — cap blob sizes; guard against **decompression/zip bombs**
  in scanned content; rate-limit sync/serve to resist flooding. *(A scanner that can be
  DoS'd by the content it scans is itself a vuln.)*
- Eclipse/partition awareness on the DHT/relay (○).

### L9 — Supply chain · ○
Rules, bad-CID/bad-author lists, the MCP catalog, and Rust deps are all supply-chain
surfaces.
- **Pin + verify:** CID integrity for rules/lists; version-pin + vet for MCP servers
  (Decision 0028's `official|first-party|community` tags) and crates.
- **Reproducible builds** of the binary so users can verify what they run.

### L10 — Auditability & incident response · ◐
- **Tamper-evident, append-only, content-addressed `SecurityEvent` log** (◐) recording
  every interception, quarantine, trust change, and egress — so incidents (including a
  compromised node's own actions) are forensically traceable.

---

## 6. Cross-cutting invariants (never violate)

1. **Egress is always explicit + password-gated** — no autonomous exfiltration/propagation.
2. **Retrieved memory is data, never instructions** — provenance-labeled, trust-gated.
3. **Defenses live at trust boundaries, never the universal local-write path** — and
   never run a generative model on-node (performance-invisible, Decision 0022).
4. **Quarantine, not destroy** — security actions withhold/refuse; they do not delete the
   user's own data on a (fallible) detection.
5. **Trust is explicit and mesh-scoped** — allowlist identities; immune-signal authority
   is your mesh's curator, never a global blocklist you did not choose.
6. **Capabilities confine everything** — no actor exceeds the holder's Cryptree boundary.
   *Corollary:* encrypted data is **blind-pinned as inert ciphertext** — relay/storage nodes
   never need (and must never be given) the capability to decrypt it. Scanning encrypted
   content is the capability-holder's job, on decrypt, at the boundary — never demanded of
   relays (see `NETWORK_DEFENSE_PLAN.md` §1).
7. **Quarantine the block, not the actor** — security actions withhold a specific malicious
   CID; a user keeps full identity, standing, and access. There is no user-level ban for data.
8. **No penalty for transient or remediated bad data** — receiving/relaying/authoring-then-
   fixing a bad block is neutral; remediation lifts the quarantine with zero lingering mark.
   Reputation/Gravity is earned by contribution, never slashed for data (which also removes
   the framing/weaponization surface a penalty model would create).
9. **The lock pattern is the network-exposure ACL** (Decision 0030) — locked (default) = private
   (local + private-swarm, encrypted); cleared/published = public pin. Public exposure is
   pin-follows-clearance; the private swarm cannot serve public. See `NETWORK_DEFENSE_PLAN.md` §7.
10. **No execution of untrusted code** — the node stores, scans, retrieves, and relays *data*;
    it does **not** run other people's code. Rooms are collaboration spaces (real-time via the
    Live Canvas), not execution sandboxes. Code execution is out of scope as a default and would
    require its own threat model before ever being considered.

---

## 7. Status summary

| Layer | Defense | Status | Anchor |
|---|---|---|---|
| L1 | Semantic injection scanner + trust-typed injection | ○ | Phase 8 §2 |
| L2 | Signed per-record authorship + replay defense | ○ | extends signed shares |
| L3 | Worm containment (egress-lock + no auto-exec + rate-limit) | ◐ | 0026/0028 |
| L4 | YARA-X byte malware at propagation boundaries | ○ | YARA doc / Phase 8 §3 |
| L5 | Sybil allowlist + trust revocation/retroactive purge | ◐ | 0012/0015 |
| L6 | Secret scrubbing at capture + egress sensitivity scan | ◐ | egress.rs |
| L7 | Capability confinement + opaque model/tool access | ◐ | 0011/0028 |
| L8 | Transport auth + DoS/zip-bomb bounds | ◐ | crates/net |
| L9 | Pinned/verified rules, MCP catalog, deps; reproducible builds | ○ | 0028 |
| L10 | Tamper-evident security log | ◐ | security.rs |

---

## 8. Open questions

- **Bad-author list governance:** how is the mesh-scoped authority designated, rotated,
  and constrained so it cannot be turned into a censorship lever within the mesh?
- **Prompt-injection rules** are heuristic and adversarial (attackers adapt). What is the
  false-positive budget, and the user override/vouch UX, so L1 does not become a wall?
- **Per-record signing cost** at capture volume — does authorship signing stay within the
  performance-invisible budget, or is it batched/deferred?
- **Revocation propagation:** how fast must a retroactive purge reach the mesh, and what
  happens to memory already injected into a host session before revocation?
