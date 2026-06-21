# Concierge Kernel — Architecture Plan

## 1. Why

Every lag we chased this session — the blank Network map, the stuck Brain tab,
search timing out — traced back to **one** cause: the memory index shares a
process, a lock, and the browser's connection pool with everything else. When the
index was busy, unrelated subsystems starved.

We fixed the symptoms in-process (on-demand build, single-store-open walk,
`try_lock` on the status poll). Those work, but the coupling can be reintroduced
by any future change — the isolation is a matter of discipline, not structure.

A **kernel / GUI split** makes the isolation structural.

**Goals**

1. **Isolation** — index/memory work can never starve the GUI, network discovery,
   or brain loading. Enforced by a process boundary, not by careful locking.
2. **Stability** — a small, well-contained core owns the data. The crash-prone
   surface (web server, browser bridge, canvas, deploy) cannot corrupt memory or
   take it down.
3. **Warm across restarts** — the index lives in a long-lived kernel, so GUI
   restarts and crashes never trigger a rebuild.

**Non-goals** — faster raw computation (it's the same work either way), replacing
IPLD/core, multi-machine distribution. Those are separate concerns.

## 2. Architecture: three layers

```
┌─────────┐   HTTP    ┌──────────────────┐   IPC    ┌──────────────────────┐
│ Browser │ ───────►  │   GUI host       │ ───────► │   Kernel (daemon)     │
│ (front- │ ◄───────  │   (stateless)    │ ◄─────── │   (stateful, stable)  │
│  end)   │           │  web server,     │  socket  │  store + index +      │
└─────────┘           │  static, canvas, │          │  capture + libp2p +   │
                      │  request routing │          │  Kubo lifecycle       │
                      └──────────────────┘          └──────────────────────┘
        replaceable / can crash & reload            long-lived / owns the data
```

- **Browser** — unchanged. Renders, calls `/api/*` over HTTP.
- **GUI host** — serves the browser, holds **no precious state**, forwards data
  operations to the kernel over IPC. Owns presentation only. Can crash/restart
  freely without losing anything.
- **Kernel** — owns the store, the warm index, capture, the libp2p node, and Kubo
  lifecycle. Exposes an IPC API. Outlives the GUI host.

## 3. Kernel boundary — what moves where

**Into the kernel (stateful / data layer):**

- `MemCli` and the IPLD store (all reads and writes).
- `Librarian`: warm index, on-demand build, snapshot, the retrieve/ranking engine.
- Capture loops (harness ingest) **and** incremental index-on-capture.
- The libp2p `ChatNode` (peer discovery, messaging) — stateful, benefits from
  staying alive across GUI restarts.
- Kubo sidekick lifecycle (already a detached child today).
- Moderation/quarantine and the privacy overlay (data-layer policy).

**Stays in the GUI host (presentation / edge):**

- The HTTP server for the browser, static asset serving, canvas live-preview.
- Request routing — translate `/api/*` into kernel IPC calls.
- CSRF minting and rate limiting at the edge.
- Browser window/heartbeat watchdog.

**Shared:** `concierge-core` remains the common library both binaries link.

## 4. IPC design

- **Transport:** Unix domain socket (macOS/Linux) + named pipe (Windows), behind a
  small `Transport` trait so the rest of the code is OS-agnostic. Local-only, no
  TCP port to manage, gated by filesystem permissions under `~/.concierge`.
- **Protocol:** reuse the **existing request/response contract**. The kernel speaks
  the same shape the GUI already produces (path + query + optional body → JSON
  response), so `read_routes` / `mutations` handlers move into the kernel almost
  verbatim and the GUI host becomes a thin proxy. Frame as length-prefixed JSON.
- **API surface:** today's endpoints become kernel RPCs — `search`, `get-record`,
  `names`/tree, `peers`, `brain`, mutations (`publish`/`pin`/`ingest`/`bookmarks`),
  `messaging`.
- **Streaming:** the activity feed and presence become either a subscribe channel
  or a cheap short-poll RPC. Critically, the activity/presence poll is served
  **without touching the index lock**, so it can never hang.

## 5. Lifecycle & supervision

- Kernel starts first, binds its socket, writes a lockfile `{pid, socket}` under
  `~/.concierge` (mirrors today's `gui-lock`).
- GUI host on launch: **find-or-spawn** the kernel (reuse the existing "auto-open /
  reuse" contract), then connect. If no kernel is running, spawn it detached.
- **Kernel outlives the GUI host:** closing the last window stops the GUI host but
  leaves the kernel (and its warm index) running. A distinct "Quit Concierge"
  fully stops the kernel.
- Reconnect: GUI host transparently reconnects on transient IPC errors; the kernel
  keeps serving any other clients (CLI, MCP, a second window).
- The kernel becomes the parent/owner of the Kubo daemon (consolidating today's
  detached-process management into the stable layer).

## 6. Warm index & snapshot in the kernel

- The warm index lives in **kernel RAM**, built on demand on first search, kept warm.
- **Incremental index-on-capture:** capture runs in the kernel, so new conversation
  CIDs go straight to the index **in-process** — no IPC hop, no store re-walk. A
  fresh user accretes their index one record at a time and never bulk-builds.
- **Snapshot:** written on kernel shutdown (atomic temp + rename) under
  `~/.concierge`; loaded on kernel start. Because the kernel rarely restarts,
  rebuilds are rare.
- **GUI restarts do not touch the index** — it lives in the kernel. This is the
  structural fix for "rebuild on restart."

## 7. Crash safety (durable by construction)

- The **store** (immutable IPLD blocks, atomic writes) and **`embed-cache.json`**
  (per-record vectors, written as they're computed) are always on disk. The
  snapshot is only an optimization layered on top.
- Snapshot writes are atomic; on load it's validated (model id + format version)
  and discarded if stale.
- If the kernel crashes and misses its snapshot: on restart it **rebuilds from the
  durable embed-cache + store** — cheap, because vectors are already cached (no
  re-embedding). Graceful degradation, never data loss.
- Optional crash backstop: a debounced snapshot after a capture burst settles —
  add **only if** post-crash rebuilds ever feel slow; default to shutdown-only to
  avoid the write churn we removed.

## 8. Migration — phased, each step shippable

- **Phase 0 — Carve.** Confirm `concierge-core` is a clean data layer (it mostly
  is). Inventory which `gui` modules are data-layer (move) vs presentation (stay).
- **Phase 1 — Kernel binary.** New `concierge-kernel` crate runs store + index +
  capture + node and exposes the IPC socket serving the existing request contract.
  No GUI changes yet; the kernel is optional/dormant.
- **Phase 2 — Thin client.** GUI host gains a kernel client; route a couple of
  endpoints (`/api/search`, `/api/peers`) through the kernel behind a flag. Verify
  byte-for-byte parity with the in-process path.
- **Phase 3 — Move data endpoints.** All data `/api/*` go through the kernel; the
  GUI host becomes a proxy + static server. Capture loops and `ChatNode` move to
  the kernel.
- **Phase 4 — Index into the kernel.** Warm index + snapshot live in the kernel;
  remove the in-process index from the GUI host. Wire incremental index-on-capture
  (kernel-internal, no IPC).
- **Phase 5 — Lifecycle.** Kernel as persistent daemon; GUI find-or-spawn; kernel
  outlives the GUI; supervision + reconnect.
- **Phase 6 — Harden.** Crash recovery, Windows named-pipe parity, contract tests
  (API parity), restart-keeps-index test, crash-rebuilds-from-cache test.

Every phase keeps the app working; the split is introduced through the existing
reuse/lifecycle seams rather than a big-bang rewrite.

## 9. Risks & trade-offs

- **IPC overhead** — negligible for the small, frequent queries (map, brain,
  presence); only `search` carries a larger payload and it's infrequent. Keep
  responses compact.
- **Complexity** — a second binary, a protocol, lifecycle management. A real cost;
  the phased migration contains it and keeps each step revertible.
- **Windows parity** — named pipes vs Unix sockets, hidden behind the `Transport`
  trait.
- **Protocol versioning** — kernel and GUI host must agree; version the handshake
  and reject mismatches with a clear message (prevents a stale GUI talking to a new
  kernel).
- **Double-running** — the lockfile + find-or-spawn prevents duplicate kernels
  (reuses today's gui-lock pattern).

## 10. What does NOT change

`concierge-core`, the IPLD store, content-addressing, the retrieve/ranking engine,
and the on-node embedder.

**CLI and MCP become kernel clients** (decided): they connect to the kernel and
share its **one warm index** rather than each building their own. This is the
payoff of the split — the AI's `retrieve` (via MCP), the CLI's `retrieve`, and the
GUI's search all hit the same warm index in the kernel, so the index is built at
most once and reused by everyone. (A `--standalone` fallback can still use core
directly for a one-shot op when no kernel is running, e.g. scripting.)

## 11. Definition of done

- The GUI host holds **no** index or store state; killing/restarting it never
  rebuilds the index.
- Index busyness **cannot** affect Network / Brain / presence latency — separate
  paths, separate process.
- A **kernel restart** is the only event that can rebuild the index, and it rebuilds
  cheaply from the embed-cache (no re-embed).
- Clean shutdown snapshots; a crash degrades to a cheap rebuild, never a loss.
