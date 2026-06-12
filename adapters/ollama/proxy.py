"""Ollama logging proxy -> host-neutral Envelope capture.

Tier-3 (the only proxy in the top-5, and it's localhost): sit in front of a local
Ollama server, forward every request transparently, and append a host-neutral
JSONL record of each prompt/response to a capture file. Point your frontend
(Open WebUI, scripts, …) at this proxy instead of Ollama directly.

    you -> http://127.0.0.1:11435  (this proxy)  ->  http://127.0.0.1:11434 (ollama)

Captured (both the native `/api/chat` and the OpenAI-compatible
`/v1/chat/completions`):

    first turn of a conversation -> session_started
    last user message            -> user_prompt {text}
    assembled assistant reply     -> model_response {text}

Constraints (Decision 0022 + Threat Model L6): never sits in the hot path beyond a
pass-through, and **never persists credentials** — any `Authorization` header is
forwarded but never written to the capture file. The pure helpers below are
unit-testable without a network.

Run:  python3 proxy.py            # proxy :11435 -> ollama :11434, capture ~/.concierge/ollama-capture.jsonl
Then: concierge-plugin ingest ~/.concierge/ollama-capture.jsonl
"""
from __future__ import annotations

import datetime
import hashlib
import json
import os
from typing import Any, Dict, List, Optional, Set

_MAX_FIELD = 8192
HOST_ID = "ollama"


def envelope(host_id, session_id, ts, event_type, **fields):
    env = {"host_id": host_id, "session_id": session_id, "ts": ts, "type": event_type}
    for key, value in fields.items():
        if value is not None:
            env[key] = value
    return env


def _now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _message_text(message: Dict[str, Any]) -> str:
    """OpenAI/Ollama message ``content`` is a str, or a list of ``{type:"text",text}``."""
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(b.get("text", "") for b in content if isinstance(b, dict))
    return ""


def last_user_prompt(messages: List[Dict[str, Any]]) -> Optional[str]:
    for message in reversed(messages):
        if isinstance(message, dict) and message.get("role") == "user":
            text = _message_text(message).strip()
            return text or None
    return None


def session_id_for(messages: List[Dict[str, Any]]) -> str:
    """A stable id per conversation: hash of the first message's text. The same
    chat (which Ollama replays in full each turn) keeps one session id."""
    root = ""
    for message in messages:
        if isinstance(message, dict) and message.get("role") in ("user", "system"):
            root = _message_text(message)
            if root:
                break
    digest = hashlib.sha1(root.encode("utf-8", "replace")).hexdigest()[:12]
    return f"ollama-{digest}"


def assemble_response(chunks: List[str], openai_style: bool) -> str:
    """Reassemble the assistant text from streamed (or single) response chunks.

    - native `/api/chat`: newline-delimited JSON, each ``{message:{content},done}``
    - OpenAI `/v1/chat/completions`: SSE ``data: {choices:[{delta:{content}}]}``
      (or a single non-streamed ``{choices:[{message:{content}}]}``)
    """
    parts: List[str] = []
    for raw in chunks:
        for line in raw.splitlines():
            line = line.strip()
            if not line:
                continue
            if openai_style:
                if line.startswith("data:"):
                    line = line[len("data:"):].strip()
                if line == "[DONE]":
                    continue
            try:
                obj = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            if openai_style:
                for choice in obj.get("choices", []):
                    delta = choice.get("delta") or {}
                    msg = choice.get("message") or {}
                    parts.append(delta.get("content") or msg.get("content") or "")
            else:
                msg = obj.get("message") or {}
                parts.append(msg.get("content") or "")
    return "".join(parts)


def events_for(request_json: Dict[str, Any], assistant_text: str,
               seen_sessions: Set[str], host_id: str = HOST_ID) -> List[Dict[str, Any]]:
    """Host-neutral envelopes for one request/response exchange."""
    messages = request_json.get("messages") or []
    if not isinstance(messages, list):
        return []
    sid = session_id_for(messages)
    ts = _now()
    out: List[Dict[str, Any]] = []
    if sid not in seen_sessions:
        seen_sessions.add(sid)
        out.append(envelope(host_id, sid, ts, "session_started"))
    prompt = last_user_prompt(messages)
    if prompt:
        out.append(envelope(host_id, sid, ts, "user_prompt", text=prompt[:_MAX_FIELD]))
    if assistant_text.strip():
        out.append(envelope(host_id, sid, ts, "model_response", text=assistant_text[:_MAX_FIELD]))
    return out


# ── The proxy server (I/O; the logic above is what the tests exercise) ───────

def _serve(listen_port: int, upstream_host: str, upstream_port: int, capture_path: str) -> None:
    import http.client
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    seen_sessions: Set[str] = set()
    chat_paths = ("/api/chat", "/v1/chat/completions")

    def write_events(events: List[Dict[str, Any]]) -> None:
        if not events:
            return
        os.makedirs(os.path.dirname(capture_path), exist_ok=True)
        with open(capture_path, "a", encoding="utf-8") as fh:
            for env in events:
                fh.write(json.dumps(env, ensure_ascii=False) + "\n")

    # Hop-by-hop / framing headers we must not blindly forward: http.client hands us
    # the already-de-chunked body, so re-sending the upstream's Content-Length /
    # Transfer-Encoding would mis-frame a streamed reply. HTTP/1.0 close-framing
    # (read-until-close) streams Ollama's line-delimited JSON + SSE correctly.
    SKIP_HEADERS = {"content-length", "transfer-encoding", "connection",
                    "keep-alive", "proxy-connection", "date", "server"}

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def log_message(self, *args):  # quiet
            pass

        def _forward(self, body: Optional[bytes]):
            conn = http.client.HTTPConnection(upstream_host, upstream_port, timeout=600)
            headers = {k: v for k, v in self.headers.items() if k.lower() != "host"}
            conn.request(self.command, self.path, body=body, headers=headers)
            return conn.getresponse()

        def do_GET(self):
            self._pass_through(None)

        def do_POST(self):
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else b""
            capture = self.path in chat_paths
            request_json = {}
            if capture:
                try:
                    request_json = json.loads(body or b"{}")
                except (json.JSONDecodeError, ValueError):
                    capture = False
            self._pass_through(body, request_json if capture else None)

        def _pass_through(self, body, request_json=None):
            try:
                resp = self._forward(body)
            except OSError as exc:
                self.send_error(502, f"ollama upstream unreachable: {exc}")
                return
            self.send_response(resp.status)
            chunks: List[str] = []
            for k, v in resp.getheaders():
                if k.lower() in SKIP_HEADERS:
                    continue
                self.send_header(k, v)
            self.send_header("Connection", "close")
            self.end_headers()
            # Stream the body back to the client, teeing chunks for capture.
            while True:
                chunk = resp.read(8192)
                if not chunk:
                    break
                if request_json is not None:
                    chunks.append(chunk.decode("utf-8", "replace"))
                try:
                    self.wfile.write(chunk)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    break
            if request_json is not None:
                openai_style = self.path == "/v1/chat/completions"
                text = assemble_response(chunks, openai_style)
                write_events(events_for(request_json, text, seen_sessions))

    server = ThreadingHTTPServer(("127.0.0.1", listen_port), Handler)
    print(f"ollama logging proxy: http://127.0.0.1:{listen_port} -> "
          f"http://{upstream_host}:{upstream_port}", flush=True)
    print(f"capture -> {capture_path}  (ingest with: concierge-plugin ingest {capture_path})", flush=True)
    server.serve_forever()


def _main(argv: List[str]) -> int:
    listen = int(os.environ.get("OLLAMA_PROXY_PORT", "11435"))
    up_host = os.environ.get("OLLAMA_HOST", "127.0.0.1")
    up_port = int(os.environ.get("OLLAMA_PORT", "11434"))
    capture = os.environ.get(
        "OLLAMA_CAPTURE",
        os.path.expanduser("~/.concierge/ollama-capture.jsonl"),
    )
    _serve(listen, up_host, up_port, capture)
    return 0


if __name__ == "__main__":
    import sys
    raise SystemExit(_main(sys.argv[1:]))
