"""Tests for the Claude Code hook -> host-neutral envelope translation.

Self-contained (no pytest required): ``python3 test_translate.py`` runs them all.
"""
from __future__ import annotations

import translate

TS = "2026-06-07T10:00:00Z"


def _one(payload):
    out = translate.from_hook_event(payload, TS)
    assert len(out) == 1, f"expected one envelope, got {out}"
    return out[0]


def test_session_start():
    env = _one({"hook_event_name": "SessionStart", "session_id": "s1", "cwd": "/work/proj"})
    assert env["type"] == "session_started"
    assert env["host_id"] == "claude-code"
    assert env["session_id"] == "s1"
    assert env["cwd"] == "/work/proj"
    assert env["project_id"] == "proj"


def test_user_prompt():
    env = _one({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "prompt": "hello there"})
    assert env["type"] == "user_prompt"
    assert env["text"] == "hello there"


def test_empty_prompt_is_dropped():
    assert translate.from_hook_event(
        {"hook_event_name": "UserPromptSubmit", "session_id": "s1", "prompt": ""}, TS
    ) == []


def test_pre_tool_use_carries_compact_args():
    env = _one(
        {
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
        }
    )
    assert env["type"] == "tool_call_started"
    assert env["tool"] == "Bash"
    assert env["args_json"] == '{"command":"ls -la"}'


def test_post_tool_use_marks_errors():
    ok = _one(
        {
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_response": {"stdout": "done"},
        }
    )
    assert ok["type"] == "tool_call_finished"
    assert ok["ok"] is True

    failed = _one(
        {
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_response": {"is_error": True, "error": "boom"},
        }
    )
    assert failed["ok"] is False


def test_post_tool_use_derives_file_events():
    out = translate.from_hook_event(
        {
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/work/proj/src/lib.rs"},
            "tool_response": {"ok": True},
        },
        TS,
    )
    kinds = {env["type"] for env in out}
    assert kinds == {"tool_call_finished", "file_written"}
    written = next(env for env in out if env["type"] == "file_written")
    assert written["path"] == "/work/proj/src/lib.rs"

    read = translate.from_hook_event(
        {
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Read",
            "tool_input": {"file_path": "/work/proj/README.md"},
            "tool_response": {},
        },
        TS,
    )
    assert any(env["type"] == "file_read" for env in read)


def test_session_end():
    env = _one({"hook_event_name": "SessionEnd", "session_id": "s1"})
    assert env["type"] == "session_ended"


def test_assistant_text_from_transcript():
    lines = [
        '{"type":"user","message":{"role":"user","content":"hi"}}',
        '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}',
        '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"final answer"},{"type":"tool_use","name":"Bash"}]}}',
        "   ",
        "{not json}",
    ]
    assert translate.assistant_text_from_transcript(lines) == "final answer"


def test_assistant_text_handles_plain_string_content():
    lines = ['{"type":"assistant","message":{"role":"assistant","content":"plain reply"}}']
    assert translate.assistant_text_from_transcript(lines) == "plain reply"


def test_assistant_turn_captures_thinking_with_text():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"thinking","thinking":"Weigh A vs B."},'
        '{"type":"text","text":"final answer"},'
        '{"type":"tool_use","name":"Bash"}]}}',
    ]
    turn = translate.assistant_turn_from_transcript(lines)
    assert turn["text"] == "final answer"
    assert turn["thinking"] == "Weigh A vs B."


def test_reasoning_rides_along_on_model_response():
    turn = translate.assistant_turn_from_transcript(
        ['{"type":"assistant","message":{"role":"assistant","content":['
         '{"type":"thinking","thinking":"why this"},{"type":"text","text":"answer"}]}}']
    )
    env = translate.model_response(
        "claude-code", "s1", TS, turn["text"],
        reasoning=translate.reasoning_block(turn["thinking"]),
    )
    assert env["type"] == "model_response"
    assert env["text"] == "answer"
    assert env["reasoning"] == {"text": "why this", "source": "thinking"}


def test_no_thinking_omits_reasoning():
    # A turn with text but no thinking blocks produces no reasoning field.
    turn = translate.assistant_turn_from_transcript(
        ['{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}']
    )
    assert turn["thinking"] is None
    env = translate.model_response(
        "claude-code", "s1", TS, turn["text"],
        reasoning=translate.reasoning_block(turn["thinking"]),
    )
    assert "reasoning" not in env


def test_redacted_thinking_is_skipped():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"redacted_thinking","data":"AQ=="},{"type":"text","text":"ok"}]}}',
    ]
    turn = translate.assistant_turn_from_transcript(lines)
    assert turn["text"] == "ok"
    assert turn["thinking"] is None


def test_tool_use_thinking_matches_by_name_and_input():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"thinking","thinking":"List files to find the spec."},'
        '{"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls -la"}}]}}',
    ]
    thinking = translate.tool_use_thinking_from_transcript(lines, "Bash", {"command": "ls -la"})
    assert thinking == "List files to find the spec."


def test_tool_use_thinking_matches_by_id():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"thinking","thinking":"why this edit"},'
        '{"type":"tool_use","id":"tu_42","name":"Edit","input":{"file_path":"a.rs"}}]}}',
    ]
    thinking = translate.tool_use_thinking_from_transcript(
        lines, "Edit", tool_input=None, tool_use_id="tu_42"
    )
    assert thinking == "why this edit"


def test_shared_thinking_applies_to_each_tool_use():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"thinking","thinking":"do both reads"},'
        '{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"x"}},'
        '{"type":"tool_use","id":"b","name":"Read","input":{"file_path":"y"}}]}}',
    ]
    assert translate.tool_use_thinking_from_transcript(lines, "Read", tool_use_id="a") == "do both reads"
    assert translate.tool_use_thinking_from_transcript(lines, "Read", tool_use_id="b") == "do both reads"


def test_tool_use_without_thinking_returns_none():
    lines = [
        '{"type":"assistant","message":{"role":"assistant","content":['
        '{"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"}}]}}',
    ]
    assert translate.tool_use_thinking_from_transcript(lines, "Bash", {"command": "ls"}) is None


def test_pre_tool_use_attaches_reasoning_when_provided():
    reasoning = {"text": "List files first.", "source": "thinking"}
    out = translate.from_hook_event(
        {"hook_event_name": "PreToolUse", "session_id": "s1", "tool_name": "Bash",
         "tool_input": {"command": "ls"}},
        TS, reasoning=reasoning,
    )
    started = next(e for e in out if e["type"] == "tool_call_started")
    assert started["reasoning"] == reasoning


def test_pre_tool_use_omits_reasoning_when_absent():
    env = _one(
        {"hook_event_name": "PreToolUse", "session_id": "s1", "tool_name": "Bash",
         "tool_input": {"command": "ls"}}
    )
    assert "reasoning" not in env


def test_unknown_event_is_ignored():
    assert translate.from_hook_event({"hook_event_name": "PreCompact", "session_id": "s1"}, TS) == []


if __name__ == "__main__":
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
        print(f"ok  {test.__name__}")
    print(f"\n{len(tests)} passed")
