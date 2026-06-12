# Continue.dev adapter (Tier-4, one-shot backfill)

Imports a **Continue.dev** session into the Universal Concierge Plugin's
host-neutral JSONL stream — observe-only, no proxy, no keys.

## Where the data is
Continue stores each conversation as `~/.continue/sessions/<sessionId>.json`
(indexed by `~/.continue/sessions/sessions.json`).

## What it does
Reads the session's `history` (per Continue's `core/index.d.ts` `Session` /
`ChatHistoryItem` / `ChatMessage` types):

| Continue field | Concierge event |
|---|---|
| `workspaceDirectory` | `session_started {cwd}` |
| `title` | `memory_recorded` |
| message role `user` | `user_prompt {text}` |
| message role `assistant` | `model_response {text, reasoning?}` |
| message role `thinking` | folded into the next assistant's `reasoning` |
| `toolCallStates[]` | `tool_call_started` + `tool_call_finished {ok}` |
| role `system` / `tool` / empty | skipped |

`content` may be a string or an array of `{type:"text", text}` parts — both are handled.

## Use
```sh
# one session
python3 translate.py ~/.continue/sessions/<id>.json | concierge-plugin ingest -
# all of them
python3 translate.py ~/.continue/sessions/*.json | concierge-plugin ingest -
```
Content-addressed ingest de-dupes by CID, so re-importing is safe.

## Test
```sh
python3 test_translate.py     # 7 self-contained unit tests
```

## Grounding note
Built from Continue's published `Session`/`ChatHistoryItem`/`ChatMessage` type
definitions (`core/index.d.ts`). No local `~/.continue/sessions` was on the build
machine to validate against — confirm against your real sessions; the parser
accepts a single session object, an array, or a `{sessions:[…]}` wrapper, and
string-or-parts content.
