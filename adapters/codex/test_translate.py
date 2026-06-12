"""Tests for the Codex rollout -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py`` runs them all. The
synthesized records mirror the real ``rollout-*.jsonl`` schema (verified against
``~/.codex/sessions``).
"""
from __future__ import annotations

import translate


def rec(rtype, ptype=None, ts="2026-06-05T11:40:00Z", **payload):
    p = dict(payload)
    if ptype is not None:
        p["type"] = ptype
    return {"type": rtype, "timestamp": ts, "payload": p}


def run(records, **kw):
    return translate.translate(records, **kw)


def test_session_meta_starts_and_eof_ends():
    out = run([rec("session_meta", cwd="/work/proj", id="abc")])
    assert out[0]["type"] == "session_started"
    assert out[0]["session_id"] == "abc"
    assert out[0]["cwd"] == "/work/proj"
    assert out[0]["project_id"] == "proj"
    assert out[-1]["type"] == "session_ended"


def test_user_and_assistant_messages():
    out = run([
        rec("session_meta", cwd="/w", id="s"),
        rec("response_item", "message", role="user",
            content=[{"type": "input_text", "text": "hello"}]),
        rec("response_item", "message", role="assistant",
            content=[{"type": "output_text", "text": "hi back"}]),
    ])
    types = [e["type"] for e in out]
    assert types == ["session_started", "user_prompt", "model_response", "session_ended"]
    assert out[1]["text"] == "hello"
    assert out[2]["text"] == "hi back"


def test_reasoning_summary_attaches_to_next_assistant():
    out = run([
        rec("session_meta", id="s"),
        rec("response_item", "reasoning",
            summary=[{"type": "summary_text", "text": "weigh options"}]),
        rec("response_item", "message", role="assistant",
            content=[{"type": "output_text", "text": "answer"}]),
    ])
    resp = [e for e in out if e["type"] == "model_response"][0]
    assert resp["reasoning"]["text"] == "weigh options"
    assert resp["reasoning"]["source"] == "thinking"


def test_function_call_and_output_resolve_tool_name():
    out = run([
        rec("session_meta", id="s"),
        rec("response_item", "function_call", name="shell",
            call_id="c1", arguments='{"cmd":"ls"}'),
        rec("response_item", "function_call_output", call_id="c1",
            output={"exit_code": 0, "stdout": "a\nb"}),
    ])
    started = [e for e in out if e["type"] == "tool_call_started"][0]
    finished = [e for e in out if e["type"] == "tool_call_finished"][0]
    assert started["tool"] == "shell"
    assert started["args_json"] == '{"cmd":"ls"}'
    assert finished["tool"] == "shell"   # resolved by call_id
    assert finished["ok"] is True


def test_function_output_nonzero_exit_is_not_ok():
    out = run([
        rec("session_meta", id="s"),
        rec("response_item", "function_call", name="shell", call_id="c1", arguments="{}"),
        rec("response_item", "function_call_output", call_id="c1",
            output={"exit_code": 2}),
    ])
    finished = [e for e in out if e["type"] == "tool_call_finished"][0]
    assert finished["ok"] is False


def test_custom_tool_call_uses_input_field():
    out = run([
        rec("session_meta", id="s"),
        rec("response_item", "custom_tool_call", name="grep", call_id="c2", input="pattern"),
        rec("response_item", "custom_tool_call_output", call_id="c2", output="match"),
    ])
    started = [e for e in out if e["type"] == "tool_call_started"][0]
    assert started["tool"] == "grep" and started["args_json"] == "pattern"


def test_patch_apply_emits_file_written_per_change():
    out = run([
        rec("session_meta", id="s"),
        rec("event_msg", "patch_apply_end", success=True,
            changes={"src/a.rs": {}, "src/b.rs": {}}, stdout="ok"),
    ])
    files = sorted(e["path"] for e in out if e["type"] == "file_written")
    assert files == ["src/a.rs", "src/b.rs"]
    patch = [e for e in out if e["type"] == "tool_call_finished" and e["tool"] == "apply_patch"][0]
    assert patch["ok"] is True


def test_noise_records_are_ignored():
    out = run([
        rec("session_meta", id="s"),
        rec("event_msg", "token_count", info={}),
        rec("event_msg", "task_started", turn_id="t"),
        rec("turn_context"),
        rec("event_msg", "user_message", message="mirror"),  # mirror, ignored
    ])
    # only session_started + session_ended survive
    assert [e["type"] for e in out] == ["session_started", "session_ended"]


def test_every_envelope_has_required_keys():
    out = run([
        rec("session_meta", id="s", cwd="/w"),
        rec("response_item", "message", role="user", content="hi"),
    ])
    for env in out:
        assert {"host_id", "session_id", "ts", "type"} <= set(env)
        assert env["host_id"] == "codex"


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
