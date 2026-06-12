"""Tests for the OpenHands trajectory -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. The samples mirror the
native event-stream array (action/observation) and the visualizer `entries` shape.
"""
from __future__ import annotations

import translate


NATIVE = [
    {"id": 0, "timestamp": "2025-03-07T17:45:00Z", "source": "user",
     "action": "message", "args": {"content": "fix the bug in app.py"}},
    {"id": 1, "timestamp": "2025-03-07T17:45:05Z", "source": "agent",
     "action": "run", "args": {"command": "pytest", "thought": "let me reproduce it"}},
    {"id": 2, "timestamp": "2025-03-07T17:45:08Z", "source": "agent",
     "observation": "run", "content": "1 failed", "extras": {}},
    {"id": 3, "timestamp": "2025-03-07T17:45:12Z", "source": "agent",
     "action": "edit", "args": {"path": "app.py", "content": "..."}},
    {"id": 4, "timestamp": "2025-03-07T17:45:20Z", "source": "agent",
     "action": "message", "args": {"content": "Fixed it."}},
    {"id": 5, "timestamp": "2025-03-07T17:45:21Z", "source": "environment",
     "observation": "error", "content": "boom", "extras": {"error": True}},
]


def test_user_and_agent_messages():
    out = translate.translate(NATIVE)
    assert [e["text"] for e in out if e["type"] == "user_prompt"] == ["fix the bug in app.py"]
    responses = [e["text"] for e in out if e["type"] == "model_response"]
    assert "Fixed it." in responses
    assert "let me reproduce it" in responses   # agent 'thought' surfaced as reasoning
    assert out[0]["host_id"] == "openhands"


def test_action_becomes_tool_call():
    out = translate.translate(NATIVE)
    starts = [e for e in out if e["type"] == "tool_call_started"]
    tools = [e["tool"] for e in starts]
    assert "run" in tools and "edit" in tools
    run = [e for e in starts if e["tool"] == "run"][0]
    assert "pytest" in run["args_json"]


def test_edit_emits_file_written():
    out = translate.translate(NATIVE)
    writes = [e for e in out if e["type"] == "file_written"]
    assert writes and writes[0]["path"] == "app.py"


def test_observation_ok_flag():
    out = translate.translate(NATIVE)
    finishes = [e for e in out if e["type"] == "tool_call_finished"]
    ok = {e["tool"]: e["ok"] for e in finishes}
    assert ok["run"] is True
    assert ok["error"] is False     # observation 'error' / extras.error -> ok False


def test_session_wraps():
    out = translate.translate(NATIVE)
    assert out[0]["type"] == "session_started"
    assert out[-1]["type"] == "session_ended"


def test_visualizer_entries_shape():
    data = {"entries": [
        {"id": 1, "type": "message", "content": "hello", "actorType": "User"},
        {"id": 2, "type": "thought", "content": "thinking...", "actorType": "Assistant"},
        {"id": 3, "type": "command", "content": "run lint", "command": "npm run lint",
         "actorType": "Assistant"},
    ]}
    out = translate.translate(data)
    assert [e["text"] for e in out if e["type"] == "user_prompt"] == ["hello"]
    assert "thinking..." in [e["text"] for e in out if e["type"] == "model_response"]
    starts = [e for e in out if e["type"] == "tool_call_started"]
    assert starts and "npm run lint" in starts[0]["args_json"]


def test_every_envelope_has_required_keys():
    for env in translate.translate(NATIVE):
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t(); print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
