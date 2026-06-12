"""Pure Hermes-event -> host-neutral Envelope translation (Layer 3 adapter core).

No Hermes or Concierge imports here, so the mapping is unit-testable on its own.
The shapes match ``ADAPTER_CONTRACT.md`` / ``crates/core/src/event.rs``:

    {"host_id", "session_id", "ts", "type", <event fields...>}

Mapping (Hermes hook -> host-neutral event):

    on_session_start         -> session_started {cwd?}
    pre_llm_call             -> user_prompt {text}
    post_llm_call            -> model_response {text}
    pre_tool_call            -> tool_call_started {tool, args_json?}
    post_tool_call           -> tool_call_finished {tool, ok, result_json?}
      (+ file_read/file_written when the tool is a file tool)
    on_session_end           -> session_ended
"""
from __future__ import annotations

from typing import Any, Dict, Optional


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


def session_started(host_id, session_id, ts, cwd=None, project_id=None):
    return envelope(host_id, session_id, ts, "session_started", project_id=project_id, cwd=cwd)


def user_prompt(host_id, session_id, ts, text, project_id=None):
    return envelope(host_id, session_id, ts, "user_prompt", project_id=project_id, text=text)


def model_response(host_id, session_id, ts, text, project_id=None):
    return envelope(host_id, session_id, ts, "model_response", project_id=project_id, text=text)


def tool_call_started(host_id, session_id, ts, tool, args_json=None, project_id=None):
    return envelope(
        host_id, session_id, ts, "tool_call_started",
        project_id=project_id, tool=tool, args_json=args_json,
    )


def tool_call_finished(host_id, session_id, ts, tool, ok, result_json=None, project_id=None):
    return envelope(
        host_id, session_id, ts, "tool_call_finished",
        project_id=project_id, tool=tool, ok=ok, result_json=result_json,
    )


def file_event(host_id, session_id, ts, path, written, project_id=None):
    etype = "file_written" if written else "file_read"
    return envelope(host_id, session_id, ts, etype, project_id=project_id, path=path)


def session_ended(host_id, session_id, ts, project_id=None):
    return envelope(host_id, session_id, ts, "session_ended", project_id=project_id)


# Hermes file tools -> (arg holding the path, was-it-written?). Lets post_tool_call
# derive FileRef events so file touches become blobs in Concierge.
FILE_TOOLS = {
    "write_file": ("path", True),
    "edit_file": ("path", True),
    "apply_patch": ("path", True),
    "read_file": ("path", False),
}


def file_event_from_tool(host_id, session_id, ts, tool, args, project_id=None):
    """A file_read/file_written envelope if ``tool`` is a Hermes file tool, else None."""
    spec = FILE_TOOLS.get(tool)
    if not spec:
        return None
    path_key, written = spec
    path = (args or {}).get(path_key)
    if not path:
        return None
    return file_event(host_id, session_id, ts, path, written, project_id=project_id)
