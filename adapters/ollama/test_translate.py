"""Tests for the Ollama proxy's capture logic (no network).

Self-contained (no pytest): ``python3 test_translate.py``.
"""
from __future__ import annotations

import json

import proxy


def test_last_user_prompt():
    msgs = [{"role": "system", "content": "be nice"},
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": "second"}]
    assert proxy.last_user_prompt(msgs) == "second"


def test_session_id_is_stable_per_conversation():
    a = [{"role": "user", "content": "hello"}]
    b = [{"role": "user", "content": "hello"}, {"role": "assistant", "content": "x"}]
    c = [{"role": "user", "content": "different"}]
    assert proxy.session_id_for(a) == proxy.session_id_for(b)   # same root -> same session
    assert proxy.session_id_for(a) != proxy.session_id_for(c)


def test_assemble_native_chat_stream():
    chunks = [
        json.dumps({"message": {"role": "assistant", "content": "Hel"}, "done": False}) + "\n",
        json.dumps({"message": {"role": "assistant", "content": "lo"}, "done": True}) + "\n",
    ]
    assert proxy.assemble_response(chunks, openai_style=False) == "Hello"


def test_assemble_openai_sse_stream():
    chunks = [
        'data: ' + json.dumps({"choices": [{"delta": {"content": "Hi"}}]}) + "\n\n",
        'data: ' + json.dumps({"choices": [{"delta": {"content": " there"}}]}) + "\n\n",
        'data: [DONE]\n\n',
    ]
    assert proxy.assemble_response(chunks, openai_style=True) == "Hi there"


def test_assemble_openai_nonstream():
    chunk = json.dumps({"choices": [{"message": {"content": "single reply"}}]})
    assert proxy.assemble_response([chunk], openai_style=True) == "single reply"


def test_events_for_full_exchange():
    seen = set()
    req = {"model": "llama3", "messages": [{"role": "user", "content": "ping"}]}
    out = proxy.events_for(req, "pong", seen)
    assert [e["type"] for e in out] == ["session_started", "user_prompt", "model_response"]
    assert out[1]["text"] == "ping" and out[2]["text"] == "pong"
    # a second exchange in the same session does not re-emit session_started
    out2 = proxy.events_for(req, "pong2", seen)
    assert [e["type"] for e in out2] == ["user_prompt", "model_response"]


def test_capture_never_contains_credentials():
    # events_for only ever reads `messages`; headers/keys can't leak into the log.
    seen = set()
    req = {"messages": [{"role": "user", "content": "x"}], "api_key": "SECRET-KEY"}
    out = proxy.events_for(req, "y", seen)
    blob = json.dumps(out)
    assert "SECRET" not in blob
    for env in out:
        assert {"host_id", "session_id", "ts", "type"} <= set(env)
        assert env["host_id"] == "ollama"


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
