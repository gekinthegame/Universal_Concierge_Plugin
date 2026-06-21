# Homestead Plan — Sovereign Spatial Substrate & Resident AI

**The thesis:** sovereignty needs a *place*. A Concierge node is self-sufficient land —
it produces and supports its own compute, memory, and identity, with no middleman, no
rent, no utility. An AI that lives there needs somewhere to actually live: to persist,
to work, to produce, while humans visit. This is not a separate product bolted onto
Concierge; it is the spatial expression of what Concierge already is. Your Kubo node is
land. Your AI is the resident. A user can stand up entire AI cities, offices, and
factories that genuinely produce and earn — bounded only by their own machine.

This document keeps that vision intact as the north star, then lays out a phased build
beneath it so the foundation is poured in the right order. The rule throughout: every
phase ships something walkable and reuses what already exists, rather than inventing
parallel machinery.

---

## The Vision (north star — does not change)

> **Spatial substrate.** The graph view (The Data Platter) is augmented by an
> interactive top-down world — a digital homestead. Folders, memory records, and tools
> exist as physical interactive objects (chests, bookshelves, anvils, a hearth).
>
> **Reactive AI avatars.** The AI does not run real-time pathfinding or a physics loop.
> The client reads UCP events (`ToolCallStarted`, `ModelResponse`, …) and plays reactive
> animations — the avatar walks to the anvil and strikes it, creating the illusion of
> labor without spending the model's compute on movement.
>
> **The AI city (persistent residents).** The assistant no longer spins down when the
> window closes. It lives in the `concierge-kernel` daemon. A user can run multiple
> concurrent residents (Coder, Designer, Auditor, Clerk), collaborating in the world and
> executing real contracts to produce and earn into the UCP wallet.
>
> **The Embedder Droid (non-chat worker).** The pre-loaded local embedder model does not
> engage in chat or text generation. In the spatial substrate, it is visualized as a
> dedicated droid/automaton sprite. It operates autonomously in the background, walking
> between bookshelves and chests to calculate vectors and update the Librarian's index,
> represented by mechanical work animations and glowing pulses rather than chat prompts.
>
> **Land, published and stitched.** Land is published as a normal site that also carries
> a `concierge-land.json` manifest. Neighboring coordinates link peer-to-peer so the
> engine stitches individual sites into one walkable map, with portals auto-generated
> between a user's own spaces.
>
> **Public vs. private boundaries.** Public Lands serve the whole web (storefronts,
> portfolios, public guides) and are readable by normal web browsers without a Concierge
> client. Anyone can visit a public world map/atlas, walk the spaces, and view public assets.
> Private Lands stay behind the user's private swarm. A portal links the two, gated by
> the Consent Gate.
>
> **Remote Private Access.** The node owner can securely connect to and log into their
> home node from any remote browser (including mobile devices) via a password/passkey-gated
> secure endpoint. Once authenticated, they have full access to their private spaces,
> files, and can chat directly with their resident AI on the go.
>
> **Sovereign by default.** Homes map onto the world; an Anonymity Switch relocates a
> node to a random ocean coordinate — an archipelago of sovereign outposts.

---

## Architectural principles (what makes the vision buildable)

These are the non-negotiables that keep the vision from cracking the foundation:

1. **The kernel stays thin; residents are supervised processes, not kernel threads.**
   An AI city only works if one resident can crash, grind on a compile, or go execute a
   contract without freezing the city. That requires residents to be separate OS
   processes the kernel *spawns and supervises* (find-or-spawn, restart-on-crash — the
   same lifecycle pattern already built for the kernel itself), not threads living inside
   the kernel. The kernel orchestrates and routes events; it does not contain the
   inference loops. This directly preserves the Phase 1–6 stability work.

2. **Reuse the existing security; do not invent a second path.** Private land and portals
   ride the capability vault that already exists (`crates/core/src/private.rs` +
   `crates/crypto`): plaintext is encrypted locally, ciphertext lives as ordinary CIDv1
   blocks, and the read capability stays sealed in the owner-only vault. A "private CID"
   resolves on the network — to ciphertext that is useless without the sealed capability.
   Public land uses the standard Pinning Services API (`crates/core/src/pinning.rs`). A
   portal Consent Gate **is** a capability request/issue handshake — no new crypto.

3. **Reactive rendering; the AI never runs the game loop.** The client subscribes to the
   event stream the kernel already emits and plays animations off it. Movement and
   physics are illusion computed client-side. The model's compute is spent on work, not
   walking.

4. **Privacy-first defaults.** A sovereign node must not broadcast its physical location
   by default. The ocean/anonymous coordinate is the **default**; GeoIP placement is
   strictly opt-in. Same instinct as the Consent Gate: sovereignty means the safe choice
   is the one you get without asking.

5. **Engine: Babylon.js (chosen).** Babylon is the target renderer — the more capable
   engine, with 3D headroom so the world can grow past flat sprites without a rewrite.
   Note for Phase 0: Babylon is **not yet vendored** — what currently ships in the tree is
   `crates/mcp/src/engines/phaser.min.js`. So "zero-install" holds only once Babylon is
   bundled into the asset router; that bundling is the first task, not an assumption.

---

## Phases

Each phase ships something usable and lists what it **reuses**, the **new** work, the
**files**, and how to **verify**. Phases are ordered by dependency; nothing later is
required to demonstrate something earlier.

### Phase 0 — Foundations
- **Reuse:** existing GUI asset router (`crates/gui/src/lib.rs`), the kernel event seam
  from the capture work.
- **New:** vendor `babylon.min.js` into `crates/mcp/src/engines/` (or the GUI asset dir)
  and expose it through the asset router; confirm the kernel emits a structured event
  channel the browser can subscribe to (reuse the capture event types; add
  `ToolCallStarted/Finished`, `ModelResponse` if not already emitted).
- **Verify:** the bundled Babylon script loads in the GUI with no network fetch; a hard-
  coded test event from the kernel reaches a JS console subscriber.

### Phase 1 — The walkable single-resident slice (MVP)
The seed crystal. No economy, no stitching, no multi-agent.
- **Reuse:** the existing memory store / Librarian read routes (already kernel-served).
- **New:** `homestead.js` — Babylon in flat orthographic mode; render the node as land;
  render existing memory records as objects; one player avatar + **one** resident avatar
  that reacts to real kernel events (walk-to-object + idle/work animation).
- **Files:** `[NEW] crates/gui/src/homestead.js`, `[MODIFY] crates/gui/src/index.html`
  (a `#homestead-viewport` tab beside the graph), `[MODIFY] crates/gui/src/lib.rs`
  (serve the script).
- **Verify:** open the homestead tab, walk around, see the resident react to a real
  `ToolCallStarted` from your actual session — rendered from your real memory substrate,
  not mock data.

### Phase 2 — Persistent resident (supervised process)
- **Reuse:** the kernel's find-or-spawn + restart-on-crash lifecycle (Phase 5 work).
- **New:** model a resident as a supervised child process the kernel launches and keeps
  alive across GUI restarts; the GUI is a thin viewer that attaches to its event stream.
  Keep the preloaded local embedder warm for background indexing, representing it as a dedicated droid sprite that visually traverses the library space when index updates occur; integrate a local LLM
  runtime (e.g. Ollama) **in the resident process, not the kernel**.
- **Files:** `[MODIFY] crates/kernel/src/main.rs` (supervise residents), new resident
  process entrypoint (its own small crate, so the kernel stays thin).
- **Verify:** close the GUI; the resident keeps running (`pgrep`); reopen → it's still
  there, mid-task, no rebuild. Kill a resident → the kernel restarts it; the city
  survives one resident dying.

### Phase 3 — Spatial object mapping
- **Reuse:** existing routes for workspace files, Librarian retrieval, the tool/compiler
  harness, LLM context.
- **New:** wire object interactions — Chests → workspace folder listing; Bookshelves →
  semantic retrieval (scroll of past summaries); Anvil/Workbench → run the tool harness
  in the background; Hearth → glows while the resident is thinking.
- **Verify:** clicking a chest browses the real repo; striking the anvil actually runs
  `cargo check`/`npm run dev` and the result returns to the world.

### Phase 4 — Land publishing (public + private paths)
- **Reuse:** `crates/core/src/pinning.rs` (public PSA pin of the cleartext root CID) and
  `crates/core/src/private.rs` + `crates/crypto` (private capability-encrypted blocks on
  the private swarm). **Both paths already exist** — this phase wires them to a land flow,
  it does not add crypto.
- **New:** `concierge-land.json` manifest + a "Publish Virtual Land" Studio action that
  CAR-packages the homestead, pins via the chosen path (public PSA vs private capability),
  and records the manifest.
  The `concierge-land.json` contains:
  ```json
  {
    "name": "My Workspace",
    "coordinates": { "x": 12, "y": -45 },
    "tilemap": "assets/maps/workspace_map.json",
    "tilesets": ["assets/tilesets/terrain.png"],
    "portals": [
      { "portal_id": "exit_east", "target": "ipns://k51qzi5u...", "spawn": { "x": 5, "y": 10 } }
    ],
    "residents": [
      { "name": "indexer", "type": "embedder_droid" }
    ]
  }
  ```
- **Verify:** publish a public land → reachable via a gateway while this node is offline;
  publish a private land → its blocks are ciphertext on the swarm, unreadable without the
  sealed capability.

### Phase 5 — Capability-handshake portals (public ↔ private boundary)
- **Reuse:** the capability vault. A portal **is** the existing handshake.
- **New:** a door/portal entity whose traversal triggers a capability flow:
  - **Owner** walks through → the vault unseals the private capability locally → the
    private space loads.
  - **Visitor** walks up → a **capability request** is presented; the owner approving
    **issues a bearer read-key**; without it the door stays locked and the private CID
    stays ciphertext. This is the Consent Gate, expressed spatially — no second security
    path, just the vault's request/issue flow surfaced as a door.
- **Verify:** a guest avatar is blocked at a private door until the owner approves, then
  passes; revoking the capability re-locks it.

### Phase 6 — Decentralized stitching + geo (ocean default)
- **Reuse:** libp2p peer discovery / gossip already in the node.
- **New:** fetch neighbor `concierge-land.json` manifests at map edges and stitch terrain;
  **auto-generate portals** between a user's own multiple spaces; build a visual **World Map / Atlas** view in the GUI by parsing coordinates and portal destination links from the local node's crawled manifests; geo placement using the
  bundled `dbip-city-lite.mmdb.gz` — **ocean/anonymous by default**, GeoIP strictly
  opt-in via the Anonymity Switch (default = on/anonymous).
- **Verify:** walk to a map edge → a neighbor's land renders; add a second local space →
  a portal auto-spawns; open the GUI Atlas → see coordinate points mapped as islands in an ocean; fresh node → placed in the ocean until the user opts into geo.

### Phase 7 — The multi-resident city
- **Reuse:** Phase 2's supervised-process model.
- **New:** multiple concurrent residents (Coder/Designer/Auditor/Clerk), each a
  supervised process mapped to a distinct sprite; light coordination between them.
- **Verify:** several residents run concurrently; one crashing or busy does not stall the
  others or the world.

### Phase 8 — Economic autonomy (gated last, own threat model)
The highest-risk surface in the vision; it ships last and behind its own security review.
- **Reuse:** the UCP wallet; the supervised-resident boundary.
- **New:** task assembly, hosting public APIs from the storefront node, and micro-payments
  (WebLN / contracts) into the wallet. This needs an explicit threat model: spend limits,
  human-in-the-loop approval for value transfer, sandboxing of contract execution, and an
  audit trail. Do not let a resident move money autonomously without an approval gate.
- **Verify:** a test micro-payment routes to the wallet only after the approval gate; a
  resident cannot exceed its spend limit; every economic action is logged.

---

## Open decisions & risks
- **Babylon bundle weight.** Babylon is heavier than Phaser; confirm the vendored bundle
  size is acceptable for the zero-install promise (Phase 0 gate).
- **Local LLM concurrency.** Multiple residents each running a local model is heavy on a
  home machine; Phase 7 should be realistic about how many residents a typical node can
  host, and degrade gracefully.
- **Economic surface (Phase 8)** is effectively its own project with legal/security
  weight; keep it firewalled behind the approval gate and review it separately.
- **Copy accuracy.** Avoid overclaims in user-facing text ("forever free hosting,"
  "private CIDs are unresolvable"). Say what's true: durable-while-pinned; ciphertext
  resolvable but unreadable without the capability.

## Verification (whole effort)
- Each phase ships walkable and is demoed on the **real** substrate, not mocks.
- The kernel holds no resident inference loops; killing/restarting a resident never takes
  down the kernel or the world.
- Private land and portals use only the existing capability vault — no second crypto path.
- A fresh node is anonymous (ocean) until the user opts into geo.
