"""Concierge memory adapter for Hermes Agent (Phase 6 — first harness adapter).

An **observe-only** Hermes plugin: it registers for Hermes's public lifecycle
hooks and records session / LLM / tool / file events into Concierge IPLD memory
via the host-neutral JSONL contract — *zero changes to Hermes core*.

Mount it by copying this directory into ``~/.hermes/plugins/concierge-memory/``
(or the bundled ``plugins/``) and enabling it (``plugins.enabled``). See README.
"""
from __future__ import annotations

from .recorder import ConciergeRecorder

_recorder: ConciergeRecorder | None = None


def register(ctx) -> None:
    """Hermes entry point: wire the recorder onto the lifecycle hooks."""
    global _recorder
    _recorder = ConciergeRecorder()
    ctx.register_hook("on_session_start", _recorder.on_session_start)
    ctx.register_hook("pre_llm_call", _recorder.pre_llm_call)
    ctx.register_hook("post_llm_call", _recorder.post_llm_call)
    ctx.register_hook("pre_tool_call", _recorder.pre_tool_call)
    ctx.register_hook("post_tool_call", _recorder.post_tool_call)
    ctx.register_hook("on_session_end", _recorder.on_session_end)
