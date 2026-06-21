# Homestead Build Log — phase-by-phase audit trail

Tracks what's actually been built against `HOMESTEAD_PLAN.md`, with concrete checks to
audit each phase. Status legend: ✅ built (needs your build to confirm) · ⏳ pending ·
🔬 in progress · ⛔ blocked.

No Rust toolchain is available in the authoring environment, so every ✅ here means
"written and reviewed by hand" — your build on the Mac is the real verification. Each
phase lists exactly how to audit it.

**Invariants every phase must preserve** (from the plan's architectural principles):
1. The kernel stays thin — residents are supervised **processes**, never kernel threads.
2. No second security path — private land + portals reuse the capability vault
   (`crates/core/src/private.rs` + `crates/crypto`); public uses PSA (`pinning.rs`).
3. Reactive rendering — the client consumes events the kernel already emits; the AI never
   runs the game loop.
4. Privacy-first defaults — ocean/anonymous by default; GeoIP strictly opt-in.

---

## Phase 0 — Foundations ⏳ pending

**Delivers:** Babylon vendored into the asset pipeline and a confirmed kernel→browser
event channel, so Phase 1 has both a renderer and a live event stream to draw from.

**Reuse:** GUI asset router (`crates/gui/src/lib.rs`); the kernel event seam from the
capture work.

**New work:**
- Vendor `babylon.min.js` (alongside `crates/mcp/src/engines/phaser.min.js`, or the GUI
  asset dir) and expose it through the asset router.
- Confirm the kernel emits a structured event channel the browser can subscribe to; add
  `ToolCallStarted` / `ToolCallFinished` / `ModelResponse` if not already emitted.

**Files (planned):**
- `[NEW]` vendored `babylon.min.js` under the engine/asset dir.
- `[MODIFY] crates/gui/src/lib.rs` — serve the Babylon asset.
- `[MODIFY] crates/kernel/*` — event channel / event types (only if missing).

**Audit:**
```bash
# Babylon loads with no network fetch:
#   open the GUI, confirm the bundled babylon.min.js is served locally (no CDN hit).
# Event channel reaches the browser:
#   emit a hard-coded test event from the kernel, see it in a JS console subscriber.
```
**Pass criteria:** Babylon script served from local assets (zero-install holds); a kernel
test event is observable in the browser. **No** behavior change to existing routes.

---

## Phase 1 — Walkable single-resident slice (MVP) ⏳ pending

**Delivers:** the seed crystal — the node rendered as land, real memory records as
objects, one player avatar + one resident avatar reacting to real kernel events. No
economy, no stitching, no multi-agent.

**Reuse:** existing memory store / Librarian read routes (already kernel-served).

**New work:** `homestead.js` (Babylon flat orthographic; land + memory-object rendering;
player + one reactive resident); a homestead tab in the GUI.

**Files (planned):**
- `[NEW] crates/gui/src/homestead.js`
- `[MODIFY] crates/gui/src/index.html` — `#homestead-viewport` tab beside the graph.
- `[MODIFY] crates/gui/src/lib.rs` — serve `homestead.js`.

**Audit:** open the homestead tab → walk around → the resident reacts to a **real**
`ToolCallStarted` from your live session, rendered from your real memory substrate (not
mock data).

**Pass criteria:** walkable world drawn from real records; resident animates off a real
event; the graph tab and all existing routes are unchanged.

---

## Phase 2 — Persistent resident (supervised process) ⏳ pending

**Delivers:** a resident that lives as a supervised child process of the kernel, outliving
GUI restarts; the GUI is a thin viewer attached to its event stream.

**Reuse:** the kernel's find-or-spawn + restart-on-crash lifecycle (kernel Phase 5).

**New work:** model a resident as a supervised process (its **own small crate**, so the
kernel stays thin); keep the embedder warm; integrate the local LLM runtime (Ollama) **in
the resident process, not the kernel**.

**Files (planned):**
- `[MODIFY] crates/kernel/src/main.rs` — supervise residents.
- `[NEW]` resident process crate (entrypoint + loop).

**Audit:** close the GUI → resident keeps running (`pgrep`) → reopen → still there,
mid-task, no rebuild. `kill` a resident → the kernel restarts it; the world survives.

**Pass criteria:** resident persists across GUI close; killing a resident never takes down
the kernel; no inference loop runs inside the kernel.

---

## Phase 3 — Spatial object mapping ⏳ pending

**Delivers:** interactive objects wired to real routes — Chests (workspace folders),
Bookshelves (Librarian retrieval), Anvil/Workbench (tool harness), Hearth (LLM context).

**Reuse:** existing workspace-file, retrieval, tool-harness, and context routes.

**New work:** object→route interaction handlers in `homestead.js`; the hearth glow bound
to "resident thinking" state.

**Audit:** clicking a chest browses the real repo; striking the anvil actually runs
`cargo check` / `npm run dev` and returns the result to the world.

**Pass criteria:** each object drives the real underlying action, not a stub.

---

## Phase 4 — Land publishing (public + private paths) ⏳ pending

**Delivers:** a `concierge-land.json` manifest and a "Publish Virtual Land" Studio action
that CAR-packages the homestead and pins it via the chosen path.

**Reuse:** `crates/core/src/pinning.rs` (public PSA pin of the cleartext root CID);
`crates/core/src/private.rs` + `crates/crypto` (private capability-encrypted blocks). Both
paths already exist — this phase wires them, it does **not** add crypto.

**New work:** manifest schema; Studio publish flow; CAR packaging; public-vs-private path
selection.

**Files (planned):**
- `[NEW]` `concierge-land.json` schema + writer.
- `[MODIFY]` Studio publish UI + the publish action.

**Audit:** publish a public land → reachable via a gateway while this node is offline;
publish a private land → its blocks are ciphertext on the swarm, unreadable without the
sealed capability.

**Pass criteria:** correct path used per choice; user-facing copy is accurate
(durable-while-pinned, **not** "forever free"; ciphertext resolvable but unreadable).

---

## Phase 5 — Capability-handshake portals ⏳ pending

**Delivers:** doors/portals whose traversal is the existing capability handshake, surfaced
spatially. Public ↔ private boundary with owner and visitor flows.

**Reuse:** the capability vault — the portal **is** the handshake; no new security path.

**New work:** portal entity + traversal flow:
- **Owner** walks through → vault unseals the private capability locally → space loads.
- **Visitor** walks up → capability **request** presented → owner approval **issues a
  bearer read-key** → door unlocks; without it the door stays locked and the CID stays
  ciphertext.

**Audit:** a guest avatar is blocked at a private door until the owner approves, then
passes; revoking the capability re-locks the door.

**Pass criteria:** access is gated entirely by the vault's request/issue flow; no portal
path bypasses the Consent Gate.

---

## Phase 6 — Decentralized stitching + geo (ocean default) ⏳ pending

**Delivers:** edge-stitched neighbor lands, auto-generated portals between a user's own
spaces, and geo placement that is anonymous by default.

**Reuse:** libp2p peer discovery / gossip already in the node; bundled
`dbip-city-lite.mmdb.gz`.

**New work:** fetch + render neighbor `concierge-land.json` at map edges; auto-spawn
portals between local spaces; **ocean/anonymous by default**, GeoIP strictly opt-in via
the Anonymity Switch.

**Audit:** walk to a map edge → a neighbor's land renders; add a second local space → a
portal auto-spawns; fresh node → placed in the ocean until the user opts into geo.

**Pass criteria:** stitching works; portals auto-generate; **default placement is the
ocean** (no physical location leaked without opt-in).

---

## Phase 7 — The multi-resident city ⏳ pending

**Delivers:** multiple concurrent residents (Coder/Designer/Auditor/Clerk), each a
supervised process mapped to a distinct sprite, with light coordination.

**Reuse:** Phase 2's supervised-process model.

**Audit:** several residents run concurrently; one crashing or busy does not stall the
others or the world. Be realistic about how many local models a typical node can host;
degrade gracefully.

**Pass criteria:** the city tolerates an individual resident dying or grinding without a
global stall.

---

## Phase 8 — Economic autonomy ⛔ gated (own threat model, ships last)

**Delivers:** task assembly, hosting public APIs from the storefront node, and
micro-payments (WebLN / contracts) into the UCP wallet.

**Reuse:** the UCP wallet; the supervised-resident boundary.

**Required before any build:** an explicit threat model — spend limits, human-in-the-loop
approval for value transfer, sandboxed contract execution, and a full audit trail. A
resident must **not** move money autonomously without an approval gate. Review this phase
separately from the rest.

**Audit:** a test micro-payment routes to the wallet only after the approval gate; a
resident cannot exceed its spend limit; every economic action is logged.

**Pass criteria:** no autonomous value transfer; spend caps enforced; complete audit log.

---

## Definition of done (whole effort)
- Each phase ships walkable and is demoed on the **real** substrate, not mocks.
- The kernel holds **no** resident inference loops; killing/restarting a resident never
  takes down the kernel or the world.
- Private land and portals use **only** the existing capability vault — no second crypto
  path.
- A fresh node is anonymous (ocean) until the user explicitly opts into geo.
