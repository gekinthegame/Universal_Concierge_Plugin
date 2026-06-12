# Codex CLI adapter (Tier-4, observe-only)

Translates OpenAI **Codex CLI** session transcripts into the Universal Concierge
Plugin's host-neutral JSONL stream. **Observe-only** — it reads Codex's own session
files; it never proxies your model traffic and never touches API keys (Fidelity
Ladder Tier 4, the safest tier; Decision 0009 + Threat Model L6).

## What it reads
Codex writes every session to `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Each
line is `{"type","timestamp","payload"}`. This adapter uses the canonical
`response_item` transcript stream (+ `event_msg/patch_apply_end` for file edits)
and ignores the parallel `event_msg` UI mirrors so nothing is double-counted.

| Codex record | Concierge event |
|---|---|
| `session_meta` | `session_started {cwd}` |
| `response_item/message` role=user | `user_prompt {text}` |
| `response_item/message` role=assistant | `model_response {text, reasoning?}` |
| `response_item/reasoning` | attached to the next assistant message as `reasoning` (readable summary; the raw chain is encrypted, so it is never invented) |
| `response_item/function_call`, `custom_tool_call` | `tool_call_started {tool, args_json}` |
| `response_item/function_call_output`, `custom_tool_call_output` | `tool_call_finished {tool, ok, result_json}` |
| `event_msg/patch_apply_end` | `file_written {path}` per change + an `apply_patch` `tool_call_finished` |

## Use
```sh
# Translate one or more sessions and ingest them:
python3 translate.py ~/.codex/sessions/2026/06/05/rollout-*.jsonl | concierge-plugin ingest -

# Backfill everything:
python3 translate.py $(ls ~/.codex/sessions/*/*/*/rollout-*.jsonl) | concierge-plugin ingest -
```
Ingest is content-addressed, so re-running de-dupes by CID — safe to run on a tail.

## Test
```sh
python3 test_translate.py      # 9 self-contained unit tests, no pytest needed
```
Verified against a real 6,195-line session (→ 4,074 clean envelopes) and end-to-end
through `concierge-plugin ingest`.
