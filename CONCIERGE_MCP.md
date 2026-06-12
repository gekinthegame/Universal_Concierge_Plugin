# Concierge MCP Contract (draft) — DEFERRED

> **Not now.** The MCP server is back-burnered and built **last**, only after the
> local surfaces are solid: core binding (Phase 1), JSONL adapter (Phase 2), CAR
> export/import (Phase 4), and free local-IPFS publishing (Phase 5). MCP is "the
> same `Plugin` API over a different transport," so it costs little once that API
> is proven. This file records the contract so the boundary is documented; it is
> not an implementation commitment yet.

The eventual goal: expose Concierge memory through MCP so any MCP-aware system
can mount it with **zero custom adapter code**.

## Tools (write actions must be explicit tool calls)

| Tool | Maps to core op |
|---|---|
| `concierge.put_node` | `put_node` |
| `concierge.put_blob` | `put_blob` |
| `concierge.bind` | `bind` |
| `concierge.resolve` | `resolve` |
| `concierge.get` | `get` |
| `concierge.recall` | explicit recall |
| `concierge.checkpoint` | `checkpoint` |
| `concierge.resume` | `resume` |
| `concierge.export_car` | `export_car` |
| `concierge.import_car` | `import_car` |
| `concierge.share` | `share` |
| `concierge.gc` | `gc` |
| `concierge.trace` | trace a subgraph |

## Resources (reads are safe and side-effect free)

```text
concierge://name/{name}
concierge://cid/{cid}
concierge://checkpoint/latest
concierge://checkpoint/{cid}
concierge://car/{root}
concierge://tombstone/{cid}
```

## Modes

- **Read-only mode** — exposes resources + read tools; rejects every write tool.
- **Write-enabled mode** — behind explicit opt-in; write tools mutate only the
  expected sidecars.
- Tombstoned lookups return a **receipt**, not a corruption error.

## Build order reminder

See the Phase 3 step breakdown in
[`UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md`](./UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md).
Do not start this crate ([`crates/mcp`](./crates/mcp)) until Phases 1, 2, 4, and
5 are done.
