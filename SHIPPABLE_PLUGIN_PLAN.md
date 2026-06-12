# Path to a Shippable, One-Click Plugin

> **This document is the spine — drive the build from here, top to bottom.**
> It sequences every phase and links out to detail and rationale. You do not need
> to read the others end-to-end; this one tells you when to open them.
>
> **Document map (where each phase points back to):**
> | Phase | What | Detail lives in |
> |---|---|---|
> | A | Vendor `mem` in-process | this doc + `crates/core/src/binding.rs` |
> | A.5 | Per-UTC-day IPLD HAMT storage rollup | this doc · **why:** DECISIONS.md 0014 |
> | B | IPFS optional | this doc |
> | C | Harness auto-attach (Claude Code) | this doc |
> | 8 | Node-resident librarian (the sidekick) | **`PHASE_8_NODE_AI_PLAN.md`** · **why:** DECISIONS.md 0022 |
> | N | Private multi-agent network | **`PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`** (its own Phases A–H) · **why:** DECISIONS.md 0015, 0016 |
> | D | Packaging, signing, one-click install | this doc |
>
> - **Why any choice was made →** `DECISIONS.md` (0013–0016 cover this plan).
> - **Phase N's full spec →** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`; it in turn
>   builds on `DATA_PLATTER_PRIVACY_LOCK_PLAN.md` and Decisions 0010–0012.
> - **The capability-encryption crypto under Phase N →** `CRYPTREE_PORT_SPEC.md`
>   (Decision 0011): the concrete `crates/crypto` port (the Wuala Cryptree design) that
>   implements the capability-encrypted private subgraphs, read/write key
>   separation, and revocation-by-key-rotation both plans assume.
> - **Phase 8's full spec →** `PHASE_8_NODE_AI_PLAN.md` (Decision 0022): the
>   node-resident librarian — small on-node embedder + LanceDB + graph-gravity
>   ranking + token-budget packing. This is the standalone *sidekick* value and the
>   wedge of the Trojan-horse strategy (0025); it also supplies the tri-kernel that
>   Phase N's Social Legibility Layer (Phase I) depends on.
> - **Original/broader project plan (Phases 0–7 context) →**
>   `UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md`.

**Goal:** turn the current workspace into a downloadable app that a user installs
with one click on Mac / Windows / Linux. It runs as its own process with its own
GUI, but it is *inert without a harness* — its job is to capture and explore a
harness's activity. Launch harness: **Claude Code** (what it is already mounted
to today).

**Status today (the honest baseline):**
- Pure-Rust workspace; the GUI is baked into the binary (`include_str!`) and
  served loopback. No cloud, no DB server. Good foundation.
- **It already works, on Claude Code, right now** — the store is full of our own
  sessions (`mounted_model = claude-code`, names `host:claude-code:session:…`),
  captured by ingesting `~/.claude/projects/<project>/*.jsonl`.
- It is **not** yet one self-contained binary: it shells out to an external
  `mem` CLI built from a *separate* repo (`~/Desktop/Concierge_V4/mem`).
- Capture is **manual** (paste a path / re-ingest), not automatic or continuous.

The four blockers below are in dependency order. **A gates everything; D is last.**

**Locked decisions:**
- *DECISIONS.md 0013 (2026-06-07):*
  1. Phase A → **A1** — vendor `mem` as an in-process library.
  2. Phase C → **watch the whole `~/.claude/projects`** (backfill all history, then watch).
  3. Phase D shell → **native window** (webview, e.g. Tauri / wry+tao).
  4. Phase D distribution → **install-script + GitHub Releases first**, native signed
     installers as a fast-follow.
- *DECISIONS.md 0014 (2026-06-08):*
  5. Phase A.5 → **one IPLD HAMT per UTC day**; every node carries a UTC timestamp;
     SQLite/Tantivy only as a throwaway query cache.
- *DECISIONS.md 0015 (2026-06-08):*
  6. **Phase N → the private multi-agent network is a ship prerequisite**
     (`PRIVATE_MULTI_AGENT_NETWORK_PLAN.md` must pass its Release + Security Review
     gates before the installer ships). Reverses 0013's "p2p out of scope for v1."
     This is the critical path.
- *DECISIONS.md 0022 (2026-06-09):*
  7. **Phase 8 → the node-resident librarian (sidekick) is in the spine**
     (`PHASE_8_NODE_AI_PLAN.md`): a small on-node embedder + LanceDB + graph-gravity
     ranking, host-invoked. The standalone wedge (0025) and the tri-kernel source for
     Phase N's Social Legibility Layer. The **only** on-node model is the embedder —
     no generative LLM on-node, performance-invisible.

---

## Definition of done (one-click)
A non-technical user, on a clean machine that has Claude Code installed:
1. Downloads one file from a release page.
2. Opens it; no Gatekeeper/SmartScreen wall, no separate `mem`/IPFS/Kubo install.
3. On first run it finds their Claude Code sessions and starts capturing.
4. The DAG explorer opens on real data, and keeps filling as they use the harness.
5. **The librarian works on their own machine** — they can ask for relevant memory
   and get correctly ranked, capability-scoped results back (the standalone sidekick
   value, Phase 8), with no network and no on-node generative model.
6. **They can pair a second computer** (explicit approval, scoped access), both
   devices keep distinct keys, an authorized namespace **converges by missing-CID
   sync**, and they can **revoke** a device — i.e. the
   `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md` Release-Gate demo passes (see Phase N).

---

## Phase A — Vendor `mem` into the workspace  ·  **the gate**  ·  size: L
Nothing ships while the app depends on a `mem` binary that lives in another repo
and must be on `PATH`. Two ways out:

- **A1 — Link `mem` as a library (recommended end state).** Move its source into
  this workspace (e.g. `crates/mem`), make its `store` / `dag` / `names` / `node`
  modules `pub`, and rewrite `crates/core/src/binding.rs` to call them in-process
  instead of spawning `mem`. The `CoreBinding` trait and its tests stay; only the
  implementation changes.
  - *Bonus:* deletes a whole class of bugs we hit this session — the `argv`
    overflow (`E2BIG`), the stdin/stdout pipe deadlock, and the per-node spawn
    latency — all artifacts of shelling out. In-process is also simply faster.
  - *Cost:* it's the single biggest chunk of work; `binding.rs` is built around
    the CLI's text surface (stdout parsing, `get-many` JSONL, `reachable`).

- **A2 — Bundle the `mem` binary beside the app (faster bridge).** Ship `mem`
  inside the app bundle and resolve a sibling path instead of `PATH`. Much less
  code, but ships two executables, keeps the subprocess overhead, and means we
  must sign/notarize the helper too (see Phase D).

**RESOLVED (DECISIONS.md 0013): A1** (vendor `mem` as an in-process library) — the
clean path; it's on the critical path anyway and removes recurring bugs. A2 was the
faster/messier alternative.

---

## Phase A.5 — Storage rollup: per-UTC-day IPLD HAMT  ·  size: M  ·  rides with A
The flat model (one name binding + one top-level graph node per event) doesn't
scale — already ~1,175 names and a ~4,000-node forest walk. CARs are export/import
artifacts, *not* per-interaction storage, so the fix is to coarsen the **index** and
tier the **storage**. See DECISIONS.md 0014.

- **One mini-database per UTC day, indexed by an IPLD HAMT.** Day root → a HAMT
  (sharding content-addressed map) keyed by event id/timestamp → event record CIDs.
  Stays DAG-CBOR/CID-addressed, so dedup, verifiable export, and the
  lock/encrypt/publish machinery keep working. New dep in the `mem` crate
  (`ipld-core` is already present; add an `ipld-hamt`-style crate). Hierarchy:
  `store → day → session → event`.
- **Every node carries a UTC timestamp.** Records already have `created_at` (unix
  epoch = UTC); now *required* on every node kind — including index/synthetic nodes
  (day root, session index, HAMT internals). It is the **day-bucket key** (which
  day-HAMT a node lands in) and is surfaced per node in the explorer. Day boundary
  is **UTC**.
- **Granularity preserved.** Every prompt / response / tool_result stays its own full
  record; the HAMT only adds one drill-down level. Reading a session's
  prompts/responses/tools is unchanged.
- **Derived query cache (throwaway).** Authoritative store stays IPLD. A disposable
  SQLite (structured queries) / Tantivy (full-text search) index accelerates GUI
  search and is always rebuildable from the DAG — never a source of truth.
- **Lifecycle.** Today = hot loose blocks, re-bind the day root per event (rides on
  the existing checkpoint + `keep_checkpoints`/GC retention). Day end = seal into a
  CAR, optionally GC loose blocks. Browse an archived day by re-importing its CAR on
  demand (import path already exists).
- **Wins:** names index drops from ~1/event to ~1/day; the default graph is bounded
  to day nodes; cold history offloads to single CAR files instead of thousands of
  tiny block files.
- **Decluttering the graph is a first-class driver** (DECISIONS.md 0014.6): the
  explorer's depth-1 ring becomes ~one node per day instead of hundreds of sessions,
  so the default view is bounded and you drill in. `drawGraph` currently hardcodes
  "depth-1 = session" — update it to "depth-1 = day" when this tier lands.
- **Why it rides with A:** Phase A rewrites `binding.rs` to call `mem` in-process —
  building the day/HAMT shape at the same time avoids doing the storage layer twice.
- **Storage layout — decided (DECISIONS.md 0014.7):** hybrid — a shared,
  dedup'd content-addressed block store with a per-day HAMT *index* at rest;
  self-contained day-CARs generated only on seal/export. Accepts cross-day
  reachability GC in the live store (needed for retention anyway).
- **Calendar tiering above the day — decided (DECISIONS.md 0014.8):** the day tier
  extends upward into a self-maintaining LOD: `store → year → month → day → session
  → event`. Months/years are HAMT/manifest nodes (same idiom as the day) that link
  their children — no event is moved or merged. A bucket **seals** into an immutable,
  exportable CID once its calendar window passes (month after its last day, year
  after Dec 31); only the current day/month/year stay hot. **Weeks are dropped** as a
  structural tier (Sun–Sat straddles month/year boundaries, so day⊂week⊂month⊂year
  is not a valid chain) — week is at most a display lens over days. This refines the
  graph-declutter point above: the explorer's depth-1 ring is the **coarsest sealed
  tier** (years for old history), expanding year→month→day→session→event on drill-in,
  so `drawGraph`'s ancestor walk must climb the calendar chain rather than assume a
  fixed "depth-1 = session/day".

---

## Phase B — Make IPFS optional  ·  size: S  ·  **DONE (2026-06-09)**
The default publish backend is Kubo at `127.0.0.1:5001`; no fresh machine has it.
But capture, ingest, the local store, and the whole GUI already work offline —
IPFS is only the "publish publicly" path.
- **Done — startup is offline-safe by design.** The node is contacted *only* in
  `IpfsBackend::post_car`, reached solely from a publish/share. Nothing on the
  startup / stats / graph / ingest path opens a socket to `:5001`, so a down node
  causes no hard failure (verified by `publishing_reads_as_opt_in_when_no_node_is_running`:
  `/api/stats` returns 200 with the node down).
- **Done — publishing presents as opt-in, not an error.** Added
  `selected_backend_reachable(cfg)` (a quick, short-timeout TCP probe; never on the
  startup path) → surfaced in `/api/stats` as `publishing_ready` + per-backend
  `reachable`/`pin_status` ("not set up — publishing is optional (everything works
  offline)"). The GUI badges the backend as opt-in and the publish review opens with
  setup guidance instead of a raw error. The "explicit reviewed publish" gate is
  unchanged; an actual publish against a down node still reports `BackendDown`.
- *Later, optional:* use the `libp2p` path already in `crates/net` as a
  no-Kubo backend, or embed one.

---

## Phase C — Harness auto-attach + continuous capture  ·  size: M  ·  **DONE (2026-06-09)**
The integration seam is **proven** (it's running on Claude Code now); it just
needs to become automatic. This is what makes the app non-inert on a fresh
machine.
- **DONE — Claude Code transcript adapter** (`crates/adapter-claude-code`). Claude
  Code's `~/.claude/projects/<slug>/*.jsonl` is its *own* format (not our
  `Envelope`); the new adapter translates it (user prompts, assistant text,
  `tool_use`→`tool_result` pairing by tool name, `thinking`→inline `Reasoning` per
  0023) and reuses the JSONL ingest path. Verified on a real 4142-line session
  (→ 2254 events: 91 prompts, 551 responses, 805 tool_results, calendar-bound).
- **DONE — auto-detect + watch.** `discovery` enumerates the whole
  `~/.claude/projects`; a `notify` file-watcher ingests appended lines
  **incrementally** (byte-offset tracked, only complete new lines). Ingest is
  content-addressed → re-reads dedupe by CID, so capture is idempotent and the
  watcher is safe to fire on every change. Exposed as `concierge-plugin claude-code
  backfill|watch`. Verified live (append → incremental capture).
- **RESOLVED (DECISIONS.md 0013): watches the whole `~/.claude/projects`** — backfills
  the user's entire Claude Code history, then watches for new sessions. ✓
- **DONE — in-app continuous capture + onboarding.** The Data Platter (gui crate)
  spawns a consent-gated background capture loop (`capture_once` every 3s — cheap: a
  stat + seek-to-EOF per file when idle). A first-run banner (`/api/claude-code/status`)
  reads "Found N Claude Code sessions — Attach"; attaching (`/api/claude-code/attach`,
  CSRF-gated) persists consent (a sentinel under the store) and the loop backfills then
  tails; the banner then shows a live "Capturing Claude Code" badge with Detach.
  Capture is **opt-in** — nothing is ingested until the user attaches; an un-attached
  node costs one stat per tick. Verified live (status→attach→backfill bound the
  session). Headless `concierge-plugin claude-code backfill|watch` remains for CLI use.
- **DONE — offset persistence + project hint.** Per-file offsets persist to
  `<store>/capture-offsets.json`, loaded at start and saved whenever they advance, so a
  relaunch resumes the tail (verified: appended line after relaunch ingested only the new
  line, not a re-scan). The status reports `current_project_sessions` (launch-cwd slug
  match) so the banner foregrounds "N sessions for this project". Phase C is complete.

---

## Phase 8 — Node-resident librarian (the sidekick)  ·  size: L  ·  the standalone wedge
Full spec: **`PHASE_8_NODE_AI_PLAN.md`** (Decision 0022). This is the part that
turns the passive store into a *functioning* substrate — and it's the standalone
value the Trojan-horse strategy (Decision 0025) lands on, so it must be excellent
on a lone, offline, private node *before* any network exists.

- **Scope:** a small on-node **embedding** model (`nomic-embed-text-v1.5` / `bge-small`
  via `fastembed-rs`) indexes ingested nodes into embedded **LanceDB**; retrieval
  ranks by **vector × graph-gravity** (port cyber's `trikernel` gravity/density) and
  packs results into a **token budget** (port `context.nu`'s knapsack). Exposed as a
  host-invoked tool, `concierge.retrieve(query, depth, token_budget)`.
- **Non-negotiables (Decision 0022):** the **only** model on the node is the small
  embedder — **no generative LLM on-node, ever**; all reasoning/synthesis defers to
  the host's model. Must be **performance-invisible** (background/batched/low-priority)
  and **capability-scoped** (per-capability index segments; embeddings never cross the
  Cryptree boundary, Decision 0011). Recall stays **explicit by default**; proactive
  injection and the Guardian are **opt-in** (trusted-authority gated).
- **Rides on A:** needs Phase A's in-process `mem` store to index against.
- **Feeds N:** supplies the tri-kernel that Phase N's **Social Legibility Layer
  (Phase I)** depends on (Decision 0024) — so it lands before that part of N.

Exit criteria:

- on a lone offline node, `concierge.retrieve` returns correctly ranked CIDs
  (graph-gravity applied, not vectors alone), packed to the requested token budget,
  strictly within the capability boundary; no generative model runs on-node and the
  host session's performance is unaffected while indexing a large library.

---

## Phase N — Private multi-agent network  ·  size: XL  ·  **critical path**
The full `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md` (its Phases A–H) — pulled into v1 by
DECISIONS.md 0015. Lets one person run a private mesh of Concierge devices/agents
that pair, scope access, sync content-addressed blocks, and converge on shared
memory without copying any identity secret. This is the largest body of work in the
program — a distributed-systems + applied-cryptography protocol — and it, not
packaging, determines the ship date.

- **Scope** (see that plan for full detail): multi-tier identity
  (User/Network/Device/Actor) + membership/actor certificates; one-use authenticated
  pairing; least-privilege capabilities + namespaces; capability-encrypted private
  subgraphs; signed head records + bounded request-response block/CAR sync;
  multi-parent `MergeCheckpoint` with deterministic, concurrent-head-preserving
  merge; revocation + key-rotation epochs; AutoNAT/rendezvous/relay discovery and
  offline store-and-forward; and the full Data Platter network UX.
- **Gates (hard):** the plan's **Release Gate** (A–E convergence, A–D cryptographic
  privacy, F real-internet networking) **and the *internal* Security Review Gate**
  (threat model, identity/crypto self-review, parser fuzzing, malicious-peer +
  resource-exhaustion tests, secret/log audit). **The external third-party audit is
  dropped** (DECISIONS.md 0016) — instead ship with an explicit **"UNAUDITED —
  community review welcome"** notice in the README + network UI, and do not
  advertise the network as audited/secure.
- **Foundations already in tree** (starting points, not done): `crates/crypto`
  (build per **`CRYPTREE_PORT_SPEC.md`** — the Decision 0011 Cryptree port that
  delivers the capability-encrypted private subgraphs + read/write key separation +
  revocation-by-key-rotation this phase depends on),
  `crates/core/src/identity.rs` / `messaging.rs` / `social.rs`, `crates/net`
  (libp2p relay/dcutr/identify).
- **Coheres with A.5:** the multi-parent `MergeCheckpoint` extends the same
  checkpoint model the per-UTC-day HAMT touches — design them together. All new
  record types ride Phase A's in-process `mem` store.
- **Non-negotiables** (from the plan): never copy a device key; private sync ≠ public
  publication; encrypt before data leaves a device; verify everything received;
  merges never mutate immutable history; least-privilege agents; revocation is
  prospective; no secret-bearing CLI args.
- **Open lever (DECISIONS.md 0015):** full A–H + audit before ship, vs a graduated
  gate (ship on A–E/F, defer some Phase G/H hardening/UX) with narrower advertised
  claims. Not yet decided.

---

## Phase D — Packaging, signing & one-click install  ·  size: L  ·  **last**
Download → double-click → running, on each OS.
- **App shell:** today it serves a loopback GUI and opens the default browser.
  - *D-tab (simplest):* keep the browser-tab model; a small launcher starts the
    server and opens the tab (optionally a menubar/tray presence).
  - *D-window (more "app"):* wrap the GUI in a native webview (Tauri / wry+tao) so
    it's a real window, not a browser tab. More work, better app feel.
  - **RESOLVED (DECISIONS.md 0013): native window** (`D-window`). The browser-tab
    model can remain a dev/fallback path, but the shipped shell is the native
    webview window.
- **Targets & builds:** macOS arm64 + x64 (ship a universal binary), Windows x64,
  Linux x64. GitHub Actions build matrix (`cargo` + `cross`).
- **Trust (the real friction):**
  - macOS: codesign + **notarize** → needs an Apple Developer ID (~$99/yr), else
    Gatekeeper blocks the download.
  - Windows: Authenticode signing cert, else SmartScreen warns.
  - Linux: usually fine unsigned (AppImage / `.deb`).
- **Installers:** `.dmg`/`.pkg` (mac), `.msi`/NSIS (windows), AppImage/`.deb`
  (linux). *Fast first cut:* a one-line install script + prebuilt binaries on a
  GitHub Releases page (great for developers, ships in days).
- **Release pipeline:** tag → Actions builds, signs, uploads to Releases.
  Auto-update is a later nicety.
- **RESOLVED (DECISIONS.md 0013): install-script + GitHub Releases first** (ship
  soonest); native signed installers (`.dmg`/`.pkg`, `.msi`/NSIS, AppImage/`.deb`)
  as a fast-follow.

---

## Sequencing
```
A (vendor mem) + A.5 (per-day HAMT) ──┬─> C (auto-attach) ───────────────┐
                                      ├─> B (IPFS optional) ──────────────┤
                                      ├─> 8 (librarian/sidekick) ──┐      │
                                      └─> N (private network, A–H) ┴──────┴─> D (package + sign + RELEASE)
                                            ▲ critical path        ▲ 8 feeds N's Phase I (social gravity)
```
- **A** is the gate — do it first. **A.5** rides with A (same `binding.rs` rewrite).
- **B**, **C**, and **8** can run in parallel once A/A.5 land (8 needs the in-process
  `mem` store from A to index against).
- **C** must be done before any real launch, or the app is inert on a clean machine.
- **8** is the **standalone sidekick value** (the Trojan-horse wedge, Decision 0025) —
  it works on a lone offline node with no network. It also supplies the tri-kernel
  that **N's Social Legibility Layer (Phase I)** needs, so land 8 before that slice of N.
- **N** is the **critical path** — the largest phase. Phase D code can be built in
  parallel, but the actual **release** is gated on N's Release Gate + the *internal*
  Security Review Gate (external third-party audit dropped — DECISIONS.md 0016 —
  shipped with an "unaudited / community review welcome" notice instead).
- **D** is last and is mostly ops/signing, not product code.

## Explicitly out of scope for v1
- MCP server / write-back into the harness (`crates/mcp` is a deliberate stub).
- Multi-harness support beyond Claude Code (Hermes/generic JSONL adapters exist
  but auto-attach targets Claude Code first).
- Auto-update.
- ~~The p2p / private-network features in `crates/net`~~ — **now in scope as Phase N**
  (DECISIONS.md 0015). The private-network plan's *own* "Explicitly Deferred" list
  still applies (global identity registry, reputation/scores, automatic semantic
  conflict resolution, post-quantum migration, etc.).

## Loose ends to clean before a public release
- Version is `0.0.0` — adopt real semver for releases.
- Untracked/`.orig` files in the tree (`crates/gui`, `crates/net`,
  `crates/cli/src/main.rs.orig`, `crates/gui/src/lib.rs.orig`, etc.) — commit or
  remove before publishing.
- Decide licensing/ownership when the `mem` repo is merged in (workspace is
  `MIT OR Apache-2.0`).
- `repository` in `Cargo.toml` is a placeholder (`example.invalid`).

## Decisions — RESOLVED (2026-06-07, DECISIONS.md 0013)
1. **A1** — vendor `mem` as an in-process library. ✓
2. **Whole `~/.claude/projects`** — backfill all history, then watch. ✓
3. **Native window** (webview). ✓
4. **Install-script + GitHub Releases first**, native signed installers as a
   fast-follow. ✓

## Suggested first move (when we start building)
Phase A1, smallest viable slice: bring `mem`'s source into the workspace as
`crates/mem`, expose its `store`/`dag`/`names`/`node` modules, and convert **one**
`MemCli` method (e.g. `get`) from subprocess to in-process as a vertical proof —
keeping `CoreBinding` and its tests green — before converting the rest. Once the
in-process seam is proven, introduce the **A.5** day/HAMT schema in the same pass
(add the HAMT crate to `mem`, require `created_at` UTC on every node, write through
a per-UTC-day HAMT) so the storage layer is built once, not twice.
