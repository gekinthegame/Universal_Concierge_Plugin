# MCP Router — Built Into Every Node (The Universal Router)

*The `crates/mcp` module is not just an endpoint — it is the **MCP Router** that
ships inside every Concierge node. When you deploy your Kubo node, the Librarian
and the Router deploy with it as one local stack. The harness connects to your
router; the router gives it access to all your MCP servers at once, plus your
node's own tools — and **retains none of the traffic**.*

---

## The node stack (what deploys together)

A Concierge node is a three-part **local** stack that comes up as a unit:

1. **Kubo** — storage + network transport (the content-addressed substrate).
2. **Librarian** — retrieval over your local memory (Phase 8); the *only* model on
   the node is the small embedder — no generative model, ever (Decision 0022).
3. **Router** — the MCP multiplexer: the single MCP server your harness connects
   to, aggregating your external servers + your node's own tools.

The plugin **is** this bundle. There is **no central seed node**: every user's
router runs on their own machine. The man-in-the-middle is *yourself*, locally —
which collapses the entire trust surface (credentials, tool I/O) down to your own
box. A hosted seed would have re-created a single point of failure and a credential
concentration; per-node built-in avoids both.

---

## The real problem (corrected premise)

Multiple MCP servers are **already supported** by the major harnesses — Claude
Code (`claude mcp add` / `.mcp.json`), Cursor (`.cursor/mcp.json`), Claude Desktop
(`claude_desktop_config.json`), VS Code, and others all let you configure several
servers at once. So the friction is **not** "harnesses only allow one server." The
friction is:

- **Per-harness reconfiguration** — every harness has its own config file and
  format; using Claude Code *and* Cursor *and* Desktop means setting your servers
  up three times, three ways.
- **No portability** — your server set does not follow you across harnesses or
  machines.
- **Manual lifecycle + secrets** — spawning/monitoring stdio servers and managing
  their credentials is repeated, by hand, in each harness.

The router solves **consolidation + portability**: configure your servers **once**,
on your node; every harness just points at the one local router; your set follows
your node. (And your node's own tools — like Librarian retrieval — come along for
free, through the same single seam.)

---

## Architecture

1. **One connection.** The harness points to exactly one MCP server: your **local
   router** (loopback). One config line per harness, then never again.
2. **The remotes.** The router reads `.concierge/mcp_remotes.toml` defining your
   external MCP servers (GitHub, Google, Postgres, …): transport (stdio/HTTP/SSE),
   command/URL, and where credentials come from.
3. **Aggregation.** On `tools/list`, the router acts as an MCP *client*, queries
   every configured remote, and merges their tools with the node's **internal**
   tools (Librarian `concierge.retrieve`, room messaging) into one unified list.
4. **Opaque routing.** On a tool call, the router parses **only the JSON-RPC
   envelope** — `method`, `id`, and the tool name — to pick the target. It proxies
   `params` and `result` through as **opaque bytes**. It does not inspect, scan, or
   store payloads. Persist nothing.
5. **Inert until enabled.** Ships with every node but costs nothing until you turn a
   server on — it simply exposes the local tools. Pointing at no external servers is a
   normal, fine state (same "not set up is fine" stance as Phase B).

---

## Preconfigured catalog (top servers, baked in, easy to change)

A fresh node ships with a **curated seed of ~6 top MCP servers** (one best-in-class
per common category), so onboarding is "flip on + authenticate," not "hand-author
TOML." Six, not ten: every shipped entry is a recommendation we version-pin and
maintain, so the seed covers only servers a majority will actually turn on — the long
tail is the user's job, which is why editing is easy.

**The seed (one best-in-class per category):**

| Category | Server | Source |
|---|---|---|
| Code host | **GitHub** | first-party (GitHub) |
| Database | **PostgreSQL** | reference/community |
| Web access | **Fetch / Brave Search** | official reference / Brave |
| Errors | **Sentry** | first-party (Sentry) |
| Reasoning | **Sequential Thinking** | official reference |
| Browser automation | **Playwright** | first-party (Microsoft) |

**Bench** (documented one-click swap candidates as the rankings shift): **Notion,
Figma, Slack, Linear, Stripe.** *(Swap Playwright→Notion if broadening past pure
coding.)*

**Curation rules:**
- **Don't bake in capabilities harnesses already have natively.** The router serves
  *multiple* harnesses, and most now have built-in file access — so a **Filesystem**
  server (and likewise a basic shell/terminal server) is dead weight and is
  deliberately **excluded**. Seed only servers that add something the harness can't
  already do.
- **Don't bake in a memory/knowledge-graph server.** The node *is* the memory layer
  (the Librarian); a competing memory MCP would muddy the product identity.
  Deliberately excluded.
- **Tag every entry `official | first-party | community`.** Official-reference and
  vendor-first-party servers are the lower-risk default; the tag tells the user what
  they're choosing to spawn (supply-chain honesty).
- **Audience-fit.** The seed leans to the Claude Code / dev workflow (the initial
  audience); the bench broadens to productivity/comms.

The top-server set churns; the seed is a starting point, not a fixed list — which is
why the "easy to change" mechanics below are first-class, not an afterthought.

Shape of each catalog entry:

- **Defined, not live.** The catalog ships each server's *definition* — transport
  (stdio/HTTP/SSE), command/URL, version, and **which credential it needs** — with
  every external server **OFF by default**. Enabling one is the explicit act that
  spawns it. The only tools live on a fresh node are the node's **own** internal
  tools (Librarian, rooms), which need no external connection or secret.
- **No shipped credentials.** The catalog never contains secrets — only the *shape*
  and the name of the token it requires. The user supplies their own, stored
  per-node, never in the DAG.
- **Baked into the binary, not fetched.** The default catalog is compiled in (like
  the GUI is `include_str!`-baked) — **no phone-home, no central directory we host**,
  preserving decentralization. New/updated entries ride plugin updates.
- **Version-pinned + labeled "community server."** A curated list that spawns
  third-party code is a supply-chain surface: entries are pinned and clearly marked
  as third-party. We curate; the user consents by enabling.
- **Easy to change (first-class).** The top-server set churns, so editing is a
  product feature, not file surgery:
  - The baked catalog is a **seed**, overlaid by the user's
    `.concierge/mcp_remotes.toml`. User edits (enable/disable, version swap, add
    custom, remove) override the seed and are **preserved across plugin updates**
    that refresh it.
  - A **GUI "Servers" panel** in the Data Platter: list the catalog, toggle each
    on/off, paste the required credential, or "+ Add custom." This is the primary way
    to manage servers — no TOML editing required.

---

## Non-negotiables (inherited from the project's posture)

- **No ingest. No panopticon.** The router is a switchboard, not a vault. It does
  **not** capture passing data into the DAG. *(Optional, future, off-by-default:
  an explicit per-tool "save this tool's results to my memory" toggle — capture
  only ever happens when deliberately invited, consistent with the rest of the
  product. Not built by default.)*
- **Local & private.** Remote configs and credentials live per-node, on the user's
  own machine. Nothing about your server set or your tokens leaves your box.
- **Performance-invisible** (Decision 0022 / sidekick posture). Opaque proxy, no
  payload parsing, async forwarding — the router adds switchboard latency only. The
  Librarian stays low-priority/batched. Three local processes (Kubo + Librarian +
  Router) must stay invisible alongside the host's large model.
- **Graceful degradation.** A slow or down remote drops to "not available" and is
  omitted/flagged on `tools/list` — it never breaks the rest of the toolset (the
  opt-in / "not set up" pattern from Phase B / Decision 0027).
- **Namespacing.** Tools are prefixed by remote (`github__read_file`) so routing is
  unambiguous when two servers expose the same tool name.

---

## Implementation plan for `crates/mcp`  ·  size: M–L

1. **Server side.** Expose one MCP server (stdio + HTTP/SSE) for the harness to
   connect to. This is "the same `Plugin` API over a different transport," so it
   reuses the proven core binding.
2. **Client side.** An async JSON-RPC client that speaks stdio + HTTP/SSE to remote
   MCP servers, including capability negotiation, notifications, and cancellation.
3. **The Registry.** A parser for `.concierge/mcp_remotes.toml` that manages the
   lifecycle (spawn / monitor / restart / kill) of stdio remote subprocesses and
   resolves their credentials from the environment / secret store (never the DAG).
4. **The Router.** Middleware mapping `tool_name → target_server`, with JSON-RPC
   `id` translation and async proxying. Envelope-only parsing; payloads pass through
   opaque. Per-remote health tracking for graceful degradation.
5. **Internal tools.** Surface the node's own tools (`concierge.retrieve` from the
   Librarian, room messaging) as first-class entries in the aggregated `tools/list`,
   so the harness reaches the Librarian through the same single seam.
6. **The baked catalog + overlay.** Compile the ~6-server seed into the binary
   (`include_str!`-style), apply it under the user's `.concierge/mcp_remotes.toml`
   overlay (user edits win and survive seed refreshes), and tag each entry
   `official | first-party | community`. Every external entry defaults to **off**.
7. **The "Servers" GUI panel.** A Data Platter view to list the catalog, toggle each
   server on/off, paste its required credential, and "+ Add custom" — the primary,
   no-TOML way to manage the set as the top-server list churns.

*Later, optional:* a shared/recommended server list or peer-shared configs — a
deliberate opt-in layer, never the default (configs stay per-node and private).

---

## Strategic fit

- **Decentralized.** Per-node, no central seed: no single point of failure, no
  credential concentration. The router is yours, on your machine.
- **Trojan horse (Decision 0025).** One seam the harness already connects to;
  capabilities expand behind it. Standalone-useful from day one (consolidation +
  portability) before any network or memory feature is invoked.
- **Makes the Librarian reachable.** Retrieval is just another tool in the router's
  list — no separate integration for the harness to learn.
- **Node uptime becomes concrete (Decision 0022 flywheel).** "Participation" stops
  being abstract: your tools and memory live only while your node is up, so you keep
  it up — and that same uptime is what a future network rides on.

## Relationship to other phases
- **Rides on Phase A** (in-process `mem` store) and **Phase 8** (the Librarian whose
  retrieval tool the router exposes).
- **Re-uses Phase B / 0027** degradation posture for down remotes.
- **Does not** depend on Phase N (network); the router is purely local. Tool/resource
  contract continues in `CONCIERGE_MCP.md`.
