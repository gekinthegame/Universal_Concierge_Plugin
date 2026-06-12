# Harness Expansion Plan (Top 10 Integration)

*This document outlines the side-project strategy to build independent adapters for the 10 most popular AI harnesses. These adapters will be developed individually and subsequently merged into the `adapters/` directory of the Universal Concierge Plugin.*

## The Strategy: The Fidelity Ladder (Decision 0009)
The Universal Concierge Plugin does not require harnesses to adopt a proprietary API. Instead, we use a "Fidelity Ladder" to extract data based on what the harness exposes:

- **Tier 0:** Native Plugin (Deepest integration, e.g., Hermes)
- **Tier 1:** Lifecycle Hooks / Custom Instructions (e.g., Cursor Rules, Claude Code hooks)
- **Tier 2:** MCP Server Interception (Harness connects to UCP as an MCP server)
- **Tier 3:** Model-API Proxy (Intercepting raw HTTP calls to OpenAI/Anthropic)
- **Tier 4:** Log/Transcript Tailing (Parsing local JSON/text logs)

Each of the 10 target harnesses will be evaluated and assigned a Tier. The adapters will convert the harness-specific output into the universal JSONL format (`Prompt`, `Response`, `ToolResult`) expected by the Concierge.

> **Two constraints (inherit Decisions 0028 + 0022 + `THREAT_MODEL.md`).**
> 1. **Prefer observe-only tiers (4/1) over Tier-3 proxies.** A Tier-3 Model-API Proxy
>    sits *in the hot path* of the user's model traffic and handles their **API keys** —
>    that risks performance-invisibility (0022), adds a failure mode (proxy down → their
>    AI down), and creates a credential surface (keys must never be persisted — L6). Use a
>    proxy only when the harness exposes no logs.
> 2. **Tier-2 capture ≠ the MCP Router's default.** The MCP Router (Decision 0028) is
>    *opaque and captures nothing*. Capturing via MCP is the **separate, explicit,
>    off-by-default capture path**, not a side effect of routing. Don't conflate "router"
>    (access/routing) with "capture."

## Target Harnesses

### 1. Ollama (The Local-First Priority) (Tier 3)
- **Mechanism:** Ollama runs a local HTTP server (usually on port 11434) that mimics the OpenAI API.
- **Adapter Design:** A Tier 3 Model-API Proxy. Because Ollama is the backbone of the local-first, user-first AI movement, capturing its activity is paramount. The adapter will run as a local proxy (e.g., on port 11435). The user points their frontend (like Open WebUI, AnythingLLM, or custom scripts) to the proxy, which logs the prompt/response pairs to the Concierge before forwarding them to the real Ollama server.

### 2. Claude Code (Already Supported - Tier 1 & 4)
- **Mechanism:** Log tailing of `~/.claude/projects/` combined with a lifecycle hook for injection.
- **Status:** Baseline complete.

### 3. Cursor (Tier 1 & 2)

- **Mechanism:** Cursor supports custom `.cursorrules` and is rapidly adopting MCP. 
- **Adapter Design:** A Tier 1 adapter using a `.cursorrules` file that instructs the agent to log decisions to a specific local file, which the Concierge then tails. If Cursor's MCP client becomes robust, we will upgrade to Tier 2 (MCP Multiplexer interception).

### 4. Gemini (CLI/Local Harnesses) (Tier 3 & 4)
- **Mechanism:** Depends on the specific Gemini wrapper. For Google's official CLI or Vertex implementations, we will likely use a Tier 4 log tailer or a Tier 3 API proxy to capture the structured prompt/response pairs.

### 5. Codex / Copilot (Tier 3)
- **Mechanism:** GitHub Copilot / Codex is highly locked down. 
- **Adapter Design:** A Tier 3 Model-API Proxy. We will route the IDE's outbound requests through a local proxy that intercepts the code completions and logs them into the Concierge DAG before forwarding them to Microsoft's servers.

### 6. Claw (Tier 1 & 4)
- **Mechanism:** Assuming Claw operates as a terminal-based agent with local state.
- **Adapter Design:** Tier 4 transcript tailing. We will write a parser that monitors Claw's session files and translates its specific markdown/JSON output into the Concierge JSONL format.

### 7. Aider (Tier 4)
- **Mechanism:** Aider stores its chat history in a local `.aider.chat.history.md` file.
- **Adapter Design:** A robust Tier 4 parser. The adapter will watch the markdown file, parsing the user prompts and the model's unified diff responses into `Prompt` and `ToolResult` (Edit) nodes.

### 8. OpenHands (formerly SWE-agent) (Tier 1)
- **Mechanism:** OpenHands has a structured event stream and workspace state.
- **Adapter Design:** Tier 1 lifecycle hook. We will write a small Python bridge that subscribes to the OpenHands event bus and pushes standard JSONL events to the Concierge.

### 9. AutoGPT (Tier 4)
- **Mechanism:** AutoGPT writes extensive logs to its `workspace/logs` directory.
- **Adapter Design:** Tier 4 log parser. The adapter will map AutoGPT's specific `thought`, `reasoning`, and `plan` JSON blocks directly into the Concierge's new inline `reasoning` schema (Decision 0023).

### 10. Continue.dev (Tier 2 & 4)
- **Mechanism:** Continue is an open-source VS Code/JetBrains extension that logs local history and supports local context providers.
- **Adapter Design:** Tier 4 parsing of its local SQLite/JSON session history. Future integration will target Tier 2 if they support external MCP servers for context gathering.

### 11. ChatGPT Desktop App / Web (via Proxy/Export) (Tier 3 & 4)
- **Mechanism:** Highly proprietary.
- **Adapter Design:** 
  - *Option A (Live):* Tier 3 proxy interception for local desktop app traffic.
  - *Option B (Archive):* A one-shot Phase 2.5 backfill importer that ingests the standard ChatGPT `conversations.json` data export into the IPLD DAG.

## Independent Development Workflow
Because the Universal Concierge Plugin consumes a standard JSONL stream, these adapters can be developed entirely independently of the core Rust codebase. 

1. **Create Repo/Folder:** For each harness, create an isolated script (e.g., `aider_to_concierge.py`).
2. **Standardize Output:** Ensure the script outputs `{"type": "prompt", ...}`, `{"type": "response", ...}`, etc.
3. **Merge:** Once verified, drop the script into the `adapters/` folder of the main project.
