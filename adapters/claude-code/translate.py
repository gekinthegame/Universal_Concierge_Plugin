"""Pure Claude Code hook -> host-neutral Envelope translation (adapter core).

No Claude Code or Concierge imports here, so the mapping is unit-testable on its
own. Output shapes match ``crates/core/src/event.rs``:

    {"host_id", "session_id", "ts", "type", <event fields...>}

Mapping (Claude Code hook event -> host-neutral event):

    SessionStart       -> session_started {cwd?}
    UserPromptSubmit   -> user_prompt {text}
    PreToolUse         -> tool_call_started {tool, args_json?, reasoning?}
                          (reasoning = the thinking of the assistant message that
                          issued this tool_use; the hook resolves it from the
                          transcript and passes it in)
    PostToolUse        -> tool_call_finished {tool, ok, result_json?}
                          (+ file_read / file_written for file tools)
    Stop               -> model_response {text, reasoning?}  (built by the hook from
                          the transcript; reasoning = the turn's thinking blocks)
    SessionEnd         -> session_ended

Reasoning capture: Claude Code transcripts carry the model's extended-thinking
blocks (``{"type":"thinking","thinking":...}``) alongside the response text. We
attach that thinking to the same ``model_response`` envelope as
``reasoning = {"text", "source":"thinking"}`` so the "why" rides along with the
step in one record (see ``ADAPTER_CONTRACT.md`` and DECISIONS.md 0023).
"""
from __future__ import annotations

import json
import os
from typing import Any, Dict, List, Optional

# Claude Code file tools -> was the file written (vs. read)?
FILE_TOOLS = {
    "Read": False,
    "Write": True,
    "Edit": True,
    "MultiEdit": True,
    "NotebookEdit": True,
}

# Cap embedded JSON so a giant tool result never bloats a memory node.
_MAX_FIELD = 8192


def envelope(
    host_id: str,
    session_id: str,
    ts: str,
    event_type: str,
    project_id: Optional[str] = None,
    **fields: Any,
) -> Dict[str, Any]:
    """Build a host-neutral envelope; ``None`` fields are omitted."""
    env: Dict[str, Any] = {
        "host_id": host_id,
        "session_id": session_id,
        "ts": ts,
        "type": event_type,
    }
    if project_id:
        env["project_id"] = project_id
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def project_id(cwd: Optional[str]) -> Optional[str]:
    """A short project label from the working directory's basename."""
    if not cwd:
        return None
    base = os.path.basename(os.path.normpath(cwd))
    return base or None


def model_response(host_id, session_id, ts, text, project_id=None, reasoning=None):
    return envelope(
        host_id, session_id, ts, "model_response",
        project_id=project_id, text=text, reasoning=reasoning,
    )


def reasoning_block(thinking: Optional[str], source: str = "thinking") -> Optional[Dict[str, Any]]:
    """A host-neutral ``reasoning`` object, or ``None`` when there is no thinking.

    ``source`` stays honest: ``"thinking"`` is genuine extended-thinking tokens.
    Never label inline preamble as thinking (ADAPTER_CONTRACT.md).
    """
    if not thinking:
        return None
    return {"text": thinking[:_MAX_FIELD], "source": source}


def from_hook_event(
    payload: Dict[str, Any],
    ts: str,
    host_id: str = "claude-code",
    reasoning: Optional[Dict[str, Any]] = None,
) -> List[Dict[str, Any]]:
    """Map one Claude Code hook payload to zero or more host-neutral envelopes.

    `Stop` is intentionally not handled here: extracting the assistant's reply
    needs to read the transcript file, which the hook script does before calling
    `model_response`.

    `reasoning` (a host-neutral reasoning object) is attached to the
    `tool_call_started` envelope for `PreToolUse`. It must be resolved from the
    transcript by the caller (the hook), keeping this mapping pure/I/O-free.
    """
    event = payload.get("hook_event_name") or ""
    session_id = payload.get("session_id") or "unknown-session"
    cwd = payload.get("cwd")
    pid = project_id(cwd)
    out: List[Dict[str, Any]] = []

    if event == "SessionStart":
        out.append(envelope(host_id, session_id, ts, "session_started", project_id=pid, cwd=cwd))
    elif event == "UserPromptSubmit":
        text = payload.get("prompt") or ""
        if text:
            out.append(envelope(host_id, session_id, ts, "user_prompt", project_id=pid, text=text))
    elif event == "PreToolUse":
        tool = payload.get("tool_name") or "tool"
        out.append(
            envelope(
                host_id, session_id, ts, "tool_call_started",
                project_id=pid, tool=tool, args_json=_compact(payload.get("tool_input")),
                reasoning=reasoning,
            )
        )
    elif event == "PostToolUse":
        tool = payload.get("tool_name") or "tool"
        out.append(
            envelope(
                host_id, session_id, ts, "tool_call_finished",
                project_id=pid, tool=tool, ok=not _is_error(payload.get("tool_response")),
                result_json=_compact(payload.get("tool_response")),
            )
        )
        file_event = _file_event(host_id, session_id, ts, tool, payload.get("tool_input"), pid)
        if file_event:
            out.append(file_event)
    elif event == "SessionEnd":
        out.append(envelope(host_id, session_id, ts, "session_ended", project_id=pid))
    return out


def assistant_turn_from_transcript(lines: List[str]) -> Dict[str, Optional[str]]:
    """The last assistant turn's ``text`` and ``thinking`` from transcript lines.

    Thinking is paired with the text-bearing turn we capture, so the reasoning
    we attach is the reasoning behind the response we record.
    """
    text: Optional[str] = None
    thinking: Optional[str] = None
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except (json.JSONDecodeError, ValueError):
            continue
        message = obj.get("message") or {}
        role = obj.get("type") or message.get("role")
        if role != "assistant":
            continue
        content = message.get("content")
        turn_text = _text_of(content)
        if turn_text:
            text = turn_text
            thinking = _thinking_of(content)
    return {"text": text, "thinking": thinking}


def assistant_text_from_transcript(lines: List[str]) -> Optional[str]:
    """The last assistant turn's text (backward-compatible thin wrapper)."""
    return assistant_turn_from_transcript(lines)["text"]


def tool_use_thinking_from_transcript(
    lines: List[str],
    tool_name: str,
    tool_input: Optional[Any] = None,
    tool_use_id: Optional[str] = None,
) -> Optional[str]:
    """Thinking of the assistant message that issued a given ``tool_use``.

    Matches by ``tool_use_id`` when present (exact), else by ``(name, input)``.
    Returns the thinking of the *most recent* matching message — that is the one
    just emitted before ``PreToolUse`` fired. ``None`` when that message had no
    thinking, so we never borrow an unrelated message's reasoning.

    Note: tool_uses that share one thinking block (issued in the same message)
    each resolve to the same reasoning text — shared rationale, by design.
    """
    found: Optional[str] = None
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except (json.JSONDecodeError, ValueError):
            continue
        message = obj.get("message") or {}
        role = obj.get("type") or message.get("role")
        if role != "assistant":
            continue
        content = message.get("content")
        if isinstance(content, list) and _has_tool_use(content, tool_name, tool_input, tool_use_id):
            found = _thinking_of(content)
    return found


def _has_tool_use(content, tool_name, tool_input, tool_use_id) -> bool:
    for block in content:
        if not isinstance(block, dict) or block.get("type") != "tool_use":
            continue
        if tool_use_id is not None:
            if block.get("id") == tool_use_id:
                return True
            continue
        if block.get("name") == tool_name and (
            tool_input is None or block.get("input") == tool_input
        ):
            return True
    return False


def _compact(value: Any) -> Optional[str]:
    if value is None:
        return None
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))[:_MAX_FIELD]
    except (TypeError, ValueError):
        return str(value)[:_MAX_FIELD]


def _is_error(response: Any) -> bool:
    if isinstance(response, dict):
        if response.get("is_error") or response.get("error"):
            return True
        status = response.get("status")
        if isinstance(status, str) and status.lower() in {"error", "failed"}:
            return True
    return False


def _file_event(host_id, session_id, ts, tool, tool_input, pid):
    if tool not in FILE_TOOLS:
        return None
    inp = tool_input or {}
    path = inp.get("file_path") or inp.get("path") or inp.get("notebook_path")
    if not path:
        return None
    etype = "file_written" if FILE_TOOLS[tool] else "file_read"
    return envelope(host_id, session_id, ts, etype, project_id=pid, path=path)


def _text_of(content: Any) -> Optional[str]:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = [
            block.get("text") or ""
            for block in content
            if isinstance(block, dict) and block.get("type") == "text"
        ]
        joined = "\n".join(part for part in parts if part)
        return joined or None
    return None


def _thinking_of(content: Any) -> Optional[str]:
    """Join the extended-thinking blocks of an assistant turn, if any.

    Only readable ``type == "thinking"`` blocks contribute; ``redacted_thinking``
    (encrypted, no readable text) is skipped.
    """
    if isinstance(content, list):
        parts = [
            block.get("thinking") or ""
            for block in content
            if isinstance(block, dict) and block.get("type") == "thinking"
        ]
        joined = "\n".join(part for part in parts if part)
        return joined or None
    return None
