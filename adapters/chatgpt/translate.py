"""ChatGPT data export (`conversations.json`) -> host-neutral Envelope translation.

Tier-4 (one-shot backfill, observe-only): import the conversations from a ChatGPT
"Export data" archive. Pure mapping; the `__main__` wrapper reads the file. Output
shapes match ``crates/core/src/event.rs``.

The export's `conversations.json` is a JSON **array** of conversations. Each is:

    {
      "title": "...", "create_time": 1700000000.0, "update_time": ...,
      "mapping": {
        "<node-id>": {
          "id": "<node-id>", "parent": "<id|null>", "children": ["..."],
          "message": {                          # may be null (root/empty nodes)
            "id": "...", "create_time": 1700000000.0,
            "author": { "role": "user|assistant|system|tool" },
            "content": { "content_type": "text", "parts": ["...", ...] }
          }
        }, ...
      },
      "current_node": "<id>"
    }

We walk each conversation's `mapping` in turn order (parent links from
`current_node` back to the root, then reversed) and map:

    role user      -> user_prompt {text}
    role assistant -> model_response {text}
    (system / tool / empty parts are skipped)
"""
from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

_MAX_FIELD = 8192


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _iso(unix: Any) -> str:
    try:
        import datetime
        return datetime.datetime.fromtimestamp(float(unix), datetime.timezone.utc) \
            .strftime("%Y-%m-%dT%H:%M:%SZ")
    except (TypeError, ValueError, OverflowError):
        return "1970-01-01T00:00:00Z"


def _parts_text(content: Any) -> Optional[str]:
    """Join the text `parts` of a message's `content` (skips non-string parts)."""
    if not isinstance(content, dict):
        return None
    parts = content.get("parts")
    if not isinstance(parts, list):
        return None
    texts = [p for p in parts if isinstance(p, str) and p]
    joined = "\n".join(texts).strip()
    return joined or None


def _ordered_messages(conversation: Dict[str, Any]) -> List[Dict[str, Any]]:
    """The conversation's messages in turn order. Follow parent links from
    `current_node` to the root (the active branch), then reverse; fall back to a
    create_time sort if the links are missing."""
    mapping = conversation.get("mapping")
    if not isinstance(mapping, dict):
        return []
    ordered: List[Dict[str, Any]] = []
    node_id = conversation.get("current_node")
    seen = set()
    while isinstance(node_id, str) and node_id in mapping and node_id not in seen:
        seen.add(node_id)
        node = mapping[node_id]
        msg = node.get("message")
        if isinstance(msg, dict):
            ordered.append(msg)
        node_id = node.get("parent")
    if ordered:
        ordered.reverse()
        return ordered
    # Fallback: every message, sorted by create_time.
    msgs = [n.get("message") for n in mapping.values() if isinstance(n.get("message"), dict)]
    msgs.sort(key=lambda m: m.get("create_time") or 0)
    return msgs


def translate(conversations: List[Dict[str, Any]], host_id: str = "chatgpt") -> List[Dict[str, Any]]:
    out: List[Dict[str, Any]] = []
    for index, conversation in enumerate(conversations):
        if not isinstance(conversation, dict):
            continue
        sid = conversation.get("conversation_id") or conversation.get("id") or f"chatgpt-{index}"
        start_ts = _iso(conversation.get("create_time"))
        title = conversation.get("title")
        out.append(envelope(host_id, sid, start_ts, "session_started", cwd=None))
        if title:
            out.append(envelope(host_id, sid, start_ts, "memory_recorded", text=f"ChatGPT conversation: {title}"))
        for message in _ordered_messages(conversation):
            role = (message.get("author") or {}).get("role")
            text = _parts_text(message.get("content"))
            if not text:
                continue
            ts = _iso(message.get("create_time") or conversation.get("create_time"))
            if role == "user":
                out.append(envelope(host_id, sid, ts, "user_prompt", text=text[:_MAX_FIELD]))
            elif role == "assistant":
                out.append(envelope(host_id, sid, ts, "model_response", text=text[:_MAX_FIELD]))
            # system / tool messages are skipped
        out.append(envelope(host_id, sid, _iso(conversation.get("update_time")), "session_ended"))
    return out


def _load(path: str) -> List[Dict[str, Any]]:
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    # The export is a top-level array; some tools wrap it as {"conversations":[...]}.
    if isinstance(data, dict) and isinstance(data.get("conversations"), list):
        return data["conversations"]
    if isinstance(data, list):
        return data
    return []


def _main(argv: List[str]) -> int:
    import sys
    if not argv:
        print("usage: translate.py <conversations.json>  > events.jsonl", file=sys.stderr)
        print("  (from a ChatGPT 'Export data' archive); pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for path in argv:
        for env in translate(_load(path)):
            sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
