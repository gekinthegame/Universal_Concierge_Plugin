# ChatGPT export adapter (Tier-4, one-shot backfill)

Imports your **ChatGPT** conversation history into the Universal Concierge Plugin's
host-neutral JSONL stream from a **data export** — observe-only, no proxy, no keys.

## Getting the data
In ChatGPT: **Settings → Data controls → Export data**. You'll get an email with a
zip; inside is `conversations.json` (a JSON array of all your conversations).

## What it does
For each conversation, it walks the message tree in turn order (parent links from
the active node back to the root):

| ChatGPT message | Concierge event |
|---|---|
| conversation start | `session_started` + `memory_recorded` (the title) |
| author role `user` | `user_prompt {text}` |
| author role `assistant` | `model_response {text}` |
| `system` / `tool` / empty | skipped |

Multimodal messages contribute only their text parts.

## Use
```sh
python3 translate.py /path/to/conversations.json | concierge-plugin ingest -
```
Content-addressed ingest de-dupes by CID, so re-importing is safe.

## Test
```sh
python3 test_translate.py     # 5 self-contained unit tests
```

## Grounding note
Built from the well-known, stable `conversations.json` export schema (mapping of
message nodes). No local export was available on the build machine to validate
against — confirm against your real export; the parser tolerates missing fields.
