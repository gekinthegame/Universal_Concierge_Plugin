"""AutoGPT agent run (cycle responses JSON) -> host-neutral Envelope translation.

Tier-4 (one-shot backfill, observe-only): import an AutoGPT run. Pure mapping; the
`__main__` wrapper reads the file. Output shapes match ``crates/core/src/event.rs``.

AutoGPT (classic) drives the model in a loop; each cycle the model returns a
structured JSON response:

    {
      "thoughts": {
        "text": "...", "reasoning": "...", "plan": "- a\n- b",
        "criticism": "...", "speak": "..."
      },
      "command": { "name": "write_to_file", "args": { "filename": "x.py", "text": "..." } }
    }

This adapter accepts the run as:
  * a JSON **array** of such cycle responses, or
  * a single cycle object, or
  * a wrapper `{ "ai_name"?, "ai_goals"?|"task"?, "history"|"cycles"|"messages": [...] }`.

Mapping:
    ai_goals[] / task          -> user_prompt {text}   (the human's objective)
    thoughts.text/.speak       -> model_response {text, reasoning}
       reasoning = reasoning + plan + criticism, joined
    command {name,args}        -> tool_call_started {tool, args_json}
       write_to_file/write_file -> + file_written {path}
       read_file/read           -> + file_read {path}
       task_complete/finish/do_nothing -> no tool call
"""
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

_MAX_FIELD = 8192

_WRITE_CMDS = {"write_to_file", "write_file", "append_to_file"}
_READ_CMDS = {"read_file", "read"}
_TERMINAL_CMDS = {"task_complete", "finish", "do_nothing", "goals_accomplished"}


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _reasoning(thoughts: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    """The host-neutral reasoning object, or None. AutoGPT's thoughts are rationale
    the model writes alongside its action (not dedicated thinking tokens), so the
    source is honestly `inline_preamble`."""
    parts = []
    for key in ("reasoning", "plan", "criticism"):
        val = thoughts.get(key)
        if isinstance(val, str) and val.strip():
            parts.append(f"{key}: {val.strip()}")
    joined = "\n".join(parts).strip()
    return {"text": joined[:_MAX_FIELD], "source": "inline_preamble"} if joined else None


def _file_arg(args: Dict[str, Any]) -> Optional[str]:
    for key in ("filename", "file", "path", "filepath"):
        val = args.get(key)
        if isinstance(val, str) and val:
            return val
    return None


def _cycle(out: List[Dict[str, Any]], host_id: str, sid: str, ts: str, cycle: Dict[str, Any]) -> None:
    thoughts = cycle.get("thoughts") if isinstance(cycle.get("thoughts"), dict) else {}
    text = (thoughts.get("speak") or thoughts.get("text") or "").strip()
    if text:
        out.append(envelope(host_id, sid, ts, "model_response",
                            text=text[:_MAX_FIELD], reasoning=_reasoning(thoughts)))

    command = cycle.get("command")
    if isinstance(command, dict) and command.get("name"):
        name = str(command["name"])
        args = command.get("args") if isinstance(command.get("args"), dict) else {}
        if name in _TERMINAL_CMDS:
            return
        out.append(envelope(host_id, sid, ts, "tool_call_started",
                            tool=name, args_json=json.dumps(args) if args else None))
        path = _file_arg(args)
        if path and name in _WRITE_CMDS:
            out.append(envelope(host_id, sid, ts, "file_written", path=path))
        elif path and name in _READ_CMDS:
            out.append(envelope(host_id, sid, ts, "file_read", path=path))


def _cycles(data: Any) -> (List[Dict[str, Any]], Dict[str, Any]):
    """Return (list of cycle dicts, run-metadata dict)."""
    meta: Dict[str, Any] = {}
    if isinstance(data, list):
        return [c for c in data if isinstance(c, dict)], meta
    if isinstance(data, dict):
        meta = data
        for key in ("history", "cycles", "messages", "responses"):
            if isinstance(data.get(key), list):
                return [c for c in data[key] if isinstance(c, dict)], meta
        if "thoughts" in data or "command" in data:   # a single cycle
            return [data], meta
    return [], meta


def translate(data: Any, host_id: str = "autogpt", session_id: str = "autogpt-session") -> List[Dict[str, Any]]:
    out: List[Dict[str, Any]] = []
    cycles, meta = _cycles(data)
    ts = "1970-01-01T00:00:00Z"

    out.append(envelope(host_id, session_id, ts, "session_started", cwd=None))

    name = meta.get("ai_name")
    if isinstance(name, str) and name:
        out.append(envelope(host_id, session_id, ts, "memory_recorded", text=f"AutoGPT agent: {name}"))
    goals = meta.get("ai_goals") or meta.get("goals")
    task = meta.get("task")
    if isinstance(goals, list):
        for goal in goals:
            if isinstance(goal, str) and goal.strip():
                out.append(envelope(host_id, session_id, ts, "user_prompt", text=goal.strip()[:_MAX_FIELD]))
    elif isinstance(task, str) and task.strip():
        out.append(envelope(host_id, session_id, ts, "user_prompt", text=task.strip()[:_MAX_FIELD]))

    for cycle in cycles:
        _cycle(out, host_id, session_id, ts, cycle)

    out.append(envelope(host_id, session_id, ts, "session_ended"))
    return out


def _load(path: str) -> Any:
    """Accept a JSON document, or JSONL (one cycle response per line)."""
    with open(path, encoding="utf-8") as fh:
        raw = fh.read()
    raw_stripped = raw.lstrip()
    if raw_stripped[:1] in ("[", "{"):
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            pass
    cycles = []
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            cycles.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return cycles


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <run.json|run.jsonl>  > events.jsonl", file=sys.stderr)
        print("  (AutoGPT cycle responses); pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for path in argv:
        sid = path.rsplit("/", 1)[-1].rsplit(".", 1)[0] or "autogpt-session"
        for env in translate(_load(path), session_id=sid):
            sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
