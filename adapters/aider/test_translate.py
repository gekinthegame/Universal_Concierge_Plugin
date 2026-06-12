"""Tests for the Aider chat-history -> host-neutral envelope translation.

Self-contained (no pytest): ``python3 test_translate.py``. The sample mirrors the
real `.aider.chat.history.md` format (session headers, '#### ' user lines,
unprefixed assistant prose, '> ' console lines).
"""
from __future__ import annotations

import translate

SAMPLE = """
# aider chat started at 2025-03-07 17:45:00

#### add a jsonl parser
#### keep it small

Sure, here is a parser:

```python
def parse(line):
    return json.loads(line)
```

> Applied edit to parser.py
> Tokens: 1.2k sent, 200 received.

#### now add a test

Done.

# aider chat started at 2025-03-08 09:00:00

#### refactor it

ok
"""


def test_two_sessions_detected():
    out = translate.translate(SAMPLE)
    starts = [e for e in out if e["type"] == "session_started"]
    ends = [e for e in out if e["type"] == "session_ended"]
    assert len(starts) == 2
    assert len(ends) == 2
    assert out[0]["host_id"] == "aider"


def test_header_timestamp_parsed():
    out = translate.translate(SAMPLE)
    first = [e for e in out if e["type"] == "session_started"][0]
    assert first["ts"] == "2025-03-07T17:45:00Z"


def test_user_lines_grouped_and_unprefixed():
    out = translate.translate(SAMPLE)
    prompts = [e["text"] for e in out if e["type"] == "user_prompt"]
    # consecutive '#### ' lines merge into one prompt; prefix stripped
    assert prompts[0] == "add a jsonl parser\nkeep it small"
    assert "now add a test" in prompts
    assert "refactor it" in prompts


def test_assistant_prose_captured():
    out = translate.translate(SAMPLE)
    responses = [e["text"] for e in out if e["type"] == "model_response"]
    assert any("here is a parser" in r and "def parse" in r for r in responses)


def test_applied_edit_becomes_file_written():
    out = translate.translate(SAMPLE)
    writes = [e for e in out if e["type"] == "file_written"]
    assert writes and writes[0]["path"] == "parser.py"


def test_token_console_line_skipped():
    out = translate.translate(SAMPLE)
    blob = " ".join(e.get("text", "") for e in out)
    assert "Tokens:" not in blob   # '> ' console noise is dropped


def test_every_envelope_has_required_keys():
    for env in translate.translate(SAMPLE):
        assert {"host_id", "session_id", "ts", "type"} <= set(env)


def _main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t(); print(f"ok  {t.__name__}")
    print(f"\n{len(tests)} passed")


if __name__ == "__main__":
    _main()
