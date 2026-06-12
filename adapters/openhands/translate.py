"""OpenHands trajectory (`SAVE_TRAJECTORY_PATH` JSON) -> host-neutral Envelope
translation.

Tier-4 (one-shot backfill, observe-only): import an OpenHands run. Pure mapping;
the `__main__` wrapper reads the file. Output shapes match
``crates/core/src/event.rs``.

OpenHands saves a trajectory as a JSON **array of events** (the event stream;
serialization in `openhands/events/serialization/`). Each event is either an
ACTION or an OBSERVATION:

    Action:
      { "id": 3, "timestamp": "2025-03-07T17:45:20Z", "source": "agent"|"user",
        "action": "message"|"run"|"run_ipython"|"edit"|"read"|"write"|"finish"|...,
        "args": { "content"?, "command"?, "code"?, "path"?, "thought"? },
        "message": "..." }
    Observation:
      { "id": 4, "timestamp": "...", "source": "agent"|"environment",
        "observation": "run"|"read"|"error"|...,
        "content": "...", "extras": {...}, "message": "..." }

Mapping:
    action message  + source user   -> user_prompt {text}
    action message  + source agent  -> model_response {text}
    action <tool>   with args.thought-> model_response {text=thought}  (the agent's reasoning)
    action <tool>                    -> tool_call_started {tool, args_json}
       edit/write                    -> + file_written {path}
       read                          -> + file_read {path}
    observation                      -> tool_call_finished {tool, ok}  (ok=false for error)

The OpenHands *visualizer* normalizes runs to `{ "entries": [ {type, content,
actorType, command} ] }`; that shape is tolerated too.
"""
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

_MAX_FIELD = 8192

# OpenHands action names that operate on a file path.
_WRITE_ACTIONS = {"edit", "write", "str_replace_editor"}
_READ_ACTIONS = {"read"}


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _ts(ev: Dict[str, Any]) -> str:
    t = ev.get("timestamp")
    if isinstance(t, str) and t:
        return t if "T" in t else "1970-01-01T00:00:00Z"
    return "1970-01-01T00:00:00Z"


def _native_event(out: List[Dict[str, Any]], host_id: str, sid: str, ev: Dict[str, Any]) -> None:
    ts = _ts(ev)
    source = ev.get("source")
    args = ev.get("args") if isinstance(ev.get("args"), dict) else {}

    if "action" in ev:
        action = ev.get("action")
        if action == "message":
            text = (args.get("content") or ev.get("message") or "").strip()
            if not text:
                return
            etype = "user_prompt" if source == "user" else "model_response"
            out.append(envelope(host_id, sid, ts, etype, text=text[:_MAX_FIELD]))
            return
        if action in ("finish", "system", "change_agent_state", "recall"):
            thought = (args.get("thought") or args.get("final_thought") or "").strip()
            if thought:
                out.append(envelope(host_id, sid, ts, "model_response", text=thought[:_MAX_FIELD]))
            return
        # A tool-ish action. Surface the agent's thought as reasoning, then the call.
        thought = (args.get("thought") or "").strip()
        if thought:
            out.append(envelope(host_id, sid, ts, "model_response", text=thought[:_MAX_FIELD]))
        out.append(envelope(host_id, sid, ts, "tool_call_started",
                            tool=str(action), args_json=json.dumps(args) if args else None))
        path = args.get("path")
        if path and action in _WRITE_ACTIONS:
            out.append(envelope(host_id, sid, ts, "file_written", path=str(path)))
        elif path and action in _READ_ACTIONS:
            out.append(envelope(host_id, sid, ts, "file_read", path=str(path)))
        return

    if "observation" in ev:
        obs = str(ev.get("observation"))
        ok = obs != "error" and not (isinstance(ev.get("extras"), dict)
                                     and ev["extras"].get("error"))
        out.append(envelope(host_id, sid, ts, "tool_call_finished", tool=obs, ok=ok))
        return


def _entries_event(out: List[Dict[str, Any]], host_id: str, sid: str, ev: Dict[str, Any]) -> None:
    """The visualizer's normalized `entries[]` shape."""
    ts = _ts(ev)
    etype = (ev.get("type") or "").lower()
    actor = (ev.get("actorType") or "").lower()
    content = (ev.get("content") or "").strip()
    if etype in ("message", "thought"):
        if not content:
            return
        kind = "user_prompt" if actor == "user" else "model_response"
        out.append(envelope(host_id, sid, ts, kind, text=content[:_MAX_FIELD]))
    elif etype == "command":
        cmd = ev.get("command")
        out.append(envelope(host_id, sid, ts, "tool_call_started",
                            tool="run", args_json=json.dumps({"command": cmd}) if cmd else None))


def translate(trajectory: Any, host_id: str = "openhands", session_id: str = "openhands-session") -> List[Dict[str, Any]]:
    """Translate one OpenHands trajectory (native array or `{entries:[...]}`)."""
    out: List[Dict[str, Any]] = []
    entries: Optional[List[Any]] = None
    native = True
    if isinstance(trajectory, dict):
        if isinstance(trajectory.get("entries"), list):
            entries, native = trajectory["entries"], False
        elif isinstance(trajectory.get("events"), list):
            entries = trajectory["events"]
        elif isinstance(trajectory.get("history"), list):
            entries = trajectory["history"]
    elif isinstance(trajectory, list):
        entries = trajectory
    if entries is None:
        return out

    first_ts = next((_ts(e) for e in entries if isinstance(e, dict)), "1970-01-01T00:00:00Z")
    out.append(envelope(host_id, session_id, first_ts, "session_started", cwd=None))
    for ev in entries:
        if not isinstance(ev, dict):
            continue
        (_native_event if native else _entries_event)(out, host_id, session_id, ev)
    last_ts = out[-1]["ts"] if len(out) > 1 else first_ts
    out.append(envelope(host_id, session_id, last_ts, "session_ended"))
    return out


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <trajectory.json> [more.json ...]  > events.jsonl", file=sys.stderr)
        print("  (an OpenHands SAVE_TRAJECTORY_PATH file); pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for path in argv:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        sid = path.rsplit("/", 1)[-1].rsplit(".", 1)[0] or "openhands-session"
        for env in translate(data, session_id=sid):
            sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
