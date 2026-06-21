# Kernel Build Log — phase-by-phase audit trail

Tracks what's actually been built against `KERNEL_PLAN.md`, with concrete checks to
audit each phase. Status legend: ✅ built (needs your `cargo build` to confirm) ·
⏳ pending · ⛔ blocked.

No Rust toolchain is available in the authoring environment, so every ✅ below means
"written and reviewed by hand" — your build on the Mac is the real verification.
Each phase lists exactly how to audit it.

---

## Phase 1 — Kernel binary (dormant) ✅ built

**Delivers:** a standalone `concierge-kernel` daemon that holds the IPLD store,
starts the consent-gated capture loops, owns a libp2p discovery node, holds one
warm search index, and serves the existing read-route contract (`path + query +
optional body -> response`) over a Unix socket.

**Files:**
- `crates/kernel/Cargo.toml` — new crate (depends on `concierge-core` + serde).
- `crates/kernel/src/protocol.rs` — `Request`/`Response` wire types.
- `crates/kernel/src/transport.rs` — length-prefixed JSON framing.
- `crates/kernel/src/state.rs` — `KernelState`: warm index, lazy on-demand build, `search`.
- `crates/kernel/src/capture.rs` — daemon-owned Claude/Aider/Codex/Gemini/Continue capture loops.
- `crates/kernel/src/node.rs` — daemon-owned libp2p discovery node and `/api/peers` JSON.
- `crates/kernel/src/main.rs` — daemon serve loop + built-in test client.
- `Cargo.toml` (workspace) — added `crates/kernel` to members.

**Audit:**
```bash
cargo build -p concierge-kernel                 # compiles clean?
./target/debug/concierge-kernel serve           # terminal 1: "kernel: listening on …kernel.sock"
./target/debug/concierge-kernel ping            # terminal 2: {"ok":true,"result":{"pong":true}}
./target/debug/concierge-kernel status          # {"ok":true,"result":{"indexed":null}}  (null until first search)
./target/debug/concierge-kernel search "peer discovery"   # ranked hits from your store
./target/debug/concierge-kernel peers           # live discovery map JSON
./target/debug/concierge-kernel status          # now {"indexed": <n>} — index stayed warm
```
**Pass criteria:** all client calls succeed; `search` returns hits matching the
GUI search JSON shape; `peers` returns this node plus discovered peers; the second
`status` shows a non-null `indexed` (warm reuse). Starting a second kernel must
refuse the live socket instead of stealing it.

**Known gaps (by design, later phases):** Unix-only (Windows = Phase 6); no GUI
find-or-spawn supervision yet (Phase 5).

---

## Phase 2 — GUI as kernel client (opt-in) ✅ built

**Delivers:** the kernel's protocol/client extracted into a shared library, and the
GUI routing `/api/search` and `/api/peers` to the kernel when opted in — with
automatic fallback to the in-process build. Default behavior unchanged.

**Files:**
- `crates/kernel/src/lib.rs` — new: exposes `protocol`, `transport`, `client`, `socket_path()`.
- `crates/kernel/src/client.rs` — new: `available()` / `send()` / `search()`.
- `crates/kernel/src/main.rs` — rewritten to use the lib (no duplicated protocol).
- `crates/gui/Cargo.toml` — added `concierge-kernel` dependency.
- `crates/gui/src/read_routes.rs` — `kernel_get()` helper + opt-in routing for `/api/search` and `/api/peers`.
- `crates/gui/src/server.rs` — GUI capture loops stay in-process only when no opted-in kernel is reachable.

**Audit:**
```bash
cargo build -p concierge-kernel                 # build the lib+bin in isolation FIRST
cargo build -p concierge-plugin                 # GUI now links the kernel lib — still clean?

# terminal 1:
./target/debug/concierge-kernel serve
# terminal 2 — GUI opted into the kernel:
CONCIERGE_USE_KERNEL=1 ./target/release/concierge-plugin gui
```
**Pass criteria:**
- With `CONCIERGE_USE_KERNEL=1` **and** the daemon running → GUI search returns the same
  hits as `concierge-kernel search "<same query>"` (parity).
- With `CONCIERGE_USE_KERNEL=1` **and** the daemon running → `/api/peers` is served by
  the kernel-owned discovery node.
- With the daemon **stopped** (or the env var unset) → GUI search still works, served
  in-process (fallback). This is the safety property: it can never break search.
- Repeated GUI searches don't rebuild — the kernel holds one warm index across them.

**Known gaps:** Phase 2 is still read-only routing. Mutations, CLI, and MCP move to
the kernel in later phases.

---

## Phase 3 — Move data endpoints to the kernel via a shared handler crate ✅ verified

Capture + the discovery node already moved (Phase 1). Phase 3 moves the pure-store
read routes through the kernel **without duplicating handler logic**.

**Architectural decision (the crux):** the kernel cannot depend on the GUI (cycle), so
GUI-side handlers (`names_json`, `record`, `PrivacyOverlay`, …) must be *extracted* into
a crate both sides share — not reimplemented in the kernel (which would drift). This
extends the pattern `/api/search` already uses (both sides are thin wrappers over
`concierge-core::Librarian`).

**Plan — new `concierge-routes` crate** holding pure-store read handlers + `PrivacyOverlay`,
parameterized over `MemCli`. Both GUI (in-process fallback) and kernel (`handle`) call it.
Move **one route group at a time** behind the existing opt-in + fallback; verify each.

**Route inventory:**
- *Move (pure-store):* `/api/names`, `/api/record`, `/api/blob`, `/api/stats`, `/api/graph`,
  `/api/checkpoints`, `/api/resolve`, `/api/meta`, `/api/hot-pins`, `/api/privacy`
  (+ `PrivacyOverlay`).
- *Done:* `/api/search`, `/api/peers`.
- *Node-coupled (later, with messaging):* `/api/rooms`, `/api/network`, `/api/thread`.
- *Stay in GUI host (presentation):* PNG assets, `/api/canvas/*`, `/api/activity` (GUI's own
  console feed), deploy/youtube/wallet/sites, `/api/heartbeat`, `/api/closing`.

**Status — implemented and verified:**
- `crates/routes/` owns the shared store-read handlers: `PrivacyOverlay`, `meta_json`,
  `resolve_json_for_query`, `names_json`, `record_json`, `blob_bytes`, `checkpoints_json`,
  `graph_json_for_query`, `stats_json_for_query`, `hot_pins_json`, and `privacy_json`.
- Kernel `handle` serves `/api/meta`, `/api/resolve`, `/api/names`, `/api/record`,
  `/api/checkpoints`, `/api/graph`, `/api/stats`, `/api/hot-pins`, and `/api/privacy`
  via `concierge-routes`.
- GUI `read_routes.rs` is now a wrapper layer: with `CONCIERGE_USE_KERNEL=1` it proxies
  moved routes to the kernel; without a daemon it falls back to the same shared handlers.
- `/api/blob` remains GUI-served because it returns binary assets, but the blob resolver is
  shared in `concierge-routes`.
- `/api/meta` proxies to the kernel for store/config data, then the GUI overwrites only the
  process-local fields (`mounted_model`, `store`, `csrf_token`) before returning it.
- `concierge-kernel routes` now prints the kernel route inventory from one shared constant.
- A reachable kernel error is preserved as an HTTP-shaped response; only unreachable kernels
  fall back to in-process handling.

**Verification:**
```bash
cargo check --locked -p concierge-routes -p concierge-kernel -p concierge-plugin
cargo test --locked -p concierge-routes
cargo test --locked -p concierge-gui
cargo test --locked -p concierge-kernel
cargo clippy --locked -p concierge-routes -p concierge-kernel -p concierge-gui --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
cargo test --locked --workspace
```

Pass: all commands above completed successfully. GUI tests include fake-kernel coverage for
`/api/meta`, `/api/resolve`, `/api/names`, `/api/record`, `/api/checkpoints`, `/api/graph`,
`/api/stats`, `/api/hot-pins`, and `/api/privacy`.

---

## Phase 4 — Warm index + snapshot fully in the kernel ✅ 4a+4c verified / 4b deferred

**Status (verified):**
- **Core** — `Librarian` gained `append_records` (O(new) embed+add), `reconcile` (scope_all
  diff → append), and `save_snapshot`/`load_snapshot` (atomic, model+version-tagged; `serde`
  on `IndexEntry`).
- **4c** — `KernelState::start_background` warms a valid snapshot immediately, `search` reconciles
  loaded/stale snapshots instead of replacing a newer in-memory index, and a SIGINT/SIGTERM handler
  in the daemon persists the snapshot on clean exit (added `libc`). Restart → loads, no rebuild;
  crash → cheap rebuild from `embed-cache.json`.
- **4a** — harness `capture_once` now returns `Vec<Cid>` and kernel capture passes those exact CIDs
  into `KernelState::append_index_records`. This is event-driven and kernel-internal: no IPC hop,
  no name-count polling, and no store re-walk on the capture path. Search reconciliation remains
  the safety net for stale snapshots or external/manual writes.
- **4b — deferred to Phase 5** (removing the GUI's in-process index is only safe once
  find-or-spawn guarantees a kernel).

**Audit:** warm a kernel (one search) → restart → `status.indexed` immediate (loaded). Attach a
harness + converse → returned CIDs append directly, no poll/rebuild. `kill -9` → next start rebuilds
from `embed-cache.json` (no re-embed). `kill -TERM` (clean) → snapshot saved, next start loads.

**Verification:** `cargo test --locked --workspace` passes, including targeted regressions for
snapshot warm/reconcile, capture append searchability, and JSONL ingest CID reporting.

---

### Original plan (for reference)

Three parts. **Recommended order: 4c → 4a, with 4b deferred to Phase 5** (it's entangled
with find-or-spawn). These re-introduce the index-growth + persistence primitives that were
reverted from the GUI — but now in the *kernel*, used correctly: event-driven appends (not a
20s poll) and a shutdown snapshot (not a per-settle write).

### 4c — Snapshot persistence (do first; self-contained, crash-safe by construction)
- **Core** — add to `Librarian`: `save_snapshot(&self, mem)` (serialize entries + vectors +
  signals to `<store>/index-snapshot.json`, atomic temp+rename, tagged by embedder id + a
  format version) and `load_snapshot(mem, &embedder) -> Option<Self>` (load iff the tag matches,
  else `None`). Derive `serde` on `IndexEntry`.
- **Kernel** — on startup (`start_background`) try `load_snapshot` → warm immediately, no build.
  On shutdown, `save_snapshot`. The accept loop needs a SIGINT/SIGTERM handler (mirror the GUI's
  signal seam) that saves before exit.
- **Durable by construction** — the store + `embed-cache.json` are the truth; a missed snapshot
  (crash) degrades to a cheap rebuild from the cache (no re-embed). Atomic write ⇒ a crash
  mid-save can't corrupt.
- **Audit** — warm a kernel (one search), restart → `status.indexed` is immediate (loaded, not
  rebuilt). `kill -9` mid-run → next start rebuilds cheaply from `embed-cache.json`.

### 4a — Incremental index-on-capture
- **Adapters** — `capture_once` returns the new CIDs it wrote (`Vec<Cid>`), not just a count.
  (Touches every harness adapter + `ingest_file`.)
- **Core** — add `Librarian::append_records(&mut self, mem, cids: &[Cid]) -> Result<usize>`:
  embed only those CIDs (reuse `embed-cache`), add entries, recompute signals once. O(new) —
  no `scope_all`, no polling.
- **Kernel** — pass the index handle into `spawn_all`; when a capture pass returns new CIDs,
  `append_records` them. Event-driven, no timer, no re-walk. Fresh users accrete the index one
  record per turn; never a bulk build.
- **Audit** — attach a harness, hold a conversation → `status.indexed` climbs with no rebuild;
  search finds the new turn within a turn.

### 4b — Warm index only in the kernel (DEFER to Phase 5)
- Removing the GUI's in-process index/fallback leaves the GUI with no search when no kernel is
  running — only safe once Phase 5's find-or-spawn guarantees a kernel. Keep the GUI fallback
  through Phase 4; remove it in Phase 5 with the lifecycle work.

---

## Phase 5 — Lifecycle & supervision ✅ built

Keystone first; then the consumers adopt it. 4b is now complete: the GUI no longer owns
or builds a retrieval index.

**Sub-steps:**
1. **find-or-spawn (✅ built):** `concierge_kernel::client::ensure_running()` — returns
   immediately if a kernel is reachable, else spawns `concierge-kernel serve` **detached**
   (new session via `setsid`, null stdio) and waits ~2s for the socket. Spawn race is safe:
   `bind_socket` refuses a second live listener, so at most one kernel runs.
2. **GUI adopts it (✅ built):** `serve_with_options` (`crates/gui/src/server.rs`) calls
   `ensure_running()` on launch in a detached `#[cfg(unix)]` thread — off the startup path so a
   slow kernel launch never delays the window. `kernel_get` (`read_routes.rs`) is now **default-on**:
   every moved route + `/api/search` routes through the kernel, replacing the Phase-2
   `CONCIERGE_USE_KERNEL` opt-in. It uses `send_supervised`, so a kernel that crashed mid-session
   is auto-restarted and the request retried before returning an explicit unavailable response.
3. **Kernel outlives the GUI (✅ by construction):** the detached `setsid` spawn means the kernel is
   in its own session — not a child of the GUI host — so closing the window / the GUI's window-close
   watchdog cannot reach it. The Phase-4c SIGTERM snapshot handler still lets a deliberate
   `kill -TERM <kernel-pid>` ("Quit Concierge") persist the index on a clean stop.
4. **CLI + MCP as kernel clients (✅ built):** `cmd_retrieve` and the MCP `retrieve` tool route
   through `client::search_supervised(...)` — which ensures a kernel is running and **auto-restarts
   it if it crashed** (one retry), returning a `KernelLifecycle` so the caller prints a one-line
   **index notice** (`memory kernel restarted — rebuilding the index from cache…`). Both reproduce
   their exact local output shape from the kernel JSON. CLI keeps the planned explicit
   `--standalone`/`--external` local path; MCP retrieve is kernel-only so the AI shares the same
   warm index as GUI + CLI.
   - ⚠ Build weight: `concierge-mcp`/`concierge-plugin` now depend on the `concierge-kernel` *crate*
     (for the client). If that pulls the kernel binary's heavy deps (net/adapters/tokio) into those
     builds and it bites, split the protocol+transport+client into a tiny `concierge-kernel-client`
     crate and depend on that instead. (The GUI already takes this dependency, so the workspace
     tolerates it today.)
5. **4b — warm index only in the kernel (✅ built):** the GUI no longer carries
   `LibrarianState`, `LibrarianCache`, or local `/api/search` indexing. If the kernel cannot be
   reached after supervision, search returns 503 instead of building a second in-process index.
6. **Lockfile (✅ built):** after binding the socket, `concierge-kernel serve` writes
   `<workdir>/.concierge/kernel.lock` as JSON `{pid, socket}` with `0600` permissions. Shutdown
   removes the lockfile and socket after saving the index snapshot.

**Audit:**
```bash
cargo check --locked -p concierge-kernel -p concierge-plugin -p concierge-mcp
cargo clippy --locked -p concierge-kernel -p concierge-gui -p concierge-plugin -p concierge-mcp \
  --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --locked --workspace

# Lifecycle, by hand:
./target/release/concierge-plugin gui          # no kernel running first
pgrep -fl concierge-kernel                      # GUI spawned one (detached)
# close all GUI windows, then:
pgrep -fl concierge-kernel                      # STILL alive, index warm
./target/release/concierge-plugin gui          # reopen → instant, no rebuild (shared warm index)
./target/debug/concierge-kernel status          # indexed == GUI's; CLI/MCP retrieve hit the same
cat ~/.concierge/kernel.lock                    # {"pid": ..., "socket": ".../kernel.sock"}
```
**Pass criteria:** GUI auto-spawns a kernel and search works; closing windows leaves the kernel
(and its warm index) alive; reopening is instant; CLI + MCP `retrieve` share `status.indexed`; a
second GUI launch spawns no duplicate kernel; the daemon writes and cleans up its `{pid, socket}`
lockfile.

### Post-Phase-5 correction — Kubo Sidekick is manual-start (not auto)

The Phase-5 idea that "the kernel owns the Sidekick relaunch" was reverted by design:
the private Kubo node must **only** start on an explicit user click, never on startup.
- **Kernel:** `start_background` no longer calls `ensure_sidekick`; the kernel launches
  nothing Kubo-related on boot (the libp2p discovery node, `ensure_node`, is separate and
  stays). `crates/kernel/src/state.rs`.
- **GUI:** the status control no longer shows "starting…" for an idle-but-enabled node
  (which read as "starting" while nothing was starting). "starting…" now appears only
  while a click-initiated launch is genuinely in flight (a transient flag), polls until
  the node is actually up, and otherwise reads an honest **OFF — click to start**.
  `crates/gui/src/studio.js`.
- Net: fresh start → OFF; click → starting… → ON; reopen later → still OFF unless clicked.

---

## Phase 6 — Harden 🔬 in progress

Four items. #1 ✅, #4 ✅, #2 ✅ (built; needs a Windows build to verify). #3 partial — the
versioning/lifecycle unit tests landed; the API-parity contract + restart/crash integration tests
remain.

### 1. Protocol versioning + stale-kernel-on-upgrade ✅ built
The kernel is detached and persistent, so it can outlive an app upgrade — a new GUI/CLI/MCP
could otherwise talk to a stale daemon from the previous version. Fixed:
- `protocol.rs` — added `PROTOCOL_VERSION` and a `version` field on `Response` (the kernel
  stamps it via every constructor; a pre-versioning kernel omits it, so it `#[serde(default)]`s
  to 0). No handshake round-trip; the version rides every response.
- `client.rs` — `send_supervised` checks `resp.version`: a mismatch means a stale daemon, so it
  `stop_running_kernel()` (SIGTERM the pid from the lockfile → it snapshots + clears its
  socket/lock), `ensure_running()` spawns the current binary, and retries once, returning the new
  `KernelLifecycle::Upgraded` (its `index_notice` tells the user the kernel was replaced).
- Robust against an old kernel that predates the field: the client detects "version absent/≠ mine"
  from the *response*, so the old daemon needs no cooperation.

**Audit:** `cargo test --locked -p concierge-kernel` (version stamp/default + lifecycle-notice
tests). By hand: run an old `concierge-kernel`, bump `PROTOCOL_VERSION`, rebuild a client, issue a
search → the old daemon is SIGTERM'd and replaced, the query succeeds, and the "upgraded" notice
prints. A second client call is `Warm` (no churn).

### 2. Windows transport ✅ built (needs a Windows build to verify)
Scope correction after reading `main.rs`: the **entire** daemon (`mod state/capture/node` + the
serve loop) was `#[cfg(unix)]`, with `main()` a stub on Windows. So this was a real port, not a
socket swap — but `state`/`capture`/`node` use only cross-platform deps, so the Unix-specific surface
was small.

**Chosen approach — keep one socket code path via AF_UNIX everywhere.** Windows 10+ has AF_UNIX;
std only exposes Unix sockets on Unix, so the `uds_windows` crate provides the same
`UnixStream`/`UnixListener` API on Windows. This keeps the **verified Unix path byte-for-byte
unchanged** and avoids a second (TCP) transport, port-in-lockfile plumbing, and hand-rolled named-pipe
FFI. The client connects to the same `socket_path()` on both platforms — no lockfile-port dance.

Changes (all additive / `cfg`-gated; the Mac build is untouched):
- **New dep** `uds_windows` (workspace), `[target.'cfg(windows)']` only — in `concierge-kernel-client`
  (client) and `concierge-kernel` (daemon listener). Pulled into `Cargo.lock` cross-target but never
  built on Mac.
- **Client** (`kernel-client/src/client.rs`): `UnixStream` is `std` on Unix, `uds_windows` on Windows;
  detached spawn is `setsid` (Unix) / `creation_flags(DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP)`
  (Windows); stale-kernel stop is `SIGTERM` (Unix) / `taskkill /F` (Windows); `lockfile_pid` returns
  `i32` (no `libc::pid_t`).
- **Daemon** (`kernel/src/main.rs`): `mod state/capture/node`, `main`, and `mod daemon` now
  `cfg(any(unix, windows))`; listener/stream type `cfg`-aliased; `bind_socket`'s `0700/0600`/`is_socket`
  checks are Unix-only (Windows relies on the profile dir's NTFS ACL); shutdown registers Unix signals
  or a `SetConsoleCtrlHandler` (mirroring `gui/src/server.rs`), both flagging the shared poller that
  snapshots + cleans up. The Unix-permission test module is `cfg(all(test, unix))`.
- **Consumers un-gated** to `cfg(any(unix, windows))`: GUI `kernel_get` + the `server.rs` ensure-kernel
  spawn; MCP and CLI `retrieve`.

**Security note (v1):** the Windows AF_UNIX socket is restricted by the user-profile dir's default
NTFS ACL rather than an explicit `0600`; tightening that ACL (or adding a lockfile token) is a small
hardening follow-up.

**Not verifiable from the authoring environment.** Mac/Unix stays green (all Windows code is
`cfg(windows)` + target-gated). On Windows, confirm: `uds_windows`'s `UnixListener::incoming()` /
`UnixStream` are `Send` as assumed; `state`/`capture`/`node` compile clean; and the
spawn/connect/shutdown round-trip works. Run a non-`--locked` build first to refresh `Cargo.lock`.

### 3. Contract + lifecycle test suite 🔬 partial
Landed with #1: protocol version stamp/default and lifecycle-notice unit tests. Still to add:
API-parity contract tests (every kernel route's response shape == what the GUI/`concierge-routes`
expect), restart-keeps-index, and crash-rebuilds-from-cache.

### 4. Split a tiny `concierge-kernel-client` crate ✅ built
`protocol.rs`, `transport.rs`, `client.rs`, and the `workdir`/`socket_path`/`lockfile_path`
helpers moved into a new dependency-light crate `crates/kernel-client` (std + serde/serde_json/url,
libc unix-only; **no `concierge-core`**). `concierge-kernel`'s `lib.rs` is now a re-export shim
(`pub use concierge_kernel_client::{protocol, transport, client, …}`), so the daemon and the GUI keep
using `concierge_kernel::*` unchanged. MCP and CLI repoint to `concierge-kernel-client` directly, so
the MCP server no longer pulls libp2p/tokio/adapters for the client. New workspace member registered.

**Audit:** `cargo tree -p concierge-mcp` shows no libp2p/tokio via the kernel; `cargo build` once
(not `--locked`) to refresh `Cargo.lock` for the new crate, then `cargo test --locked --workspace`.

> Note: adding a new crate makes `Cargo.lock` stale, so the first `--locked` build will fail until
> the lock is regenerated — run one plain `cargo build`/`cargo check` first.

**Audit (planned):** contract tests pass on macOS, Linux, and Windows; a kernel/client protocol
mismatch is replaced cleanly with the upgrade notice; MCP/CLI build without libp2p/tokio in their
dependency tree.

---

## Definition of done (whole effort)

- GUI host holds **no** index/store state; restarting it never rebuilds the index.
- Index busyness **cannot** affect Network / Brain / presence latency.
- A **kernel restart** is the only rebuild trigger, and it rebuilds cheaply from cache.
- Clean shutdown snapshots; a crash degrades to a cheap rebuild, never a loss.
