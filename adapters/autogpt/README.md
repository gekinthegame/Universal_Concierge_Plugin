# AutoGPT adapter (Tier-4, one-shot backfill)

Imports an **AutoGPT** run into the Universal Concierge Plugin's host-neutral JSONL
stream — observe-only, no proxy, no keys.

## Where the data is
AutoGPT (classic) drives the model in a loop; each cycle the model returns a
structured JSON response (`thoughts{…}` + `command{name,args}`). Capture those
cycle responses to a file (a JSON array, JSONL, or a run wrapper with a
`history`/`cycles` array). Agent state lives under `data/agents/<name>/` and a
human-readable trace under `logs/` — this adapter reads the **structured JSON**.

## What it does

| AutoGPT field | Concierge event |
|---|---|
| `ai_name` | `memory_recorded` |
| `ai_goals[]` / `task` | `user_prompt {text}` (the human's objective) |
| `thoughts.speak` / `.text` | `model_response {text, reasoning}` |
| ‎ — `reasoning` = `reasoning` + `plan` + `criticism` | |
| `command {name, args}` | `tool_call_started {tool, args_json}` |
| `write_to_file` / `write_file` (with a filename) | + `file_written {path}` |
| `read_file` (with a filename) | + `file_read {path}` |
| `task_complete` / `finish` / `do_nothing` | no tool call |

## Use
```sh
python3 translate.py run.json | concierge-plugin ingest -      # array or {history:[…]}
python3 translate.py run.jsonl | concierge-plugin ingest -     # one cycle per line
```
Content-addressed ingest de-dupes by CID, so re-importing is safe.

## Test
```sh
python3 test_translate.py     # 8 self-contained unit tests
```

## Grounding note
Built from AutoGPT's documented cycle-response contract (`thoughts.{text,reasoning,
plan,criticism,speak}` + `command.{name,args}`). No local AutoGPT run was on the
build machine to validate against — confirm against your real run; the parser
accepts a JSON array, JSONL, a single cycle, or a `{ai_name, ai_goals, history:[…]}`
wrapper. AutoGPT command names vary by version; file detection covers the common
`write_to_file`/`read_file` set — extend `_WRITE_CMDS`/`_READ_CMDS` if yours differ.
