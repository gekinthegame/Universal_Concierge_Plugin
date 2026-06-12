# Phase 8 — Active AI Node: Librarian, Guardian, and Context Compiler

> **Governed by Decision 0022.** This plan is the implementation of the
> node-resident librarian. Two non-negotiables flow down from that decision and
> bind every section below:
> 1. **Recall stays explicit by default (librarian-as-tool).** The host asks; the
>    librarian answers smartly. *Proactive* injection (§2, §4) is the **opt-in
>    librarian-as-agent path** — it requires the write-back seam **and** a
>    harness-specific *trusted-authority* grant (MemoryOS "Ground Truth" lesson).
>    It is never on by default and never inferred.
> 2. **Everything runs inside the Cryptree capability boundary (Decision 0011).**
>    The librarian unwraps only what the holder's capabilities already permit. The
>    embedding/vector index is itself sensitive (embeddings of decrypted content
>    leak content) and must be **capability-scoped — never one global plaintext
>    index across capabilities.**
>
> Ranking is **vector + graph-gravity**, packed to a token budget (see §1) — not
> vector similarity alone.
>
> 3. **The only model on the node is a small *embedding* model. No generative LLM
>    on-node — ever, not even an optional tier.** Governing rule: **the plugin
>    must not measurably affect the harness's or the machine's performance.** The
>    plugin is *Universal* — it runs on whatever machine the user already has,
>    alongside the host's model, and must stay effectively invisible in resource
>    use. The embedder is tiny (~100–140M, CPU-friendly) and runs **background,
>    batched, low-priority**. All generation, reasoning, and synthesis are
>    **deferred to the host's model** (which already exists). Never a full LLM on
>    Kubo; never a "router" LLM either.

## Objective
Turn the passive content-addressed (Kubo/IPLD) node into an **Active AI Node** —
a node-resident *librarian* that makes retrieval good (the network-participation
flywheel: the librarian needs the node online → the user keeps it running → the
mesh gains a participant). By default it is a *tool* the host calls for smarter
retrieval; with explicit opt-in + trusted authority it can also moderate rooms
and proactively suggest context.

## 1. The Embedded Librarian (Semantic + Graph Retrieval)
Implement an embedded vector database within `crates/core` to index the Merkle-DAG.

- **Crate:** `crates/core`
- **Library:** [LanceDB](https://github.com/lancedb/lancedb) (embedded, serverless, pure-Rust option).
- **Indexing:**
  - As `memory`, `text`, `decision`, and `checkpoint` nodes are ingested, the Librarian generates embeddings using a **small** local embedding model — e.g. `nomic-embed-text-v1.5` (~137M, CPU-friendly, 8K context, Matryoshka 768→64) or `bge-small-en-v1.5` — via `fastembed-rs`. This is the **only** model on the node; small is a **hard requirement, not a tuning knob** (Decision 0022 rule 3).
  - **Performance discipline (zero measurable impact):** embedding runs **background, batched, low-priority** so ingest never competes with the host. Matryoshka dims let the index shrink for very large libraries (100K files ≈ 0.1–0.3 GB; truncate dims before RAM becomes a concern). LanceDB is disk-backed/memory-mapped — the index does not sit resident in RAM.
  - Vectors are stored in LanceDB, mapped 1:1 to the node's CID.
  - **Capability-scoped index (Decision 0011/0022):** there is no single global plaintext index. Vectors live in per-capability index segments; the Librarian only embeds/searches content the active capability set already unwraps. Embeddings never persist outside the capability boundary they were derived in.
- **Ranking — vector + graph-gravity (not vectors alone):** reference cyber / cyberia-to (`~/Downloads/cyber-master`, `analizer/`).
  - **Graph importance:** compute gravity/density over the IPLD link graph (port `trikernel.nu`'s diffusion/springs/heat kernels — PageRank-family). A node's score blends embedding similarity *and* its graph gravity, so well-connected, load-bearing nodes outrank lexically-similar orphans.
  - **Context packing:** port `context.nu` — score candidate CIDs by `similarity × gravity/density`, then **greedy-knapsack into the requested token budget**, with a `--pinned` override to force specific CIDs in first.
  - *Borrow the graph-ranking + token-packing math only; reject cyber's public token/staking economy (Decision 0022).*
- **Retrieval Tool (default = librarian-as-tool, host-invoked):**
  - `concierge.retrieve(query, depth=["brief", "summary", "full"], token_budget)`
  - Returns the knapsack-packed top CIDs + content for the depth/budget.
  - "Multi-hop" reasoning: follow links from retrieved CIDs to discover related context (e.g., the `ToolResult` for a retrieved `Decision`) — traversal stays within the capability boundary.

## 2. The Context Compiler (Proactive Injection) — OPT-IN, librarian-as-agent

> **Node-side DONE (2026-06-10); harness write-back deferred.** Built:
> `crates/core/src/event.rs::ContextSuggested {cids, reason, authority}` (the one
> node→host event); `[injection]` config (`proactive` default **false**, `wake_on`
> default `["user_prompt"]`, `confidence`, `max_suggestions`, `budget_tokens`);
> `crates/core/src/compiler.rs::ContextCompiler` with `should_wake` + `suggest`,
> enforcing **both** gates — opt-in config **and** a `TrustedAuthority` grant —
> plus the confidence threshold; `ADAPTER_CONTRACT.md` documents the outbound event.
> Tested (the §2 verification cases): default-off emits nothing & never wakes;
> grant-absent is refused; opt-in + grant produces a suggestion attributed to the
> authority; confidence threshold respected.
>
> **Write-back seam DONE (2026-06-10).** `crates/core/src/outbox.rs` — the node→host
> transport: an append-only JSONL outbox at `<store>/outbox.jsonl` with **offset-based**
> consumption (`<store>/outbox.offset`) so a harness drains idempotently (a crash never
> drops or double-delivers). `MemCli::emit_context_suggested` (append), `outbox_peek`
> (pending, no advance), `outbox_drain` (pending + advance), and **`proactive_wake(event_type,
> query, authority_id)`** — the live wake trigger a capture loop calls: it runs the §2 gates
> (wake policy + opt-in + trusted authority) and enqueues a suggestion *only* if all pass,
> emitting nothing on the default tool-only path. The **harness "inject" half** lives in
> `crates/adapter-claude-code::render_injection` — formats a drained `ContextSuggested` into a
> `<suggested-context authority=…>` block the harness prepends, attributed (never silently
> merged — threat-model L1). **CLI:** `outbox <peek|drain|wake <query> [--authority ID]>` +
> `claude-code inject [--peek]`. Live-verified end-to-end: default-off → refused without
> authority → opt-in+grant enqueues → peek → inject renders the attributed block → idempotent
> drain. Tests: 3 (outbox) + 1 (render). **Remaining (genuinely external):** a live harness
> that *consumes* the block into its own context window (that side is the harness's, not ours).
> **Off by default (Decision 0022).** This is the *active* path; it turns the
> librarian from a tool into an agent. It activates only when the host has both
> (a) wired the write-back seam and (b) granted a harness-specific **trusted
> authority** (the MemoryOS "Ground Truth" lesson — injected memory the agent is
> not told to trust gets ignored or causes drift). Never inferred from capture.

Enable the node to suggest context to the host harness before it even asks.

- **Two-Way Contract:** Extend `ADAPTER_CONTRACT.md` and `crates/core/src/event.rs`.
- **Event:** `ContextSuggested { cids: Vec<String>, reason: String }`
- **Opt-in gate:** emitted only when the host's adapter config sets
  `proactive_injection: on` **and** presents a valid trusted-authority grant.
  Absent either, the Librarian stays tool-only (§1) and never pushes.
- **Wake policy (first-class, configurable — Decision 0022):** the look-ahead
  trigger is explicit, not "on every event." Default: on `user_prompt` only,
  rate-limited and budget-bounded; `tool_call_started` look-ahead is a separate
  opt-in (cost). Define when the librarian wakes, not just that it can.
- **Mechanism:**
  - On the configured wake trigger, the Librarian runs a background look-ahead retrieval (§1 ranking, within the capability boundary).
  - If the packed result clears the configured confidence threshold, the plugin pushes a `ContextSuggested` event back to the host.
  - **Harness Implementation:** The harness (e.g., Hermes or Claude Code) prepends the suggested CIDs to its internal context window, attributed as trusted per the grant.

## 3. The Room Guardian (Moderation & Safety)
Implement an autonomous moderation actor for p2p rooms.

> **Single-node core DONE (2026-06-10); P2P parts deferred.** `crates/core/src/moderation.rs`:
> **ExtensionPolicy** (block/allow-list extension gating; default shared-room blocklist of
> executables/scripts — blocks `.exe/.bat/.sh/.apk/…`); **QuarantineRegistry** — the local,
> **reversible, block-scoped** bad-CID list (quarantine the block, never the actor;
> persisted at `<store>/quarantine.json`); **`verify_block(cid, bytes)`** hash-validation
> (proof-of-scan primitive); **`Guardian`** screening (`screen_message` enforces RoomPolicy
> AI-send + mute + quarantine; `screen_file` enforces the extension gate) returning a
> `Verdict` (Allow/Mute/Quarantine/Block). `MemCli::quarantine_cid/release_cid/is_quarantined/
> quarantine_registry`; CLI `quarantine <list|add|release>`. Tested (7) + live CLI verified.
> **Quarantine wired to surfaces (2026-06-10):** the Librarian excludes quarantined CIDs from
> the index (never embedded/ranked/returned; reversible on `release`), and the Data Platter graph
> badges them (dashed vermilion, dimmed) — visible locally for review/release, withheld from
> retrieval. **Egress also blocks it (DONE):** `build_egress_plan` refuses to publish/export any
> quarantined CID — even a cleared one (safety > clearance, reversible via release) — surfaced in
> the publish-review panel via `blocking_locks`. Tested. Quarantine now blocks at all 3 points:
> retrieval (excluded), local graph (badged-visible), egress (refused).
> **Deferred (network era, `crates/net`):** the Guardian AgentID participating in P2P rooms;
> **tombstone-sync** over Gossipsub feeding trusted quarantine entries (mesh-scoped authority,
> reversible — `NETWORK_DEFENSE_PLAN.md` §4); inbound proof-of-scan on received blocks. Also
> not yet wired: surfaces consulting quarantine (retrieval-skip, GUI badge, relay-refuse).

- **Actor:** `Guardian` AgentID.
- **Participation Policy:**
  - Enforce `RoomPolicy` (`ai_send: "off"` / `"on_mention"`).
  - Automatically "mute" (locally hide) messages from unverified or flagged AgentIDs.
- **Safety Verifier:**
  - **Hash Validation:** Verify CIDs match their content hashes during ingest.
  - **Extension Gating:** Restrict which `FileRef` extensions are allowed in specific rooms (e.g., block `.exe`, `.bat` in shared project rooms).
  - **Tombstone Sync:** If the Guardian discovers a CID has been globally tombstoned (via a shared "Bad CID" list), it automatically removes it from the local store.

## 4. Summarization Checkpoints (Memory Synthesis)
Proactively compress long threads into synthesized "Decision" nodes.

- **Action:** When a room thread exceeds 50 messages, the Guardian flags it as a "Synthesis Candidate." No on-node generation happens.
- **Synthesis runs on the host's model, not the node (Decision 0022, rule 3).** The node has no generative LLM. The Guardian assembles the thread (via the §1 packer, within the capability boundary) and requests synthesis through the host harness's existing model; the returned summary comes back as content to store. If no host model is available, the candidate simply stays unsynthesized — never spin up a local LLM to do it.
- **Output:** Writes the returned summary as a new `Decision` node to the DAG that links back to the entire 50-message sub-graph as its "provenance." Writing the synthesis node is always allowed (capture is universal).
- **Surfacing (not auto-injection):** the summary CID is offered as a `--pinned` candidate to the §1 context-packer, so it ranks first *when a participant's host requests context*. Auto-pushing it into a session is the §2 opt-in path (trusted-authority gate), not a default — consistent with Decision 0022.

> **Progress (2026-06-10): §4 node-side DONE.** `crates/core/src/synthesis.rs`
> implements the three node responsibilities with **no on-node generation**:
> - `synthesis_candidates(mem, threshold)` enumerates rooms (message book +
>   bound `room-latest-` names) and flags those whose thread length ≥ threshold
>   (`SYNTHESIS_THRESHOLD = 50`).
> - `assemble_thread(mem, room)` joins the verified thread into text **for the host
>   to summarize** and returns the provenance CIDs (the exact sub-graph).
> - `record_synthesis(mem, room, host_summary, provenance)` persists the
>   **host-returned** summary as a `decision` node *derived from* the provenance —
>   via a new `MemCli::put_node_derived` (records `Source::Derived { from }`), so
>   `walk`/graph-gravity follow real, gravity-counted links back to the source
>   thread (not just inert body fields, which a strict `Decision` struct drops).
> No code path generates text; the summary must be supplied by the host.
> **CLI:** `synthesis <candidates [--threshold N] | assemble <room> | record <room>
> <host-summary…>>`. Tests: 3 (threshold gating, thread assembly + provenance,
> derived-link round-trip via `walk`). The live host-loop wiring (auto-detect a
> candidate → request host synthesis → record) rides the same deferred §2
> write-back path as proactive injection.

## 5. Development Steps

> **Progress (2026-06-10): §1 Librarian engine DONE.** `crates/core/src/retrieval.rs`
> implements the full ranking pipeline — an `Embedder` trait, PageRank gravity
> (`trikernel` diffusion kernel) + link density over the IPLD graph, the
> similarity×gravity blend, the greedy-knapsack packer (`token_budget` + `pinned`),
> capability-scoped indexing (`index(roots)` / `index_all`, calendar-tier aware), and a
> `Depth` control. The embedder is **pluggable**: the always-on default is a
> zero-dependency `LexicalEmbedder` (deterministic, offline, the test + fallback
> backend); fastembed (`nomic-embed`) + LanceDB are the **feature-gated backends behind
> the same trait** — so core's default build stays light and the novel IP is unit-tested
> now. Exposed as CLI `concierge-plugin retrieve <query> [--budget N] [--depth …]`.
> Verified on real captured memory (1675 nodes; an "egress lock privacy" query surfaces
> the actual egress-locking work, budget-packed). 7 unit tests cover the verification
> cases below (hub-outranks-orphan, budget, pinned, capability boundary).
> **Update (2026-06-10): §1 backends + surfaces DONE.**
> - **Swappable, config-selected embedder (the model is NOT baked in).** `[librarian]`
>   config picks the backend: `lexical` (zero-dep), `fastembed` (in-process ONNX,
>   `embedding_model` resolved by name against fastembed's catalog — bge-small, nomic,
>   mxbai, …; feature-gated `semantic-embed`), or `http` (an [`HttpEmbedder`] against ANY
>   model server at `embedding_url`, Ollama/OpenAI-style — zero-dep, works in the base
>   build). `default_embedder(&LibrarianConfig)` selects + degrades to lexical on failure.
>   Since models age fast, you swap the model via config (or point at an external server),
>   no recompile. Verified: fastembed resolves bge-small by name + semantic > lexical;
>   HttpEmbedder calls a mock server, normalizes, learns dims, degrades gracefully when down.
> - **GUI semantic-search bar** (§5.5): a Search tab in the Data Platter → `/api/search`
>   (ranked CIDs, click a hit to open it), backed by a TTL-cached, lazily-built index.
> - **Persistence:** a lightweight, model-tagged **embedding cache**
>   (`<store>/embed-cache.json`) reused across rebuilds/restarts so unchanged nodes are
>   never re-embedded (the real cost, esp. semantic) — `index_all_persistent`. Verified a
>   second build re-embeds zero cached nodes. **LanceDB (ANN + columnar) remains the
>   future feature-gated *scale* backend** for million-vector libraries; at personal-node
>   scale, brute-force cosine over the in-memory index + this cache is the right call
>   (avoids pulling Arrow/async into the default build).
> **Multi-hop retrieval (2026-06-10, DONE).** `retrieve_multihop(query, budget, pinned,
> depth, hops)` packs direct matches (now requiring similarity > 0 — no zero-relevance
> filler), then follows links from those matches breadth-first to pull in **related
> context** (e.g. a retrieved decision's linked provenance), tagged by `hop` depth,
> staying inside the capability scope and the token budget. Exposed as CLI `--hops N` and
> the GUI search "+ related" toggle. Tested (linked provenance surfaced at hop 1; budget
> respected).
> **Recency + kind filtering (2026-06-10, DONE).** Ranking now blends similarity ×
> gravity × density × **recency** (exponential decay, 14-day half-life) — conversational
> capture is mostly flat (uniform gravity), so recency is the main differentiator among
> equally-relevant memories. `retrieve_multihop(…, kinds: Option<&[String]>)` restricts
> *direct matches* to given node kinds (related context via multi-hop can be any kind;
> pinned exempt). CLI `--kind a,b`; GUI `&kinds=`. *Caveat:* recency uses the record-level
> `created_at` = **ingest** time, so a one-shot backfill is ~uniform recency; live capture
> accumulated over days ranks correctly. (A later refinement could prefer the original
> event timestamp.) Tested: newer-beats-older tie-break; kind filter excludes other kinds.
> **"Enable Sidekick" control (2026-06-10, DONE).** The Sidekick *is* this embedding model;
> the user enables it (never "deploy Kubo") via `crates/core/src/node.rs` — `enable_sidekick`
> launches the **private Kubo node** and persists consent; they are **coupled** (Sidekick
> needs the node; the private node only runs as part of the Sidekick). The node launches as
> a true **private swarm** — swarm key (PSK) + public bootstrap removed + `LIBP2P_FORCE_PNET=1`
> (`launch_private_node`), not merely a private repo, because a default Kubo node serves any
> CID publicly and would bypass the egress lock (see `NETWORK_DEFENSE_PLAN.md` §6). **Remaining
> for "graph-on-node is secure":** encrypt non-public content to inert ciphertext before it
> reaches Kubo (Cryptree 0011) — the egress→pin encryption wiring. `SidekickStatus {kubo_installed, node_running, enabled,
> operational, disclaimer}`; no-Kubo → honest install-guidance error. Surfaces: CLI `sidekick
> <status|enable|disable>`, GUI top banner (Enable + disclaimer / "node starting…" / "active"),
> CSRF-gated `/api/sidekick/*`, 5s-polled. (Actual daemon spawn untested in-sandbox; status/
> flag/coupling/disclaimer tested + live-verified.)
> **Remaining for §1:** prefer original event time for recency; batched/background semantic
> indexing for very large libraries; LanceDB scale backend when needed; private-node port
> isolation (non-default API port to avoid clashing with a user's general IPFS).

1.  **Crate Setup:** Add `lancedb` and `fastembed` dependencies to `crates/core/Cargo.toml` (pin a *small* embedding model — Decision 0022) — **feature-gated** so the default build stays light. **No generative-LLM runtime dependency** (no on-node `candle`/`llama`/etc. for generation) — synthesis defers to the host model.
2.  **Librarian Core — ✅ DONE:** the `Librarian` struct in `crates/core/src/retrieval.rs` — the `trikernel`-style gravity/density pass over the link graph and the `context.nu`-style greedy-knapsack packer (`token_budget` + `pinned`), capability-scoped. Default entry point is the host-invoked `concierge.retrieve` tool (CLI `retrieve` today). *(LanceDB segments + vector NN are the persistence backend, still to wire behind the trait.)*
3.  **Guardian Actor:** Implement the `Guardian` actor in `crates/core/src/moderation.rs`.
4.  **Adapter Update — ✅ DONE:** `crates/core/src/event.rs` has the `ContextSuggested` event; `crates/core/src/compiler.rs::ContextCompiler` gates emission behind the `injection.proactive` opt-in **and** a `TrustedAuthority` grant; the default path emits nothing. The JSONL write-back transport is `crates/core/src/outbox.rs` (append-only outbox + offset-based drain + `proactive_wake` live trigger) — a harness receives suggestions by draining the outbox.
5.  **GUI Integration — ✅ DONE:**
    - **Moderator badge** in the Messenger panel: `thread_json` returns a `moderation` block (Guardian active, `ai_send` policy, muted count, message count, synthesis-candidate flag at the §4 threshold); `loadThread` renders it as a badge row above the messages.
    - **Semantic Search bar** in the Data Platter (Search tab → `/api/search`) — done in §1.
6.  **Harness Demo — ✅ DONE:** `crates/adapter-claude-code::render_injection` reads a drained `ContextSuggested` and renders the attributed `<suggested-context>` block a Claude Code harness prepends; CLI `claude-code inject [--peek]` drives it (resolves each suggested CID to a preview). The harness *consuming* the block into its live context window is the one genuinely external piece.

## Verification
- **Test Case (retrieval quality):** Ingest 10 different project sessions. Ask a semantic question. Verify the Librarian returns the correct CIDs, and that a well-linked hub node outranks a lexically-similar orphan (graph-gravity is actually applied, not vectors alone).
- **Test Case (token budget):** Request `depth=summary` with a fixed `token_budget`; verify the packed result respects the budget and that a `pinned` CID is always included.
- **Test Case (capability boundary — Decision 0011/0022):** Hold a read capability for subtree A only. Verify the Librarian neither embeds, ranks, nor returns any node from sibling subtree B, and that no B-derived vectors exist in A's index segment.
- **Test Case (tool-default):** With `proactive_injection` off (default), drive a full host session and verify **zero** `ContextSuggested` events are emitted — recall happens only when `concierge.retrieve` is called.
- **Test Case (opt-in injection):** With `proactive_injection: on` + a valid trusted-authority grant, verify `ContextSuggested` CIDs are correctly prepended to a Hermes session prompt; with the grant absent, verify emission is refused.
- **Test Case (no on-node generation):** Trigger a Synthesis Candidate with no host model available; verify the node spins up **no** local LLM and the thread stays unsynthesized (rather than running generation on-node).
- **Test Case (performance impact):** Index a 100K-file library while a host session runs. Verify embedding is background/low-priority and that host-session latency and machine CPU/RAM stay within a defined negligible budget (the plugin is effectively invisible in resource use).
- **Test Case:** Set a room to "Human-only." Send an AI-signed message. Verify the Guardian mutes it.

## 6. External Knowledge Connectors (Distributed Knowledge)
Extend the Librarian's reach beyond the local store by connecting to external, decentralized knowledge bases like **Nexus STC (Standard Template Construct)**.

- **Mechanism:** Implement "Distributed Search Connectors" that can query remote indices stored on IPFS (using libraries like `summa-embed` or porting the `geck` client pattern).
- **Interface:**
  - `concierge.connect_external(source_url, alias)`
  - Registers an IPNS or CID as a searchable external library (e.g., `libstc.cc`).
- **Retrieval Integration:**
  - When `concierge.retrieve` is called, the Librarian can optionally federate the search to connected external sources.
  - Results from external sources are returned as **External CID References**, which the host harness can resolve directly from the global IPFS network.
- **Verification:**
  - **Test Case:** Connect to a mock STC index. Query for a scientific term. Verify the Librarian returns CIDs from the external index alongside local project history.

> **Progress (2026-06-10): §6 node-side DONE.** `crates/core/src/connectors.rs`.
> The **trust/privacy boundary** is the load-bearing design point:
> - **Querying an external source is egress** — the query leaves the device. Sources
>   are *opt-in* (explicitly registered) and federation is *off by default* in
>   `retrieve` (gated behind `--external`); nothing is queried until the user asks.
> - **External results are untrusted `ExternalHit` References** — kept in their own
>   section, always attributed (`source_alias`), **never merged into local
>   graph-gravity** and **never auto-injected** (the §2 trusted-authority gate still
>   applies). The node returns *references*; the host resolves the CIDs from the
>   global IPFS network if it wants them. The node never ingests remote bytes into
>   the local DAG and never generates.
> - **Quarantine applies**: a quarantined CID returned by a source is withheld.
> - **Best-effort**: a down/erroring source contributes nothing rather than failing
>   the whole search.
>
> Shape: `ExternalConnector` trait + `HttpIndexConnector` (v1, std-TCP HTTP POST
> `{query,limit}` → tolerant parse of `results`/`hits`/`data` with `cid`+optional
> `title`/`snippet`/`score`); `ConnectorRegistry` persisted at
> `<store>/connectors.json`; `federate()` ranks by score, withholds quarantined,
> truncates to limit. `MemCli`: `connector_registry / connect_external /
> disconnect_external / federate_search`. **CLI:** `connect <list | add <url>
> <alias> | remove <alias> | search <query…> [--limit N]>` plus `retrieve
> --external`. Tests: 5 (registry round-trip, http-only guard, mock-index CID refs,
> quarantine-withhold + score-rank, down-source degradation). `ipns`/`stc`-native
> kinds are future connectors behind the same trait.
