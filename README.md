# Universal Concierge Plugin

### The Concierge — _Serving up Data on a Silver Platter._

**A small sidekick to your large model.**

Universal Concierge Plugin rides alongside any AI harness as a lightweight
sidekick. It remembers everything the agent does in a portable, content-addressed
graph (IPLD, CAR, optional network publishing) — and, with a tiny on-node
embedder, it *ranks and serves* the right memory back to the host's large model
when asked. The model stays the muscle that thinks; the Concierge is the sidekick
that knows where everything is and hands over the right thing at the right moment.
Handed to any harness without rewriting a line, and without adopting Concierge's
runtime, UI, or agent loop.

```text
Any Harness  ->  Universal Concierge Plugin  ->  IPLD DAG  ->  LocalBlocks / CAR / Pinata / MCP
```

Concierge is the **substrate, not the harness** — and a *functioning* one: it
doesn't just store, it serves. The plugin observes durable agent activity —
prompts, responses, tool calls, file references, decisions, checkpoints — and
records it into a content-addressed IPLD graph. The tiny embedder then indexes
and ranks that graph so the substrate can hand the host's model the right context
on demand — a small, performance-invisible addition that compounds into a large
help to whatever model you bring. CIDs become portable references across agents,
tools, and machines; CAR files become the migration and backup format; and
publishing is a backend concern, not a harness concern. Standard Kubo/IPFS is public-networked unless explicitly isolated, so
the plugin never treats a local Kubo node as private and requires the explicit
`publish-public` operation plus a reviewed manifest and irreversible-publication
confirmation.

> Recall is **explicit**. The plugin is not an auto-context compiler — it won't
> inject memory into prompts unless the host asks.

## Why a Python (or any-language) harness works with a Rust core

The core is Rust, but a harness never needs to *be* Rust. It only emits
**host-neutral events** across a language-agnostic boundary:

```text
python harness  ->  events.jsonl  ->  concierge-plugin ingest events.jsonl  ->  IPLD records
```

One JSON object per line is the whole contract. No FFI, no bindings required.
(In-process Python bindings via PyO3 are a possible later optimization, not a
requirement.) See [`ADAPTER_CONTRACT.md`](./ADAPTER_CONTRACT.md).

## Workspace layout

This is a Cargo workspace. Boundaries between core, adapters, and MCP are kept
explicit from day one:

| Crate | Plan layer | Status |
|---|---|---|
| [`crates/core`](./crates/core) | Layer 1 (core binding) + Layer 2 (host event API) | contracts declared (Phase 0) |
| [`crates/adapter-jsonl`](./crates/adapter-jsonl) | Layer 3 (generic JSONL adapter) | line parser (Phase 0) |
| [`crates/mcp`](./crates/mcp) | Layer 4 (MCP server) | **deferred** stub — built last |
| [`crates/cli`](./crates/cli) | `concierge-plugin` entry point | command surface stubbed (Phase 0) |

## Build & run

```sh
cargo build
cargo test
cargo run --bin concierge-plugin -- help
```

## Status

Phase 0 (Project Shape) is in place: workspace scaffold, host-neutral event
contract, deferred-MCP boundary, and a decision log. No command does real work
yet — each subcommand names the phase that implements it.

The full plan, including the per-phase step breakdown, lives in
[`UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md`](./UNIVERSAL_CONCIERGE_PLUGIN_PLAN.md).
Architecture/MCP decisions are tracked in [`DECISIONS.md`](./DECISIONS.md).
