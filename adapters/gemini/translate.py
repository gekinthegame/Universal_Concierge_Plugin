"""Gemini CLI chat session -> host-neutral Envelope translation.

Tier-4 (observe-only): parse Gemini CLI's own session files, no proxy, no keys.
Pure mapping; the ``__main__`` wrapper reads the file. Output shapes match
``crates/core/src/event.rs``.

Schema confirmed against real ``~/.gemini/tmp/<project>/chats/session-*.jsonl``.
Lines are newline-delimited JSON:

    {sessionId, projectHash, startTime, lastUpdated, kind}   (header)  -> session_started
    {type:"user",   content:[{text}], ...}                             -> user_prompt {text}
    {type:"gemini", content:"…", thoughts:[{subject,description}],
                    toolCalls:[{name,args,result,status}], ...}        -> tool_call_started/
                                                                          tool_call_finished
                                                                          + model_response
                                                                          {text, reasoning?}
    {"$set":{…}}                                              (stream delta)  -> ignored
    {type:"info", …}                                          (system note)   -> ignored
    (end of file)                                                            -> session_ended

``thoughts`` are Gemini's reasoning summaries — attached to the response as
``reasoning {source:"thinking"}``. ``toolCalls`` bundle the call *and* its result,
so each becomes a started + finished pair.
"""
from __future__ import annotations

import json
import os
from typing import Any, Dict, Iterable, List, Optional

_MAX_FIELD = 8192


def envelope(host_id, session_id, ts, event_type, project_id=None, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    if project_id:
        env["project_id"] = project_id
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _parts_text(content: Any) -> Optional[str]:
    """User content is a list of ``{text}`` parts; gemini content is a plain str."""
    if isinstance(content, str):
        return content or None
    if isinstance(content, list):
        parts = [b.get("text") for b in content if isinstance(b, dict) and b.get("text")]
        joined = "\n".join(p for p in parts if p)
        return joined or None
    return None


def _reasoning(thoughts: Any) -> Optional[Dict[str, Any]]:
    if not isinstance(thoughts, list) or not thoughts:
        return None
    lines = []
    for t in thoughts:
        if isinstance(t, dict):
            subject = (t.get("subject") or "").strip()
            desc = (t.get("description") or "").strip()
            line = f"{subject}: {desc}" if subject and desc else (subject or desc)
            if line:
                lines.append(line)
    text = "\n".join(lines)
    return {"text": text[:_MAX_FIELD], "source": "thinking"} if text else None


def _compact(value: Any) -> Optional[str]:
    if value is None:
        return None
    if isinstance(value, str):
        return value[:_MAX_FIELD]
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))[:_MAX_FIELD]
    except (TypeError, ValueError):
        return str(value)[:_MAX_FIELD]


def translate(records: Iterable[Dict[str, Any]], host_id: str = "gemini",
              session_id: Optional[str] = None,
              project_id: Optional[str] = None) -> List[Dict[str, Any]]:
    out: List[Dict[str, Any]] = []
    sid = session_id
    pid = project_id
    started = False

    for rec in records:
        if not isinstance(rec, dict):
            continue
        if "$set" in rec:
            continue  # streaming state delta
        rtype = rec.get("type")
        ts = rec.get("timestamp") or rec.get("startTime") or "1970-01-01T00:00:00Z"

        if rtype is None and rec.get("sessionId"):
            sid = sid or rec.get("sessionId")
            out.append(envelope(host_id, sid or "gemini-session", ts, "session_started",
                                project_id=pid))
            started = True
            continue

        sid_now = sid or "gemini-session"
        if rtype == "user":
            text = _parts_text(rec.get("content"))
            if text:
                out.append(envelope(host_id, sid_now, ts, "user_prompt",
                                    project_id=pid, text=text))
        elif rtype == "gemini":
            for call in rec.get("toolCalls") or []:
                if not isinstance(call, dict):
                    continue
                name = call.get("name") or "tool"
                out.append(envelope(host_id, sid_now, call.get("timestamp") or ts,
                                    "tool_call_started", project_id=pid, tool=name,
                                    args_json=_compact(call.get("args"))))
                status = (call.get("status") or "").lower()
                ok = status not in ("error", "failed", "cancelled")
                result = call.get("resultDisplay") or call.get("result")
                out.append(envelope(host_id, sid_now, call.get("timestamp") or ts,
                                    "tool_call_finished", project_id=pid, tool=name,
                                    ok=ok, result_json=_compact(result)))
            text = _parts_text(rec.get("content"))
            if text:
                out.append(envelope(host_id, sid_now, ts, "model_response",
                                    project_id=pid, text=text,
                                    reasoning=_reasoning(rec.get("thoughts"))))
        # type == "info" and anything else: ignored

    if started:
        out.append(envelope(host_id, sid or "gemini-session",
                            "1970-01-01T00:00:00Z", "session_ended", project_id=pid))
    return out


def _project_from_path(path: str) -> Optional[str]:
    """The readable project dir under ``~/.gemini/tmp/<project>/chats/…``."""
    parts = os.path.normpath(path).split(os.sep)
    if "tmp" in parts:
        i = parts.index("tmp")
        if i + 1 < len(parts):
            name = parts[i + 1]
            # skip if it's an opaque 64-hex hash
            if not (len(name) == 64 and all(c in "0123456789abcdef" for c in name)):
                return name
    return None


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <session-*.jsonl> [more...]  > events.jsonl", file=sys.stderr)
        print("  pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for path in argv:
        records = []
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except (json.JSONDecodeError, ValueError):
                    continue
        for env in translate(records, project_id=_project_from_path(path)):
            sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
