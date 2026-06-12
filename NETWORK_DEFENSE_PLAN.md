# Network Defense Plan: The Inverted Security Paradigm

*This document outlines the security architecture of the Universal Concierge Plugin. It is based on a philosophical inversion of traditional cybersecurity: instead of building a moat to protect the user from a toxic internet, we build an immune system to keep the decentralized network itself mathematically and structurally healthy. By protecting the network, the user is inherently protected.*

> **Companion to `THREAT_MODEL.md`.** That document is the taxonomy (assets,
> adversaries, defense layers L1–L10); this is the **network-era mechanisms**.
> §1 = L4 (byte malware) + L1 (semantic injection); §2 + §4 = L5 (Sybil + trust
> revocation); §5 = L8 (DoS). All sections obey the project invariants:
> egress-locked-by-default (0026), capability confinement (0011), mesh-scoped
> authority (0028/0029), and the two below.

## 0. The two invariants that govern every mechanism here

1. **Quarantine the block, not the actor.** The immune system attacks the
   *pathogen* (a specific malicious CID — refuse to relay/serve/surface it,
   reversibly), never the *host*. A user whose node received, relayed, or even
   authored a bad block keeps full identity, standing, and access; only that one
   CID is withheld. There is **no user-level ban** for data.
2. **No penalty for transient or remediated bad data.** Holding/relaying a block
   that turns out bad is neutral (you didn't author it and couldn't have known);
   fixing it fast — re-scan clean, publish a corrected version, or tombstone your
   own bad block — is neutral-to-positive and lifts the quarantine with **zero
   lingering mark**. Reputation/Gravity is *earned by positive contribution*, never
   *slashed* for bad data. (This is also security: a reputation-penalty model is
   weaponizable — attackers send you bad blocks to frame you — so we simply don't
   have that surface.)

## 1. Proof-of-Scan (No Blind Pinning of *plaintext*)
A node must never act as a blind host for malicious **plaintext** payloads — but it
*must* be able to relay **encrypted** payloads it cannot read (that is the whole
point of capability encryption). The rule therefore splits by readability:
- **Public (plaintext) data:** scanned before pinning/relaying via the embedded
  YARA-X engine (`YARA_X_MALWARE_SCANNER_PLAN.md`) **and** the semantic/prompt-
  injection scan (`THREAT_MODEL.md` L1) — a plaintext memory node can carry an
  injection payload that byte-scanning alone misses. A clean scan signs a
  `Certification Node` (Proof-of-Scan); a flagged block is refused for relay.
- **Encrypted (capability-sealed) data:** blind-pinned as **opaque, inert
  ciphertext.** A relay/storage node does **not** hold (and must never require) the
  capability to decrypt — demanding decrypt-to-pin would break Cryptree
  (Decision 0011) by forcing capabilities onto infrastructure. Ciphertext cannot
  infect a node that can't read it, so storing it blindly is safe. **Scanning is
  the capability-holder's job, at the boundary:** when a holder decrypts for their
  own use (egress / ingress-promotion), *they* run YARA-X + the injection scan on
  the plaintext and sign the Proof-of-Scan. The relay just carries ciphertext.
- **Chain of Custody:** every new *plaintext* block appended to a thread carries its
  own Proof-of-Scan; one malicious attachment is refused without invalidating the
  previous clean history.

## 2. Topological Sybil Defense
To prevent botnets from inflating the "Graph Gravity" of spam or malicious CIDs, the network uses structure as a signal — layered on explicit allowlist trust (`THREAT_MODEL.md` L5), not as a standalone verdict.
- **Visual Anomaly Detection:** Sybil attacks (10,000 fake identities linking to one node) tend to look like dense, isolated anomalies lacking organic links to established clusters. This is a strong *heuristic*, not proof: adversaries adapt (building organic-looking links), and a legitimate new tight-knit community looks similar — so it is a **down-weight + a flag for review**, never an automatic ban.
- **Gravity down-weighting (block-scoped):** when the topology around a CID looks inflated, the Guardian **down-weights that CID's Gravity** so the "Seer" doesn't surface it — it does **not** penalize the identities involved (invariant §0.1; a real user caught in a suspicious cluster keeps full standing). The blast radius is already small: inflated Gravity only matters in the *public/sandbox* ranking, and sandbox content is never auto-injected anyway (Decision 0029).

## 3. The Live Security Map (Everyone is a Watchdog)
Security is not hidden in a background process; it is a built-in, standard feature of the Data Platter GUI.
- **The Visual Immune System:** Users have access to a "Live Security Map" view. This visualizes the public linked map of the network.
- **Crowdsourced Defense:** Users can literally *see* botnet clusters forming, or watch zero-day threats light up as they are quarantined. By giving every user full visibility into the network's structural health, the entire community acts as a decentralized watchdog.
- **Public layer only:** the map visualizes the **public/sandbox** graph. Private, capability-encrypted topology is never rendered or leaked (capability confinement, Decision 0011) — the immune system is legible without exposing anyone's private memory.

## 4. Tombstone Ripples (Retrospective Healing) — trust-scoped, reversible
Threats evolve, and zero-day exploits may occasionally bypass the initial YARA-X ruleset.
- **The Mechanism:** when a security authority **you trust** (one in your mesh's trust domain — a Security Collective or Library Node *you follow*) detects a threat and publishes a `Tombstone` CID for it, that tombstone ripples across Gossipsub.
- **Trust-scoped, not global:** a node acts on a tombstone only from an authority **it has chosen to trust** — never a global blocklist anointed by unspecified "reputation." A globally-rippling kill-switch keyed on "reputable" is a centralized censorship/abuse vector (compromise or earn reputation → purge anything), which violates mesh-scoped authority (Decisions 0028/0029).
- **Self-Healing = reversible quarantine, never destruction:** Guardians honoring a trusted tombstone **quarantine the malicious CID locally** (refuse to relay/serve/surface it; local copy intact, local override available) — the quarantine-not-destroy invariant. Within a trust domain this still heals fast; it just can't be weaponized into network-wide deletion. And per §0, it quarantines *the block*, not the users who held it.

## 5. Sync Rate-Limiting (Resource Exhaustion Protection)
P2P networks are vulnerable to Denial of Service (DoS) by flooding.
- **ActorID Quotas:** The `crates/net` module enforces strict byte-rate limits per ActorID.
- **Auto-Pause:** If a public room (sandbox) suddenly experiences a massive, unnatural influx of data, the Guardian automatically pauses local synchronization to protect the host machine's disk and the Librarian's index pipeline from resource exhaustion. *(This is resource protection against a behavior, not a penalty on a user — invariant §0.)*

## 6. The private node: "private + locked" is NOT secure by itself
When the Sidekick is enabled, it launches the user's **private Kubo node**, and the
memory graph may live there for sync/transport. The intuition "the node is private
and the data is egress-locked, so it's secure" is **only half true** — and the missing
half is a real leak if ignored:

- **Egress-lock (Decision 0026) is a *policy gate*, not storage/transport secrecy.** It
  stops the *Concierge's* publish/share action. It does **not** encrypt blocks, and it
  does **not** stop Kubo from serving them.
- **A *default* Kubo node is public.** It joins the public IPFS DHT and serves *any*
  block by CID over bitswap to anyone who asks — which would **bypass the egress lock**
  for plaintext blocks sitting in the node. A *private repo* (`IPFS_PATH`) is not enough;
  privacy is a *network* property, not a storage-location one.

Two mechanisms make it actually secure (the strong claim needs both):

1. **Private swarm (DONE in `crates/core/src/node.rs::launch_private_node`).** The node
   is launched with a **swarm key** (PSK), public bootstrap peers removed, and
   `LIBP2P_FORCE_PNET=1` — so it refuses any peer that does not hold the key. Only the
   user's own paired devices / trusted mesh (Phase N) can connect or fetch. The node
   holds the graph, but on a **closed** network.
2. **Capability-encryption at rest (Cryptree, Decision 0011 — REMAINING).** Anything not
   *deliberately* public must reach Kubo only as **inert ciphertext** (the blind-pin rule,
   §1). Then even a fetched block is useless without the capability, and the egress-lock's
   *intent* ("this stays private") is enforced at the storage layer, not just the UI.
   The egress→Kubo path must encrypt non-public content before pinning. *(Tracked; the
   encryption-before-pin wiring is the open piece.)*

**Default today (safe):** the local `mem` blockstore holds the graph; Kubo only ever
receives content via the explicit, reviewed `publish-public` flow — i.e. content the user
*chose* to make public. Nothing locked is auto-mirrored into Kubo. Mirroring the graph
onto the node is only safe once **both** mechanisms above hold.

## 7. The lock pattern is the exposure ACL (Decision 0030)
There is one source of truth for *who can reach what*: the egress **cleared registry**
(Decision 0026). A subgraph's lock state selects its network-exposure **tier**, and both
the egress guard and the network/pin layer read the same registry:

| Lock state | Tier | Reach |
|---|---|---|
| **Locked** (default) | Private | local store + the user's own paired devices via the **private swarm** (PNET), as capability-encrypted inert ciphertext. Never the world. |
| **Cleared / published** | Public | pinned to public IPFS; the only tier with public-facing abilities (web publishing, distribution, global live-watch). |

**Pin-follows-clearance:** a CID is publicly exposed **iff** it is in the cleared set.
`clear-for-egress` makes a subgraph eligible for public pin; `re-fence` stops the node
serving it publicly (best-effort unpin — others may already hold copies, but the node stops
announcing). All data starts locked → all data starts private; the user **progressively
opens** hand-picked subgraphs. The private-swarm node (PNET) cannot serve public IPFS, so
**public pinning is a separate, explicit act on a public backend** — never a hole in the
private swarm.
