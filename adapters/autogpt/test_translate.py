"""Tests for the AutoGPT run -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. The samples mirror
AutoGPT's per-cycle response JSON (thoughts + command).
"""
from __future__ import annotations

import translate


RUN = {
    "ai_name": "ResearchGPT",
    "ai_goals": ["Find the answer", "Write it to a file"],
    "history": [
        {"thoughts": {"text": "I will search the web",
                      "reasoning": "need current info", "plan": "- search\n- write",
                      "criticism": "could be slow", "speak": "Searching now"},
         "command": {"name": "web_search", "args": {"query": "rust ipld"}}},
        {"thoughts": {"text": "Saving results", "speak": "Writing the file"},
         "command": {"name": "write_to_file", "args": {"filename": "out.md", "text": "..."}}},
        {"thoughts": {"text": "Done", "speak": "All goals complete"},
         "command": {"name": "task_complete", "args": {"reason": "done"}}},
    ],
}


def test_goals_become_user_prompts():
    out = translate.translate(RUN)
    prompts = [e["text"] for e in out if e["type"] == "user_prompt"]
    assert prompts == ["Find the answer", "Write it to a file"]
    assert out[0]["host_id"] == "autogpt"


def test_ai_name_memory():
    out = translate.translate(RUN)
    mem = [e for e in out if e["type"] == "memory_recorded"][0]
    assert "ResearchGPT" in mem["text"]


def test_thoughts_become_response_with_reasoning():
    out = translate.translate(RUN)
    first = [e for e in out if e["type"] == "model_response"][0]
    assert first["text"] == "Searching now"          # speak preferred
    r = first["reasoning"]
    assert r["source"] == "inline_preamble"          # honest provenance, {text,source}
    assert "reasoning: need current info" in r["text"]
    assert "plan:" in r["text"] and "criticism:" in r["text"]


def test_command_becomes_tool_call():
    out = translate.translate(RUN)
    starts = [e for e in out if e["type"] == "tool_call_started"]
    tools = [e["tool"] for e in starts]
    assert "web_search" in tools and "write_to_file" in tools
    assert "rust ipld" in [e for e in starts if e["tool"] == "web_search"][0]["args_json"]


def test_write_to_file_emits_file_written():
    out = translate.translate(RUN)
    writes = [e for e in out if e["type"] == "file_written"]
    assert writes and writes[0]["path"] == "out.md"


def test_terminal_command_has_no_tool_call():
    out = translate.translate(RUN)
    tools = [e["tool"] for e in out if e["type"] == "tool_call_started"]
    assert "task_complete" not in tools


def test_bare_array_of_cycles():
    out = translate.translate([
        {"thoughts": {"speak": "hi"}, "command": {"name": "do_nothing", "args": {}}},
    ])
    assert [e["type"] for e in out] == ["session_started", "model_response", "session_ended"]


def test_every_envelope_has_required_keys():
    for env in translate.translate(RUN):
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t(); print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
