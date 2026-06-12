# Adapter Contract (draft)

This is the **public, language-agnostic boundary** between a host harness and the
Universal Concierge Plugin. A harness adapter's only job is to translate the
harness's local events into the shapes below and deliver them to the plugin.

The host side never needs to understand DAG-CBOR, CAR, tombstones, or IPLD links.

> Status: **draft** (Phase 0). The shapes here are mirrored by the Rust types in
> [`crates/core/src/event.rs`](./crates/core/src/event.rs). Phase 2 finalizes the
> ingest semantics (streaming, validation, idempotency, scoped names).

## Transport

The first and lowest-friction transport is **JSONL**: one JSON object per line,
delivered to `concierge-plugin ingest <file.jsonl>` (or piped on stdin). Any
harness in any language that can emit JSON can use this — a Python harness just
`json.dumps` one envelope per event.

Later transports (subprocess request/response, local socket/HTTP, and eventually
the deferred MCP server) carry the *same* event shapes.

## Envelope

Every event is wrapped in an envelope that keeps names collision-free across
concurrent harnesses:

| Field | Type | Required | Notes |
|---|---|---|---|
| `host_id` | string | yes | Identifies the emitting harness/host. |
| `session_id` | string | yes | Stable id for the session. |
| `project_id` | string | no | Project scope, when known. |
| `ts` | string | yes | RFC 3339 timestamp. |
| `event_id` | string | no | Stable event id; preferred dedupe key when present. |
| `imported_from` | object | no | Backfill provenance (`source_system`, `original_id`, `original_ts`). |
| `reasoning` | object | no | The model's "why" behind this step (see below). |
| `type` | string | yes | Event discriminator (see below). |
| …event fields | — | — | Flattened alongside `type`. |

### `reasoning` — the "why" travels with the step

A step (a tool call, response, or decision) usually has model reasoning behind it
that the bare artifact does not capture. When the harness exposes that reasoning,
the adapter attaches it to the **same envelope as the step**, so it lands in the
*same* record/CID — never a separate node:

| Field | Type | Required | Notes |
|---|---|---|---|
| `text` | string | yes | The reasoning text. |
| `source` | string | yes | `thinking` (genuine reasoning/thinking tokens) or `inline_preamble` (rationale written inline before acting). |

Rules:
- **Optional + additive.** Omit `reasoning` entirely when the harness exposes no
  thinking — a fidelity-ladder rung, never fabricated. Older adapters that never
  send it stay valid.
- **Honest `source`.** Never label inline preamble as `thinking`. A record must
  not claim reasoning the model did not actually produce.
- **No stripping on share.** Reasoning is the highest-signal text for retrieval;
  it travels with the step under the step's own capability boundary.

## Event types

The discriminator is the `type` field (snake_case). Payload fields are flattened
into the same object as the envelope.

| `type` | Fields | Meaning |
|---|---|---|
| `session_started` | `cwd?` | A session began. |
| `user_prompt` | `text` | A prompt from the user. |
| `model_response` | `text` | A response from the model. |
| `tool_call_started` | `tool`, `args_json?` | A tool call began. |
| `tool_call_finished` | `tool`, `ok`, `result_json?` | A tool call finished. |
| `file_read` | `path` | The host read a file. |
| `file_written` | `path` | The host wrote a file. |
| `decision_recorded` | `text` | A decision worth persisting. |
| `memory_recorded` | `text` | A note/memory worth persisting (used by markdown backfill). |
| `checkpoint_requested` | `label?` | Explicit checkpoint request. |
| `session_ended` | — | The session ended. |

## Outbound: `context_suggested` (Phase 8 §2 — OFF by default)

Every event above flows **host → node** (capture). The Context Compiler adds the
*only* event that flows **node → host**:

| `type` | Fields | Meaning |
|---|---|---|
| `context_suggested` | `cids` (string[]), `reason`, `authority` | The node's Librarian proactively suggests prior-context CIDs for the host to prepend. |

This is the **opt-in, librarian-as-agent path** and is **never emitted by default**
(Decision 0022). Two gates must both pass before a single suggestion is produced:

1. **Config opt-in** — `[injection] proactive = true` (default `false`).
2. **Trusted-authority grant** — the host presents a grant at request time; the
   suggestion is attributed to that `authority`, and the host treats the CIDs as
   trusted *per that authority* — never silently as instructions (threat-model L1,
   the MemoryOS "Ground Truth" lesson).

Wake policy is explicit and configurable (`[injection] wake_on`, default
`["user_prompt"]`), rate-limited and budget-bounded — the node does not wake on
every event. Adapters that do not implement the write-back seam simply never
receive `context_suggested`; the node stays tool-only (`concierge.retrieve`).

### Write-back transport — the outbox

`context_suggested` is delivered through an append-only **outbox**:
`<store>/outbox.jsonl`, one JSON object per line, each `{ "at": <unix>, "type":
"context_suggested", "cids": […], "reason": …, "authority": … }`. The node enqueues
a line **only** when both gates pass (a default node never writes here).

Consumption is **offset-based** (`<store>/outbox.offset`) so an adapter drains
idempotently — a crash mid-drain never drops or double-delivers:

- **peek** — read pending entries without advancing the offset.
- **drain** — read pending entries and advance the offset past them.

The node-side trigger is `proactive_wake(event_type, query, authority_id)` (a capture
loop calls it on a wake event). A reference adapter read/inject path is
`concierge-plugin claude-code inject` → renders the drained suggestion into a
`<suggested-context source="concierge" authority="…">` block the harness prepends,
attributed (never silently merged). CLI: `outbox <peek|drain|wake …>`.

## Example (JSONL)

```jsonl
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:00Z","type":"session_started","cwd":"/work/app"}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:01Z","type":"user_prompt","text":"add a health check endpoint"}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:05Z","type":"tool_call_started","tool":"write_file","args_json":"{\"path\":\"src/health.rs\"}","reasoning":{"text":"Put the handler in its own file so the route table stays small.","source":"thinking"}}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:06Z","type":"tool_call_finished","tool":"write_file","ok":true}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:06Z","type":"file_written","path":"src/health.rs"}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:09Z","type":"model_response","text":"Added /health returning 200."}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:10Z","type":"checkpoint_requested","label":"after-health-endpoint"}
{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:11Z","type":"session_ended"}
```

## Adapter responsibilities (Layer 3)

- Stamp every event with `host_id`, `session_id`, and (when known) `project_id`.
- Emit valid envelopes; the plugin validates and rejects malformed lines with a
  line number, skipping per policy (one bad line must not abort the stream).
- Honor an explicit include/exclude policy for file ingestion (Security Model).
- Never embed secrets in payloads — pass references or redacted metadata.

## Mapping to IPLD (informative — Phase 2)

Adapters do **not** need to know this; it is how the plugin will map events to
nodes:

`user_prompt → Prompt`, `model_response → Response`,
`tool_call_finished → ToolResult`/`ToolCall`,
`file_read`/`file_written → FileRef` + `Blob`,
`decision_recorded → Decision`, `memory_recorded → Memory`,
`checkpoint_requested`/`session_ended → Checkpoint`.
