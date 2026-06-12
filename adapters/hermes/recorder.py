"""The Concierge recorder: binds Hermes lifecycle hooks to the host-neutral
event stream and feeds it to ``concierge-plugin ingest``.

**Observe-only.** It registers for Hermes's public hooks and never returns a
directive (``pre_llm_call`` returns ``None`` — no context injection, no veto), so
mounting it makes *zero changes to Hermes core* — the Phase 6 exit criterion.
Each session's events accumulate in ``.concierge/hermes-sessions/<id>.jsonl`` and
are ingested at ``on_session_end`` (idempotent: re-ingest is a no-op).
"""
from __future__ import annotations

import json
import os
import subprocess
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Optional

try:  # works both as a package (in Hermes) and standalone (in tests)
    from . import translate
except (ImportError, ValueError):  # pragma: no cover
    import translate  # type: ignore


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


class ConciergeRecorder:
    def __init__(self, host_id: str = "hermes", workdir: Optional[str] = None,
                 binary: str = "concierge-plugin"):
        self.host_id = host_id
        self.workdir = Path(workdir or os.getenv("CONCIERGE_WORKDIR") or os.getcwd())
        self.binary = os.getenv("CONCIERGE_BIN") or binary
        self._lock = threading.Lock()
        self._sessions_dir = self.workdir / ".concierge" / "hermes-sessions"
        self._gui_started = False

    def session_path(self, session_id: str) -> Path:
        safe = (session_id or "session").replace("/", "_")
        return self._sessions_dir / f"{safe}.jsonl"

    def _emit(self, env: Dict[str, Any]) -> None:
        path = self.session_path(env["session_id"])
        line = json.dumps(env)
        with self._lock:
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("a", encoding="utf-8") as handle:
                handle.write(line + "\n")

    # -- hooks (defensive **kwargs; observe only) ---------------------------

    def on_session_start(self, session_id: str = "", cwd: Optional[str] = None,
                         model: Optional[str] = None, **_kw):
        self.mount_gui(model or "Hermes")
        self._emit(translate.session_started(self.host_id, session_id, _now(), cwd=cwd))

    def pre_llm_call(self, session_id: str = "", user_message: str = "", **_kw):
        if user_message:
            self._emit(translate.user_prompt(self.host_id, session_id, _now(), user_message))
        return None  # never inject context — this is a recorder, not a memory provider

    def post_llm_call(self, session_id: str = "", assistant_response: str = "", **_kw):
        if assistant_response:
            self._emit(translate.model_response(self.host_id, session_id, _now(), assistant_response))

    def pre_tool_call(self, session_id: str = "", function_name: str = "", name: str = "",
                      function_args: Optional[dict] = None, **kw):
        tool = function_name or name or kw.get("tool") or "tool"
        args = function_args if function_args is not None else kw.get("args")
        self._emit(translate.tool_call_started(
            self.host_id, session_id, _now(), tool,
            args_json=json.dumps(args) if args is not None else None,
        ))

    def post_tool_call(self, session_id: str = "", function_name: str = "", name: str = "",
                       function_args: Optional[dict] = None, result: Any = None,
                       ok: Optional[bool] = None, **kw):
        tool = function_name or name or kw.get("tool") or "tool"
        args = function_args if function_args is not None else kw.get("args")
        succeeded = ok if ok is not None else not _looks_like_error(result)
        now = _now()
        self._emit(translate.tool_call_finished(
            self.host_id, session_id, now, tool, succeeded, result_json=_to_json(result),
        ))
        file_env = translate.file_event_from_tool(self.host_id, session_id, now, tool, args)
        if file_env:
            self._emit(file_env)

    def on_session_end(self, session_id: str = "", **_kw):
        self._emit(translate.session_ended(self.host_id, session_id, _now()))
        self.ingest(session_id)

    # -- feed the host-neutral stream into Concierge ------------------------

    def mount_gui(self, model: str) -> None:
        """Open one standalone read-only explorer when Hermes mounts us."""
        if self._gui_started:
            return
        command = [self.binary, "gui", "--model", model]
        port = os.getenv("CONCIERGE_GUI_PORT", "").strip()
        if port:
            command.extend(["--port", port])
        if os.getenv("CONCIERGE_GUI_NO_OPEN", "").lower() in {"1", "true", "yes", "on"}:
            command.append("--no-open")
        try:
            subprocess.Popen(
                command,
                cwd=str(self.workdir),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            self._gui_started = True
        except FileNotFoundError:
            pass

    def ingest(self, session_id: str) -> None:
        path = self.session_path(session_id)
        if not path.exists():
            return
        try:
            subprocess.run(
                [self.binary, "ingest", str(path)],
                cwd=str(self.workdir), check=False, capture_output=True,
            )
        except FileNotFoundError:
            pass  # concierge-plugin not installed — the JSONL trail still survives


def _looks_like_error(result: Any) -> bool:
    if isinstance(result, str):
        try:
            obj = json.loads(result)
        except Exception:
            return False
        return isinstance(obj, dict) and "error" in obj
    return isinstance(result, dict) and "error" in result


def _to_json(result: Any) -> Optional[str]:
    if result is None:
        return None
    if isinstance(result, str):
        return result
    try:
        return json.dumps(result)
    except Exception:
        return str(result)
