# Gemini CLI adapter (Tier-4, observe-only)

Translates **Gemini CLI** chat sessions into the Universal Concierge Plugin's
host-neutral JSONL stream. **Observe-only** — reads Gemini's own session files,
never proxies traffic or touches API keys (Fidelity Ladder Tier 4; Decision 0009).

## What it reads
Gemini CLI records sessions to `~/.gemini/tmp/<project>/chats/session-*.jsonl`
(newline-delimited JSON).

| Gemini record | Concierge event |
|---|---|
| header `{sessionId, startTime, …}` | `session_started` |
| `type:"user"`, `content:[{text}]` | `user_prompt {text}` |
| `type:"gemini"`, `content`, `thoughts` | `model_response {text, reasoning}` (thoughts → `reasoning{source:"thinking"}`) |
| `type:"gemini"`, `toolCalls:[{name,args,result,status}]` | `tool_call_started` + `tool_call_finished` per call |
| `{"$set":…}` stream deltas, `type:"info"` | ignored |

## Use
```sh
python3 translate.py ~/.gemini/tmp/*/chats/session-*.jsonl | concierge-plugin ingest -
```
Content-addressed ingest de-dupes by CID, so re-runs are safe.

## Test
```sh
python3 test_translate.py     # 7 self-contained unit tests
```
Verified against a real session file (0 invalid envelopes).
