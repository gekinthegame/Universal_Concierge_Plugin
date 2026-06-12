"""Tests for the ChatGPT export -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. The synthesized export
mirrors the real `conversations.json` mapping shape.
"""
from __future__ import annotations

import translate


def conv():
    return {
        "title": "Build a parser", "create_time": 1700000000.0, "update_time": 1700000300.0,
        "current_node": "n3",
        "mapping": {
            "root": {"id": "root", "parent": None, "children": ["n1"], "message": None},
            "n1": {"id": "n1", "parent": "root", "children": ["n2"],
                   "message": {"create_time": 1700000001.0, "author": {"role": "user"},
                               "content": {"content_type": "text", "parts": ["how do I parse JSONL?"]}}},
            "n2": {"id": "n2", "parent": "n1", "children": ["n3"],
                   "message": {"create_time": 1700000002.0, "author": {"role": "assistant"},
                               "content": {"content_type": "text", "parts": ["Read it line by line."]}}},
            "n3": {"id": "n3", "parent": "n2", "children": [],
                   "message": {"create_time": 1700000003.0, "author": {"role": "user"},
                               "content": {"content_type": "text", "parts": ["thanks"]}}},
        },
    }


def test_conversation_in_turn_order():
    out = translate.translate([conv()])
    types = [e["type"] for e in out]
    # session_started, (title memory), user, assistant, user, session_ended
    assert types[0] == "session_started"
    assert types[-1] == "session_ended"
    prompts = [e["text"] for e in out if e["type"] == "user_prompt"]
    responses = [e["text"] for e in out if e["type"] == "model_response"]
    assert prompts == ["how do I parse JSONL?", "thanks"]   # in order, via parent links
    assert responses == ["Read it line by line."]
    assert out[0]["host_id"] == "chatgpt"


def test_title_becomes_a_memory():
    out = translate.translate([conv()])
    mem = [e for e in out if e["type"] == "memory_recorded"][0]
    assert "Build a parser" in mem["text"]


def test_system_and_empty_messages_skipped():
    c = {"create_time": 1.0, "update_time": 2.0, "current_node": "b",
         "mapping": {
             "a": {"id": "a", "parent": None, "children": ["b"],
                   "message": {"author": {"role": "system"}, "content": {"parts": ["you are helpful"]}}},
             "b": {"id": "b", "parent": "a", "children": [],
                   "message": {"author": {"role": "assistant"}, "content": {"parts": [""]}}},
         }}
    out = translate.translate([c])
    assert [e["type"] for e in out] == ["session_started", "session_ended"]


def test_multimodal_parts_take_only_text():
    c = {"create_time": 1.0, "current_node": "a",
         "mapping": {"a": {"id": "a", "parent": None, "children": [],
                           "message": {"author": {"role": "user"},
                                       "content": {"content_type": "multimodal_text",
                                                   "parts": [{"image": "x"}, "the text part"]}}}}}
    out = translate.translate([c])
    up = [e for e in out if e["type"] == "user_prompt"][0]
    assert up["text"] == "the text part"


def test_every_envelope_has_required_keys():
    for env in translate.translate([conv()]):
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t(); print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
