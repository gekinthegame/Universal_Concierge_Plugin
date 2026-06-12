"""Codex CLI ``rollout-*.jsonl`` -> host-neutral Envelope translation.

Tier-4 (observe-only): parse Codex's own session transcript, no proxy, no keys.
Pure mapping (no I/O here, so it is unit-testable); the ``__main__`` wrapper reads
the file. Output shapes match ``crates/core/src/event.rs``:

    {"host_id", "session_id", "ts", "type", <event fields...>}

Schema confirmed against real ``~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl``.
Each line is ``{"type", "timestamp", "payload"}``. We use the ``response_item``
stream — the canonical API transcript — plus ``event_msg/patch_apply_end`` for
file edits, and ignore the parallel ``event_msg`` user/agent mirrors so nothing
is double-counted:

    session_meta                          -> session_started {cwd}
    response_item/message role=user       -> user_prompt {text}
    response_item/message role=assistant  -> model_response {text, reasoning?}
    response_item/reasoning               -> held, attached to the next assistant
                                             message as reasoning {source:"thinking"}
                                             (the readable summary; the raw chain is
                                             encrypted, so we never invent it)
    response_item/function_call           -> tool_call_started {tool, args_json}
    response_item/custom_tool_call        -> tool_call_started {tool, args_json}
    response_item/function_call_output     -> tool_call_finished {tool, ok, result_json}
    response_item/custom_tool_call_output  -> tool_call_finished {tool, ok, result_json}
    event_msg/patch_apply_end             -> file_written {path} per change
    (end of file)                         -> session_ended
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


def _project_id(cwd):
    if not cwd:
        return None
    base = os.path.basename(os.path.normpath(cwd))
    return base or None


def _text_of(content: Any) -> Optional[str]:
    """Join the text of an OpenAI Responses ``content`` (str, or list of blocks
    like ``{"type":"input_text"/"output_text"/"text","text":...}``)."""
    if isinstance(content, str):
        return content or None
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict):
                t = block.get("type") or ""
                if t.endswith("text") and block.get("text"):
                    parts.append(block["text"])
            elif isinstance(block, str):
                parts.append(block)
        joined = "\n".join(parts)
        return joined or None
    return None


def _summary_text(summary: Any) -> Optional[str]:
    """The readable reasoning summary (list of ``{type:"summary_text","text"}``)."""
    if isinstance(summary, list):
        parts = [b.get("text") for b in summary if isinstance(b, dict) and b.get("text")]
        joined = "\n".join(p for p in parts if p)
        return joined or None
    if isinstance(summary, str):
        return summary or None
    return None


def _compact(value: Any) -> Optional[str]:
    if value is None:
        return None
    if isinstance(value, str):
        return value[:_MAX_FIELD]
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))[:_MAX_FIELD]
    except (TypeError, ValueError):
        return str(value)[:_MAX_FIELD]


def translate(records: Iterable[Dict[str, Any]], host_id: str = "codex",
              session_id: Optional[str] = None) -> List[Dict[str, Any]]:
    """Map an iterable of parsed rollout records to host-neutral envelopes."""
    out: List[Dict[str, Any]] = []
    pid: Optional[str] = None
    sid = session_id
    call_names: Dict[str, str] = {}      # call_id -> tool name (resolve outputs)
    pending_reasoning: Optional[Dict[str, Any]] = None
    started = False

    def ts_of(rec):
        return rec.get("timestamp") or "1970-01-01T00:00:00Z"

    for rec in records:
        if not isinstance(rec, dict):
            continue
        rtype = rec.get("type")
        payload = rec.get("payload") or {}
        ts = ts_of(rec)

        if rtype == "session_meta":
            cwd = payload.get("cwd")
            pid = _project_id(cwd)
            sid = sid or payload.get("id")
            out.append(envelope(host_id, sid or "codex-session", ts, "session_started",
                                 project_id=pid, cwd=cwd))
            started = True
            continue

        if rtype != "response_item" and not (
            rtype == "event_msg" and payload.get("type") == "patch_apply_end"
        ):
            continue  # token_count, task_*, turn_context, user/agent mirrors, etc.

        ptype = payload.get("type")
        sid_now = sid or "codex-session"

        if ptype == "reasoning":
            pending_reasoning = None
            summary = _summary_text(payload.get("summary"))
            if summary:
                pending_reasoning = {"text": summary[:_MAX_FIELD], "source": "thinking"}
        elif ptype == "message":
            role = payload.get("role")
            text = _text_of(payload.get("content"))
            if not text:
                continue
            if role == "user":
                out.append(envelope(host_id, sid_now, ts, "user_prompt",
                                    project_id=pid, text=text))
                pending_reasoning = None
            elif role == "assistant":
                out.append(envelope(host_id, sid_now, ts, "model_response",
                                    project_id=pid, text=text, reasoning=pending_reasoning))
                pending_reasoning = None
        elif ptype in ("function_call", "custom_tool_call"):
            name = payload.get("name") or "tool"
            call_id = payload.get("call_id")
            if call_id:
                call_names[call_id] = name
            args = payload.get("arguments") if ptype == "function_call" else payload.get("input")
            out.append(envelope(host_id, sid_now, ts, "tool_call_started",
                                project_id=pid, tool=name, args_json=_compact(args)))
        elif ptype in ("function_call_output", "custom_tool_call_output"):
            call_id = payload.get("call_id")
            name = call_names.get(call_id, "tool")
            output = payload.get("output")
            out.append(envelope(host_id, sid_now, ts, "tool_call_finished",
                                project_id=pid, tool=name, ok=not _looks_error(output),
                                result_json=_compact(output)))
        elif rtype == "event_msg" and ptype == "patch_apply_end":
            ok = bool(payload.get("success", True))
            for path in _changed_paths(payload.get("changes")):
                out.append(envelope(host_id, sid_now, ts, "file_written",
                                    project_id=pid, path=path))
            # also a tool_call_finished for the patch, so the edit shows as an action
            out.append(envelope(host_id, sid_now, ts, "tool_call_finished",
                                project_id=pid, tool="apply_patch", ok=ok,
                                result_json=_compact(payload.get("stdout") or payload.get("stderr"))))

    if started:
        out.append(envelope(host_id, sid or "codex-session",
                            "1970-01-01T00:00:00Z", "session_ended", project_id=pid))
    return out


def _changed_paths(changes: Any) -> List[str]:
    if isinstance(changes, dict):
        return [p for p in changes.keys() if isinstance(p, str)]
    if isinstance(changes, list):
        out = []
        for c in changes:
            if isinstance(c, dict):
                p = c.get("path") or c.get("file") or c.get("file_path")
                if p:
                    out.append(p)
            elif isinstance(c, str):
                out.append(c)
        return out
    return []


def _looks_error(output: Any) -> bool:
    if isinstance(output, dict):
        if output.get("is_error") or output.get("error"):
            return True
        code = output.get("exit_code")
        if isinstance(code, int) and code != 0:
            return True
    return False


def _session_id_from_path(path: str) -> Optional[str]:
    """The UUID embedded in ``rollout-<ts>-<uuid>.jsonl``."""
    base = os.path.basename(path)
    stem = base[len("rollout-"):-len(".jsonl")] if base.startswith("rollout-") else base
    parts = stem.split("-")
    if len(parts) >= 5:
        return "-".join(parts[-5:])
    return None


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <rollout-*.jsonl> [more...]  > events.jsonl", file=sys.stderr)
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
        for env in translate(records, session_id=_session_id_from_path(path)):
            sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
