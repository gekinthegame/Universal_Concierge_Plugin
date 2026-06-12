"""Tests for the Gemini CLI -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. Synthesized records
mirror the real ``~/.gemini/tmp/<project>/chats/session-*.jsonl`` schema.
"""
from __future__ import annotations

import translate


def run(records, **kw):
    return translate.translate(records, **kw)


HEADER = {"sessionId": "sess-1", "projectHash": "h", "startTime": "2026-06-11T18:40:00Z",
          "lastUpdated": "x", "kind": "chat"}


def test_header_starts_session():
    out = run([HEADER])
    assert out[0]["type"] == "session_started"
    assert out[0]["session_id"] == "sess-1"
    assert out[-1]["type"] == "session_ended"


def test_user_parts_are_joined():
    out = run([HEADER, {"type": "user", "timestamp": "t",
                        "content": [{"text": "line one"}, {"text": "line two"}]}])
    up = [e for e in out if e["type"] == "user_prompt"][0]
    assert up["text"] == "line one\nline two"


def test_gemini_response_with_thoughts():
    out = run([HEADER, {"type": "gemini", "timestamp": "t", "content": "the answer",
                        "thoughts": [{"subject": "Plan", "description": "do X"}]}])
    resp = [e for e in out if e["type"] == "model_response"][0]
    assert resp["text"] == "the answer"
    assert resp["reasoning"]["text"] == "Plan: do X"
    assert resp["reasoning"]["source"] == "thinking"


def test_tool_calls_become_started_and_finished():
    out = run([HEADER, {"type": "gemini", "timestamp": "t", "content": "done",
                        "toolCalls": [{"name": "read_file", "args": {"path": "a.py"},
                                       "result": ["ok"], "status": "success",
                                       "timestamp": "tc"}]}])
    started = [e for e in out if e["type"] == "tool_call_started"][0]
    finished = [e for e in out if e["type"] == "tool_call_finished"][0]
    assert started["tool"] == "read_file"
    assert started["args_json"] == '{"path":"a.py"}'
    assert finished["ok"] is True
    # tool events precede the model_response in this turn
    assert [e["type"] for e in out] == [
        "session_started", "tool_call_started", "tool_call_finished",
        "model_response", "session_ended"]


def test_error_status_is_not_ok():
    out = run([HEADER, {"type": "gemini", "content": "x",
                        "toolCalls": [{"name": "t", "args": {}, "status": "error"}]}])
    finished = [e for e in out if e["type"] == "tool_call_finished"][0]
    assert finished["ok"] is False


def test_set_and_info_are_ignored():
    out = run([HEADER, {"$set": {"lastUpdated": "x"}},
               {"type": "info", "content": "system note", "timestamp": "t"}])
    assert [e["type"] for e in out] == ["session_started", "session_ended"]


def test_every_envelope_has_required_keys():
    out = run([HEADER, {"type": "user", "content": [{"text": "hi"}]}], project_id="proj")
    for env in out:
        assert {"host_id", "session_id", "ts", "type"} <= set(env)
        assert env["host_id"] == "gemini"
        assert env.get("project_id") == "proj"


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
