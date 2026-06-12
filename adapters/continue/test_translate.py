"""Tests for the Continue.dev session -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. The sample mirrors the
real `~/.continue/sessions/<id>.json` `Session`/`ChatHistoryItem` shape.
"""
from __future__ import annotations

import translate


def session():
    return {
        "sessionId": "abc-123", "title": "Add a parser",
        "workspaceDirectory": "/home/me/proj",
        "history": [
            {"message": {"role": "user", "content": "how do I parse jsonl?"}},
            {"message": {"role": "thinking", "content": "they want line-by-line"}},
            {"message": {"role": "assistant",
                         "content": [{"type": "text", "text": "Read it "},
                                     {"type": "text", "text": "line by line."}]},
             "toolCallStates": [
                 {"status": "done",
                  "toolCall": {"function": {"name": "read_file",
                                            "arguments": "{\"path\":\"a.py\"}"}}}]},
            {"message": {"role": "system", "content": "you are helpful"}},
        ],
    }


def test_workspace_and_title():
    out = translate.translate(session())
    assert out[0]["type"] == "session_started"
    assert out[0]["cwd"] == "/home/me/proj"
    mem = [e for e in out if e["type"] == "memory_recorded"][0]
    assert "Add a parser" in mem["text"]
    assert out[0]["host_id"] == "continue"


def test_roles_mapped():
    out = translate.translate(session())
    assert [e["text"] for e in out if e["type"] == "user_prompt"] == ["how do I parse jsonl?"]
    resp = [e for e in out if e["type"] == "model_response"][0]
    assert resp["text"] == "Read it line by line."   # list-of-parts concatenated


def test_thinking_folds_into_reasoning():
    out = translate.translate(session())
    resp = [e for e in out if e["type"] == "model_response"][0]
    # reasoning is the host-neutral {text, source} object, not a bare string
    assert resp.get("reasoning") == {"text": "they want line-by-line", "source": "thinking"}


def test_tool_call_emitted():
    out = translate.translate(session())
    starts = [e for e in out if e["type"] == "tool_call_started"]
    finishes = [e for e in out if e["type"] == "tool_call_finished"]
    assert starts and starts[0]["tool"] == "read_file"
    assert "a.py" in starts[0]["args_json"]
    assert finishes[0]["ok"] is True


def test_system_message_skipped():
    out = translate.translate(session())
    blob = " ".join(e.get("text", "") for e in out)
    assert "you are helpful" not in blob


def test_session_array_and_string_content():
    data = [{"sessionId": "x", "history": [{"message": {"role": "user", "content": "hi"}}]}]
    sessions = translate._iter_sessions(data)
    out = translate.translate(sessions[0])
    assert [e["type"] for e in out] == ["session_started", "user_prompt", "session_ended"]


def test_every_envelope_has_required_keys():
    for env in translate.translate(session()):
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t(); print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
