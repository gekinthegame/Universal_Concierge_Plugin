#!/usr/bin/env python3
"""Claude Code hook -> Concierge IPLD memory (observe-only).

Wired into Claude Code's hook events. On each event it reads the hook JSON on
stdin, appends a host-neutral envelope to
``<workdir>/.concierge/claude-code-sessions/<id>.jsonl``, and ingests the session
into Concierge on ``Stop`` (per turn) and ``SessionEnd``. It NEVER blocks the
session: every failure path exits 0 with no output, and the JSONL trail survives
even if ``concierge-plugin`` is not installed.

Environment overrides:
    CONCIERGE_WORKDIR         store directory (default: the hook's reported cwd)
    CONCIERGE_BIN             path to concierge-plugin (default: on PATH)
    CONCIERGE_INGEST_EVERY_TURN  "0" to ingest only at SessionEnd (default: per turn)
"""
from __future__ import annotations

import datetime
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import translate  # noqa: E402


def _now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _workdir(payload: dict) -> Path:
    return Path(os.getenv("CONCIERGE_WORKDIR") or payload.get("cwd") or os.getcwd())


def _binary() -> str:
    return os.getenv("CONCIERGE_BIN") or "concierge-plugin"


def _safe_name(session_id: str) -> str:
    return "".join(c if c.isalnum() or c in "-_" else "_" for c in session_id) or "session"


def _append(path: Path, envelopes: list) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for env in envelopes:
            handle.write(json.dumps(env, ensure_ascii=False) + "\n")


def _ingest(workdir: Path, session_path: Path) -> None:
    try:
        subprocess.run(
            [_binary(), "ingest", str(session_path)],
            cwd=str(workdir),
            check=False,
            capture_output=True,
            timeout=120,
        )
    except Exception:
        pass  # concierge-plugin missing/slow — the JSONL trail still survives


def _disabled(name: str) -> bool:
    return os.getenv(name, "").lower() in {"1", "true", "yes", "on"}


def _mount_gui(workdir: Path) -> None:
    """Open the Data Platter for this store on session start (like Hermes).

    `concierge-plugin gui` is idempotent, so this reuses an existing server for
    the same store instead of spawning a duplicate. Detached and best-effort:
    a failure never affects the session. `CONCIERGE_GUI_DISABLE=1` skips it;
    `CONCIERGE_GUI_NO_OPEN=1` serves without opening a browser tab.
    """
    if _disabled("CONCIERGE_GUI_DISABLE"):
        return
    command = [_binary(), "gui", "--model", "claude-code"]
    port = os.getenv("CONCIERGE_GUI_PORT", "").strip()
    if port:
        command += ["--port", port]
    if _disabled("CONCIERGE_GUI_NO_OPEN"):
        command.append("--no-open")
    parent_pid = os.getppid()
    if parent_pid > 1:
        command += ["--watch-pid", str(parent_pid)]
    try:
        subprocess.Popen(
            command,
            cwd=str(workdir),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except Exception:
        pass  # observe-only: never break the session


def _assistant_turn(payload: dict):
    """The last assistant turn's text + thinking from the transcript."""
    transcript = payload.get("transcript_path")
    if not transcript:
        return {"text": None, "thinking": None}
    try:
        with open(transcript, "r", encoding="utf-8") as handle:
            return translate.assistant_turn_from_transcript(handle.readlines())
    except Exception:
        return {"text": None, "thinking": None}


def _tool_call_reasoning(payload: dict):
    """Reasoning behind a tool call: the thinking of the message that issued it."""
    transcript = payload.get("transcript_path")
    if not transcript:
        return None
    try:
        with open(transcript, "r", encoding="utf-8") as handle:
            thinking = translate.tool_use_thinking_from_transcript(
                handle.readlines(),
                payload.get("tool_name") or "tool",
                payload.get("tool_input"),
                payload.get("tool_use_id"),
            )
        return translate.reasoning_block(thinking)
    except Exception:
        return None


def main() -> int:
    try:
        raw = sys.stdin.read()
        payload = json.loads(raw) if raw.strip() else {}
    except Exception:
        return 0
    if not isinstance(payload, dict):
        return 0

    try:
        event = payload.get("hook_event_name") or ""
        session_id = payload.get("session_id") or "unknown-session"
        workdir = _workdir(payload)
        session_path = (
            workdir / ".concierge" / "claude-code-sessions" / f"{_safe_name(session_id)}.jsonl"
        )
        ts = _now()

        reasoning = _tool_call_reasoning(payload) if event == "PreToolUse" else None
        envelopes = translate.from_hook_event(payload, ts, reasoning=reasoning)
        if event == "Stop":
            turn = _assistant_turn(payload)
            text = turn.get("text")
            if text:
                envelopes.append(
                    translate.model_response(
                        "claude-code",
                        session_id,
                        ts,
                        text,
                        project_id=translate.project_id(payload.get("cwd")),
                        reasoning=translate.reasoning_block(turn.get("thinking")),
                    )
                )

        if envelopes:
            _append(session_path, envelopes)

        # Auto-open the Data Platter when the session begins (uniform across
        # harnesses; idempotent so repeated sessions reuse the one server).
        if event == "SessionStart":
            _mount_gui(workdir)

        ingest_every_turn = os.getenv("CONCIERGE_INGEST_EVERY_TURN", "1").lower() not in {
            "0",
            "false",
            "no",
            "off",
        }
        if session_path.exists() and (
            event == "SessionEnd" or (event == "Stop" and ingest_every_turn)
        ):
            _ingest(workdir, session_path)
    except Exception:
        pass  # observe-only: a hook must never break the session
    return 0


if __name__ == "__main__":
    sys.exit(main())
