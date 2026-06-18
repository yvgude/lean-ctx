"""Engine configuration, read from ``LEANCTX_*`` environment variables.

The ``compression.*`` block in Hermes' ``config.yaml`` is specific to the
built-in ``ContextCompressor``; per the plugin guide each engine defines its
own config. We use a dedicated ``LEANCTX_*`` namespace (analogous to
``LCM_*``) so the two never collide.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Optional

# Default port of the lean-ctx HTTP tools API (`lean-ctx serve`). The plugin
# speaks the `/v1/tools/call` REST contract, which is served by `lean-ctx serve`
# (and the daemon's IPC socket) — NOT by the LLM proxy on the 4444+ port, whose
# router 404s `/v1/tools/call`. So the default targets the serve port; set
# ``LEANCTX_BASE_URL`` (or ``LEANCTX_HTTP_PORT``) for non-default binds.
_DEFAULT_HTTP_PORT = 8080
# Sensible default context window when the host has not called update_model().
_DEFAULT_CONTEXT_LENGTH = 200_000


def _default_base_url() -> str:
    raw = os.environ.get("LEANCTX_HTTP_PORT")
    port = _DEFAULT_HTTP_PORT
    if raw and raw.strip():
        try:
            port = int(raw.strip())
        except ValueError:
            port = _DEFAULT_HTTP_PORT
    return f"http://127.0.0.1:{port}"


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        return int(raw.strip())
    except ValueError:
        return default


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        return float(raw.strip())
    except ValueError:
        return default


def _env_str(name: str, default: Optional[str] = None) -> Optional[str]:
    raw = os.environ.get(name)
    if raw is None:
        return default
    raw = raw.strip()
    return raw or default


@dataclass(frozen=True)
class LeanCtxConfig:
    """Immutable engine configuration."""

    base_url: str
    token: Optional[str] = None
    timeout: float = 30.0
    workspace_id: Optional[str] = None
    channel_id: Optional[str] = None

    context_length: int = _DEFAULT_CONTEXT_LENGTH
    # Fraction of the context window at which compaction fires.
    threshold_fraction: float = 0.75
    # Recent context to always keep verbatim (the "fresh tail").
    protect_fraction: float = 0.25
    protect_min_messages: int = 6
    protect_min_tokens: int = 2_000

    # Expose lean-ctx recall/intelligence tools natively to the agent.
    enable_tools: bool = True
    # Prefer the daemon's `ctx_transcript_compact` core tool (Single Source of
    # Truth) over the local Python compaction. Falls back automatically when the
    # daemon is unavailable or the tool is missing (older daemon).
    use_core_compaction: bool = True

    def threshold_tokens(self) -> int:
        return max(1, int(self.context_length * self.threshold_fraction))

    def protect_tokens(self) -> int:
        return max(self.protect_min_tokens, int(self.context_length * self.protect_fraction))

    def with_context_length(self, context_length: int) -> "LeanCtxConfig":
        """Return a copy with an updated context window (on model switch)."""
        from dataclasses import replace

        return replace(self, context_length=max(1, int(context_length)))

    @classmethod
    def from_env(cls) -> "LeanCtxConfig":
        return cls(
            base_url=_env_str("LEANCTX_BASE_URL") or _default_base_url(),
            token=_env_str("LEANCTX_TOKEN"),
            timeout=_env_float("LEANCTX_TIMEOUT", 30.0),
            workspace_id=_env_str("LEANCTX_WORKSPACE_ID"),
            channel_id=_env_str("LEANCTX_CHANNEL_ID"),
            context_length=_env_int("LEANCTX_CONTEXT_LENGTH", _DEFAULT_CONTEXT_LENGTH),
            threshold_fraction=_env_float("LEANCTX_THRESHOLD_FRACTION", 0.75),
            protect_fraction=_env_float("LEANCTX_PROTECT_FRACTION", 0.25),
            protect_min_messages=_env_int("LEANCTX_PROTECT_MIN_MESSAGES", 6),
            protect_min_tokens=_env_int("LEANCTX_PROTECT_MIN_TOKENS", 2_000),
            enable_tools=_env_str("LEANCTX_ENABLE_TOOLS", "1") not in {"0", "false", "no", "off"},
            use_core_compaction=_env_str("LEANCTX_CORE_COMPACTION", "1")
            not in {"0", "false", "no", "off"},
        )
