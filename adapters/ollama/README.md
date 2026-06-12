# Ollama adapter (Tier-3, local logging proxy)

Captures **Ollama** prompt/response pairs into the Universal Concierge Plugin's
host-neutral JSONL stream by sitting in front of your local Ollama server as a
transparent proxy. This is the *only* proxy in the top-5 (Tier 3), and it's
**localhost-only**.

```
your frontend  ->  http://127.0.0.1:11435  (this proxy)  ->  http://127.0.0.1:11434  (ollama)
```

Point Open WebUI / AnythingLLM / your scripts at `:11435` instead of `:11434`.
Every `/api/chat` and `/v1/chat/completions` exchange is forwarded unchanged and a
record is appended to the capture file:

| | |
|---|---|
| first turn of a conversation | `session_started` |
| last user message | `user_prompt {text}` |
| assembled assistant reply (stream-aware) | `model_response {text}` |

## Safety (Decisions 0022 / 0028, Threat Model L6)
- **Pass-through only** — it never alters traffic; if Ollama is down it returns 502
  (it doesn't fail open silently).
- **Never persists credentials** — an `Authorization` header is forwarded but never
  written to the capture file (the capture logic only ever reads `messages`).
- Capture is **off unless you run it** and point a frontend at it — capture is the
  explicit, opt-in path, never a side effect of routing.

## Use
```sh
python3 proxy.py        # proxy :11435 -> ollama :11434, capture ~/.concierge/ollama-capture.jsonl
# then, periodically (or on a tail):
concierge-plugin ingest ~/.concierge/ollama-capture.jsonl
```
Env overrides: `OLLAMA_PROXY_PORT`, `OLLAMA_HOST`, `OLLAMA_PORT`, `OLLAMA_CAPTURE`.

## Test
```sh
python3 test_translate.py    # 7 unit tests (no network): stream assembly, sessions, key-safety
```
Forwarding + streamed-response capture verified end-to-end against both the real
local Ollama (`/api/version`) and a deterministic fake upstream.
