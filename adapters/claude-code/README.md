# Concierge adapter for Claude Code (hook-driven)

Captures a live Claude Code session into Concierge IPLD memory by listening to
Claude Code's **hook events**. It is **observe-only**: every hook reads the event,
records it, and exits 0 — it never blocks a prompt, a tool call, or the session.

## How it works

```
Claude Code hook (JSON on stdin)
        │
        ▼
concierge_hook.py ──> translate.py  (host-neutral envelope)
        │
        ▼
<workdir>/.concierge/claude-code-sessions/<session_id>.jsonl   (append)
        │
        ▼   (on Stop / SessionEnd)
concierge-plugin ingest <session>.jsonl  ──>  IPLD memory + scoped names
```

Hook → host-neutral event mapping:

| Claude Code hook | Concierge event |
|---|---|
| `SessionStart` | `session_started {cwd}` |
| `UserPromptSubmit` | `user_prompt {text}` |
| `PreToolUse` | `tool_call_started {tool, args_json}` |
| `PostToolUse` | `tool_call_finished {tool, ok, result_json}` (+ `file_read`/`file_written` for `Read`/`Write`/`Edit`/`MultiEdit`/`NotebookEdit`) |
| `Stop` | `model_response {text}` (the last assistant turn, read from the transcript) |
| `SessionEnd` | `session_ended` |

## Install

1. Build the plugin once (debug is fine):

   ```
   cargo build -p concierge-plugin
   ```

   You also need a `mem` binary on `PATH` (the IPLD store the plugin shells out to).

2. Merge `settings.snippet.json` into your Claude Code settings — either
   `~/.claude/settings.json` (all projects) or `<project>/.claude/settings.json`
   (one project). It registers the same hook command on all six events. The
   snippet already points `CONCIERGE_BIN` at this repo's debug binary and the
   absolute path to `concierge_hook.py`, so no `PATH` changes are needed. Edit
   those two absolute paths if you move the repo.

3. Start a Claude Code session and work normally. **The Data Platter opens
   automatically** on `SessionStart` (like the Hermes adapter), and memory begins
   accumulating under the project's `.concierge/` after the first assistant turn.

   The GUI launch is **idempotent**: `concierge-plugin gui` reuses an existing
   Data Platter for the same store instead of spawning a duplicate, so opening
   many sessions in one project never piles up servers. (Set
   `CONCIERGE_GUI_DISABLE=1` to skip the auto-open, or `CONCIERGE_GUI_NO_OPEN=1`
   to serve without popping a browser tab.)

4. Re-open it any time with `concierge-plugin gui` in the project dir.

## Where memory lands

By default the store is the **project directory** Claude Code reports as `cwd`,
so each project gets its own `.concierge/`. The raw event trail is kept at
`.concierge/claude-code-sessions/<session_id>.jsonl` (re-ingest is idempotent, so
it is safe to replay).

## Environment overrides

| Variable | Effect |
|---|---|
| `CONCIERGE_WORKDIR` | Force a single central store instead of per-project `cwd`. |
| `CONCIERGE_BIN` | Path to `concierge-plugin` (the snippet sets this). |
| `CONCIERGE_INGEST_EVERY_TURN` | `0` to ingest only at `SessionEnd` instead of after every turn (lower overhead on long sessions). |
| `CONCIERGE_GUI_DISABLE` | `1` to not auto-open the Data Platter on session start. |
| `CONCIERGE_GUI_NO_OPEN` | `1` to serve the Data Platter without opening a browser tab. |
| `CONCIERGE_GUI_PORT` | Preferred port (default `4173`; auto-scans upward if taken). |

## Notes & limits

- **Observe-only.** The hook never returns a blocking decision; a failure (even a
  missing `concierge-plugin`) just leaves the JSONL trail and exits 0.
- **`model_response`** is recovered by reading the session transcript on `Stop`;
  if the transcript is unavailable the turn's text is simply skipped.
- **Per-turn ingest** re-reads the growing session file each turn (idempotent).
  Set `CONCIERGE_INGEST_EVERY_TURN=0` for very long sessions.
- Tool inputs/results are stored compactly and truncated to 8 KB per field.
- This is a manual mount against a debug build — not a packaged installer.

Run the translation unit tests with `python3 test_translate.py`.
