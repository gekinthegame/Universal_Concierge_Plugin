"""Continue.dev session (`~/.continue/sessions/<id>.json`) -> host-neutral
Envelope translation.

Tier-4 (one-shot backfill, observe-only): import a Continue session. Pure mapping;
the `__main__` wrapper reads the file(s). Output shapes match
``crates/core/src/event.rs``.

A Continue session file (per `core/index.d.ts` `Session` / `ChatHistoryItem` /
`ChatMessage`) is:

    {
      "sessionId": "...", "title": "...", "workspaceDirectory": "/abs/path",
      "history": [
        {
          "message": { "role": "user|assistant|thinking|system|tool",
                       "content": "..." | [ {type:"text", text:"..."}, ... ] },
          "contextItems": [...],
          "toolCallStates": [ { "toolCall": { "function": { "name": "...",
                               "arguments": "{...}" } }, "output": [...] }, ... ]
        }, ...
      ]
    }

Mapping:

    workspaceDirectory       -> session_started {cwd}
    title                    -> memory_recorded
    role user                -> user_prompt {text}
    role assistant           -> model_response {text, reasoning?}   (reasoning from a paired 'thinking')
    role thinking            -> folded into the next assistant's reasoning
    toolCallStates[]         -> tool_call_started {tool, args_json} + tool_call_finished {tool, ok}
    role system / tool / empty -> skipped
"""
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

_MAX_FIELD = 8192


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _content_text(content: Any) -> Optional[str]:
    """Continue `content` is a string OR a list of MessageParts ({type:text,text})."""
    if isinstance(content, str):
        return content.strip() or None
    if isinstance(content, list):
        texts = []
        for part in content:
            if isinstance(part, str):
                texts.append(part)
            elif isinstance(part, dict) and isinstance(part.get("text"), str):
                texts.append(part["text"])
        joined = "".join(t for t in texts if t).strip()
        return joined or None
    return None


def _iso(value: Any) -> str:
    if value is None:
        return "1970-01-01T00:00:00Z"
    try:
        import datetime
        v = float(value)
        if v > 1e12:        # milliseconds
            v /= 1000.0
        return datetime.datetime.fromtimestamp(v, datetime.timezone.utc) \
            .strftime("%Y-%m-%dT%H:%M:%SZ")
    except (TypeError, ValueError, OverflowError):
        # already a string timestamp? pass it through if it looks ISO-ish
        s = str(value)
        return s if "T" in s else "1970-01-01T00:00:00Z"


def translate(session: Dict[str, Any], host_id: str = "continue") -> List[Dict[str, Any]]:
    """Translate one Continue session object into envelopes."""
    out: List[Dict[str, Any]] = []
    if not isinstance(session, dict):
        return out
    sid = session.get("sessionId") or session.get("id") or "continue-session"
    cwd = session.get("workspaceDirectory")
    ts = _iso(session.get("dateCreated") or session.get("createdAt"))

    out.append(envelope(host_id, sid, ts, "session_started", cwd=cwd or None))
    title = session.get("title")
    if title and title not in ("New Session", "New Chat"):
        out.append(envelope(host_id, sid, ts, "memory_recorded", text=f"Continue session: {title}"))

    history = session.get("history")
    if not isinstance(history, list):
        history = []

    pending_reasoning: Optional[Dict[str, Any]] = None
    for item in history:
        if not isinstance(item, dict):
            continue
        message = item.get("message") if isinstance(item.get("message"), dict) else item
        role = message.get("role")
        text = _content_text(message.get("content"))

        if role == "thinking":
            if text:
                # Continue's 'thinking' role is genuine model reasoning tokens.
                pending_reasoning = {"text": text[:_MAX_FIELD], "source": "thinking"}
            continue
        if role == "user":
            if text:
                out.append(envelope(host_id, sid, ts, "user_prompt", text=text[:_MAX_FIELD]))
        elif role == "assistant":
            if text:
                out.append(envelope(host_id, sid, ts, "model_response",
                                    text=text[:_MAX_FIELD],
                                    reasoning=pending_reasoning))
            pending_reasoning = None
        # system / tool messages: skipped (tool *calls* handled below)

        for call in _tool_calls(item):
            out.append(envelope(host_id, sid, ts, "tool_call_started",
                                tool=call["name"], args_json=call.get("args_json")))
            out.append(envelope(host_id, sid, ts, "tool_call_finished",
                                tool=call["name"], ok=call.get("ok", True)))

    out.append(envelope(host_id, sid, ts, "session_ended"))
    return out


def _tool_calls(item: Dict[str, Any]) -> List[Dict[str, Any]]:
    calls: List[Dict[str, Any]] = []
    states = item.get("toolCallStates")
    if not isinstance(states, list):
        return calls
    for state in states:
        if not isinstance(state, dict):
            continue
        fn = (((state.get("toolCall") or {}).get("function")) or {})
        name = fn.get("name")
        if not name:
            continue
        args = fn.get("arguments")
        status = state.get("status")
        calls.append({
            "name": name,
            "args_json": args if isinstance(args, str) else (json.dumps(args) if args is not None else None),
            "ok": status not in ("errored", "canceled"),
        })
    return calls


def _iter_sessions(data: Any) -> List[Dict[str, Any]]:
    """A single session object, an array of sessions, or {sessions:[...]}."""
    if isinstance(data, dict):
        if isinstance(data.get("sessions"), list):
            return [s for s in data["sessions"] if isinstance(s, dict)]
        if "history" in data or "sessionId" in data:
            return [data]
        return []
    if isinstance(data, list):
        return [s for s in data if isinstance(s, dict)]
    return []


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <session.json> [more.json ...]  > events.jsonl", file=sys.stderr)
        print("  (e.g. ~/.continue/sessions/<id>.json); pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for path in argv:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        for session in _iter_sessions(data):
            for env in translate(session):
                sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
