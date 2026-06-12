"""Cursor -> host-neutral Envelope translation.

Tier-4 (observe-only): read Cursor's local chat database, no proxy, no keys.
Pure mapping (``translate``) plus a SQLite reader (``read_state_vscdb``). Output
shapes match ``crates/core/src/event.rs``.

Cursor stores chats in a SQLite file ``state.vscdb`` (globalStorage), table
``cursorDiskKV``:

    composerData:<composerId>            -> one conversation's metadata
        {composerId, name?, createdAt?, fullConversationHeadersOnly?:[{bubbleId,type}]}
    bubbleId:<composerId>:<bubbleId>     -> one message "bubble"
        {type: 1=user | 2=assistant, text, toolFormerData?}

Older workspace DBs keep the same JSON under ``ItemTable`` key
``workbench.panel.aichat.view.aichat.chatdata``; we read that too when present.

Mapping per ordered bubble:
    type 1 (user)       -> user_prompt {text}
    type 2 (assistant)  -> model_response {text}
    toolFormerData       -> tool_call_started {tool, args_json} + tool_call_finished

Schema is the community-documented Cursor format (cursor-chat-export /
cursor-history); robust to missing fields and version drift.
"""
from __future__ import annotations

import json
import os
from typing import Any, Dict, Iterable, List, Optional

_MAX_FIELD = 8192
_MS_PER_S = 1000


def envelope(host_id, session_id, ts, event_type, project_id=None, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    if project_id:
        env["project_id"] = project_id
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _ts(ms: Any) -> str:
    """Cursor timestamps are ms since epoch; render ISO-8601 UTC."""
    try:
        import datetime
        return datetime.datetime.fromtimestamp(float(ms) / _MS_PER_S, datetime.timezone.utc) \
            .strftime("%Y-%m-%dT%H:%M:%SZ")
    except (TypeError, ValueError, OverflowError):
        return "1970-01-01T00:00:00Z"


def _bubble_text(bubble: Dict[str, Any]) -> Optional[str]:
    for key in ("text", "richText", "content"):
        v = bubble.get(key)
        if isinstance(v, str) and v.strip():
            return v
    return None


def _compact(value: Any) -> Optional[str]:
    if value is None:
        return None
    if isinstance(value, str):
        return value[:_MAX_FIELD]
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))[:_MAX_FIELD]
    except (TypeError, ValueError):
        return str(value)[:_MAX_FIELD]


def translate(composers: Iterable[Dict[str, Any]], host_id: str = "cursor") -> List[Dict[str, Any]]:
    """Map conversations (each ``{id, name?, createdAt?, bubbles:[…ordered…]}``) to
    host-neutral envelopes."""
    out: List[Dict[str, Any]] = []
    for composer in composers:
        if not isinstance(composer, dict):
            continue
        sid = composer.get("id") or composer.get("composerId") or "cursor-session"
        ts = _ts(composer.get("createdAt"))
        pid = composer.get("name") or None
        out.append(envelope(host_id, sid, ts, "session_started", project_id=pid))
        for bubble in composer.get("bubbles") or []:
            if not isinstance(bubble, dict):
                continue
            btype = bubble.get("type")
            tool = bubble.get("toolFormerData")
            if isinstance(tool, dict):
                name = tool.get("name") or tool.get("tool") or "tool"
                out.append(envelope(host_id, sid, ts, "tool_call_started",
                                    project_id=pid, tool=name,
                                    args_json=_compact(tool.get("rawArgs") or tool.get("params"))))
                status = (tool.get("status") or "").lower()
                out.append(envelope(host_id, sid, ts, "tool_call_finished",
                                    project_id=pid, tool=name,
                                    ok=status not in ("error", "failed"),
                                    result_json=_compact(tool.get("result"))))
            text = _bubble_text(bubble)
            if not text:
                continue
            if btype == 1:
                out.append(envelope(host_id, sid, ts, "user_prompt", project_id=pid, text=text))
            elif btype == 2:
                out.append(envelope(host_id, sid, ts, "model_response", project_id=pid, text=text))
        out.append(envelope(host_id, sid, ts, "session_ended", project_id=pid))
    return out


# ── SQLite reading (the I/O half) ───────────────────────────────────────────

def _kv_rows(conn) -> Dict[str, str]:
    """All key->value rows from whichever table this DB uses."""
    rows: Dict[str, str] = {}
    for table, kcol, vcol in (("cursorDiskKV", "key", "value"), ("ItemTable", "key", "value")):
        try:
            cur = conn.execute(f"SELECT {kcol}, {vcol} FROM {table}")
        except Exception:
            continue
        for k, v in cur.fetchall():
            if isinstance(v, (bytes, bytearray)):
                v = v.decode("utf-8", "replace")
            rows[k] = v
    return rows


def read_state_vscdb(path: str) -> List[Dict[str, Any]]:
    """Read ``state.vscdb`` into ordered composer dicts ready for ``translate``."""
    import sqlite3
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        rows = _kv_rows(conn)
    finally:
        conn.close()

    bubbles: Dict[str, Dict[str, Any]] = {}     # "<composerId>:<bubbleId>" -> bubble
    composers: Dict[str, Dict[str, Any]] = {}
    for key, value in rows.items():
        if key.startswith("bubbleId:"):
            rest = key[len("bubbleId:"):]
            bubbles[rest] = _loads(value)
        elif key.startswith("composerData:"):
            cid = key[len("composerData:"):]
            meta = _loads(value)
            meta["composerId"] = meta.get("composerId") or cid
            composers[cid] = meta

    out: List[Dict[str, Any]] = []
    for cid, meta in composers.items():
        headers = meta.get("fullConversationHeadersOnly") or meta.get("conversation") or []
        ordered: List[Dict[str, Any]] = []
        if headers:
            for h in headers:
                bid = h.get("bubbleId") if isinstance(h, dict) else h
                b = bubbles.get(f"{cid}:{bid}")
                if b is not None:
                    if isinstance(h, dict) and "type" in h and "type" not in b:
                        b["type"] = h["type"]
                    ordered.append(b)
        else:
            ordered = [b for k, b in bubbles.items() if k.startswith(f"{cid}:")]
        out.append({
            "id": cid,
            "name": meta.get("name") or meta.get("title"),
            "createdAt": meta.get("createdAt"),
            "bubbles": ordered,
        })
    return out


def _loads(value: Any) -> Dict[str, Any]:
    if isinstance(value, dict):
        return value
    try:
        return json.loads(value)
    except (TypeError, ValueError, json.JSONDecodeError):
        return {}


def _default_db() -> Optional[str]:
    candidates = [
        "~/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
        "~/.config/Cursor/User/globalStorage/state.vscdb",
    ]
    for c in candidates:
        p = os.path.expanduser(c)
        if os.path.exists(p):
            return p
    return None


def _main(argv: List[str]) -> int:
    import sys
    path = argv[0] if argv else _default_db()
    if not path:
        print("usage: translate.py [state.vscdb]   (default: Cursor globalStorage)", file=sys.stderr)
        print("  pipe to: concierge-plugin ingest -", file=sys.stderr)
        return 2
    for env in translate(read_state_vscdb(path)):
        sys.stdout.write(json.dumps(env, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
