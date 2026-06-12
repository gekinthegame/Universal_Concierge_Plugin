# OpenHands adapter (Tier-4, one-shot backfill)

Imports an **OpenHands** run into the Universal Concierge Plugin's host-neutral
JSONL stream — observe-only, no proxy, no keys.

## Where the data is
Run OpenHands with a trajectory path set, e.g. CLI `--save-trajectory-path traj.json`
or env `SAVE_TRAJECTORY_PATH=traj.json`. It writes the event stream as a JSON array.
(The `entries`-shaped files produced by the OpenHands *trajectory-visualizer* also work.)

## What it does
Walks the event stream (per `openhands/events/serialization/`):

| OpenHands event | Concierge event |
|---|---|
| `action: message`, `source: user` | `user_prompt {text}` |
| `action: message`, `source: agent` | `model_response {text}` |
| any agent action with `args.thought` | `model_response {text}` (the agent's reasoning) |
| `action: run` / `run_ipython` / `edit` / … | `tool_call_started {tool, args_json}` |
| `action: edit` / `write` (with `args.path`) | + `file_written {path}` |
| `action: read` (with `args.path`) | + `file_read {path}` |
| an `observation` | `tool_call_finished {tool, ok}` (`ok:false` for `error`) |

## Use
```sh
python3 translate.py traj.json | concierge-plugin ingest -
```
Content-addressed ingest de-dupes by CID, so re-importing is safe.

## Test
```sh
python3 test_translate.py     # 7 self-contained unit tests
```

## Grounding note
Built from the OpenHands event model (action/observation events with
`source`/`args`/`observation`, serialized under `openhands/events/serialization/`)
and the visualizer's normalized `entries` shape. No local OpenHands trajectory was
on the build machine to validate against — confirm against your real trajectory;
the parser accepts a bare array, `{events:[…]}`, `{history:[…]}`, or `{entries:[…]}`.
