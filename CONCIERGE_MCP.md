# Concierge MCP Server

Expose the Concierge's memory + site-building over the **Model Context Protocol**
so any MCP-aware host (Claude Code, Cursor, …) can drive it with **zero custom
adapter code**. The capture adapters (`adapters/`, Decision 0009) only *read* a
harness's transcript; **MCP is the write/action surface** — the channel through
which the host AI actually *uses* the Concierge.

- **Implemented** in [`crates/mcp`](./crates/mcp) — JSON-RPC 2.0 over **stdio**.
- **Protocol version:** `2025-11-25` (per the MCP spec).
- **Run it:** `concierge-plugin mcp serve` (stdout = JSON-RPC only; logs go to stderr).

## Two safety rules (baked in)

1. **Write tools are off by default, toggled in the GUI.** The server starts
   **read-only** — write tools are not even listed, and a call to one is rejected.
   The user enables them with the **Studio → "AI writes" toggle** (Decision 0028:
   write-enabled is opt-in). The toggle is a persistent store setting the server
   **re-reads on every request**, so flipping it takes effect on the AI's next call
   — no re-registration. (`--write` is a dev *force-override* that ignores the
   toggle; don't register a harness with it — let the user opt in.)
2. **No tool publishes or egresses.** `concierge.write_site` *stages* a draft the
   user previews and publishes from the GUI — **publishing stays the user's
   explicit, password-gated act** (Decision 0026). The AI prepares; the user ships.

## Registering it with a harness

### Claude Code
```sh
# user scope = available in every project. NO --write: read-only until the user
# enables write tools with the Studio "AI writes" toggle.
claude mcp add --scope user --transport stdio concierge -- \
  /ABS/PATH/concierge-plugin mcp serve

# verify:
claude mcp get concierge          # → Status: ✔ Connected
# remove:
claude mcp remove concierge -s user
```
MCP tools load at **session start** — start a new `claude` session for the
`concierge.*` tools to appear. The server's store is `$HOME/.concierge` (the
`workdir` default), independent of the harness's working directory.

### Cursor (and other MCP hosts)
Add to the host's MCP config (Cursor: `~/.cursor/mcp.json` or project `.mcp.json`):
```json
{
  "mcpServers": {
    "concierge": {
      "type": "stdio",
      "command": "/ABS/PATH/concierge-plugin",
      "args": ["mcp", "serve"]
    }
  }
}
```
No `--write` — write tools stay off until the user enables them with the Studio
"AI writes" toggle.

## Tools (implemented)

Read tools are always available; **write tools require `--write`**.

| Tool | Args | What it does |
|---|---|---|
| `concierge.recall` | `name` | Resolve a bound name + fetch its record. |
| `concierge.resolve` | `name` | Resolve a bound name → content id (CID). |
| `concierge.get` | `cid` | Fetch a record by CID. |
| `concierge.retrieve` | `query`, `budget?` | **Semantic search** over memory (ranked by meaning × graph importance × recency; token-budgeted). Find context by topic, not exact name. |
| `concierge.design_guide` | `topic?` | **Design knowledge** (the bundled Impeccable skill): `typography` · `color` · `spacing` · `motion` · `interaction` · `responsive` · `writing` · `critique` (omit → overview). Load before building UI/media. |
| `concierge.design_audit` | `site_name?` | **Deterministic design audit** of a staged site's `index.html` — flags AI-slop tells (overused fonts, gradient text, AI palette, side-tab borders, gray-on-color, flat hierarchy, monotonous spacing, bounce easing, buzzwords…). Advisory. |
| `concierge.put_node` *(write)* | `kind`, `fields_json` | Store a memory node → CID. |
| `concierge.put_blob` *(write)* | `text`, `media_type` | Store a text blob → CID. |
| `concierge.bind` *(write)* | `name`, `cid` | Bind a name to a CID. |
| `concierge.write_site` *(write)* | `html`, `name?` | **Stage a website** (`<store>/canvas/<name\|draft>/index.html`) — it appears live in the GUI **Studio → Write** tab; the user publishes it. Staging only; never publishes. |
| `concierge.write_asset` *(write)* | `path`, `content`, `site?`, `base64?` | **Stage any file** (JS/CSS/SVG/image/glTF…) into `<store>/canvas/<site>/<path>` — build multi-file media/games. Staging only. |
| `concierge.scaffold_engine` *(write)* | `engine` (`three`\|`phaser`), `site?` | **Drop in a vendored renderer** (Three.js 3D / Phaser 2D) so a game stays self-contained/offline; returns a ready-to-use snippet. Staging only. |

Write actions are explicit tool calls; a tool-level failure returns `isError: true`
with an actionable message (vs a JSON-RPC error for a malformed request).

## The `write_site` → Studio bridge

`concierge.write_site` writes `<store>/canvas/draft/index.html`. The GUI Studio's
Write tab polls `GET /api/canvas/draft` every ~2s and fills the editor + live
preview — so "tell the AI to build the page" lands in the HTML window in real time,
and the user previews and publishes it. (It won't clobber text the user is already
editing.)

## Resources (side-effect-free reads)

```text
concierge://name/{name}     # resolve + read a named record
concierge://cid/{cid}       # read a record by CID
```
`resources/list` advertises `concierge://name/latest`. Tombstoned lookups return a
**receipt**, not a corruption error.

## Planned (contracted, not yet implemented)

These core ops are designed into the contract and will be added as tools/resources
as needed — note that egress ones (`share`, `export_car`) must route through the
same reviewed, password-gated gate as the GUI, never autonomously:

`concierge.checkpoint` · `concierge.resume` · `concierge.trace` ·
`concierge.export_car` · `concierge.import_car` · `concierge.share` · `concierge.gc`
· resources `concierge://checkpoint/{latest,cid}`, `concierge://car/{root}`,
`concierge://tombstone/{cid}`.
