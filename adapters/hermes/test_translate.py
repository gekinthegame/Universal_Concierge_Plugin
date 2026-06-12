#!/usr/bin/env python3
"""Phase 6 fixture: replay a canned Hermes session through the Concierge adapter
and assert the host-neutral event stream it produces (Step 7).

Run: python3 adapters/hermes/test_translate.py
"""
import json
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import translate  # noqa: E402
import recorder  # noqa: E402


def test_pure_translation_shapes():
    env = translate.user_prompt("hermes", "s1", "2026-06-05T00:00:00Z", "hello")
    assert env == {
        "host_id": "hermes",
        "session_id": "s1",
        "ts": "2026-06-05T00:00:00Z",
        "type": "user_prompt",
        "text": "hello",
    }, env
    fe = translate.file_event_from_tool("hermes", "s1", "t", "write_file", {"path": "src/a.rs"})
    assert fe["type"] == "file_written" and fe["path"] == "src/a.rs", fe
    assert translate.file_event_from_tool("hermes", "s1", "t", "web_search", {}) is None
    print("ok: pure translation shapes")


def test_canned_session_produces_expected_event_stream():
    with tempfile.TemporaryDirectory() as tmp:
        # binary points nowhere so on_session_end's ingest is a harmless no-op.
        rec = recorder.ConciergeRecorder(workdir=tmp, binary="concierge-plugin-does-not-exist")
        sid = "sess-1"
        rec.on_session_start(session_id=sid, cwd=tmp)
        rec.pre_llm_call(session_id=sid, user_message="add a health endpoint")
        rec.pre_tool_call(
            session_id=sid, function_name="write_file", function_args={"path": "src/health.rs"}
        )
        rec.post_tool_call(
            session_id=sid,
            function_name="write_file",
            function_args={"path": "src/health.rs"},
            result='{"bytes": 12}',
        )
        rec.post_llm_call(session_id=sid, assistant_response="Added /health returning 200.")
        rec.on_session_end(session_id=sid)

        events = [json.loads(l) for l in rec.session_path(sid).read_text().strip().splitlines()]
        types = [e["type"] for e in events]
        assert types == [
            "session_started",
            "user_prompt",
            "tool_call_started",
            "tool_call_finished",
            "file_written",
            "model_response",
            "session_ended",
        ], types
        assert all(e["host_id"] == "hermes" and e["session_id"] == sid and "ts" in e for e in events)
        assert any(e["type"] == "file_written" and e["path"] == "src/health.rs" for e in events)
    print("ok: canned session -> expected host-neutral event stream")


def test_mount_opens_one_model_labeled_gui():
    with tempfile.TemporaryDirectory() as tmp:
        rec = recorder.ConciergeRecorder(workdir=tmp, binary="/tmp/concierge-plugin")
        with patch.object(recorder.subprocess, "Popen") as popen:
            rec.on_session_start(session_id="s1", model="hermes-model")
            rec.on_session_start(session_id="s2", model="other-model")
        assert popen.call_count == 1, popen.call_count
        assert popen.call_args.args[0] == [
            "/tmp/concierge-plugin", "gui", "--model", "hermes-model",
        ], popen.call_args
    print("ok: mount opens one model-labeled GUI")


if __name__ == "__main__":
    test_pure_translation_shapes()
    test_canned_session_produces_expected_event_stream()
    test_mount_opens_one_model_labeled_gui()
    print("ALL HERMES ADAPTER TESTS PASSED")
