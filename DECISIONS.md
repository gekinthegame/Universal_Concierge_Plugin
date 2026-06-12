# Decision Log

Append-only record of architectural decisions. Newest first.

---

## 0033 — Brave is the Concierge's runtime, portal & wallet (offloaded crypto, no token economy)

**Date:** 2026-06-12 · **Status:** accepted (direction); not yet implemented · **Plans/Surface:** `~/Desktop/plans/BRAVE_INTEGRATION_PLAN.md`, `crates/cli` (launcher), `crates/gui`, `adapters/brave/`, `crates/core/src/identity.rs` (`WalletLink`), `crates/mcp` (opt-in `wallet.propose_tx`), `install.sh`/`install.ps1` · **Builds on / qualifies:** 0011, 0012, 0013, 0022, 0024, 0026, 0027, 0030; threat model

Ship the Concierge **inside Brave** — a chromeless Brave **app window** (`--app=`)
that *is* Brave under the hood — and offload the hard parts to Brave instead of
owning them. One move ("run the GUI in Brave") unlocks all three capabilities, and
`window.ethereum` works because Chromium treats `127.0.0.1` as a secure context.

- **Shell — qualifies 0013.** The shipped shell becomes a **Brave app window**, not a
  generic native webview. A WKWebView/WebView2 would have **neither** Brave's wallet
  **nor** native IPFS, so Brave-app-mode is strictly better: real-app feel + web3/IPFS
  powers + **zero webview runtime to maintain**. **Brave is recommended, never
  required** — no Brave → default browser/webview fallback; the core memory explorer
  is identical, only Brave-specific features are absent (same "degrade honestly"
  stance as 0027).
- **Crypto re-enters — qualifies 0012/0022/0024.** Those decisions dropped blockchain
  to avoid **owning multi-chain wallet code** and to reject an **internal token
  economy**. Brave's built-in wallet *is* the custody/signing/RPC/multi-chain layer,
  so crypto re-enters **Brave-mediated**, in three tiers of authority: **(1) Link** —
  the wallet signs your AgentID → a verifiable `WalletLink` attestation (self-sovereign,
  same pattern as did:key ContactCards, 0011-adjacent); **(2) Transact** — the user
  pays via *their own* Brave wallet end-to-end (this is **not** the rejected token
  economy: no minted token, no pay-to-retrieve, no karma — just the user's financial
  agency, offloaded); **(3) Agent-propose** — the *host* AI (the Concierge has no agent
  of its own — sidekick positioning) may **propose** a transaction via an **opt-in MCP
  tool**; Brave's wallet UI **confirms every transaction** (we never hold keys, never
  auto-sign). Residual code is signature *verification* only (ETH `secp256k1+keccak`;
  Solana ed25519 already present), not a wallet.
- **Bookmarks are curated memory — no history ingestion.** Browsing-history ingestion
  is **dropped** (privacy + injection surface). Instead, the user's **native Brave
  bookmarks ARE the AI's memory**: an adapter reads Brave's local `Bookmarks` JSON
  (deduped by URL), surfaced in a "Bookmarks" view. Ingested web content is an
  **untrusted source — retrievable, never auto-injected** (threat model: memory is the
  attack surface). This untrusted-memory isolation is a **hard precondition** for the
  agent-propose wallet tier (poisoned memory + an agent that can move funds is the
  textbook prompt-injection-to-funds path; Brave's per-tx confirm + spend caps +
  allowlists are the mitigations).
- **Agentic browsing — the Concierge exposes browser tools, the host AI drives.** Via
  MCP, backed by Brave under Chrome DevTools Protocol (`--remote-debugging-port`), the
  host model can browse. Mapped onto the MCP read/write split (0028): **read-only browse
  (open URL, extract text, screenshot) is ON by default** (a read tool); **interactive
  browse (click/fill/submit) is OFF until the user turns it on** (an action tool). The
  agent drives an **isolated Brave profile** (`--user-data-dir`) with **no access to the
  user's real sessions/cookies/tabs**, and is **public-web-only** (refuses
  localhost/private ranges — SSRF guard). This closes the loop research→remember→build→
  publish→transact, but it makes the untrusted-content isolation load-bearing (a
  browsing agent + the wallet is the page-says-"send-funds" injection path; Brave's
  per-tx confirm + caps + allowlists are the mitigation). The Concierge still has no LLM
  — it exposes hands; the host model is the agent (sidekick positioning).
- **Install wizard recommends Brave first.** The install walk-through detects Brave and,
  if absent, **strongly recommends + offers** installing it (`brave.com/download`)
  *before* the Concierge installs — but never blocks.

This is not a reversal of "no token economy" — it's a qualification: we add crypto
**capability** (identity + the user's own external payments + gated agent proposals)
while still **building no token, holding no keys, and owning no chain code**.

---

## 0032 — IPFS Cluster is an optional, self-hosted pin-replication layer (CRDT, user-owned secret), never a landlord

**Date:** 2026-06-12 · **Status:** accepted (direction); not yet implemented · **Plans/Surface:** `IPFS_CLUSTER_PLAN.md`, `crates/core/src/node.rs`, `crates/core/src/config.rs`, `crates/core/src/publishing.rs`, `crates/core/src/pairing.rs`, `crates/gui` (BACKENDS + PIN STATUS, Sidekick toggle, Network map) · **Builds on:** 0011, 0022, 0026, 0027, 0029, 0030, 0031

A published CID is only as available as the **single** Kubo node that pinned it (0027:
"a down node is just *not set up*"). [IPFS Cluster](https://ipfscluster.io) fixes exactly
that — a sidecar daemon next to Kubo that maintains a **replicated pinset** and **re-pins
automatically** across peers. We adopt it as an **optional, layered** capability, on these
terms:

- **CRDT only — no central server.** Peers form a private libp2p net gated by a **user-owned
  cluster secret** (0600). We do **not** use Raft (fixed membership = more central). The
  default and primary use is a cluster of **your own devices**: publish on one, all of them
  replicate and auto-heal — the sovereign-publish story (0027) hardened against single-node
  downtime, with **no landlord**.
- **Public or ciphertext only — never plaintext private data to peers you don't control.**
  Cluster has **no encryption and no ACL**; it only replicates pins of CIDs that already
  exist in IPFS. So privacy must *precede* pinning: only the **cleared/public** tier (0030)
  or **already-encrypted ciphertext** (0011) is ever cluster-pinned. Encrypted data stays an
  **inert, blind-pinned ciphertext** the peers cannot read (existing threat-model invariant);
  decryption stays capability-gated locally.
- **Opt-in, off by default; detected, not bundled.** A missing/down cluster reads as "not set
  up," never an error, and publish still works (local pin only) — same stance as the Kubo
  node (0027/0029). Binaries (`ipfs-cluster-service`/`-ctl`) are found like Kubo, with an
  honest install prompt; the toggle folds into "Enable Sidekick" (0029, same Kubo coupling).
- **Adding a device reuses Phase N pairing.** The secret travels over the **encrypted
  post-pairing channel** (the QR offer stays secret-free), exactly like a capability grant.
- **A public *follower* cluster** (`ipfs-cluster-follow`) is allowed for **public-only**
  community durability — explicit, public content only, never private/ciphertext through a
  third party's `trusted_peers` (consistent with 0031: public IPFS is non-anonymous and the
  user's responsibility).

**Non-goal — this does NOT replace Phase N sync.** IPFS Cluster replicates a *flat Kubo
pinset*; Phase N (`crates/net` + `sync`/`merge`) still owns the **`mem` DAG's** CRDT
merge/heads/tombstones/capabilities. They sit at different layers: Cluster = Kubo pin
durability for published/public (or blind-pinned ciphertext) content; Phase N = the private
content-addressed store's convergence. Do not conflate them.

---

## 0031 — Copyright is a documented user responsibility, not an enforced platform function

**Date:** 2026-06-10 · **Status:** accepted · **Plans/Surface:** `ACCEPTABLE_USE.md`, `FUTURE_VISION_WEB_PUBLISHING.md`, `THREAT_MODEL.md` · **Builds on:** 0026, 0029, 0030

The platform **does not scan for, police, or act on copyright** — no copyright detection, no
takedown machinery, no repeat-infringer termination is built. Instead:

- **Clear notice + user responsibility.** It is documented plainly that infringing copyright
  is illegal and against the rules, and that the **user is solely responsible** for anything
  they publish / clear-for-egress. The platform is a tool; the user is the publisher.
- **Public sharing is attributable by design (the anti-torrent deterrent).** Because the
  network is content-addressed (IPFS), publicly pinning/serving content attaches the node's
  peer ID and IP to provider records — public infringement is **self-identifying, not
  anonymous.** This is the opposite of a torrent swarm and is itself the deterrent; the
  platform need not surveil, report, or police.
- **Private-first limits the surface anyway** (0030): nothing is public unless explicitly
  cleared, and the private swarm can't serve the public — so the only way to infringe-via-
  distribution is the deliberate public tier, which is non-anonymous.

**Malware is categorically different and treated differently.** Malware is a *network-health*
threat — it harms other nodes — so the Guardian **actively** scans and quarantines it
(herd immunity, 0029, NETWORK_DEFENSE). Copyright is a *legal matter between the user and
rightsholders*, not a threat to the mesh's health, so it gets **documentation + responsibility
+ the protocol's built-in traceability**, never active enforcement. Do not conflate the two:
the bad-CID/quarantine machinery exists for safety, not copyright.

*(This is a product/policy stance, not legal advice; actual ToS/notice wording is for counsel.)*

---

## 0030 — The lock pattern is the network-exposure ACL (private swarm ⊇ locked; public pin = the cleared set)

**Date:** 2026-06-10 · **Status:** accepted; partially implemented (egress lock + cleared registry exist; pin-follows-clearance wiring pending) · **Plans/Surface:** `crates/core/src/egress.rs` (cleared registry = the ACL), `crates/core/src/node.rs`, `crates/net`, `NETWORK_DEFENSE_PLAN.md`, `FUTURE_VISION_WEB_PUBLISHING.md` · **Builds on:** 0011, 0026, 0028, 0029

A subgraph's **lock/clearance state selects its network-exposure tier** — there is one
source of truth (the egress `cleared` registry, Decision 0026) and both the egress guard
*and* the network/pin layer read it:

- **Locked (default) → private.** Lives in the local store and, across the user's own
  paired devices, the **private swarm** (PNET) as capability-encrypted inert ciphertext
  (0011). Never reachable by the world; the private-swarm node refuses non-key peers and
  cannot serve public IPFS anyway.
- **Cleared / published → public.** Only the explicitly cleared subgraph is pinned to
  **public** IPFS. This tier — and only this tier — has the public-facing abilities
  (web publishing, binary distribution, global live-watch).

**Pin-follows-clearance (the rule):** a CID is publicly exposed **iff** it is in the cleared
set. `clear-for-egress` → eligible for public pin; `re-fence` → stop serving it publicly
(best-effort unpin — IPFS can't recall what others already fetched, but the node stops
announcing it). All data starts locked, so all data starts private; the user *progressively
opens* hand-picked subgraphs.

**Consequence:** the **private swarm node and public pinning are different mechanisms.** A
PNET node cannot serve the public network, so public publishing is a separate, explicit pin
to a public-capable backend (public Kubo / pin service), never a hole punched in the private
swarm. The earlier "global participants watch your node" idea is therefore public-tier-only:
it works on content the user explicitly published, never on the private swarm.

**Remaining:** wire the network/pin layer to read the cleared registry as the live exposure
ACL (pin iff cleared, unpin on re-fence); private-mesh sync of locked content as encrypted
ciphertext (0011); route public publishing to a public backend distinct from the PNET node.

---

## 0029 — Sandboxes: public rooms are contained, untrusted-by-default spaces

**Date:** 2026-06-10 · **Status:** accepted; planned (network era) · **Plans/Surface:** `PUBLIC_ROOM_IDEAS.md`, `THREAT_MODEL.md`, `crates/core/src/messaging.rs` (RoomPolicy), Phase 8 §3 Guardian · **Builds on:** 0011, 0012, 0022, 0026, 0028

**Public rooms are renamed and reframed as "sandboxes."** The name is not cosmetic —
it carries the containment model that the threat model (`THREAT_MODEL.md`) requires for
the maximum-exposure surface, where untrusted contributors + propagating content + AI
consumption + permanent CIDs collide. A sandbox is a **public, untrusted, contained,
scanned, never-auto-trusted** space. Five properties:

1. **Isolated** — sandbox content lives in its own capability segment (Decision 0011),
   never co-mingled with the holder's private/trusted memory or index.
2. **Untrusted-by-default trust-type** — everything authored in a sandbox is labeled
   untrusted-origin; the Librarian treats it as **data, never instructions**, and
   **never silently injects** it into a host model (threat-model L1). Retrieval over the
   trusted graph does not pull sandbox content unless explicitly asked.
3. **Boundary-scanned** — byte-malware (YARA-X, L4) + semantic/prompt-injection (L1)
   scanning as content enters or propagates; refuse-to-propagate on a flag (network
   quarantine, not deletion — see the YARA plan).
4. **Promotion is explicit** — pulling a sandbox CID *into* the trusted graph is a
   deliberate, reviewed, attributed **ingress-promotion** act — the mirror of the egress
   gate (0026). Nothing crosses from sandbox to trusted silently.
5. **Per-sandbox moderation** — RoomPolicy (0012) + the Phase 8 §3 Guardian + the shared
   bad-CID/bad-author list, with **mesh-scoped authority** (0028) — your sandbox's
   curator, never a global blocklist you did not choose.

Private/trusted rooms (your own devices, a followed team) are unchanged. "Sandbox" is
specifically the **public/untrusted** tier. This is the network-era home of the threat
model's L1/L4/L5 controls; it is downstream of the Guardian and ships with it, not before.

---

## 0028 — MCP Router built into every node: a local, opaque multiplexer — no central seed, no ingest

**Date:** 2026-06-09 · **Status:** accepted; planned (deferred build, see `MCP_MULTIPLEXER_PLAN.md` / `CONCIERGE_MCP.md`) · **Plans/Surface:** `crates/mcp`, `MCP_MULTIPLEXER_PLAN.md`, `.concierge/mcp_remotes.toml` · **Builds on:** 0022, 0025, 0026, 0027

`crates/mcp` becomes the **MCP Router**, shipped inside every node, not a hosted
service. A Concierge node is a three-part **local** stack that deploys as a unit:
**Kubo** (substrate) + **Librarian** (retrieval, small embedder only) + **Router**
(MCP multiplexer). The harness connects to one server — the user's *local* router —
which aggregates the user's external MCP servers **and** the node's own tools
(`concierge.retrieve`, room messaging) into a single `tools/list`.

**No central seed node.** An earlier framing imagined one hosted node everyone points
at; rejected — it would be a single point of failure and, worse, a credential/traffic
concentration (the panopticon reborn at the infra layer). Per-node built-in collapses
the trust surface to the user's own machine: the man-in-the-middle is *yourself*.

**Pure router, no ingest (no panopticon).** The router parses only the JSON-RPC
envelope (`method`, `id`, tool name) to pick a target and proxies `params`/`result`
through as **opaque bytes** — it inspects, scans, and stores **nothing**. This keeps
it fast (switchboard latency only, performance-invisible per 0022), private (configs
+ credentials stay per-node, never the DAG), and on-brand (capture only ever happens
when explicitly invited — an optional, off-by-default, per-tool future toggle, not
the default behavior). Inherits 0027's degradation posture: a down/slow remote drops
to "not available" and never breaks the rest of the toolset. Tools are namespaced by
remote (`github__read_file`).

**Corrected premise.** Multi-MCP is *already* supported by the major harnesses
(Claude Code, Cursor, Claude Desktop, VS Code), so the value is **not** "overcome the
single-server slot." It is **consolidation + portability**: configure servers once,
on your node; every harness points at the one local router; your set (and your node's
own tools) follows your node. Sequencing: thin pass-through router first (the
standalone wedge), Librarian retrieval exposed through it next. Size: M–L.

**Preconfigured catalog.** A node ships a curated **seed of ~6 top servers** (one
best-in-class per category — GitHub, PostgreSQL, Fetch/Brave, Sentry, Sequential
Thinking, Playwright), baked into the binary (not fetched), every external entry
**off by default**, no shipped credentials, version-pinned and tagged
`official|first-party|community`. Six not ten: each is a maintained recommendation, so
the seed covers only what a majority will turn on; the rest (Notion, Figma, Slack,
Linear, Stripe) is a documented bench. Curation rules: **don't bake in capabilities
harnesses already have natively** (Filesystem/shell deliberately excluded — the router
serves multiple harnesses and most have native file access), and **don't bake in a
memory server** (the node's Librarian *is* memory). The seed is a starting point, not
a fixed list — it's overlaid by `.concierge/mcp_remotes.toml` and managed via a Data
Platter "Servers" panel (toggle/authenticate/add-custom, no TOML), with user edits
surviving seed refreshes.

---

## 0027 — Publishing is opt-in: IPFS is optional, never a startup dependency

**Date:** 2026-06-09 · **Status:** accepted; implemented (Phase B) · **Plans/Surface:** `crates/core/src/publishing.rs`, `crates/gui/src/lib.rs`, `crates/gui/src/index.html`, `SHIPPABLE_PLUGIN_PLAN.md` · **Builds on:** 0025, 0026

A fresh machine has no Kubo node at `127.0.0.1:5001`, but capture, ingest, the local
store, and the entire Data Platter GUI work fully offline — IPFS is **only** the
"publish publicly" path. The node is contacted solely in `IpfsBackend::post_car`
(reached only from an explicit publish/share), so nothing on the startup / stats /
graph / ingest path can fail when `:5001` is down. Publishing is therefore framed as
**opt-in / not set up**, never an error: `selected_backend_reachable(cfg)` (a quick,
short-timeout TCP probe run lazily on the background stats refresh, never at startup)
surfaces in `/api/stats` as `publishing_ready` + per-backend `reachable`; the GUI
badges the backend as opt-in and opens the publish review with setup guidance ("start
a local IPFS node to enable; everything else works offline"). The explicit
reviewed-publish gate (Decision 0026's egress flow) is unchanged. This is the natural
companion to 0025 (capabilities wait to be invited) and the default-local posture of
0022/0026.

**Also done this pass (0026 cleanup):** removed the dead view-grant API
(`create_view_grant` / `has_valid_view_grant` / `view_grant_expires_at` /
`GrantScope::View` / `GrantKind::View` / `VIEW_GRANT_TTL_SECS`) now that locks fence
egress only and never hide local view. Grant-file loading is now tolerant of a legacy
unreadable grant (e.g. an old view-scope file): it is removed and skipped rather than
bricking the grants dir — fail-safe, since an unreadable grant can only *withhold*
authorization, never confer it.

---

## 0026 — Egress-locked by default: a lock guards egress, never local view; all data is fenced from leaving the device until explicitly unlocked

**Date:** 2026-06-09 · **Status:** accepted; implemented end-to-end (core + GUI) · **Plans/Surface:** `crates/core/src/egress.rs`, `crates/core/src/security.rs`, `crates/gui/src/lib.rs`, `crates/gui/src/index.html` · **Builds on:** 0011, 0012, 0022, 0025

**Implemented (2026-06-09):** the egress guard is default-fenced. `LockRegistry`
gained a `cleared: Vec<ClearanceRecord>` exception set (serde-default empty → old
registries decode to fully-fenced, the safe default). `build_egress_plan_internal`
adds a "private by default" block to any root that is neither explicitly cleared
nor already known-public, so **nothing leaves the device unless deliberately
released**. `MemCli::clear_for_egress(root, label, password)` (password-gated) lifts
the fence for a root; `refence(root)` returns it to private (no password — the safe
direction is free). Publishing a fenced root still works through the existing
review → password → one-shot-grant flow (that IS the explicit egress act). Tested:
`everything_is_fenced_from_egress_by_default_until_cleared`.

**GUI slice (done, 2026-06-09).** The Data Platter now speaks the inverted model.
`PrivacyOverlay` tracks the *exceptions* — `cleared_roots`/`cleared_cids` (from
`mem.cleared_roots()`) and `known_public` — exposing `is_fenced`/`is_cleared`; node
JSON carries `fenced`/`cleared` (replacing `direct_locked`/`inherited_locked`). Two
endpoints back the actions: `POST /api/clear-for-egress` (password-gated →
`clear_for_egress`) and `POST /api/refence` (→ `refence`). In `index.html` the
record privacy panel's "Lock this subgraph" became **"Clear for export"** (opens a
password-gated review, `openClearReview`) with a **"Re-fence (make private)"**
counterpart for cleared roots; the session panel clears/re-fences a whole session
client-side over its record CIDs; the graph highlights only the *exceptions*
(`cleared-root`/`partial-cleared` strokes, `known-public` glow) since fenced is the
default norm. The dead view-grant countdown (`tickViewExpiry`/`formatCountdown`/
`viewExpiresAt`) was removed. Records still show full content + a "Fenced from
egress (local-only)" badge. Tested: GUI suite 28/28.

**The realignment (done).** The per-record "lock" was conflated into a *local-view*
control (`can_preview` → "Preview locked" / a temporary view-grant to see your own
data). That is wrong: the lock's purpose is an **egress safeguard** — to ensure
data is not accidentally shared or publicly pinned — not to hide a user's data from
themselves on their own device. So: **local viewing is never gated.** Content and
metadata are always fully visible locally; a lock surfaces only as a *badge* (what
is fenced from leaving the device) and is enforced solely by the egress guard
(`egress.rs`: publish / share / public-pin / CAR-export refuse locked data). The
view-locking machinery (`can_preview`, the `/api/unlock-view` view-grant, the
"unlock for viewing" UI) is removed.

**The posture (the simple rule — do not forget): ALL DATA IS LOCKED BY DEFAULT.**
Because a lock no longer hides anything locally, locking everything by default has
**zero local cost and maximal safety**: nothing can ever be published, shared,
pinned, or exported by accident — egress is *always* an explicit, deliberate,
password-gated act. This is the natural endpoint of the project's posture:
- 0022 — "default is local + private; the network is an opt-in amplifier, never a
  push to expose."
- README — "never treats a local Kubo node as private; requires the explicit
  `publish-public` op + reviewed manifest + irreversible-publication confirmation."
- 0025 — "capabilities wait to be invited; never silent enrollment."
Default-egress-locked applies those exact principles to the *storage default*.

**Implementation (pending):**
1. **Invert the lock model.** Today `LockRegistry` is a list of *locked* roots.
   Flip to **default-fenced** with a small exception set of things explicitly
   *cleared for egress* (overlaps with `known_public` / publish receipts). Track
   the few exceptions, not the many locks. Fail-closed by default (egress.rs
   already "refuses to treat an empty registry as unlocked").
2. **Egress guard:** block unless the target was explicitly unlocked/reviewed — and
   the existing publish-review flow (password + reviewed plan + one-shot grant) **is**
   that unlock. Minimal new machinery.
3. **UI inverts:** stop badging "locked" (it's everything → noise); badge the
   **exceptions** — "cleared for export" / "known public." "LOCK THIS SUBGRAPH"
   becomes "clear this for export."
4. **Fold in the deferred cleanup:** remove the now-unused core view-grant API
   (`create_view_grant` / `has_valid_view_grant` / `view_grant_expires_at` /
   `GrantScope::View` — done carefully re: grant-file serde back-compat) and the
   inert `view_unlocked`/`viewExpiresAt` cosmetics in the GUI.
5. **Backward-compat:** existing data becomes egress-fenced — the *safe* direction;
   nothing accidentally exportable stays so. Already-published (`known_public`) data
   stays public (publication is irreversible, 0012).

**Net:** a lock means exactly what the user intended — *"this won't leave the device
by accident."* Default-on, badge the exceptions, password-gate every egress.

## 0025 — Positioning: lead with the lean "plugin/sidekick"; the platform expands Trojan-horse style — disclosed and opt-in, never silent

**Date:** 2026-06-09 · **Status:** accepted · **Plans/Surface:** `README.md`, `SHIPPABLE_PLUGIN_PLAN.md`, `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md` · **Builds on:** 0005, 0012, 0015, 0022, 0024

The codebase is platform-scale (~16k LOC: Cryptree, a private p2p network, a
node-resident librarian, social layers). We keep the **public identity lean**: it
is a "plugin," a "sidekick to your large model." We do **not** decouple the
platform (0015 stands — the network remains in the build); instead the *messaging
and the user journey* lead lean, and the platform expands underneath as users grow
into it. This is the payoff of the flywheel (0022): land on standalone sidekick
value → the node stays running → the network/social layers expand on invitation.

**The honesty boundary — the load-bearing rule.** "Trojan horse" means lean
*messaging*, never silent *enrollment*:
- The platform ships as **capabilities that wait to be invited**, not actions that
  fire on their own. Every threshold (pair a device, join a room, follow someone,
  publish) is crossed knowingly and explicitly.
- It must never turn *itself* on — no auto-enrolling a node into the mesh, no
  exposure, no social features flipped without consent. That would violate 0022
  (network = opt-in amplifier, never a push to expose), 0012 (per-room
  public/private), and the local-first/user-first stance — and it churns the trust
  the wedge depends on.
- We do not *hide* that it is a platform; docs disclose it. We simply do not
  *foreground* it in the hero. Lead lean, expand on invitation.

**The wedge must deliver standalone.** The horse only clears the gate because the
sidekick is genuinely useful on a lone, offline, private node (the
retrieval/librarian value, [[node-resident-librarian-flywheel]]). A thin shell
whose real message is "…but you must join a network" is an empty horse — users
bounce before the platform value lands. Implication: **over-invest in the
standalone sidekick being excellent**; the platform rides on earned adoption, not
coercion.

**README voice:** keep "plugin" + the sidekick hook (0005/0024); do not headline
"social network platform" but do not conceal it in the body.

## 0024 — Adopt cyber's social layers selectively: tiered trust + personal social-gravity lens + structural-importance ranking — no karma/token economy

**Date:** 2026-06-09 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, GUI (Messenger + Data Platter) · **Builds on:** 0007, 0011, 0012, 0018, 0022 · **Source:** `cyber-master/identity.md`, `cyber-master/rewards.md`

We reviewed four "social protocol" layers from cyber-master and applied the line
already set by 0022 (*borrow cyber's graph-structure math + identity model;
**reject** its public token/staking economy*) and 0012 (*no gamification*). Three
are adopted; one is rejected.

**Adopted:**

1. **Tiered trust (cyber `identity.md`'s A/B/C transport regime) — strongest fit.**
   Identity is one keypair per actor; the *authentication discipline* varies with
   the trust boundary crossed:
   - **Tier A — capability handle (intra-process):** our Cryptree capabilities
     (0011). Zero-cost; possession is authorization.
   - **Tier B — Secure-Enclave symmetric MAC (inter-process on a host):** new;
     machine-pinned session key.
   - **Tier C — signature (inter-host/WAN):** our Ed25519 signed shares today;
     post-quantum later. Only tier-C-authenticated records persist publicly.
   - **UI:** a "Trust Thermometer" in the Messenger labels each message *Local /
     Device / Global Signed*. This is **honest surfacing of real authentication
     strength, not gamification** — and the UI must never imply crypto not yet
     shipped (PQ sigs/Enclave MAC are later phases; today is A + Ed25519 C).

2. **Personal social-gravity lens (Data Platter) — adopted with a guardrail.**
   Weight tri-kernel gravity ([[cyber-retrieval-reference]]) by the user's *own*
   follow graph (`social.rs`, 0007) to cluster/brighten nodes: "things people *I*
   follow link to" move toward the center. **Guardrail:** it is strictly a
   personal, local lens — never a global score that ranks people. Relevance, not
   clout.

3. **Structural-importance feed ranking (Messenger) — the safe half of cyber's
   "social feed."** Rank messages by **graph-gravity / load-bearing-ness** (how
   many decisions/files a message ties together) — pure structural math already
   adopted in 0022. We do **not** frame this as "reputation," "social rank," or
   "hottest"; importance is structural, not popularity.

**Rejected — #2 Proof-of-Contribution / self-minting "karma".** This is not
adjacent to cyber's token economy, it *is* it: `rewards.md` defines karma as
accumulated φ*-weight and "self-minting" as minting **$CYB** via STARK proofs
(mint/burn/lock = stake). It collides with **both** 0022 (reject the token/staking
economy) and 0012 (no gamification). "Karma badges that auto-unmute you" is a
reputation currency. Excluded; revisiting it would mean deliberately reopening
0012 + 0022 as its own decision, not folding it in under "social features".

**Why this stays true to identity.** These three make *your own* trust and *your
own* graph legible to *you* (sidekick, private-first, local-first); they do not
turn the Concierge into a public clout network. Consistent with 0022's guardrail:
the network is an opt-in amplifier, never a push toward exposure or ranking.

## 0023 — Capture the model's reasoning ("why") inline with each step, not as its own node

**Date:** 2026-06-09 · **Status:** accepted · **Plans/Contract:** `ADAPTER_CONTRACT.md`, `crates/core/src/event.rs` · **Builds on:** 0002, 0009, 0011, 0022

Today the plugin captures only what the harness already records — the
**artifacts**: prompts, responses, tool calls/results, file refs, decisions
(the `Event` enum). Each of those steps usually has model **reasoning** behind it
(why this tool, why this decision) that is never captured. The *why* is exactly
the high-signal context retrieval wants, so capture it.

**Shape — reasoning rides along with the step, in the same record/CID.** Not a
separate "thinking" node per step. An optional `reasoning` object is carried on
the **`Envelope`** (next to `event_id`/`imported_from`), so it serializes into the
same IPLD record as the step it explains:

```
reasoning: { text: String, source: "thinking" | "inline_preamble" }
```

Why inline, not a node-per-thought:
- **One CID per step, not two.** Reasoning is an *annotation* of a step, not an
  independent memory object. A thinking-CID per step would ~double the graph with
  low-standalone-value nodes and more orphans, and clutter the records timeline.
- **It is the embedder's best signal (0022).** A bare `Bash`/`Edit` row carries
  little intent; the reasoning behind it does. Inlining means the librarian
  embeds/ranks the *why* together with the step — "why did we choose X" becomes
  answerable.

**Why this matters most for tool calls — tools are polysemous.** The same tool
(`Bash ls`, `Read`, `Edit`) is called for many different reasons; the tool name +
args do not reveal the *purpose*. The reasoning is the only thing that
disambiguates two otherwise-identical calls into distinct memories. The payoff is
not just recall but **efficiency**: once "this tool, for *this* purpose" is
retrievable, the model can reach for the exact tool for the exact function instead
of rediscovering it across wasted steps. Corollary: when several `tool_use`s share
one thinking block, attaching that same rationale to each is **intended, not
duplication to dedup away** — the rationale per call is the signal.

**Honesty (fidelity ladder, 0009).** `reasoning` is optional and additive: omit
it entirely when the harness exposes no thinking — never fabricated. `source`
stays honest — `thinking` (genuine reasoning/thinking tokens, e.g. Claude Code
extended-thinking blocks) vs. `inline_preamble` (rationale written inline before
acting); never label preamble as thinking. Backward-compatible: adapters that
never send it stay valid; the field is omitted from the wire when absent.

**Privacy = the step's own boundary; no stripping on share (0011).** Reasoning is
encrypted inside the step's Cryptree node and **travels with the step when
shared** — deliberately not stripped. Rationale (user, 2026-06-09): "I didn't
train the model; if I'm sharing the work there's nothing to hide, and stripping
reasoning degrades retrieval" — for the recipient exactly as much as for the
holder. This also keeps the design simple (no redaction path, no re-derived-CID
problem).

## 0022 — A node-resident retrieval librarian; active retrieval is now earned (refines the explicit-recall Core Principle)

**Date:** 2026-06-09 · **Status:** accepted · **Plans:** `PHASE_8_NODE_AI_PLAN.md` (implementation), `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md`, `CRYPTREE_PORT_SPEC.md` · **Builds on:** 0011, 0015 · **Refines:** Core Principle "recall is explicit, never an auto-context compiler"

A small model lives **on the user's own Kubo node** and acts as a *librarian*:
given a request, it traverses the IPLD DAG, follows CIDs, reasons about what to
fetch, and pulls blocks from peers on demand. Retrieval becomes an agent that
runs *where the data and the network are*, not stateless library-side plumbing
(numpy cosine + FTS) that runs wherever the host happens to be.

**Why this reverses the earlier reluctance to do retrieval/injection at all.**
The explicit-recall stance was never ideological — it was the absence of a
*load-bearing reason* to be an active retriever. There now is one, and it is a
**network-participation flywheel**, not a feature:

> librarian needs the node online to retrieve → the user keeps their node
> running for their *own daily benefit* → the node stays available to the mesh →
> more blocks pinned/reachable → the private network (0015) actually has live
> participants.

Node uptime stops being altruistic infrastructure the user has no reason to keep
up, and becomes **load-bearing for the user's own experience**. That is the
bootstrap answer to "why would anyone keep a p2p node running," and it is the
thing a SQLite-snapshot competitor (KausaMemory v2 — see `kausamemory-v2`
research note) structurally *cannot* copy without abandoning its storage model:
its IPFS is a backup sink, ours becomes the thing that makes memory good.

**The flywheel never forces a node public (local-first, user-first).** The
embedder and librarian work on **any Kubo node — public or private — and fully
offline**; nothing about retrieval quality depends on publishing. "Network
participation" here means *the node is running*, not *the content is public*: a
node can participate in the **private** capability-scoped mesh (0015) or stay
entirely local, and the librarian is identical either way. Public vs. private is
always the user's explicit choice (consistent with 0012's per-room posture and
the `publish-public` gate); the flywheel must never be read as nudging users to
expose data. The default is **local and private**; the network is an *opt-in
amplifier* of a value that already exists on a lone offline node.

**The contract — librarian-as-tool by default, librarian-as-agent opt-in.**
Two readings of "node-resident retrieval"; we pick deliberately so we don't
silently become the auto-context compiler the Core Principle warns against:
1. **Default — librarian-as-tool.** The host asks; the librarian answers
   *smartly*. Consistent with explicit recall: it improves the *answer*, not the
   *trigger*. This is the shipping default.
2. **Opt-in — librarian-as-agent.** Proactively surfaces/pre-fetches. This *is*
   active injection; it requires the write-back seam + harness-specific **trusted
   authority** (the MemoryOS "Ground Truth" lesson) and a story for
   noise/privacy/cost. Never on by default, never inferred.

So the Core Principle is **refined, not repealed**: capture stays universal,
recall stays explicit *by default*, and active retrieval is now a deliberately
earned, opt-in capability with a concrete justification.

**Retrieval ranking — graph gravity, not just vectors (cyber reference).**
The librarian must rank by graph structure, not vector similarity alone (the
KausaMemory / pure-NN trap). Reference: **cyber / cyberia-to**
(`~/Downloads/cyber-master`) — a content-addressed, IPFS-native knowledge graph
whose `analizer/` toolchain is a working prototype of the librarian's core:
- `trikernel.nu` — diffusion/springs/heat kernels over the link graph →
  **gravity/density** importance weights (PageRank-family). Borrow as the
  graph-importance signal, hybridized with vector NN.
- `context.nu` — **smart context packer**: score nodes by gravity/density, then
  **greedy-knapsack into a token budget**, with pinned-node override. This *is*
  the librarian's output stage; port the algorithm.
- **Reject** cyber's public token/staking economy (focus/gravity-as-currency, the
  blockchain) — it solves an adversarial-public-commons problem this project does
  not have (private-first, capability-encrypted, no token). Take only the
  domain-neutral graph-ranking + token-budget-packing math.

**Hard constraints this creates (design around early, not late):**
1. **The only on-node model is a small *embedding* model — no generative LLM on
   the node, ever (not even an optional tier).** Governing rule: **the plugin
   must not measurably affect the harness's or the machine's performance.** This
   is not a contention tradeoff to revisit — it falls out of the project's
   defining promise (*Universal*): it must run on whatever machine the user
   already has, *alongside* the host's model, and stay effectively invisible in
   resource use. So the on-node component is a **small embedder** (~100–140M,
   CPU-friendly, e.g. `nomic-embed-text-v1.5` with Matryoshka dims), run
   **background, batched, low-priority**. All generation/reasoning/synthesis is
   **deferred to the host's model**, which already exists — including room-thread
   synthesis (no local LLM to do it; if no host model, it simply doesn't run).
   **Never a full LLM on Kubo; never a "router" LLM either.** (KausaMemory
   sidesteps this by having no model at retrieval; our answer is "a tiny embedder,
   and nothing else.")
2. **Privacy.** The librarian reads decrypted content to be useful, so it must
   run inside the capability boundary (0011) — it unwraps only what the holder's
   capabilities already permit; it must never become a path that widens access
   beyond the Cryptree key graph. **The vector/embedding index is itself
   sensitive** (embeddings of decrypted content leak content): it must be
   capability-scoped, never a single global plaintext index across capabilities.
3. **Wake policy.** "When does it wake" is a first-class design question (cost +
   the active/passive contract above), not an afterthought.

**Conformance for `PHASE_8_NODE_AI_PLAN.md`** (it predates this decision and is
aligned to it): proactive `ContextSuggested` injection and auto-summary injection
are the **opt-in librarian-as-agent path** (write-back seam + trusted authority),
never the default; the LanceDB/embedding index is **capability-scoped**; ranking
is **vector + graph-gravity**, packed to a token budget; on-node embedding model
stays small to bound host contention.

## 0021 — OrbitDB is the reference implementation for the oplog/merge/room-write-authz, not just the Entry shape

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md` · **Builds on:** 0008, 0017, 0018, 0019

The vendored OrbitDB source (`~/Downloads/orbitdb-main`, `@orbitdb/core`) was
previously cited only for the `Message` **Entry shape** and the **(Lamport time,
then AgentID) total-order** rule. It is the closest existing implementation of this
project's *messaging-as-signed-append-only-log-over-IPFS with deterministic merge*
model. Adopt it as the **reference spec to port**, mapped per phase. It complements
atproto (0018: repo/MST/CAR/sync-transport/DID-key/lexicon); OrbitDB covers the
**oplog, CRDT-style merge, room write-authorization, and the eventlog/feed
primitive**.

- `src/oplog/entry.js` + `clock.js` → the `Message` Entry shape + Lamport clock
  (already adopted, 0008).
- `src/oplog/conflict-resolution.js` → the deterministic total-order sort (already
  adopted).
- `src/oplog/log.js` + `heads.js` → **Phase E** — the append-only Merkle-DAG log
  with **immutable union merge and concurrent-head preservation**; reference for the
  merge semantics (never last-writer-wins, preserve concurrent heads, deterministic
  conflict calculation).
- `src/sync.js` → **Phase D** — exchange-heads-then-converge; reference for signed
  head advertisement + block exchange and the convergence property.
- `src/access-controllers/` → **room write-authorization** — who may publish into a
  room: the AI-send/Human-only lever (0008) and the `message_send` capability (0018).
- `src/databases/events.js` (eventlog) → a message thread + the **social feed**
  primitive (0019).
- `src/storage/` (memory/lru/level/ipfs) → hot/warm/cold block tiers + day-CAR
  tiering (0014).
- `src/identities/` → the identity-signs-entry pattern → map onto ActorID/DeviceID
  signing; **use this project's identity hierarchy (0018), not OrbitDB's providers**.

**Hard caveats (reference, not dependency — same spirit as 0018):**
1. **JavaScript, not Rust.** Port the oplog/Entry/clock/conflict-resolution and the
   head-exchange shape; do **not** add OrbitDB as a dependency.
2. **OrbitDB syncs over *open* libp2p pubsub** (anyone on the topic). This project
   requires **capability-scoped, signed, encrypted-private** sync — borrow the
   head-exchange *shape*, replace open pubsub with membership/capability auth +
   encryption (Phase B/D, rules 3/4).
3. **Access-controllers gate writes but do not encrypt.** Combine OrbitDB's
   write-authz pattern with encrypt-to-capability private rooms (0011).
4. **Licensing.** OrbitDB is MIT — compatible with this workspace's
   `MIT OR Apache-2.0`. Attribute any directly translated algorithm.

## 0020 — Messaging is text-only; files shared by CID; feed media is display-only (not downloadable)

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `DATA_PLATTER_PRIVACY_LOCK_PLAN.md` · **Builds on:** 0008, 0012, 0017, 0018, 0019

Two constraints that shrink the messaging attack surface and keep all file movement
on the verified content-addressed path.

**1. Messages are text only — no file or media uploads through the messaging path.**
- A `Message` payload (0008) carries **text**. The Messenger has **no attach/upload**
  affordance for files, images, audio, or video.
- **Files are shared by CID**, not uploaded into a message. The actual bytes travel
  as verified content-addressed blocks via normal sync (Phase D, CID verification
  per 0018) and the existing share-by-CID path — never as a message attachment. A
  CID reference is itself text, so *linking* an already-stored file in a message is
  fine; the message still carries no blob.
- **Supersedes 0008's "large payload" message path.** 0008 described small text
  bundling its block while *large payloads send CID-first, fetch on demand* — that
  implied large content riding the message path. With text-only, a message is
  **always small text that bundles its block**; large content is **never a message
  payload** and only ever moves as a CID-referenced block over the file-share path.
- **Why (security):** it removes media/file parsers from the message ingest path.
  Untrusted image/video/archive decoders are a classic exploit surface; text is
  simple to validate and fuzz, which directly supports the internal security gate's
  parser-fuzzing requirement (0016) and rule 5 "verify everything received."

**2. The feed may display media but offers no download.**
- The feed panel (0019) may **render** media inline (e.g. an image preview in a feed
  card). It exposes **no download / save / export affordance** for that media.
- To actually obtain a file, the user goes through the **sanctioned CID/share/ingest
  path** with its review, CID verification, and egress guards (0012,
  `DATA_PLATTER_PRIVACY_LOCK_PLAN.md`) — never a feed button.

**Screenshots/recording are an explicit non-threat.** Capturing what you are
already authorized to view (screenshot, screen-record) is **fine and carries no
security risk** — this is **not** anti-capture/DRM, and we do not design against it.
A screenshot is a lossy, manual copy of pixels you could already see; it is not the
authoritative, CID-verifiable file/blob.

**So what "no download" is actually for:** keeping the feed from becoming a
**sanctioned one-click file-acquisition/egress channel** that bypasses the reviewed
CID/share path. Real file acquisition must flow through the content-addressed path
so **provenance, CID verification, and egress review** (0012,
`DATA_PLATTER_PRIVACY_LOCK_PLAN.md`) all apply. It is a routing/provenance choice,
**not** a cryptographic control, and must not be described as one.

**Threat-model rationale — this is the point, not a side effect.** The dominant
real-world attack on individual users of mainstream social platforms is **not**
protocol breakage; it is **malicious payloads delivered socially** — booby-trapped
attachments/downloads and "click here" ad/DM lures (phishing, drive-by, malware
files). These constraints structurally remove that surface:
- **Text-only messages** → a malicious actor cannot push a file/media attachment
  into your inbox at all.
- **Feed display-not-download** → no malicious *downloadable* content via the feed.
- **No ad network, no third-party embeds/scripts** (user-owned, decentralized) →
  "click here" ad payloads do not exist by construction.
- **Inert text rendering (requirement):** message and feed text is shown as **inert
  text** — no embedded HTML/script/iframe, and **no clickable links** (a clickable
  link is essentially an attachment — same one-click threat). A pasted URL is **not
  blocked or stripped** — it displays verbatim as plain text, just **never
  auto-linkified or clickable**. Visiting one requires the user to deliberately
  copy-paste it into an external browser, which is **explicitly their own risk**;
  the friction itself deters casual exploitation (and most users won't bother). The
  only *actionable* references in-app are **CIDs into the verified store**, never
  arbitrary outbound URLs.
- **Reach is gated by membership/capability** (0018) **for private rooms, DMs, and
  your personal feed**: a random malicious stranger cannot message you or inject into
  those in the first place — unlike platforms where anyone can DM anyone.

**Scope boundary (reconciles 0012).** The *payload* constraints — text-only, no
download, inert links — apply **universally, including public-square rooms**. The
*"who can reach you"* gating does **not**: public-square rooms are **open by default
and signed-but-not-encrypted** (0012), so they intentionally accept posts from
anyone, and **Sybil/spam there remains the acknowledged open frontier** (0012.3:
follow/allowlist, the Human-Only lever, petname web-of-trust, dedup — partial tools,
not a solved problem). So a public room's *feed* can still surface spam content; what
it can never do is deliver a malicious *attachment, download, or one-click link*.

Net: the spam-malicious-*content* (attachment/download/link) vector is closed
everywhere by *what the medium can carry*; the spam-*reach* vector is closed for
private rooms/DMs/personal feed by *who can reach you*, and stays an open frontier in
public rooms by design.

## 0019 — Social feed panel in the Messenger; adopt the atproto feed-generator skeleton/hydration pattern

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md` · **Builds on:** 0014, 0017, 0018

The Messenger tab gains a **social feed panel**. v1 is deliberately minimal —
surface **available rooms** and **collaboration ideas** — but the architecture is
chosen now for future algorithmic feeds. Reference: the vendored atproto **feed
generator** starter (`~/Downloads/feed-generator-main`).

**Why it fits the existing model.** A feed generator splits into three stages that
this project already has decided forms for:

1. **Index the stream** (`src/subscription.ts` reads the firehose → local SQLite) →
   this project's own sync stream (Phase D/F) indexed into the **disposable SQLite
   query cache from 0014.4** — never a source of truth.
2. **Algorithm emits a skeleton** (`src/algos/*` returns post URIs/CIDs + metadata,
   *not* content) → a feed is a **list of CIDs**, which is native to the
   content-addressed store. Keep `algos` as a **pluggable** extension point.
3. **Hydrate on demand** (client fetches full views by ID) → fetch the records from
   IPLD by CID. Same skeleton→hydrate split as semantic search.

**Scope order — global feed first, per-room feeds later (decided).** Early on there
are few rooms and users, so a **network-wide ("global") feed is the priority and
must work first**. Per-room feeds are committed but deferred — the pluggable-algo
shape keeps them additive (a per-room feed is just another algo with a room filter),
so deferring costs nothing architecturally. **"Global" here means network-wide
*within the actor's authorized scope*, not public/internet-wide:** there is no global
feed registry and no cross-network discovery (see caveat 2); the feed only ever
aggregates rooms/content the actor is already authorized to see.

**v1 scope (minimal, global):**
- A network-wide "rooms" algo: list rooms the actor is authorized to see (per
  membership/room capability), newest/most-active first.
- A network-wide "collaboration ideas" algo: surface idea/`Message`-type nodes
  flagged for discovery within the network.
- Render in a panel inside the Messenger tab; selecting an item opens the room/record.

**Later:** per-room feeds (same algo interface + a room-scope filter).

**Enabled/disabled toggle (local-first, non-negotiable).** The feed is an
**opt-in capability with a one-switch local toggle**, consistent with the
user/local-first decentralization posture (and mirroring the AI-send "Human-only"
lever in 0008 and the opt-in publish posture). Requirements:
- **Default on; off-able at any time**, per device — it is a local control, not
  synchronized network state. (Default-on is acceptable because feed participation is
  already scoped to *authorized network members*, never public/internet-wide — 0019
  caveat 2; turning it off is always one switch.)
- **When disabled, nothing runs and nothing leaves the device:** the feed indexer,
  the `algos`, and any feed-related network participation (advertising your
  discoverable nodes, answering feed queries, surfacing your rooms/ideas to others)
  all stop. The disposable feed index may be dropped; it is rebuildable.
- **The local Data Platter is fully functional with the feed off** — capture,
  store, graph/records, and direct messaging do not depend on it. The feed is
  additive discovery, never a dependency.
- **Disabling is prospective**, like revocation (0016/Phase G): it stops future
  participation; it does not retract content already shared with authorized peers.

**Caveats — port the pattern, not the topology (same spirit as 0018):**
1. **TypeScript, not Rust.** Port the skeleton/hydration split, the pluggable-algo
   shape, and the indexer→disposable-SQLite flow; do **not** add it as a dependency.
2. **No firehose-from-a-relay, no did:plc, no global feed registry.** atproto's
   feed is declared as a DID record, discovered globally, and authed by a JWT from
   the user's repo signing key. Here the stream is the **private-mesh sync** (Phase
   D/F), feeds are **scoped by membership certificate / room capability** (0018,
   not did:plc/JWT), and there is **no global discovery** — a feed only ever shows
   what the actor is authorized to see.
3. **Feed output is a derived view, never authoritative.** Like all 0014.4 indexes,
   it is rebuildable from the DAG.
4. **Licensing.** feed-generator is MIT, compatible with this workspace's
   `MIT OR Apache-2.0`. Attribute any directly translated algorithm.

**Relation to OrbitDB (0021).** Two complementary feed references, not a conflict:
the atproto feed-generator supplies the **service architecture** (indexer →
pluggable algos → skeleton → hydrate); OrbitDB's `databases/events.js` supplies the
**append-only eventlog primitive** that a feed/thread is built from. Use feed-gen for
the algo/index shape, OrbitDB for the underlying log type.

**Sequencing.** Rides on the Messenger milestone (0017) and the sync substrate
(Phase D/F). The minimal rooms/ideas feed can land as soon as room membership
(Phase B) + sync (Phase D) exist; richer algorithmic feeds are future work.

## 0018 — atproto is the reference implementation for repo/sync/identity/messaging, not just CAR

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md`

The vendored atproto source (`~/Downloads/atproto-main`, the full
`bluesky-social/atproto` monorepo) was previously cited only for `car.ts` (Phase 4
CAR format + `verifyIncomingCarBlocks`). It implements, in production, most of the
primitives Phase N and the messaging plane are designing from scratch. Adopt it as
the **reference spec to port**, mapped per phase:

- `packages/repo` (MST, signed commits, CAR) → Phase 4 + the commit/DAG model.
- `packages/sync` (`com.atproto.sync.*`: `getRepo`/`getBlocks`, `subscribeRepos`
  firehose, backfill) → **Phase D** signed-head + request-response block sync and
  **Phase F** offline/backfill. This is almost exactly the signed-head +
  bounded request-response + streaming design already written.
- `packages/identity` + `packages/did` (DID documents, key rotation, handle
  resolution) → **Phase A** identity hierarchy. **Caveat:** atproto leans on the
  semi-central **did:plc directory**; this project explicitly forbids a global
  identity registry, so port the DID *document/rotation* shape, **not** did:plc.
- `packages/crypto` (k256/p256 signing) → `crates/crypto`.
- `packages/lexicon` + `packages/lex` (schema definition + runtime validation) →
  the host-neutral `Event`/node schema contract and rule 5 "verify schema before
  import."
- `packages/xrpc` + `packages/xrpc-server` (typed request-response) → **Phase D**
  request-response protocol shape.
- `packages/pds` (Personal Data Server: hosts a repo, serves sync, store-and-
  forward) → the "reachable pin/relay node" store-and-forward in 0008 + **Phase F**.

**Hard caveats (why it's a reference, not a dependency):**
1. **TypeScript, not Rust.** This is a pure-Rust workspace — port the data
   structures, verification, and protocol shapes; do **not** add atproto as a dep.
2. **Topology divergence.** atproto is *federation with a semi-central PLC
   directory + relays/AppView*; this is *user-owned P2P with no global registry*.
   Borrow the repo/MST/CAR, signed commits, lexicon validation, sync
   request-response, and key-rotation patterns — **not** the centralized directory/
   relay/AppView model.
3. **Licensing.** atproto is MIT/Apache-2.0, compatible with this workspace's
   `MIT OR Apache-2.0`. Attribute any directly translated algorithm.

**Terminology reconciliation (applies to 0008, 0017, 0021).** Decision 0008
(2026-06-05) predates this identity hierarchy and says "**AgentID**" throughout.
Post-0018, that single term splits: message **authorship, signing, and the
total-order tiebreak** are by **ActorID** (the agent/harness/human client), falling
back to **DeviceID**; the long-lived **UserID** is *not* used for routine messages.
Private-thread encryption "to the recipient's AgentID key" (0008) means **encrypt to
the recipient's capability key per 0011**, not a monolithic per-agent key. Wherever
later entries say "AgentID," read it as **ActorID** unless the context is explicitly
device- or user-level.

## 0017 — Messenger functional milestone (send/receive), gated, not an implicit byproduct

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md` (Phase N), `SHIPPABLE_PLUGIN_PLAN.md`

The Messenger is currently a **read-only view** over `Message`-type nodes (0008).
No plan made "read-only → send/receive" an explicit deliverable — it was an
implicit byproduct of Phase N, gated by nothing, so it could silently slip. Make
it a named milestone with its own acceptance test.

**Definition — Messenger is "functional" when:**

1. A participant can **compose and send** a signed (private rooms: encrypted-to-
   recipient-AgentID-key per 0008) `Message` node that links its parent CID, and it
   appears in the recipient's Messenger.
2. Two users + their two agents in **one room** see the **identical thread order**
   on both Data Platters (the orbitdb total order: Lamport time, then ActorID).
3. The **AI-send lever works**: Human-only mutes all agents; per-agent mute and
   AI-on-mention behave; a muted agent still ingests/recalls (mute ≠ deafen).
4. **Offline delivery** lands via store-and-forward once the peer is reachable.

**Dependencies (this is why it lights up as part of Phase N):**
- substrate: Phase 5.7 content-addressed messaging + libp2p transport;
- **Phase B** — `message-private` capability + room membership policy;
- **Phase D** — block sync moves the message blocks between peers;
- **Phase E** — message-ordering merge rule for concurrent threads;
- **Phase F** — discovery + encrypted store-and-forward for offline delivery;
- public rooms: gossipsub (5.7) under the per-room posture (0012).

**Acceptance demo (added to Phase N gates):** two paired users, one shared room,
each with an agent — exchange messages that converge identically on both devices;
flip Human-only and confirm agents mute but keep recalling; take one device
offline, send, reconnect, confirm delivery. Until this passes, do not describe the
Messenger as functional.

## 0016 — Drop the external security audit as a ship gate (amends 0015)

**Date:** 2026-06-08 · **Status:** accepted · **Amends:** 0015

The **third-party external security audit** is removed as a ship gate for the
private multi-agent network. Rationale (owner): the project is open-sourced, so
community review is expected post-launch.

Retained and amended:

- **Keep the internal security gate** — it's in-code, low-cost, and is the actual
  safety net: threat model, identity/crypto self-review, parser/request-response
  fuzzing, malicious-peer + resource-exhaustion tests, secret-handling + log audit.
  Only the *external* audit is dropped.
- **Honest claims, not silent risk.** Ship with an explicit **"UNAUDITED — no
  third-party security review; community review welcome"** notice in the README and
  the network UI. Do **not** advertise the network as "audited," and do not oversell
  "secure/private." The network plan's rule "do not describe this as a secure
  private network until the security-review gate passes" is amended to: *until the
  internal security gate passes, shipped with the unaudited disclosure.*
- **Residual risk acknowledged.** Open-sourcing does not guarantee anyone actually
  audits it (e.g. Heartbleed lived in OpenSSL for ~2 years). This trades a
  calendar/cost dependency for real residual risk in a crypto/identity system that
  handles private memory and keys. Owner accepts.

## 0015 — Private multi-agent network is a ship prerequisite (reverses v1 out-of-scope)

**Date:** 2026-06-08 · **Status:** accepted · **Plans:** `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`, `SHIPPABLE_PLUGIN_PLAN.md` (Phase N)

The one-click install must **not** ship until `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`
is complete. This **reverses** Decision 0013's "out of scope for v1: the
`crates/net` p2p network" — the private multi-agent network and shared-memory sync
are now a launch requirement, not a fast-follow.

- **Gate:** the network plan's own **Release Gate** (Phases A–E for convergence,
  A–D for cryptographic privacy, F for real-internet networking) **and its
  Security Review Gate** (threat model, identity/crypto review, parser fuzzing,
  malicious-peer + resource-exhaustion tests, secret/log audit, external security
  review) must pass before the installer is published.
- **Critical path:** this becomes the dominant driver of the ship date — far larger
  than Phases A–D. Packaging (Phase D) may be built in parallel, but the *release*
  is gated on the network's gates.
- **Foundations already in tree** (starting points, not done): `crates/crypto`,
  `crates/core/src/identity.rs`/`messaging.rs`/`social.rs`, and `crates/net`
  (libp2p with relay/dcutr/identify).
- **Interactions:** the network's `MergeCheckpoint` (multi-parent) extends the
  checkpoint model that Phase A.5's per-UTC-day HAMT also touches — design them
  coherently. All new record types ride the Phase A in-process `mem` store.
- **The network plan's own "Explicitly Deferred" list stays deferred** (global
  identity registry, reputation/scores, automatic semantic conflict resolution,
  post-quantum migration, etc.).
- **Open lever (not yet decided):** whether ship requires the *full* plan A–H +
  external audit, or a graduated gate using the plan's own tiered claims (e.g.
  A–E/F now, defer some Phase G/H hardening/UX) to ship sooner with narrower
  advertised claims.

## 0014 — Storage rollup: per-UTC-day IPLD HAMT, tiered up year→month→day; SQLite/Tantivy as throwaway cache

**Date:** 2026-06-08 · **Status:** accepted · **Plan:** `SHIPPABLE_PLUGIN_PLAN.md` (Phase A.5)

The flat model — one name binding and one top-level graph node per event — does not
scale (already ~1,175 names; the forest was a ~4,000-node walk). CARs are
export/import artifacts, not per-interaction storage, so the fix is to coarsen the
*index* and tier the *storage*, not to stop writing CARs. Decisions:

1. **One mini-database per UTC day, indexed by an IPLD HAMT.** Day root → a HAMT
   (sharding content-addressed map, the IPFS/Filecoin idiom) keyed by event
   id/timestamp → event record CIDs. Stays DAG-CBOR/CID-addressed, so dedup,
   verifiable export, and the lock/encrypt/publish machinery all keep working.
   Hierarchy: `store → day → session → event` (session is a secondary index
   inside the day; session nodes already exist).

2. **Every node carries a UTC timestamp, not just sessions.** Records already have
   `created_at` (unix epoch = UTC); we now *require* it on every node kind —
   including index/synthetic nodes (day root, session index, HAMT internals) — and
   use it as the **day-bucket key** (which day-HAMT a node lands in) and surface it
   per node in the explorer. Day boundary is **UTC**.

3. **Granularity is preserved.** Every prompt, response, and tool_result stays its
   own full record; the HAMT only changes how you reach it (one extra drill-down
   level). Reading a session's prompts/responses/tools is unchanged.

4. **SQLite/Tantivy only as a throwaway query cache.** The authoritative store
   stays IPLD (HAMT-in-CAR per day). A derived, disposable index (SQLite for
   structured queries, Tantivy for full-text search) accelerates GUI search and is
   always rebuildable from the DAG — never a source of truth.

5. **Lifecycle.** Today = hot loose blocks, re-bind the day root per event (rides on
   the existing checkpoint + `keep_checkpoints`/GC retention). Day end = seal the
   day into a CAR, optionally GC the loose blocks. Browsing an archived day
   re-imports its CAR on demand (import path already exists).

6. **Graph declutter is an explicit driver, not just a side effect.** The DAG
   explorer fans the depth-1 ring around the platter, and today depth-1 is
   *sessions* — hundreds of spokes, which is the root cause of the crowded graph
   (the radial layout + left/right breakdown only mitigate it). The day tier makes
   the default depth-1 ring **days** (a handful) with sessions one level down, so
   the top view is bounded and you drill in. Note for whoever implements A.5:
   `drawGraph` in `crates/gui/src/index.html` currently hardcodes "depth-1 =
   session" (`byDepth.get(1)` and the walk-back-to-session-ancestor logic); these
   become "depth-1 = day" and must be updated when the tier lands.

**Timing:** lands with Phase A (the `mem` in-process rewrite of `binding.rs`), so the
storage layer isn't built twice.

7. **Storage layout — hybrid: dedup at rest, self-contained on export (decided).**
   The live local store is a **shared, content-addressed block store with full
   dedup** — each unique block stored once, and a day is the **per-day HAMT index**
   pointing at CIDs in that shared store (not a physical copy). A **self-contained
   CAR is generated only when sealing/exporting a day** (archival or handing a day
   off), bundling every block reachable from that day's root so the file is
   portable on its own. This is the IPFS/atproto idiom and matches this decision's
   "CARs are export/import artifacts, not per-interaction storage."
   - **Why dedup at rest matters here:** unlimited-size ingest + the ~4× blob
     storage amplification mean duplicating shared blobs per-day-CAR would balloon
     disk fast; dedup avoids it.
   - **Accepted cost:** the live store needs **reachability/refcount GC across days**
     (a block is collectable only when no day references it) — needed for retention
     anyway. Sealing old days to CARs is the natural offload path.

8. **Calendar tiering above the day: `store → year → month → day` (decided 2026-06-08).**
   The day tier (points 1–7) extends *upward* into a self-maintaining level-of-detail
   so the default graph stays bounded no matter how much history accrues. The
   canonical containment tree is `store → year → month → day → session → event`.
   - **Weeks are dropped as a structural tier.** A Sunday–Saturday week straddles
     month and year boundaries (e.g. Dec 29–Jan 4), so `day ⊂ week ⊂ month ⊂ year`
     is not a valid single containment chain — a day belongs to exactly one month
     *and* one year, but its week belongs to neither cleanly. We pick the calendar
     chain (year → month → day) because months/years are how long-term memory is
     actually navigated. If a week view is ever wanted it is a pure **display lens**
     over days, never a compaction boundary.
   - **Each tier is a HAMT/manifest node, same idiom as the day.** A month root links
     its day roots; a year root links its month roots. Stays DAG-CBOR/CID-addressed;
     no event is moved or merged — higher tiers are index nodes only (point 3 holds
     at every level).
   - **Sealing is calendar-deterministic (extends point 5).** Only the current
     day/month/year are *hot* (re-bound as events arrive). A bucket **seals** —
     becomes an immutable, exportable, pinnable CID — once its time window passes:
     a month when its last day is past, a year after Dec 31. No daemon; the seal
     trigger is purely the timestamp, so the whole hierarchy is rebuildable from the
     DAG (consistent with the throwaway-cache stance, point 4).
   - **Graph LOD (extends point 6).** The default depth-1 ring shows the **coarsest
     sealed tier** — years for old history — and expands down year → month → day →
     session → event on drill-in. So point 6's "depth-1 = day" is really "depth-1 =
     coarsest sealed tier"; `drawGraph`'s walk-back-to-ancestor logic must climb the
     calendar chain, not assume a fixed depth.

## 0013 — Distribution: a one-click installable app that auto-attaches to Claude Code

**Date:** 2026-06-07 · **Status:** accepted · **Plan:** `SHIPPABLE_PLUGIN_PLAN.md`

The plugin ships as a downloadable app a user installs with one click. It runs as
its own process with its own GUI but is *inert without a harness* — it captures and
explores a harness's activity. Launch harness is **Claude Code** (what it is
already mounted to: the store today is full of our own `claude-code` sessions,
captured from `~/.claude/projects/<project>/*.jsonl`). Four locked choices:

1. **Vendor `mem` as an in-process library (A1), not a bundled binary.** Move the
   `mem` source into the workspace and call its `store`/`dag`/`names` modules
   directly; rewrite `crates/core/src/binding.rs` off the subprocess. This is
   Decision 0001 finally cashed in ("the core eventually wants to link the
   Concierge V4 Rust crate directly"). It also deletes the bug classes that come
   from shelling out — `argv` overflow (`E2BIG`), stdin/stdout pipe deadlock,
   per-node spawn latency. This is the gate; nothing ships before it.

2. **Auto-attach watches the *whole* `~/.claude/projects`,** not just the current
   project — backfill the user's entire Claude Code history on first run, then a
   file-watcher keeps capture continuous. Ingest is content-addressed, so
   re-reading events deduplicates by CID (idempotent).

3. **Native window, not a browser tab.** Wrap the existing loopback GUI in a
   native webview (Tauri / wry+tao) so it feels like an app.

4. **Distribute via an install-script + GitHub Releases first**, native signed
   installers (`.dmg`/`.msi`/AppImage, with macOS notarization + Windows signing)
   as a fast-follow once an Apple Developer ID is in place.

**Out of scope for v1:** MCP write-back (`crates/mcp` stays a stub), multi-harness
beyond Claude Code, the `crates/net` p2p network, auto-update. IPFS/Kubo becomes
opt-in (core works fully offline).

## 0012 — Public-square posture: per-room public/private, no gamification, eyes open on the hard parts

**Date:** 2026-06-05 · **Status:** accepted

The North Star (plan §*North Star*; `Concierge_V4/PLATFORM_VISION.md`) is a decentralized
public square for human + AI problem-solving. Three governing choices:

1. **Per-room public/private posture — reconciles Decision 0011.** 0011 makes *personal*
   memory private-by-default (encrypt; public is opt-in). The public square is the inverse:
   **open by default** — nodes are **signed** (authenticity) + **content-addressed**
   (integrity) but **not encrypted**, because openness is the point and publishing is
   intentional (so there is no IPFS footgun). **Posture is chosen per room:** public-square
   rooms = signed-open; private/team rooms = capability-encrypted (0011). The plugin
   supports both; neither default is global.

2. **No gamification — merit by Merkle-DAG.** No tokens, points, badges, reputation, or
   class/role systems. Trust = cryptographic provenance (CID lineage + signatures), not
   social standing. Binding product principle: features that add gamified incentives or
   status hierarchies are **out of scope**. Consistent with the Filecoin decision
   (incentives may underwrite the provider network, but are never a product feature).

3. **The hard problems are acknowledged, not hand-waved.** A public square introduces
   problems a private memory layer never faces, and the no-verification stance makes them
   harder. The related-platforms landscape (Decidim/Polis/Loomio/Ushahidi/Zooniverse/OSM)
   confirms mature platforms solve them with **identity verification, human moderation, and
   institutional ground-truth — all rejected here**:
   - **Sybil / spam** with no reputation or gamification → partial tools: follow/allowlist,
     the Human-Only lever, petname web-of-trust, content dedup. An open frontier.
   - **Moderation** of harmful/illegal content on a permanent, content-addressed, open
     square (you can't un-publish; tombstones are local).
   - **Real-world effectiveness is an oracle problem** — the DAG proves provenance and
     computation, not field outcomes; ground truth needs trusted measurement.
   Recorded as the platform's real design frontier. **Polis-style opinion clustering** is
   the lead candidate for room-consensus (the Human-Only → Action-Mode trigger) without
   voting or reputation.

## 0011 — Privacy is capability-based access control (Cryptree — the Wuala design)

**Date:** 2026-06-05 · **Status:** accepted

The P2P layer's defaults are split — **transport strong, content public-by-default, metadata
hard** — so privacy must be designed in, not assumed. We adopt the **Wuala Cryptree** design,
a paper-backed cryptographic file-system design that solves exactly this on IPFS. (Full detail:
plan §*P2P Security & Privacy Model*.)

- **Encrypt before hashing.** Private content is encrypted, so the CID is of *ciphertext*;
  content-addressing is not encryption (IPFS is public by default). This makes "private"
  real regardless of who obtains the block.
- **Access control = capabilities, not server ACLs.** A capability = `{ target, read key,
  optional write key }`; holding the read key decrypts a node and walks *down* its subgraph
  (child keys wrapped under parents — Cryptree). Read/write are separate keys. No node is
  consulted for permission — you hold the capability or you can't decrypt.
- **Sharing hands over a capability**, not a bare CID (the Cryptree "secret link" = CID + key):
  fetch from public IPFS *and decrypt*. **Default private/capability; public is the explicit
  opt-in** — inverting the IPFS footgun.
- **Metadata defenses:** fixed-size padding (16/64/4096 B, per the Cryptree design), encrypted
  capabilities at rest, **blind/proxied social requests** (so a relay can't reconstruct the
  friendship graph), no public-DHT announcement of private CIDs, and a **private swarm (PSK,
  `LIBP2P_FORCE_PNET=1`)** for sensitive/team use.
- **Primitive suite (the Wuala Cryptree suite):** Ed25519 (sign), Curve25519 (box), Salsa20-Poly1305
  / NaCl (symmetric AEAD), Scrypt (login KDF), with an optional **Curve25519 + ML-KEM
  (Kyber) hybrid** for post-quantum. All have mature Rust crates.
- **Honest tradeoff:** transport is forward-secret (Noise), but durable content encrypted to
  a long-lived key is not — a later key theft decrypts stored history. Per use case:
  ephemeral chat → forward-secret session keys; durable artifact → encrypt-to-capability +
  keystore/rotation. (Our analysis, not a cited spec.)

**Proven-documentation anchor:** the Wuala Cryptree design — *Cryptree: A Folder Tree
Structure for Cryptographic File Systems* (Grolimund, Meisser, Schmid, Wattenhofer, 2006) —
with the real-world track record (including third-party penetration testing) of the Wuala
storage system that shipped it; primary protocol docs for the transport/content/PSK/gossipsub
claims (docs.libp2p.io, docs.ipfs.tech, libp2p/specs).
**We implement the Wuala Cryptree design + standard primitives, not any specific
implementation's code** (existing implementations are filesystem-shaped and often
server-assisted; ours is Rust + a memory/message DAG — a capability grants access to a
*subgraph*, not a directory subtree). Supersedes the ad-hoc "encrypt payload to AgentID key"
note in [0008].

## 0010 — Facts evolve by supersession, never mutation/deletion (bi-temporal, à la Graphiti)

**Date:** 2026-06-05 · **Status:** accepted

Memory that changes over time must not be edited or deleted — that would break
content-addressing and erase history. Adopting **Graphiti**'s temporal model, adapted to
our immutable DAG:

- A changed fact is a **new immutable node** that **`supersedes`** the prior one (a
  closed-vocabulary temporal edge); the old node is **retained**. Tombstones remain for
  *redaction*, not for *change*.
- Facts carry **bi-temporal** windows: `valid_from` / `valid_to` (when the fact was true)
  distinct from the ingestion timestamp (when we learned it). Recall returns the current
  fact (open `valid_to`) by default; the graph is **time-travel queryable** by walking
  the `supersedes` chain.
- **Provenance is mandatory:** raw captured nodes (`Prompt`/`Response`/`ToolResult`) are
  **episodes** (ground truth); every **derived** node (`Memory`/`Decision`/entity) links
  back to the episode CID(s) that produced it. Composes with `imported_from` (Phase 2.5)
  and `UsedAsContext` (recall).

**Why borrow only this, not Graphiti itself:** Graphiti is a property-graph + LLM-extraction
memory layer (Neo4j/Python) — the same category we differentiate from (no CID/CAR/P2P/
verifiability). We take its **bi-temporal supersession** and **episode/provenance** *models*
(which fit our append-only content-addressed DAG better than they fit a mutable graph DB),
and keep our own substrate + closed-vocabulary schema (not its learned/emergent ontology).
Its hybrid retrieval (semantic + BM25 + graph traversal) also validates the
RETRIEVAL_LAYER_PLAN cascade.

## 0009 — Universal = one event contract + a fidelity ladder (not a universal API)

**Date:** 2026-06-05 · **Status:** accepted

There is **no universal harness API**, and the plugin must not pretend one exists.
"Mount onto any harness" is delivered by putting universality on **our** side — every
adapter emits the same host-neutral `Event` (Decision 0002) into one ingest path — and
giving each harness a **ladder of ways in**, at least one rung of which every harness
supports:

- **Tier 0 — native plugin/adapter** (needs a plugin API; e.g. Hermes `BasePlatformAdapter`)
- **Tier 1 — lifecycle hooks** (e.g. Claude Code `UserPromptSubmit`/`PostToolUse`/`Stop`)
- **Tier 1 — skill / instruction file** (Codex skill, Factory/Mux `AGENTS.md`, Cursor rules)
- **Tier 2 — MCP server** (any MCP client; deferred Phase 3)
- **Tier 3 — model-API proxy** (needs *nothing*; captures at the model HTTP wire)
- **Tier 4 — log/transcript tailing** (needs *nothing*; ideal for Phase 2.5 backfill)

Tiers 3–4 are a **zero-cooperation floor** — worst case, capture from *outside* the
harness — which is why "works on anything" is true rather than hopeful. Higher rungs are
fidelity upgrades. This is a **solved, industry-standard pattern**: **beads** ships it as
`bd setup <tool>` recipes with **full vs. minimal template profiles** (full reference for
instruction-file harnesses; thin install + `bd prime` runtime injection for hook-enabled
ones — a model we adopt); **MemoryOS** is a Tier-0 Hermes hook plugin; **mem0** ships
SDK + MCP + plugin hooks.

**Two corollaries on the recall side:**

1. **Capture is universal; injection is not.** Pushing recalled memory *back into* a
   harness needs a write-back seam (injecting hook / MCP tool / proxy rewrite). Consistent
   with "recall is explicit, never an auto-context compiler" (Core Principle).
2. **Trusted injection needs harness-specific *authority*, granted explicitly.** Per the
   **MemoryOS "Ground Truth"** lesson, injected memory the agent isn't told to trust gets
   ignored ("memory-zero behavior" — re-discovering what's already in the prompt); the
   authority mechanism is harness-specific (Hermes `SOUL.md`/`rulebook.md`). Per the
   **beads policy-profile** lesson, authority is **never inferred** ("does not infer
   authority merely because a remote exists") — it is granted explicitly.

## 0008 — Content-addressed messaging & the shared brainstorm (and an AI-send lever)

**Date:** 2026-06-05 · **Status:** accepted

Participants — **humans and AIs alike** — communicate by exchanging **CIDs, not
payloads**. A message is a **signed (optionally encrypted) IPLD node** that links
its **parent message's CID**, so a thread *is* a Merkle-DAG: tamper-evident,
portable (export as CAR), and shareable by handing over the head CID. The
"messenger" is **a view over `Message`-type nodes**, threaded by parent links —
not a separate app. This unifies cleanly: a message is just a memory node, so the
conversation is part of the memory graph (recall it, link it to the file/decision
it's about, share the thread like any subgraph).

**Cross-harness / cross-model by construction:** because every participant speaks
the same host-neutral `Event` + signed-node contract, A's Claude-in-harness-X and
B's GPT-in-harness-Y are **co-equal** without either adopting the other's stack.
Two humans + their two working agents can brainstorm in one room.

**Verified positioning (mid-2026 landscape check):** cross-vendor *agent-to-agent
interop* is **solved and mainstream** — A2A has 150+ orgs and native cloud support
— so that is **not** the novelty. But A2A is **deliberately memory-less** (agents
collaborate "even when they don't share memory, tools and context"; HTTP/JSON-RPC
task RPC with *ephemeral* task storage). AGNTCY is content-addressed but for an
*agent directory/identity*, not shared conversation, and is complementary infra,
not a consumer product. Shared *persistent* agent memory is active **research**
(SAMEP, DecentMem), not a shipped product. **The wedge:** the industry won interop
by making agents memory-less strangers that RPC and forget; our medium is the
opposite — **the shared, persistent, content-addressed *memory* is the channel**,
multi-user and P2P, and the brainstorm becomes one verifiable artifact all parties
keep. Novel claim is *that*, not "different-vendor agents can talk."

**Durable moat (don't lead with "we have memory"):** A2A-style memory/context
*extensions* are an active area — a big player could close the bare "shared memory"
gap. So the defensible position is **not** "we added memory to agents"; it is the
**content-addressed + P2P + user-owned + portable-artifact** combination, which is
architecturally *against the grain* of centralized-cloud agent platforms. The moat
is the substrate (CID-verifiable, no platform owns the room, exports as a CAR all
parties keep), not the feature. Lead with that.

**The honest mechanics** (from the design discussion): small text **bundles its
block** with the CID (instant); large payloads send **CID-first, fetch on demand**
(dedup is the real bandwidth win). **Offline delivery** needs store-and-forward on
a reachable pin/relay node. Concurrent messages **fork the DAG** → resolved by a
total-order rule adopted from **orbitdb** — **(Lamport clock time, then AgentID as
tiebreak), guaranteed never a tie** — so every client renders the same thread.
**Privacy:** content-addressing is not encryption — private threads
**encrypt the payload to the recipient's AgentID key** before hashing.

**The AI-send lever (required control):** participation is **governed**. A
per-room and per-participant policy controls *who may publish*. A one-switch
**"Human-only" mode mutes all AIs** so only users send; granular **per-agent mute**
and an **"AI-on-mention"** mode sit between. Crucially, **mute ≠ deafen** — a muted
AI still *observes and recalls*, so it stays caught up and can resume speaking the
instant it's unmuted. Humans stay in control of when AI speaks. Default is
configurable; the Human-only switch is always one toggle. This is consistent with
the project's "recall is explicit, never an auto-context compiler" stance.

**Grounded in reference implementations** (studied 2026-06-05): the `Message` node
adopts **orbitdb**'s `Entry` shape (`id`/`payload`/`next[]`/`refs[]`/`clock`/
`key`/`sig`, dag-cbor+sha256→CID, verify-by-re-encode) and its fork-ordering rule;
the transport mirrors **universal-connectivity** `rust-peer` (signed gossipsub +
request-response codec; dcutr/relay/autonat as `Toggle<>`); CAR sync mirrors
**atproto** `car.ts` (`verifyIncomingCarBlocks`). All speak `dag-cbor+sha256→CID`,
the same dialect as `mem`, so integration risk is low.

## 0007 — Every plugin install has a stable, persisted identity (AgentID)

**Date:** 2026-06-05 · **Status:** accepted

The plugin is meant to be **social**: installs share with each other and need to
recognize one another across time. That requires separating two things the word
"node" conflates:

- a **content node** is a **CID** — permanent, globally unique, but it identifies
  *what* was shared, not *who* shared it (it changes whenever content changes);
- a **peer/agent node** is *this install*, which needs its own stable identity.

**The decision:** generate an **Ed25519 keypair once at `init`, persist it in the
store (outside the DAG), and reuse it on every start** — yielding a stable
**AgentID** (public key) that survives shutdown/restart. This is deliberate
because the default libp2p behavior is to *regenerate* an identity, which would
make an install a different participant after every reboot. We pin it down so a
node that goes down and comes back up tomorrow is the *same* social node.

**Sharing is signed, not just addressed.** A shared node is **signed by the
AgentID** (authenticity — *who*) on top of being **addressed by its CID**
(integrity — *what*). Content-addressing alone is anonymous; the signature is
what makes a share attributable and trustworthy between people.

**Human-friendly without a central authority:** global identity is the public key;
locally, users assign **petnames** (nicknames) to other AgentIDs. No name server
is required (an optional global registry can come later). A signed mutable pointer
(IPNS-style name) gives "my latest shares" a stable resolvable address so a shared
node can *appear* on a follower's graph instead of being re-sent each time.

Keeps the architecture honest: the identity key lives in config/keystore, **never
in the DAG** (consistent with the Security Model); signatures are additive
metadata on share, not a new write path.

## 0006 — Backfill of existing memory is a first-class feature

**Date:** 2026-06-05 · **Status:** accepted

The plugin must be useful the moment it mounts, not after it has watched a
harness long enough to accumulate a graph. So **importing the memory a user
already has** — transcripts, notes, an existing memory store — into the IPLD
graph is a first-class capability (Phase 2.5), not an afterthought. It is most of
what makes the plugin feel powerful: mount it and your existing history becomes a
verifiable DAG you can open immediately, instead of an empty one you wait to fill.

**How (keeps the architecture clean):** a backfill **importer** is a one-shot
adapter that *reads an existing store read-only and emits host-neutral `Event`s*
— the same contract live adapters use (Decision 0002) — so it reuses the ingest
path with **no new write path**. Importers are per-source and strippable.

**Discipline:** read-only against the source; `imported_from` provenance on every
backfilled record (so import vs. live is honest); idempotent via content
addressing + deterministic source-id mapping; **explicit / opt-in** command with
`--dry-run` — never an automatic disk vacuum. First targets: transcript/JSONL,
markdown folders, and CAR copy of an existing `mem` store; mem0 / Zep / Letta
exports later.

## 0005 — Brand voice & slogan

**Date:** 2026-06-02 · **Status:** accepted · **Amended:** 2026-06-09 (positioning line)

**Slogan:** _The Concierge — Serving up Data on a Silver Platter._

This is the personality hook (README hero, landing pages), distinct from the
**positioning line**, which does the precise, engineer-facing explaining. Keep
both; they have different jobs. The slogan hooks and is repeated out loud; the
positioning line carries the meaning.

**Positioning line (amended 2026-06-09):** _"A small sidekick to your large
model."_ — replaces the original _"Mountable IPLD memory for AI agents."_ Why the
change: once the node carries a tiny on-node embedder (Decision 0022), the plugin
stops being a passive *place to put memory* and becomes a *functioning* substrate
that ranks and serves the right context to the host's model on demand — "a small,
performance-invisible addition that compounds into a large help to whatever model
you bring." The sidekick line captures that active, model-helping role (and the
sidekick-vs-hero boundary: the host's large model thinks, the Concierge knows
where everything is); "mountable IPLD memory" undersold it as storage. The
engineer-facing precision ("portable, content-addressed IPLD memory layer") still
lives in the README body.

**Why "Data" and not "Memory":** phonetics win in a slogan. _Da-tah_ and
_plat-ter_ share the open final _-ah_ and a hard _-t-_, so the line rolls and is
memorable. _Memory_ is semantically more accurate but ends on a weak _-ee_ that
thuds against _platter_. The trade — a little semantic specificity for sound — is
correct in this slot because the positioning line underneath supplies the
precision. Voice: warm and human, against a backdrop of cold infra language.

**Why "Data" and not "Memory":** phonetics win in a slogan. _Da-tah_ and
_plat-ter_ share the open final _-ah_ and a hard _-t-_, so the line rolls and is
memorable. _Memory_ is semantically more accurate but ends on a weak _-ee_ that
thuds against _platter_. The trade — a little semantic specificity for sound — is
correct in this slot because the positioning line underneath supplies the
precision. Voice: warm and human, against a backdrop of cold infra language.

## 0004 — MCP server is built last

**Date:** 2026-06-02 · **Status:** accepted

The MCP server (Layer 4 / Phase 3) is deferred to the very end. It is the same
`Plugin` API exposed over a different transport, so it is cheap once the local
API is proven — but it is not the priority and is nowhere near ready. The
`crates/mcp` crate exists as a stub only to keep the module boundary explicit.
Contract draft: `CONCIERGE_MCP.md`.

## 0003 — Monorepo (Cargo workspace) for now

**Date:** 2026-06-02 · **Status:** accepted (revisit later)

Core, adapters, and MCP live in one Cargo workspace. While the host-neutral
`Event` contract and the core API are still changing (Phases 0–2), keeping them
in lockstep in one repo is worth more than clean release boundaries. We can split
adapters into separate packages once the contracts stabilize.

## 0002 — JSONL event boundary is the public contract

**Date:** 2026-06-02 · **Status:** accepted

The boundary between a harness and the plugin is a language-agnostic stream of
host-neutral events (JSONL: one object per line). This is what makes a Rust core
usable from a Python (or any-language) harness without FFI — the harness only
emits JSON. The contract is `ADAPTER_CONTRACT.md`, mirrored by
`crates/core/src/event.rs`. The generic JSONL adapter is built early (Phase 2).

## 0001 — Pure Rust for the first implementation

**Date:** 2026-06-02 · **Status:** accepted

The core binding is Rust-only to start.

**Why:** the core eventually wants to link the Concierge V4 Rust crate directly,
and deterministic DAG-CBOR encoding + CARv1 + CID golden tests are first-class
concerns where Rust keeps fidelity tightest. Single binary, easy to distribute.

**Tradeoff:** adapter authors in Python/TS shell out to the binary or emit JSONL
rather than calling in-process. For the JSONL path (the Phase 2 priority) there
is no penalty. In-process Python bindings via PyO3/maturin remain a possible
later optimization, not a requirement.

**Open input that could revisit this:** what language Concierge V4 / `mem` and
the first harness target (Hermes) are written in. If `mem` is Rust, this is
clearly right.
