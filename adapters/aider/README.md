# Aider adapter (Tier-4, one-shot backfill)

Imports an **Aider** session transcript into the Universal Concierge Plugin's
host-neutral JSONL stream — observe-only, no proxy, no keys.

## Where the data is
Aider appends a Markdown transcript to **`.aider.chat.history.md`** in the repo
you run it in (also `~/.aider.chat.history.md` for some setups).

## What it does
Parses the Markdown by line prefix (per aider's `io.py append_chat_history`):

| Aider line | Concierge event |
|---|---|
| `# aider chat started at <ts>` | new `session_started` (+ `session_ended` for the previous one) |
| `#### <text>` (consecutive lines merge) | `user_prompt {text}` |
| unprefixed prose / code blocks | `model_response {text}` |
| `> Applied edit to <path>` | `file_written {path}` |
| other `> …` console lines (tokens, warnings) | skipped |

## Use
```sh
python3 translate.py /path/to/repo/.aider.chat.history.md | concierge-plugin ingest -
```
Content-addressed ingest de-dupes by CID, so re-importing after more aider use is safe.

## Test
```sh
python3 test_translate.py     # 7 self-contained unit tests
```

## Grounding note
Built from aider's documented history format (`# aider chat started at`, `#### `
user prefix, `> ` blockquoted console lines), confirmed against aider's `io.py`
source. No local `.aider.chat.history.md` was on the build machine to validate
against — confirm against your real transcript; the parser tolerates blank lines,
missing timestamps, and multiple sessions per file.
