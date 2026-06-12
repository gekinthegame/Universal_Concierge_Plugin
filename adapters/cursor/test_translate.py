"""Tests for the Cursor -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. Covers the pure mapping
AND the SQLite reader, by building a synthesized ``state.vscdb`` in a temp file
(so it is testable without Cursor installed).
"""
from __future__ import annotations

import json
import os
import sqlite3
import tempfile

import translate


def test_user_and_assistant_bubbles():
    out = translate.translate([{
        "id": "c1", "name": "proj", "createdAt": 1_700_000_000_000,
        "bubbles": [
            {"type": 1, "text": "fix the bug"},
            {"type": 2, "text": "fixed it"},
        ],
    }])
    types = [e["type"] for e in out]
    assert types == ["session_started", "user_prompt", "model_response", "session_ended"]
    assert out[1]["text"] == "fix the bug"
    assert out[2]["text"] == "fixed it"
    assert out[0]["project_id"] == "proj"
    assert out[0]["host_id"] == "cursor"
    assert out[0]["ts"].startswith("2023-11-14T")  # createdAt ms -> ISO


def test_tool_bubble_becomes_started_and_finished():
    out = translate.translate([{
        "id": "c1",
        "bubbles": [{"type": 2, "text": "ran it",
                     "toolFormerData": {"name": "edit_file", "rawArgs": "{\"p\":\"a\"}",
                                        "status": "completed", "result": "ok"}}],
    }])
    started = [e for e in out if e["type"] == "tool_call_started"][0]
    finished = [e for e in out if e["type"] == "tool_call_finished"][0]
    assert started["tool"] == "edit_file"
    assert finished["ok"] is True


def test_empty_bubbles_dropped():
    out = translate.translate([{"id": "c1", "bubbles": [{"type": 1, "text": ""}]}])
    assert [e["type"] for e in out] == ["session_started", "session_ended"]


def test_read_state_vscdb_roundtrip():
    # Build a synthesized state.vscdb the way Cursor's cursorDiskKV stores it.
    path = os.path.join(tempfile.mkdtemp(), "state.vscdb")
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT)")
    composer = {
        "composerId": "c1", "name": "my-feature", "createdAt": 1_700_000_000_000,
        "fullConversationHeadersOnly": [
            {"bubbleId": "b1", "type": 1},
            {"bubbleId": "b2", "type": 2},
        ],
    }
    conn.execute("INSERT INTO cursorDiskKV VALUES (?,?)",
                 ("composerData:c1", json.dumps(composer)))
    conn.execute("INSERT INTO cursorDiskKV VALUES (?,?)",
                 ("bubbleId:c1:b1", json.dumps({"type": 1, "text": "hello cursor"})))
    conn.execute("INSERT INTO cursorDiskKV VALUES (?,?)",
                 ("bubbleId:c1:b2", json.dumps({"type": 2, "text": "hi"})))
    conn.commit()
    conn.close()

    composers = translate.read_state_vscdb(path)
    assert len(composers) == 1
    assert composers[0]["id"] == "c1"
    assert len(composers[0]["bubbles"]) == 2

    out = translate.translate(composers)
    texts = [e.get("text") for e in out if e["type"] in ("user_prompt", "model_response")]
    assert texts == ["hello cursor", "hi"]
    for env in out:
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
