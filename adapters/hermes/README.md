# Concierge ↔ Hermes adapter (Phase 6)

The first real harness adapter. It mounts onto **Hermes Agent** and records its
sessions into Concierge IPLD memory — **without changing Hermes's core loop**.

## How it works

It's an *observe-only* Hermes plugin. Hermes calls `invoke_hook(name, **kwargs)`
at lifecycle points; this plugin registers callbacks for the memory-relevant
hooks and translates each into the **host-neutral `Event`** shape
([`ADAPTER_CONTRACT.md`](../../ADAPTER_CONTRACT.md)), then feeds them to
`concierge-plugin ingest` — the same JSONL path any harness uses (Decision 0002,
no FFI).

| Hermes hook | host-neutral event |
|---|---|
| `on_session_start` | `session_started {cwd}` |
| `pre_llm_call` | `user_prompt {text}` |
| `post_llm_call` | `model_response {text}` |
| `pre_tool_call` | `tool_call_started {tool, args_json}` |
| `post_tool_call` | `tool_call_finished {tool, ok, result_json}` (+ `file_read`/`file_written` for file tools) |
| `on_session_end` | `session_ended` → ingest the session |

The plugin **never returns a hook directive** (`pre_llm_call` returns `None`), so
it can't alter Hermes's behavior — it only watches. Events accumulate in
`.concierge/hermes-sessions/<id>.jsonl` and are ingested at session end
(idempotent — re-ingest is a no-op).

## Mount it

```bash
# 1. Build the plugin CLI and put `concierge-plugin` on PATH.
cargo install --path crates/cli      # or symlink target/debug/concierge-plugin

# 2. Drop this adapter into Hermes's plugins and enable it.
cp -r adapters/hermes ~/.hermes/plugins/concierge-memory
#   then add `concierge-memory` to `plugins.enabled` in ~/.hermes/config.yaml

# 3. (optional) point it at a specific store / binary:
export CONCIERGE_WORKDIR=/path/to/project   # where .concierge lives
export CONCIERGE_BIN=/path/to/concierge-plugin
```

Now every Hermes session writes a checkpointed, content-addressed memory graph
under `host:hermes:session:<id>:*`. When the plugin mounts, it opens the
standalone read-only Visual Memory Explorer with Hermes's declared model in the
Mounted badge. Set `CONCIERGE_GUI_NO_OPEN=true` only for a headless install.

## Test

```bash
python3 adapters/hermes/test_translate.py
```

Replays a canned Hermes session and asserts the host-neutral event stream
(Phase 6, Step 7). The exact hook keyword arguments are read defensively, the way
real Hermes memory plugins (e.g. Icarus/MemoryOS) do, so the adapter is forward
compatible.

## Deferred

The adapter targets Hermes's *current* public hook surface. Live verification
against a running Hermes session (vs. the canned fixture) requires a Hermes
install; the translation and the JSONL→IPLD path are covered here and by the Rust
ingest tests.
