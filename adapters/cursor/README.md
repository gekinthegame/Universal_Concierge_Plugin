# Cursor adapter (Tier-4, observe-only)

Translates **Cursor** chat history into the Universal Concierge Plugin's
host-neutral JSONL stream. **Observe-only** — reads Cursor's local SQLite chat
database, never proxies traffic or touches API keys (Fidelity Ladder Tier 4).

## What it reads
Cursor stores chats in `state.vscdb` (SQLite), table `cursorDiskKV`:
- `composerData:<composerId>` — a conversation's metadata + bubble ordering
- `bubbleId:<composerId>:<bubbleId>` — one message bubble (`type` 1=user, 2=assistant)

Default location (macOS): `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
(Linux: `~/.config/Cursor/...`). Older workspace DBs under `ItemTable` are read too.

| Cursor bubble | Concierge event |
|---|---|
| `type:1` (user) | `user_prompt {text}` |
| `type:2` (assistant) | `model_response {text}` |
| `toolFormerData` | `tool_call_started` + `tool_call_finished` |

## Use
```sh
python3 translate.py | concierge-plugin ingest -                 # default globalStorage DB
python3 translate.py /path/to/state.vscdb | concierge-plugin ingest -
```

## Test
```sh
python3 test_translate.py     # 4 tests, incl. a synthesized state.vscdb round-trip
```

## Note on grounding
Built from Cursor's community-documented `cursorDiskKV` schema (the format used by
`cursor-chat-export` / `cursor-history`) and proven against a synthesized
`state.vscdb`. Cursor was not installed on the build machine; confirm against your
real DB once available — the reader tolerates field/version drift.
