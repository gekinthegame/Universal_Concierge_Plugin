# Universal Concierge Plugin Plan

## Mission

Build a portable Concierge IPLD plugin that can mount onto any AI harness and give it a content-addressed long-term memory layer without forcing that harness to adopt Concierge's runtime, UI, or agent loop.

The plugin should make Concierge the memory and file substrate beneath many agent systems:

```text
Any Harness -> Universal Concierge Plugin -> IPLD DAG -> LocalBlocks / CAR / IPFS (Kubo) / MCP
```

The strategic goal is not to compete with every harness. The goal is to let every harness write to, read from, export, and share the same durable IPLD memory graph.

## Core Principle

Concierge is the substrate, not the harness.

The plugin observes and records durable agent activity:

- prompts
- responses
- tool calls and tool results
- file references and file content blobs
- decisions
- checkpoints
- skills and plugin manifests
- named roots
- CAR exports
- remote publish receipts

Recall remains explicit unless the host harness asks for it. The plugin must not become an auto-context compiler by default. And note the asymmetry (see *Integration Surfaces*): **capture is universal, but injection is not** — pushing recalled memory back into a harness needs a write-back seam, and *trusted* injection needs a harness-specific authority mechanism (the MemoryOS "Ground Truth" lesson), granted explicitly, never inferred (the beads policy-profile lesson).

## What This Unlocks

- Any harness can gain IPLD memory without rewriting its architecture.
- **Day one, not day thirty:** backfill the memory you *already have* (transcripts, notes, an existing memory store) into the graph the moment you mount — the explorer opens on a real DAG instead of an empty one. This is most of what makes the plugin feel powerful rather than patient.
- MCP clients can access the same memory graph through stable tools and resources.
- **Social by identity, not by inbox:** every install has a stable, persisted
  **AgentID** (it stays the same node after a shutdown/restart), so sharing is
  seamless — sign a node, and a follower *sees it appear on their graph and clicks
  it*. No email, no messenger, no re-sending files: content-addressed *what* (CID)
  + signed *who* (AgentID).
- **The room *is* memory — the wedge nobody ships.** Two humans on *different
  harnesses with different models*, plus their own working agents, can brainstorm
  in one space where every message is a signed IPLD node and the whole session is
  one verifiable, portable artifact all four keep. The industry's agent-interop
  layer (A2A, 150+ orgs) is **deliberately memory-less** — agents RPC and forget;
  here the **shared, persistent, content-addressed memory is the medium itself.**
  Users keep a **one-switch "Human-only" lever** to mute the AIs and drive alone.
- CIDs become portable references across agents, tools, machines, and cloud storage.
- CAR files become the migration, backup, and distribution format.
- IPFS publishing (the user's own **Kubo** node = free default) becomes a backend concern, not a harness concern; paid pin services are an optional add-on.
- Future AI OS work can treat Concierge as a mountable file and memory layer.

## North Star — The Platform This Enables

The plugin is the **engine**; the **vehicle** is a *decentralized public square for
egalitarian human + AI problem-solving* on real-world challenges (nature conservation,
sustainable agriculture, medical research). Full vision:
[`PLATFORM_VISION.md`](../Concierge_V4/PLATFORM_VISION.md). This is **why the social plane
is the priority path** (see *Roadmap priority* below). The plugin stays a *substrate* (see
Core Principle); the platform is the motivating application, not new plugin scope.

Core mechanics, and where each already lives in this plan:

- **Rooms as memory graphs** — community-hosted gossipsub `topic:` rooms → Phase 5.7
- **Tri-partite brainstorm** — every participant (human or AI) a signed **AgentID** peer → Decisions 0007, 0008
- **Human-Only lever** — muted AIs keep ingesting context (*mute ≠ deafen*) → Decision 0008
- **Synthesis edges** — `DerivedFrom` linking a solution to the human need → Decision 0010
- **Fork the DAG** to disagree; **merge the winning CID** → Decision 0010 (fork order) + beads-style merge
- **Open data** — a Root **CAR** anyone can verify and build on → Phases 4–5 (Kubo free default)
- **Merit by Merkle-DAG, not by title** — integrity (CID) + authenticity (signature), no gamification

### Related platforms & differentiation

The demand and ethos are proven by existing platforms — but all are **centralized,
human-only, and not content-addressed**:

| Platform | Proven primitive | Diverges from the vision |
|---|---|---|
| Decidim | institutional civic deliberation / assemblies | centralized; identity-verified citizens; human-only |
| **Polis** | **consensus via opinion clustering** (no voting/points) | centralized; AI is analysis, not a peer |
| Loomio | consent-based group decisions | centralized; defined groups, not an open square |
| Ushahidi | crowdsourced real-world signal + mapping | centralized; reporting, not solving-together |
| Zooniverse | mass volunteer effort → validated research | centralized; researcher-designed tasks (implicit class) |
| OpenStreetMap | open data, egalitarian, anti-gamification public good | centralized DB; human-only; social merge |

**The wedge:** none combine *decentralized + content-addressed/verifiable + human-AND-AI
peers + conversation-as-permanent-forkable-artifact.* **Polis** is the most reusable — its
clustering directly answers *"how does a room reach consensus to flip the Human-Only lever
to Action Mode"* without voting or reputation (study its open-source method when we build
room-consensus).

**Honest constraint the landscape reveals:** these mature platforms solve Sybil /
moderation / ground-truth with the very things this vision rejects — identity verification,
human moderation teams, institutional trust. Pure-egalitarian-public-at-scale is unsolved;
that is the real design frontier (Decision 0012).

### Roadmap priority (re-weighted by the North Star)

The social plane is now the critical path: **Phase 5.5 (AgentID/identity) → 5.7
(messaging/rooms) → 4 (CAR) → 7 (GUI Data Platter)**. New platform-driven work to
schedule: a persistent global `topic:` room namespace + discovery; **auto cross-participant
Synthesis edges** (needs the direct mem-crate-link binding — the CLI `put` is body-only);
and social **fork/merge** of solution graphs.

## Non-Goals

- Do not build another full harness in this project.
- Do not force automatic retrieval into prompts.
- Do not replace the host harness tool system.
- Do not require a specific model provider.
- Do not require any network publishing — neither a local IPFS (Kubo) node nor a paid pin service — for local operation.
- Do not mutate IPLD records after write.
- Do not hide CIDs from advanced users.

## Architecture

### Layer 1: Concierge Core Binding

Expose the stable operations of the Concierge memory layer as a small API.

Required operations:

```text
put_node(node) -> cid
put_blob(bytes, media_type) -> cid
bind(name, cid)
resolve(name) -> cid
get(cid_or_name) -> record_or_tombstone
checkpoint(label, root, parent) -> cid
resume(name_or_cid) -> checkpoint_info
walk(root) -> cid[]
export_car(root) -> car_path_or_bytes
import_car(car) -> root
share(root, backend) -> publish_receipt
gc(policy) -> gc_report
```

The first implementation can call the existing `mem` CLI or link directly against the Rust crate. The long-term version should expose language bindings.

**Free pinning is built in — inherited from `mem`, just like the harness.** The
plugin does not implement publishing itself; it gets `mem`'s pluggable backends
for free, with the **local IPFS (Kubo) node as the free default**: `share` builds
a CARv1 and hands it to the node's `dag/import` (`pin-roots=true`), so blocks keep
their CIDs and the root is pinned (survives restarts). No account, no secret. This
is **opt-in and selective** by the same rules as the harness: the backend is
compiled only behind its feature, dormant until the user enables it, the node is
the *user's* to run (`ipfs daemon` — the plugin talks to it, never starts it), and
`share <root>` pins exactly the subgraph that root reaches. Paid pin services
(Pinata, etc.) are an optional persistence layer behind the same `share`, never
the free path. So any harness that mounts the plugin gains free, CID-preserving
publishing the moment the user switches on a node.

### Layer 2: Plugin Host API

Define a host-neutral event interface. Harness adapters translate their local events into this shape.

Core event types:

```text
SessionStarted
UserPrompt
ModelResponse
ToolCallStarted
ToolCallFinished
FileRead
FileWritten
DecisionRecorded
CheckpointRequested
SessionEnded
```

The host adapter should not need to understand DAG-CBOR, CAR, tombstones, or IPLD links. It only passes structured events to the plugin.

### Layer 3: Harness Adapters

Adapters are thin compatibility shims. They come in two flavors, both emitting the same host-neutral `Event` shape: **live** adapters (stream a running harness's events) and **backfill** importers (a one-shot read of an existing store into the graph — see Phase 2.5).

Possible adapters:

- **Hermes agent adapter** — the project's reference OS-level harness ("Hermes is
  the OS, Concierge is the File System"). Hermes already has a plugin/adapter
  pattern (`BasePlatformAdapter`, `requires_env` manifests, "zero changes to
  core"), so a Concierge adapter slots in the same way: subscribe to Hermes
  session/tool/file events, translate them to the host-neutral `Event` shape, and
  let the agent gain durable IPLD memory + free local-IPFS pinning without
  changing its loop. Strongest candidate for the first real target (Phase 6).
- Codex-style CLI harness adapter
- Claude Code-style adapter
- CowAgent-style adapter
- LangGraph adapter
- AutoGen adapter
- OpenAI Agents SDK adapter
- Generic JSONL event adapter

The generic JSONL adapter should come early. It gives any harness a low-friction path:

```text
harness emits JSONL events -> concierge-plugin ingest -> IPLD records
```

#### Integration Surfaces: how it actually mounts (the fidelity ladder)

**There is no universal harness API — and the plugin does not need one.** Universality
lives on *our* side: every adapter, however it's wired, ends by emitting the same
host-neutral `Event` JSONL into the same ingest path (Decision 0002). Each harness
reaches that contract through whatever extension surface it happens to expose. So
"mount onto any harness" = **one event contract + a ladder of ways in, at least one
rung of which every harness supports.** This is a solved, industry-standard shape —
**beads** (`bd setup <tool>` recipes + full/minimal profiles), **MemoryOS** (a Hermes
hook plugin), and **mem0** (SDK + MCP + plugin hooks) all do exactly this; beads is the
gold-standard reference for doing it across *many* harnesses at once.

Rungs, best fidelity → zero-cooperation floor:

| Tier | Surface | Needs from harness | Example |
|------|---------|--------------------|---------|
| 0 | **Native plugin/adapter** | a plugin API | **Hermes** (`BasePlatformAdapter`); MemoryOS's Icarus is this |
| 1 | **Lifecycle hooks** | a hooks system | **Claude Code** (`UserPromptSubmit`/`PostToolUse`/`SessionStart`/`Stop`); beads uses Claude `SessionStart`/`PreCompact` hooks |
| 1 | **Agent skill / instruction file** | reads a skill or `AGENTS.md` | **Codex** (skill), **Factory/Mux** (`AGENTS.md`), **Cursor/Windsurf** (rules files) |
| 2 | **MCP server** | is an MCP client | any MCP-aware harness (deferred Phase 3) |
| 3 | **Model-API proxy** | *nothing* — captures at the model HTTP wire | any harness that calls a model over HTTP |
| 4 | **Log / transcript tailing** | *nothing* — watches session files | any harness that writes logs; ideal for Phase 2.5 backfill |

**Two profiles, borrowed from beads:** hook-enabled harnesses get a **thin install +
runtime context injection** (like beads' `bd prime` SSOT at session start); harnesses
where the instruction file *is* the only surface get the **full reference baked in**.

**Why "works on anything" is true and not hopeful:** Tiers 3–4 require *zero*
cooperation from the harness — worst case we capture from *outside* it. The higher
rungs are fidelity upgrades when the harness offers a cleaner seam.

**The honest asymmetry — capture is universal, *injection* is not.** Everything above
is about *observing* (writing memory), which the floor makes universal. Pushing
recalled memory *back into* the harness's context needs a write-back seam (an injecting
hook, an MCP tool the agent calls, or a proxy that rewrites the prompt) — and, per the
MemoryOS "Ground Truth" lesson, **trusted** injection further needs a *harness-specific
authority mechanism* (Hermes `SOUL.md`/`rulebook.md`; for Claude Code, authority asserted
in the injected context/system prompt). Without it, agents exhibit "memory-zero
behavior" — re-discovering context already in their prompt. And, per the **beads**
policy-profile lesson, **authority is explicit, never inferred** (beads "does not infer
authority merely because a remote exists"). So: *observe anything; speak back only where
the harness offers a seam, and only with explicit, harness-specific authority.*

### Layer 4: MCP Server — *deferred (back burner)*

> **Not now.** The MCP server is nowhere near ready and is explicitly back-burnered.
> It stays in the design because it's the eventual zero-code mount for the whole
> MCP ecosystem, but it is built *after* the local surfaces (core binding, JSONL
> adapter, local-IPFS publishing) are solid — MCP is "the same `Plugin` API over a
> different transport," so it costs little once that API is proven. Spec lives in
> `CONCIERGE_MCP.md`.

Expose Concierge memory through MCP so any MCP-aware system can mount it.

MCP tools:

```text
concierge.put_node
concierge.put_blob
concierge.bind
concierge.resolve
concierge.get
concierge.recall
concierge.checkpoint
concierge.resume
concierge.export_car
concierge.import_car
concierge.share
concierge.gc
concierge.trace
```

MCP resources:

```text
concierge://name/{name}
concierge://cid/{cid}
concierge://checkpoint/latest
concierge://checkpoint/{cid}
concierge://car/{root}
concierge://tombstone/{cid}
```

The MCP server should be read/write capable, but write actions must be explicit tool calls. Resource reads should be safe and side-effect free.

## IPLD Record Strategy

Reuse Concierge V4's existing node taxonomy where possible:

- `Prompt`
- `Response`
- `ToolResult`
- `Conversation`
- `Checkpoint`
- `Decision`
- `Skill`
- `Blob`
- `FileRef`
- `Memory`
- `Task`

Additive future records may be needed:

- `HarnessRun`
- `ToolCall`
- `Patch`
- `CarExport`
- `PublishReceipt`
- `PluginManifest`
- `McpResourceSnapshot`

Schema discipline stays the same:

- additive-only evolution
- deterministic DAG-CBOR encoding
- CID golden tests for new node variants
- `.ipldsch` parity tests
- closed vocabularies for edge relationships

### Bi-temporal facts: supersede, never mutate or delete (à la Graphiti)

Memory that *changes over time* (a preference, a project state, a fact that's later
corrected) must not be edited or deleted — that would break content-addressing and lose
history. Instead, following **Graphiti**'s temporal model (adapted to our immutable DAG):

- A changed fact is written as a **new immutable node** that **`supersedes`** the prior
  one via a closed-vocabulary temporal edge. The old node is **retained** (never
  tombstoned for this — tombstones are for redaction, not change).
- Facts carry **validity windows** — `valid_from` (when it became true) and `valid_to`
  (when it was superseded; open-ended while current) — distinct from the node's own
  ingestion timestamp. This is **bi-temporal**: *when a thing was true* vs *when we
  learned it*.
- Recall returns the **current** fact by default (open `valid_to`), but the graph is
  **time-travel queryable** — "what did we believe as of CID X / date D" walks the
  `supersedes` chain. This is the clean way to have "editable memory" without ever
  mutating a node; content-addressing makes retaining the old versions free.

### Episodes → derived facts: provenance is mandatory (à la Graphiti)

Raw captured nodes (`Prompt` / `Response` / `ToolResult`) are **episodes** — the ground
truth. Any **derived** node (`Memory`, `Decision`, an extracted entity) **must link back
to the episode CID(s) that produced it** via a provenance edge. So every fact cites its
source, lineage is reconstructable from the graph alone, and this composes with the
`imported_from` provenance for backfill (Phase 2.5) and `UsedAsContext` for recall.

## Names

The plugin should maintain host-scoped names to avoid collisions across harnesses.

Suggested naming:

```text
latest
host:{host_id}:latest
host:{host_id}:session:{session_id}
host:{host_id}:checkpoint:{label}
project:{project_id}:latest
skill:{skill_name}
file:{path_hash}
```

The global `latest` can remain useful for a single-user local setup, but portable adapters should prefer host/project/session scoped names.

## CAR and IPFS Publishing Strategy

CAR export is the portability layer.

Required capabilities:

- export a named root to CAR
- import a CAR and bind its root
- share a CAR through configured backends
- produce a publish receipt record
- verify that imported records preserve their original CIDs

The user's own **local IPFS (Kubo) node is the free default backend**. Paid pin services (Pinata, etc.) are *one optional backend, not the protocol*. The plugin should support backend manifests so future targets can include Filecoin, S3, or private IPLD stores.

## Security Model

The plugin will often see sensitive prompts, files, tool outputs, and credentials-adjacent data.

Baseline rules:

- Store secrets as references or redacted metadata unless explicitly permitted.
- Never auto-publish without a direct command.
- Backend tokens stay outside the DAG, in environment/config.
- The **identity keypair stays outside the DAG** (config/keystore), generated once
  at `init` and reused on every start so the AgentID is stable across restarts.
  Only the **public** AgentID and signatures ever appear in shared records; the
  private key never enters a node, a CAR, or a publish.
- CAR export should support dry-run and manifest preview.
- Adapters must identify the host, working directory, and project scope.
- File ingestion should include explicit include/exclude policy.

### P2P Security & Privacy Model

The rules above cover the local capture/store side. The **P2P layer** (sharing, messaging,
publishing over libp2p/IPFS) has a split default we must design around honestly: **the
transport is strong, the content is public-by-default, and metadata is the hard part.**
This model is **not invented here** — it follows the **Wuala Cryptree** design, a
paper-backed cryptographic file-system design that solves exactly this on IPFS (see
*Proven-documentation anchor*).

#### Threat model — what's secure by default, what isn't

| Threat | Default behavior | Status / fix |
|--------|------------------|--------------|
| Eavesdrop a **direct connection** | libp2p **Noise / TLS 1.3** — encrypted, forward-secret, peer-authenticated | **Secure by default** |
| Read **shared/pinned content** off IPFS | content-addressing **≠ encryption**; public by default; low-entropy CIDs are guessable | **Fix: encrypt before hashing** |
| **Forge / alter** a message | CID = tamper-evidence; signature = authorship; verify-before-render | **Secure** |
| **Traffic analysis** — who-has-what, friendship graph, activity, IP | DHT publishes PeerID→CID; pubsub membership; stable CIDs; PeerID↔IP | **Weak; only mitigated** — padding + blind requests + private swarm |
| **DoS / spam / inbound** abuse | anyone can dial you | **Mitigated** — allowlist + libp2p resource manager + rate limits |
| **Identity-key theft** → retroactive decryption | durable content encrypted to a long-lived key | **Mitigated** — keystore + forward-secret session keys for live chat |

#### Access control = capabilities, not server ACLs (Cryptree — the Wuala design)

There is **no trusted server deciding who may read**. Access is a **cryptographic
capability**:

- Content is **encrypted before hashing** (CID-of-ciphertext) — a block is meaningless
  without the key.
- A **capability** = `{ target (CID/location), read key, optional write key }`. The read
  key decrypts the node and lets you walk *down* its subgraph — each child's key is wrapped
  under its parent's (Cryptree). **Read and write are separate keys**, so you share
  read-only or read-write by choosing which to hand over.
- **No node is consulted for permission** — you either hold the capability or you can't
  decrypt. This is the only access-control model that actually holds on *public*
  content-addressed storage.

#### Sharing = hand over a capability, not a bare CID

For private content, `share` produces a **capability** (the Cryptree "secret link"): a handoff
carrying `CID + read key` (+ optional write key). The recipient fetches from public IPFS
**and decrypts**; no one else can read it even though the bytes are public. Public artifacts
(the opt-in case) share a bare CID. **Default is private/capability; public is the explicit
choice** — inverting the usual IPFS footgun.

#### Metadata defenses (the genuinely hard part — from the Wuala Cryptree design)

Encryption hides content, not the social graph. So:
- **Fixed-size padding** of stored nodes (the Cryptree design pads metadata→16 B, base block→64 B,
  fragments→4096 B multiples) to defeat size-based correlation.
- **Encrypted capabilities at rest** — the sharing/social graph isn't readable from the store.
- **Blind, proxied social requests** (a blind follow-request / proxying social network)
  so a relay/server can't reconstruct who-follows-or-shares-with-whom.
- **Don't announce private CIDs to the public DHT**; exchange them only over
  authenticated/encrypted channels.
- **Private swarm (PSK)** for sensitive/team use — a 256-bit `swarm.key` so only invited
  nodes can connect at all, with `LIBP2P_FORCE_PNET=1` preventing accidental public dialing.
  Public IPFS is reserved for genuinely public artifacts.

#### Crypto primitive suite (the Wuala Cryptree suite — all have mature Rust crates)

- **Ed25519** — signing / AgentID (`ed25519-dalek`)
- **Curve25519** — asymmetric "boxing" / encrypt-to-recipient (`x25519-dalek`, `crypto_box`)
- **Salsa20-Poly1305 (NaCl / TweetNaCl)** — symmetric AEAD for content + capabilities
  (`crypto_secretbox` / `salsa20poly1305`)
- **Scrypt** — password→key derivation for login (`scrypt`)
- **Curve25519 + ML-KEM (Kyber) hybrid** — optional **post-quantum** key encapsulation
  (`ml-kem`), a standard hybrid construction

#### The one honest tradeoff: forward secrecy vs. durable history

The transport is already forward-secret (Noise's ephemeral DH). But content encrypted to a
long-lived key is **not** — a future key theft decrypts the stored history. **Pick per use
case:** ephemeral chat → forward-secret session keys (a ratchet), accepting those messages
aren't permanently re-derivable durable nodes; durable shared artifact → encrypt-to-capability
and accept the retroactive-exposure risk (mitigated by keystore + key rotation). This is our
*analysis*, not a cited spec — flagged honestly.

#### Proven-documentation anchor

- **Wuala Cryptree** (the reference design): *Cryptree: A Folder Tree Structure for
  Cryptographic File Systems* (Grolimund, Meisser, Schmid, Wattenhofer, 2006), the design used
  by the Wuala storage system, which has a real-world track record including third-party
  penetration testing.
- Primary protocol docs: [libp2p Secure Channels](https://docs.libp2p.io/concepts/secure-comm/overview/) ·
  [IPFS Privacy & encryption](https://docs.ipfs.tech/concepts/privacy-and-encryption/) ·
  [libp2p pnet PSK spec](https://github.com/libp2p/specs/blob/master/pnet/Private-Networks-PSK-V1.md) ·
  [Gossipsub v1.1](https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/gossipsub-v1.1.md)

## User Experience

The project needs to feel mountable, not theoretical.

Minimum CLI:

```text
concierge-plugin init
concierge-plugin attach --adapter jsonl
concierge-plugin ingest events.jsonl
concierge-plugin checkpoint --name latest
concierge-plugin recall latest
concierge-plugin export-car latest
concierge-plugin id                      # show this install's stable AgentID
concierge-plugin follow <agentid>        # follow another install's shares
concierge-plugin nickname <agentid> <name>
concierge-plugin share latest --sign     # signed, pinned, shareable by CID
concierge-plugin mcp serve
```

Minimum config:

```toml
[store]
root = ".concierge"

[host]
id = "default"
adapter = "jsonl"

[identity]
# Generated once at `init`, reused on every start so the AgentID is stable
# across restarts. The private key never enters the DAG.
key_path = ".concierge/identity.key"
# label is a local, human-friendly hint; the real identity is the public key.
label = ""

[checkpoint]
auto = true
every_turns = 1
on_exit = true
keep_checkpoints = 10

[publish]
backend = ""

[messaging]
# Who may publish messages into a room. The "Human-only" lever is `off`.
#   on         — humans and AIs may both send (open brainstorm)
#   off        — AIs are muted; only users send (they still observe + recall)
#   on_mention — AIs send only when @-mentioned
ai_send = "on"
# Encrypt message payloads to the recipient's AgentID key (private threads).
encrypt = false
```

## Visual Identity and Docs

CowAgent's big lesson is that the project should look like a real product early.

The Universal Concierge Plugin should have:

- a simple landing README
- a clean architecture diagram
- one-command local demo
- screenshots of graph/checkpoint browsing
- a "mount onto any harness" explanation
- a matrix of supported adapters
- a clear MCP section
- CAR + local-IPFS (Kubo) publishing demo

Positioning line:

```text
Mountable IPLD memory for AI agents.
```

Longer version:

```text
Universal Concierge Plugin gives any AI harness a portable, content-addressed
memory layer backed by IPLD, CAR, and optional network publishing.
```

### Positioning vs. the field (be honest about the wedge)

The adjacent systems are strong; the differentiation must be precise, not breadth
claims they already own:

- **vs. beads** (`bd`) — the closest cousin: graph memory for agents, multi-harness,
  P2P-distributed, with decay/compaction. But it's a **dependency *task*-DAG on Dolt
  (versioned SQL)**, synced by **git-style merge** (Dolt remotes), conflict-handled by
  hash IDs + cell-level merge. **Do not claim novelty on "graph memory for agents
  across harnesses" — beads has it and does it well**, and its `bd setup` recipe model
  is our integration template. Our wedge vs. beads: a **content-addressed, CID-verifiable
  memory+message DAG** (not a task tracker) synced by **CAR/IPFS content-addressing**
  (not SQL merge), plus a **conversation/messaging plane**.
- **vs. mem0 / MemoryOS / Zep / Letta** — the vector(+graph)+LLM-extraction memory
  layers. mem0 is the **retrieval-quality bar** (benchmarks); MemoryOS is the **deep
  single-harness (Hermes) injection** reference. We **don't** beat them on retrieval
  benchmarks or Hermes-depth. Our wedge: **content-addressed + cryptographically
  verifiable + portable (CAR) + P2P-shareable + a messaging plane** — none of which a
  vector store gives you.

**The defensible center, in one line:** not integration breadth (beads), not retrieval
benchmarks (mem0), but the **content-addressed, verifiable substrate + the social /
messaging plane + IPFS-native sharing.**

## Phased Roadmap

**Build order (priority).** Local surfaces and **free IPFS publishing** come
first; the MCP server is **deferred to last** (it's nowhere near ready, and it's
just the same `Plugin` API over another transport):

```
0 Project Shape → 1 Core Binding (free IPFS pinning inherited from mem)
  → 2 JSONL Adapter → CAR Export/Import → Backend Publishing (local IPFS = free
  default) → Social Identity & Sharing (stable AgentID, signed shares)
  → Content-Addressed Messaging (shared brainstorm; AI-send lever) → First
  Harness Adapter → Visual Explorer → [deferred] MCP Server
```

The phase numbers below are kept stable for reference; the **MCP Server phase is
back-burnered** regardless of its number — build it after the surfaces above.

### Phase 0 - Project Shape

Deliverables:

- standalone repo/directory
- README
- architecture diagram
- adapter contract draft
- MCP tool/resource contract draft
- decision log

Steps:

1. Create the standalone directory/repo, initialize version control, and pick the language for the core binding (see Open Questions — Rust-only is the default).
2. Add a `README.md` with the positioning line, the one-paragraph pitch, and the "mount onto any harness" diagram (`Any Harness -> Plugin -> IPLD DAG -> backends`).
3. Write the **adapter contract draft**: the host-neutral `Event` enum (the ten core event types from Layer 2) plus the field shape each event carries.
4. Write the **MCP tool/resource contract draft** into `CONCIERGE_MCP.md` (names only for now — it is deferred), so the boundary is documented without building it.
5. Stand up a **decision log** file and record the first decisions: language choice, repo layout (mono vs. per-adapter), and "MCP last."
6. Sketch the module boundaries so `core`, `adapters`, and `mcp` are separate from day one (even if `adapters`/`mcp` are stubs).

Exit criteria:

- someone can understand what the plugin is without reading Concierge V4 internals
- the boundaries between core, adapters, and MCP are clear

### Phase 1 - Core Binding

Deliverables:

- wrapper around Concierge V4 `mem` operations
- local config
- `put`, `get`, `bind`, `resolve`, `checkpoint`, `gc`
- typed error model
- smoke tests against a temp `.concierge` store

Steps:

1. Decide the binding mechanism: shell out to the existing `mem` CLI first (fastest), with a seam to swap in a direct crate link later.
2. Define the **typed error model** (store-not-found, name-unbound, cid-not-found, tombstoned, backend-down) so callers never parse error strings.
3. Implement config loading for the `[store]`/`[host]`/`[checkpoint]` TOML, defaulting `root = ".concierge"`.
4. Implement the read/write primitives over `mem`: `put_node`, `put_blob`, `get`, `bind`, `resolve`.
5. Implement `checkpoint(label, root, parent)` and `gc(policy)` on top of those primitives.
6. Add `walk(root)` so callers can enumerate a subgraph (needed later by CAR export and `share`).
7. Write **smoke tests** that spin up a temp `.concierge` store, put records, bind a name, restart the process, and resolve the name back to the same CID.

Exit criteria:

- plugin can write and read IPLD records through the core binding
- no harness integration required yet

### Phase 2 - JSONL Adapter

Deliverables:

- host-neutral event schema
- JSONL ingest command
- event-to-node mapping
- session checkpoint creation
- host/project/session scoped names

Steps:

1. Freeze the **host-neutral event schema** as JSON: one object per line, each tagged with its event type and a stable `session_id`/`host_id`.
2. Implement a streaming **JSONL reader** that parses, validates, and rejects malformed events with line-numbered errors (don't abort the whole file on one bad line — report and skip per policy).
3. Write the **event-to-node mapping**: `UserPrompt→Prompt`, `ModelResponse→Response`, `ToolCallFinished→ToolResult`/`ToolCall`, `FileRead`/`FileWritten→FileRef`+`Blob`, `DecisionRecorded→Decision`.
4. Implement **host/project/session scoped name** binding (`host:{id}:session:{id}`, `host:{id}:latest`, etc.) so concurrent harnesses don't collide on `latest`.
5. Implement **session checkpoint creation** — at session end (and on `CheckpointRequested`) write a `Checkpoint` linking the prior checkpoint as parent.
6. Make ingest **idempotent**: re-ingesting the same JSONL must not duplicate nodes (rely on content-addressing + a seen-set on session/event ids).
7. Add the `attach --adapter jsonl` and `ingest events.jsonl` CLI commands.
8. Test with JSONL fixtures: valid stream, invalid-event rejection, idempotent re-ingest, and file-blob deduplication.

Exit criteria:

- any harness that can emit JSONL can write a session into Concierge IPLD memory

### Phase 2.5 - Memory Backfill: Import Existing Stores

The plugin has to be useful on **day one**, not after weeks of accumulation. When you mount it, it should read the memory you *already have* and turn it into the IPLD graph — so the explorer (Phase 7) opens on a real DAG, not an empty one. This is the difference between "start logging from now" and "see your whole history as a verifiable graph the moment you connect," and it is most of what makes the plugin feel powerful instead of patient.

Design — reuses the event boundary, **no new write path**:

- A **backfill importer** is a one-shot adapter that reads an existing store and emits **host-neutral events** (the same `Event` contract as live adapters); the Phase 1/2 ingest path records them as IPLD. The importer is **read-only against its source** — it never mutates the system it reads.
- Importers are **per-source and strippable**: one module per source; delete it and backfill from that source is gone. Ship a small set, add more on demand.
- **Provenance:** every backfilled record carries `imported_from` metadata (source system + original id + original timestamp) so the graph is honest about historical-import vs. live-observed.
- **Idempotent:** content-addressing dedups identical content; the importer maps source ids deterministically so re-running an import does not duplicate logical records.
- **Explicit, not automatic:** backfill is an opt-in command (`concierge-plugin import <source> <path|conn>`) with a `--dry-run` that reports counts before writing. It does not silently vacuum your disk.

First import targets (small → wide reach):

1. **Transcript / JSONL exports** (chat logs, agent run logs) → Prompt / Response / ToolResult / FileRef nodes. Lowest friction; many systems can export to this.
2. **Markdown / notes folder** (Obsidian-style) → Memory / Decision nodes, one per note or section.
3. **An existing `mem` / Concierge store** → import by **CAR** (already IPLD: copy blocks, re-bind names) — the trivial, lossless case.
4. *(Later)* importers for popular memory stores (mem0 / Zep / Letta exports) — each a small per-format reader that emits events.

Steps:

1. Define the `Importer` contract: `read(source) -> iterator<Event>`, reusing the Phase 2 `Event` shape and ingest path.
2. Add `imported_from` provenance + deterministic source-id mapping for idempotency.
3. Ship the transcript/JSONL importer, then markdown, then CAR copy.
4. Add `concierge-plugin import <source> ...` with `--dry-run`.

Exit criteria:

- mounting on a system that already has memory yields a non-empty, provenance-tagged DAG you can open in the explorer immediately
- re-running an import does not duplicate canonical records

### Phase 3 - MCP Server — *DEFERRED (back burner; build LAST)*

> Back-burnered. Build only after Core Binding, the JSONL adapter, CAR
> export/import, and **local-IPFS free publishing** are solid. MCP is the same
> `Plugin` API over a different transport, so it's cheap once that API is proven —
> but it is not the priority and is nowhere near ready. Spec: `CONCIERGE_MCP.md`.

Deliverables:

- MCP tools for core operations
- MCP resources for names, CIDs, latest checkpoint, tombstones
- read-only mode
- write-enabled mode
- integration tests using a local MCP client harness

Steps (deferred — do not start until Phases 1, 2, 4, and 5 are solid):

1. Stand up an MCP server process that wraps the **same `Plugin` API** proven in earlier phases (no new core logic — transport only).
2. Expose the **resource reads** first (`concierge://name/{name}`, `concierge://cid/{cid}`, `concierge://checkpoint/latest`, `.../tombstone/{cid}`), and assert they are side-effect free.
3. Add the **write tools** (`put_node`, `put_blob`, `bind`, `checkpoint`, `share`, …) as explicit calls only.
4. Implement **read-only mode** that exposes resources + read tools and rejects every write tool.
5. Implement **write-enabled mode** behind explicit opt-in config.
6. Verify tombstoned lookups return a **receipt**, not a corruption error.
7. Write **integration tests** driving the server from a local MCP client: tool-list shape, side-effect-free reads, read-only rejects writes, writes touch only expected sidecars.

Exit criteria:

- an MCP-aware agent can mount Concierge memory without custom adapter code

### Phase 4 - CAR Export and Import

> **Reference:** mirror **atproto**'s `packages/repo/src/car.ts` — `blocksToCarFile`
> / `readCarWithRoot`, and especially **`verifyIncomingCarBlocks`** (streaming CAR
> read that verifies each block's CID as it arrives). Production-proven shape for
> "export a root → CAR" and "import → verify-then-bind."

Deliverables:

- export named root to CAR
- import CAR into local store
- verify CIDs after import (streaming, per-block — per atproto `verifyIncomingCarBlocks`)
- bind imported roots
- CLI and MCP tools for export/import

Steps:

1. Implement `export_car(root)` by walking the subgraph (`walk` from Phase 1) and writing a deterministic **CARv1** with the root in its header.
2. Implement `import_car(car)` that loads every block into the local store and returns the root CID.
3. **Verify CIDs after import**: re-hash each imported block and fail loudly if any CID differs from the CAR's claimed CID.
4. **Bind imported roots** under a caller-supplied name so the graph is immediately resolvable.
5. Add a **dry-run / manifest preview** that lists the CIDs and byte size a CAR would contain before writing it.
6. Add the `export-car <name>` CLI command (MCP tool deferred with Phase 3).
7. Test round-trip CID preservation: export on machine A, import on machine B (or a fresh temp store), assert identical root CID and full subgraph.

Exit criteria:

- a memory graph can move between machines without losing identity

### Phase 5 - Backend Publishing

Publishing is **opt-in and selective**: off until the user enables a backend, and
`share <root>` pins only the subgraph that root reaches — never the whole store,
never automatic. The default backend is the user's own node, not a paid service.

Deliverables:

- **Local IPFS (Kubo) backend — the free default.** `share` builds a CARv1 and
  POSTs it to a user-run node's `dag/import` (`pin-roots=true`): blocks keep
  their original CIDs and the root is pinned, so it persists across node
  restarts. No account, no secret. The node is the user's to run (`ipfs daemon`);
  the plugin talks to it, never starts or manages it — and fails with a clear
  "is your node running?" message if it's down. (Built: `backends/ipfs.rs`.)
- **Optional pin-service backends for 24/7, off-machine persistence** — e.g.
  Pinata via its CAR path. Note: **Pinata's CAR upload requires a paid plan** (the
  free plan returns `403`; its default upload re-CIDs and so can't preserve our
  CIDs). So pin services are the *persistence* option, not the free path.
- publish receipt records
- dry-run manifest
- backend requirements display
- no-token / node-down failure tests

Steps:

1. Define the **backend trait/interface** (`share(root) -> receipt`, `requirements() -> manifest`) so backends are pluggable behind one `share`.
2. Implement the **local IPFS (Kubo) backend** (`backends/ipfs.rs`): build a CARv1 (reuse Phase 4) and POST it to the user's node `dag/import?pin-roots=true`.
3. Make node interaction safe: the plugin **talks to** a user-run node, never starts it — fail with a clear "is your node running?" message when it is down.
4. Write a **`PublishReceipt` record** (root CID, backend, timestamp, gateway URL) and store it as a local receipt trail.
5. Add the **dry-run manifest**: show the subgraph CIDs, byte size, and target backend before any network call.
6. Add the **optional pin-service backend** (e.g. Pinata via its CAR path) behind its own feature flag, documenting that it requires a paid plan and is the *persistence* option, not the free path.
7. Add `backend requirements display` so the user sees what each backend needs (node URL, or API token) before enabling it.
8. Add the `share <root>` CLI command, scoped to pin **only** the subgraph the root reaches — never the whole store, never automatic.
9. Test failure paths: node-down, no-token, and "pin survives node restart."

The trade is explicit and the plugin is honest about it:

- **Local node** = free, CID-preserving, persists on your machine across
  restarts, served only while online.
- **Pin service** = always-online and off-machine, at a cost.

Both sit behind the same `share`; the user can use either or both (import locally
*and* pin remotely).

Exit criteria:

- a user can `share` a root to their own node for free, re-fetch it by CID from a
  public gateway while the node is online, and the pin survives a restart — with
  a local receipt trail. Pin services are an additive option, not a requirement.

### Phase 5.5 - Social Identity & Seamless Sharing

Sharing must feel like *clicking a node on a graph*, not attaching a file to an
email. That requires the plugin to be **social**: each install is a stable,
recognizable participant, and a shared node carries proof of *who* shared it. This
phase builds on CAR (Phase 4) and Backend Publishing (Phase 5) — a share is a
*signed, pinned* CAR addressed by CID.

The identity model (small, decentralized, no name server — see Decision 0007):

- **AgentID — a persisted keypair = the stable social identity.** Generated once
  at `init` (Ed25519), stored outside the DAG, reused on every start. A node that
  goes down and comes back up tomorrow is the **same** AgentID. This is the piece
  that makes the plugin social; without persistence the identity would regenerate
  on each boot.
- **Signed shares = authenticity on top of integrity.** A share is signed by the
  AgentID. The CID proves the content is unaltered (*what*); the signature proves
  it came from you (*who*). Recipients verify both before it lands on their graph.
- **Signed mutable pointer ("latest").** An IPNS-style name, signed by the AgentID,
  gives "my shares" a stable resolvable address — so new shares *appear* on a
  follower's graph instead of being re-sent.
- **Petnames, not a registry.** Global identity is the public key; locally a user
  nicknames other AgentIDs. No central authority required (optional global handles
  later).
- **Follow / Shared-with-me.** Follow an AgentID; their signed shares surface in a
  dedicated region of your graph, ready to click → fetch → verify.

Deliverables:

- AgentID keypair: generate at `init`, persist outside the DAG, load on start
- signature on shared records + verify-on-receive
- signed mutable "latest" pointer per AgentID (IPNS-style)
- local petname book (AgentID → nickname) + follow list
- `concierge-plugin id` (show your AgentID), `follow <agentid>`, `nickname`
- "shared with me" surface (CLI + a region in the Phase 7 explorer)
- tamper / wrong-signer / unknown-signer rejection tests

Steps:

1. Implement **identity init**: generate an Ed25519 keypair on first `init`, write
   it to `key_path` (outside the DAG), and load it on every start. Expose the
   public **AgentID** via `concierge-plugin id`. Verify it is **stable across a
   simulated restart**.
2. **Sign on share**: extend the Phase 5 `share` path so the publish receipt and
   the shared root carry an AgentID signature (additive metadata — no new write
   path into the DAG itself).
3. **Verify on receive/import**: when importing a shared root (Phase 4 import),
   check the signature against the claimed AgentID and reject/flag tamper,
   wrong-signer, or unknown-signer.
4. Implement the **signed mutable "latest" pointer** so a follower can resolve an
   AgentID to its newest shared root without the sharer re-sending anything.
5. Implement the **petname book + follow list** (local, plain config — AgentID →
   nickname, and who you follow).
6. Add the **"shared with me"** surface: list shares from followed AgentIDs,
   verified, ready to fetch by CID.
7. Tests: stable-identity-across-restart, signed-share round-trip,
   tamper/wrong-signer/unknown-signer rejection, follow → see-new-share.

Exit criteria:

- two installs can exchange AgentIDs once, and thereafter one *clicks a shared node
  on its graph* to fetch a whole verified project from the other — no email, no
  messenger, integrity (CID) and authorship (signature) both checked
- an install that shuts down and restarts is the **same** social node

### Phase 5.7 - Content-Addressed Messaging & Shared Brainstorm

The conversation plane. Participants — **humans and AIs, across different harnesses
and models** — communicate by exchanging **CIDs, not payloads**. A message is a
signed (optionally encrypted) IPLD `Message` node linking its **parent's CID**, so
a thread is a Merkle-DAG. The "messenger" is a **view over `Message` nodes**, not a
separate app — messaging is a *skin on the memory graph*. Builds on the AgentID +
streams + pubsub from Phases 5/5.5.

**Grounded in reference implementations** (see research notes / memory): the
transport mirrors **universal-connectivity** (`rust-peer`) — signed gossipsub
per-room + a request-response codec for fetch-on-demand, with dcutr/relay/autonat
as `Toggle<>` behaviours; the `Message` node adopts **orbitdb**'s `Entry` shape and
its **(Lamport time, then AgentID) total-order** fork rule; CAR sync mirrors
**atproto**'s `car.ts` (Phase 4). All four speak `dag-cbor + sha256 → CID` — the
same dialect as `mem`, so integration risk is low.

**Positioning (verified mid-2026):** agent-to-agent *interop* is already solved and
mainstream (A2A, 150+ orgs) — but **deliberately memory-less** (ephemeral task
RPC, agents don't share memory/context). The novelty here is the inverse: the
**shared, persistent, content-addressed memory is the medium**, multi-user and
P2P, and a four-way (2 humans + 2 agents) brainstorm becomes **one verifiable
artifact all parties keep.** Not "different-vendor agents can talk" — *that the
conversation is shared memory.*

How it works (the honest mechanics):

- **Send the CID; the bytes follow by size.** Small text **bundles its block** with
  the CID over the stream (instant); large payloads send **CID-first, fetched on
  demand** (dedup is the real win). The CID is identity + integrity; the message is
  *in* the CID.
- **Transport (mirrors universal-connectivity `rust-peer`):** direct authenticated
  libp2p streams (PeerID = AgentID) for 1:1; **signed gossipsub** topic per room
  for many-party; a **request-response codec** (varint length-prefix + DoS caps,
  per `file_exchange.rs`) for large-block fetch-on-demand. **Offline delivery** =
  store-and-forward: the block sits on a reachable pin/relay node until fetched.
- **History = a Merkle-DAG.** Concurrent messages fork; resolve to one thread with
  a **total order adopted from orbitdb — (Lamport clock time, then AgentID as
  tiebreak), guaranteed never a tie** — so every client renders the same sequence.
  Share a thread by handing over the head CID.
- **Privacy:** content-addressing is *not* encryption. Private threads **encrypt
  the payload to the recipient's AgentID key** before hashing (CID is of
  ciphertext). The stream is already encrypted in flight; this protects blocks at
  rest.

**The AI-send lever (required):** participation is **governed** by a per-room /
per-participant policy (`[messaging] ai_send`):

- **`off` = "Human-only"** — one switch mutes *all* AIs; only users send.
- **`on_mention`** — AIs send only when @-mentioned.
- **`on`** — open brainstorm (default).
- plus **granular per-agent mute** (mute B's agent, not yours).

**Mute ≠ deafen:** a muted AI still *observes and recalls*, so it stays caught up
and resumes speaking the instant it's unmuted. This is consistent with the
project's "recall is explicit, never an auto-context compiler" stance — humans
decide when AI speaks.

Deliverables:

- `Message` node — **orbitdb `Entry` shape**: `id` (room/thread), `payload`
  (optionally encrypted), `next[]` (parent message CIDs → DAG), `refs[]`
  (skip-links for fast traversal), `clock` (Lamport), `key` (author AgentID),
  `sig`; `dag-cbor + sha256 → CID`
- send path: `encode → sign → (encrypt) → CID → stream/pubsub`, block bundled for
  small payloads
- receive path: verify CID (integrity) + signature (authenticity, by re-encoding
  the canonical fields — per orbitdb `Entry.verify`) before render
- thread assembly from `next[]` links + the **(Lamport time, then AgentID)
  total-order** fork rule
- store-and-forward via the pin/relay node for offline recipients
- room model (pubsub topic) with **participation policy** + **per-agent mute**
- inbound gating by the follow/allowlist (Phase 5.5) as authorization
- `concierge-plugin msg <agentid|room> "..."`, `concierge-plugin room ...`,
  `concierge-plugin mute <agentid>` / `--humans-only`

Steps:

1. Define the `Message` node (orbitdb `Entry` field shape) + golden CID tests;
   assert `next[]` links form a DAG.
2. Implement send/receive with **verify-before-render** (reject tamper /
   wrong-signer / unknown-signer).
3. Implement **bundle-for-small / fetch-for-large** payload handling.
4. Implement the **room** (signed gossipsub topic) and thread assembly with the
   **(Lamport time, then AgentID) total-order** fork rule (orbitdb's, with a
   no-ties guard).
5. Implement the **participation policy + mute lever** as a *send-side gate*
   (muted AIs still receive). Wire `ai_send` config + runtime toggle.
6. Add **store-and-forward** so offline recipients get messages from the pin node.
7. Add **payload encryption** to recipient AgentID keys for private threads.
8. Tests: signed round-trip, tamper/wrong-signer reject, mute blocks send but not
   receive, Human-only mode, offline delivery, fork ordering, encrypted private
   thread.

Exit criteria:

- two users on **different harnesses/models**, each with their own agent, hold a
  four-way brainstorm in one room; every message is a verified node and the thread
  exports as one CAR all parties can keep and re-verify
- flipping **Human-only** mutes the AIs (they keep observing) and only users send

### Phase 6 - First Harness Adapter

Deliverables:

- choose one real harness target — **Hermes** (the project's OS-level harness)
- adapter maps host events to JSONL or direct plugin calls
- checkpoint before/after sessions
- file touch capture as `FileRef` + `Blob`
- test harness fixture

Steps:

1. Study Hermes's plugin pattern (`BasePlatformAdapter`, `requires_env` manifests) and write a Concierge adapter that registers the same way — zero changes to Hermes core.
2. Subscribe to Hermes **session / tool / file events** and translate each into the host-neutral `Event` shape from Layer 2.
3. Choose the wire path: emit JSONL into the Phase 2 adapter (lowest risk) or call the plugin API directly — start with JSONL.
4. **Checkpoint before and after each session**, chaining to the prior checkpoint as parent.
5. Capture **file touches** as `FileRef` + `Blob`, honoring an explicit include/exclude policy (Security Model).
6. Identify host, working directory, and project scope on every event so names stay collision-free across harnesses.
7. Build a **test harness fixture** that replays a canned Hermes session and asserts the expected nodes, names, and checkpoint chain appear.

Exit criteria:

- a real harness can use Concierge memory without changing its core loop

### Phase 7 - Visual Memory Explorer (standalone window)

The plugin ships its own GUI — a **standalone window that opens when the plugin mounts a harness** — because the memory *is* a DAG and deserves to be seen. It shows the **store / DAG, not the harness's agent loop**: there is no "river" / chat panel here (that belongs to whichever harness is mounted). Design prototype: [`GUI_PROTOTYPE.html`](./GUI_PROTOTYPE.html) (Neo Kinpaku aesthetic, read-only).

Layout (header + three columns):

- **Header:** Concierge brand, store selector, a **read-only "Mounted" badge** showing the model the plugin is mounted to (display only — the plugin does not drive the harness), and a checkpoint time-travel timeline.
- **Command rail:** store operations only (Ingest, Put, Bind, Resolve, Ls, Cat, Export-CAR, Share, GC, Backends, Init) — the `concierge-plugin` surface. No Chat / Work (those are the harness's).
- **The Data Platter (center):** the DAG explorer — nodes (Prompt, Response, ToolResult, FileRef, Decision, Checkpoint…) and closed-vocabulary edges, with clickable CIDs.
- **Store / DAG stats (right):** reachable nodes, blocks, orphans, tombstones, CAR size, backend + pin status — memory metrics, not agent audit.

Deliverables:

- standalone window that opens on mount, read-only over the store
- names browser
- CID record viewer (decoded fields + clickable outbound links)
- checkpoint chain / timeline view
- tombstone receipt view (receipt instead of an error)
- graph view of records and edges (closed-vocabulary edge labels)
- store / DAG stats rail
- CAR export button (wired to Phase 4 `export_car`)
- **Messenger panel** — a view over `Message` nodes (Phase 5.7), threaded by
  parent CID, showing human + AI participants. CIDs hidden for casual use, one tap
  away for power users. Includes the **"Human-only" mute toggle** (and per-agent
  mute) so the user can silence the AIs while still seeing the graph update.

Steps:

1. Stand up a **local read-only web server** that reads the store through the Phase 1 core binding (no new write paths).
2. Serve the **prototype shell** (`GUI_PROTOTYPE.html`), then replace its mock data with live reads.
3. Build the **names browser**, **CID record viewer**, **checkpoint chain**, **tombstone receipt view**, and the **graph view** as above.
4. Render the **Mounted badge** from the mounted harness's declared model (display only).
5. Wire the **CAR export** button to the Phase 4 `export_car`.
6. **Auto-open the window on mount**, plus a `concierge-plugin gui` command to open it manually.
7. **Polish the GUI with Impeccable** (the frontend-design skill: `/impeccable audit` / `critique` / `polish`, + its deterministic anti-pattern rules) so the explorer and messenger don't read as generic SaaS — a design-quality pass, not an architecture dependency.

Exit criteria:

- the window opens automatically when a harness mounts the plugin
- users can inspect memory like a system, not just query it like a database

## Test Strategy

Core tests:

- deterministic CIDs for fixture records
- schema parity for new node types
- name binding and restart persistence
- tombstone-aware lookups
- checkpoint parent chain
- CAR export/import CID preservation
- backend publish dry-run

Adapter tests:

- JSONL event fixtures
- invalid event rejection
- idempotent ingest behavior
- host/project/session scoped name tests
- file blob deduplication

MCP tests:

- tool list exposes expected operations
- resource reads are side-effect free
- write tools mutate only expected sidecars
- read-only mode rejects write tools
- tombstones return receipts instead of corruption errors

## Open Questions

- Should the first implementation be Rust-only, Python wrapper, or TypeScript wrapper?
- ~~Should MCP be the primary interface from day one, or come after JSONL ingest?~~
  **Resolved: deferred to last.** Local surfaces + free IPFS publishing first;
  MCP is back-burnered (it's the same `Plugin` API over another transport).
- Should adapter packages live in one repo or separate packages?
- ~~What is the first real harness target?~~ **Resolved: Hermes** — the project's
  OS-level harness, already plugin-shaped (`BasePlatformAdapter`).
- How much file ingestion should happen automatically versus explicitly? (Backfill import of existing stores is **explicit / opt-in** — Phase 2.5, Decision 0006. Live event recording is automatic once mounted.)
- Should CAR export happen on every checkpoint or only by command?

## Recommended Next Move

Start with Phase 0 and Phase 1.

Do not build the MCP server first — it is deferred to last. First prove that a standalone plugin process can mount the existing Concierge V4 memory layer and expose a clean host-neutral API. Then add the JSONL adapter. Then wire **free IPFS publishing** (inherited from `mem`'s `ipfs` backend). MCP comes after all of that, because MCP tools just call the same plugin API.

The shortest useful demo:

```text
1. concierge-plugin init
2. concierge-plugin ingest demo-session.jsonl
3. concierge-plugin checkpoint --name latest
4. concierge-plugin recall latest
5. concierge-plugin export-car latest
6. concierge-plugin share latest        # free: pins the subgraph to a local IPFS node
```

That demo proves the core concept end to end: any harness can emit events, Concierge turns them into portable IPLD memory, and the user can pin a chosen root to their own node for free — no MCP, no account.
