"""Resilient gateway around the lean-ctx HTTP ``/v1`` SDK.

The engine runs inside the agent's synchronous turn loop, so every call must be
fast and must never raise into the host: a missing ``leanctx`` package or a
down daemon degrades to a logged no-op, not a crash.
"""

from __future__ import annotations

import logging
import time
from typing import Any, Dict, Optional

from .config import LeanCtxConfig

logger = logging.getLogger(__name__)

# How long a successful/failed health probe is trusted before re-checking.
_HEALTH_TTL_SECONDS = 30.0


class ToolGateway:
    """Lazy, fault-tolerant facade over ``leanctx.LeanCtxClient``."""

    def __init__(self, config: LeanCtxConfig) -> None:
        self._config = config
        self._client: Any = None
        self._client_error: Optional[str] = None
        self._healthy: Optional[bool] = None
        self._health_checked_at: float = 0.0

    # --- client construction ------------------------------------------------

    def _get_client(self) -> Any:
        if self._client is not None or self._client_error is not None:
            return self._client
        try:
            from leanctx import LeanCtxClient
        except Exception as exc:  # pragma: no cover - depends on install
            self._client_error = f"leanctx SDK not importable: {exc}"
            logger.warning(
                "lean-ctx engine: %s — install with `pip install leanctx`. "
                "Compaction/recall will no-op until resolved.",
                self._client_error,
            )
            return None
        try:
            self._client = LeanCtxClient(
                self._config.base_url,
                bearer_token=self._config.token,
                workspace_id=self._config.workspace_id,
                channel_id=self._config.channel_id,
                timeout=self._config.timeout,
            )
        except Exception as exc:
            self._client_error = f"failed to construct LeanCtxClient: {exc}"
            logger.warning("lean-ctx engine: %s", self._client_error)
            return None
        return self._client

    # --- health -------------------------------------------------------------

    def is_available(self, *, force: bool = False) -> bool:
        """Return whether the daemon is reachable, cached for a short TTL."""
        now = time.monotonic()
        if (
            not force
            and self._healthy is not None
            and (now - self._health_checked_at) < _HEALTH_TTL_SECONDS
        ):
            return self._healthy
        client = self._get_client()
        if client is None:
            self._healthy = False
            self._health_checked_at = now
            return False
        try:
            client.health()
            self._healthy = True
        except Exception as exc:
            if self._healthy is not False:
                logger.warning(
                    "lean-ctx engine: daemon unreachable at %s (%s). "
                    "Operating in degraded no-op mode until it returns.",
                    self._config.base_url,
                    exc,
                )
            self._healthy = False
        self._health_checked_at = now
        return self._healthy

    # --- calls --------------------------------------------------------------

    def call_text(self, name: str, arguments: Optional[Dict[str, Any]] = None) -> Optional[str]:
        """Call a tool, returning its text result or ``None`` on any failure."""
        client = self._get_client()
        if client is None:
            return None
        try:
            return client.call_tool_text(name, arguments or {})
        except Exception as exc:
            logger.warning("lean-ctx engine: tool '%s' failed: %s", name, exc)
            self._healthy = False
            self._health_checked_at = time.monotonic()
            return None

    def get_metrics(self) -> Optional[Dict[str, Any]]:
        client = self._get_client()
        if client is None:
            return None
        try:
            result = client.metrics()
            return result if isinstance(result, dict) else None
        except Exception as exc:
            logger.debug("lean-ctx engine: metrics() failed: %s", exc)
            return None

    def get_context_summary(self) -> Optional[Dict[str, Any]]:
        client = self._get_client()
        if client is None:
            return None
        try:
            result = client.context_summary()
            return result if isinstance(result, dict) else None
        except Exception as exc:
            logger.debug("lean-ctx engine: context_summary() failed: %s", exc)
            return None
