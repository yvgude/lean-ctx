"""Fallback ``ContextEngine`` ABC.

A faithful, behaviour-equivalent re-declaration of Hermes Agent's
``agent.context_engine.ContextEngine`` interface
(https://hermes-agent.nousresearch.com/docs/developer-guide/context-engine-plugin).

This module is **only** used when the real host class cannot be imported — e.g.
when the plugin runs outside a Hermes checkout (standalone benchmarks, the test
suite, or `pip install`-only environments). Inside Hermes the genuine ABC is
imported instead and takes precedence (see ``engine.py``), so the engine always
subclasses the host's own contract at runtime.

Keeping the contract here lets the package import, type-check and test without a
Hermes installation. It declares the documented surface only; it never
re-implements any context-management behaviour.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
import json
from typing import Any, Dict, List, Optional

# Sentinel so a host that auto-imports this module can tell it apart from the
# real ABC (e.g. for warnings). Hermes' own class does not define it.
IS_LEANCTX_FALLBACK_ABC = True


class ContextEngine(ABC):
    """Minimal stand-in for ``agent.context_engine.ContextEngine``.

    Required overrides: :pyattr:`name`, :meth:`update_from_response`,
    :meth:`should_compress`, :meth:`compress`. Everything else has a sensible
    default, matching the documented host behaviour.
    """

    #: Class attributes the host reads directly for display and logging.
    last_prompt_tokens: int = 0
    last_completion_tokens: int = 0
    last_total_tokens: int = 0
    threshold_tokens: int = 0
    context_length: int = 0
    compression_count: int = 0

    def __init__(self, context_length: int = 0, **_: Any) -> None:
        # Instance-level copies so subclasses and instances stay independent.
        self.last_prompt_tokens = 0
        self.last_completion_tokens = 0
        self.last_total_tokens = 0
        self.context_length = int(context_length or 0)
        self.threshold_tokens = 0
        self.compression_count = 0

    # --- required -----------------------------------------------------------

    @property
    @abstractmethod
    def name(self) -> str:
        """Short identifier; must match the ``context.engine`` config value."""

    @abstractmethod
    def update_from_response(self, usage: Dict[str, Any]) -> None:
        """Update token counters from an LLM response ``usage`` dict."""

    @abstractmethod
    def should_compress(self, prompt_tokens: Optional[int] = None) -> bool:
        """Return ``True`` if compaction should fire this turn."""

    @abstractmethod
    def compress(
        self,
        messages: List[Dict[str, Any]],
        current_tokens: Optional[int] = None,
        focus_topic: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Return a new, valid OpenAI-format message list (possibly shorter)."""

    # --- optional (defaults mirror the host) --------------------------------

    def on_session_start(self, session_id: str, **kwargs: Any) -> None:
        """No-op by default; override to load persisted state."""

    def on_session_end(self, session_id: str, messages: List[Dict[str, Any]]) -> None:
        """No-op by default; override to flush state / close connections."""

    def on_session_reset(self) -> None:
        """Reset per-turn token counters (``/new`` or ``/reset``)."""
        self.last_prompt_tokens = 0
        self.last_completion_tokens = 0
        self.last_total_tokens = 0

    def update_model(
        self,
        model: str,
        context_length: Optional[int] = None,
        **kwargs: Any,
    ) -> None:
        """Recalculate budgets on model switch."""
        if context_length:
            self.context_length = int(context_length)

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        """Engine-provided agent tools; empty by default."""
        return []

    def handle_tool_call(self, name: str, args: Dict[str, Any], **kwargs: Any) -> str:
        """Dispatch an engine tool call; error JSON by default."""
        return json.dumps({"error": f"Unknown tool: {name}"})

    def should_compress_preflight(self, messages: List[Dict[str, Any]]) -> bool:
        """Cheap pre-API-call estimate; ``False`` by default."""
        return False

    def get_status(self) -> Dict[str, Any]:
        """Standard token/threshold status dict."""
        return {
            "name": self.name,
            "context_length": self.context_length,
            "threshold_tokens": self.threshold_tokens,
            "compression_count": self.compression_count,
            "last_prompt_tokens": self.last_prompt_tokens,
            "last_completion_tokens": self.last_completion_tokens,
            "last_total_tokens": self.last_total_tokens,
        }
