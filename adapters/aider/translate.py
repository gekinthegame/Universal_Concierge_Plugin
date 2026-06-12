"""Aider chat history (`.aider.chat.history.md`) -> host-neutral Envelope translation.

Tier-4 (one-shot backfill, observe-only): import an `aider` session log. Pure
mapping; the `__main__` wrapper reads the file. Output shapes match
``crates/core/src/event.rs``.

Aider appends a Markdown transcript as you work. The format (from aider's
`io.py append_chat_history`):

    # aider chat started at 2025-03-07 17:45:00      <- session boundary

    #### add a parser for jsonl                      <- USER input ('#### ' prefix)

    Here's a parser...                               <- ASSISTANT reply (no prefix)
    ```python
    ...
    ```

    > Applied edit to parser.py                      <- tool/console line ('> ' prefix)

Mapping:

    '# aider chat started at <ts>'  -> session_started (new session)
    '#### <text>'  (consecutive)    -> user_prompt {text}
    unprefixed prose (consecutive)  -> model_response {text}
    '> Applied edit to <path>'      -> file_written {path}
    other '> ' console lines        -> skipped (noise)
"""
from __future__ import annotations

import re
from typing import Any, Dict, List, Optional

_MAX_FIELD = 8192

_HEADER = re.compile(r"^# aider chat started at (.+?)\s*$")
_USER = re.compile(r"^#### ?(.*)$")
_QUOTE = re.compile(r"^> ?(.*)$")
_APPLIED = re.compile(r"^Applied edit to (.+?)\s*$")


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _iso(stamp: str) -> str:
    """'2025-03-07 17:45:00' -> ISO-8601 UTC; tolerant of odd/empty stamps."""
    stamp = (stamp or "").strip()
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"):
        try:
            import datetime
            return datetime.datetime.strptime(stamp, fmt) \
                .replace(tzinfo=datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        except ValueError:
            continue
    return "1970-01-01T00:00:00Z"


def translate(text: str, host_id: str = "aider") -> List[Dict[str, Any]]:
    """Translate one `.aider.chat.history.md` document into envelopes."""
    out: List[Dict[str, Any]] = []
    session_n = 0
    sid = "aider-0"
    ts = "1970-01-01T00:00:00Z"
    started = False

    kind: Optional[str] = None       # 'user' | 'assistant' currently accumulating
    buf: List[str] = []

    def open_session():
        nonlocal started
        if not started:
            out.append(envelope(host_id, sid, ts, "session_started", cwd=None))
            started = True

    def flush():
        nonlocal kind, buf
        joined = "\n".join(buf).strip()
        if joined and kind:
            open_session()
            etype = "user_prompt" if kind == "user" else "model_response"
            out.append(envelope(host_id, sid, ts, etype, text=joined[:_MAX_FIELD]))
        kind, buf = None, []

    for raw in text.splitlines():
        line = raw.rstrip("\n")

        header = _HEADER.match(line)
        if header:
            flush()
            if started:
                out.append(envelope(host_id, sid, ts, "session_ended"))
            session_n += 1
            sid = f"aider-{session_n}"
            ts = _iso(header.group(1))
            started = False
            continue

        quote = _QUOTE.match(line)
        if quote:
            flush()
            applied = _APPLIED.match(quote.group(1))
            if applied:
                open_session()
                out.append(envelope(host_id, sid, ts, "file_written", path=applied.group(1)))
            continue  # other console lines are skipped

        user = _USER.match(line)
        if user is not None and line.startswith("####"):
            if kind != "user":
                flush()
                kind = "user"
            buf.append(user.group(1))
            continue

        # Unprefixed line -> assistant prose (blank lines preserved within a block).
        if kind != "assistant":
            flush()
            kind = "assistant"
        buf.append(line)

    flush()
    if started:
        out.append(envelope(host_id, sid, ts, "session_ended"))
    return out


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <.aider.chat.history.md>  > events.jsonl", file=sys.stderr)
        print("  pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    import json
    for path in argv:
        with open(path, encoding="utf-8") as fh:
            for env in translate(fh.read()):
                sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
