"""LiteLLM integration for lean-ctx.

Compresses the ``messages`` of an outbound LiteLLM request through the local
proxy before it is sent to the provider. Two entry points:

* :func:`compress_request_data` — framework-agnostic helper that rewrites a
  request ``dict`` in place (testable without LiteLLM installed).
* :class:`LeanCtxLiteLLMHandler` — a LiteLLM ``CustomLogger`` that hooks
  ``async_pre_call_hook`` for use with LiteLLM Proxy or ``litellm.callbacks``.

Register programmatically::

    import litellm
    from lean_ctx.litellm import LeanCtxLiteLLMHandler

    litellm.callbacks = [LeanCtxLiteLLMHandler(model="gpt-4o")]
"""

from __future__ import annotations

import asyncio
from typing import Any, Dict, Optional

from .errors import LeanCtxError
from .proxy import ProxyClient

_PRECALL_TYPES = ("completion", "text_completion")


def compress_request_data(
    data: Dict[str, Any],
    *,
    client: Optional[ProxyClient] = None,
    model: Optional[str] = None,
    raise_on_error: bool = False,
) -> Dict[str, Any]:
    """Compress ``data["messages"]`` in place and return ``data``.

    Mirrors the LiteLLM/OpenAI request shape. On a proxy failure the messages are
    left untouched — a compaction hiccup must never block the LLM call — unless
    ``raise_on_error`` is set.
    """
    messages = data.get("messages")
    if not isinstance(messages, list) or not messages:
        return data
    proxy = client or ProxyClient()
    try:
        data["messages"] = proxy.compress(messages, model=model or data.get("model")).messages
    except LeanCtxError:
        if raise_on_error:
            raise
    return data


try:  # pragma: no cover - import wiring, exercised by presence/absence of litellm
    from litellm.integrations.custom_logger import CustomLogger

    _BASE: type = CustomLogger
    _LITELLM_AVAILABLE = True
except Exception:  # noqa: BLE001 - any litellm import failure means "not available"
    _BASE = object
    _LITELLM_AVAILABLE = False


class LeanCtxLiteLLMHandler(_BASE):  # type: ignore[misc,valid-type]
    """LiteLLM ``CustomLogger`` that compresses requests in ``async_pre_call_hook``.

    The synchronous (urllib) :class:`ProxyClient` call is dispatched to a thread
    so it never blocks the proxy event loop.
    """

    def __init__(
        self,
        *,
        model: Optional[str] = None,
        raise_on_error: bool = False,
        base_url: Optional[str] = None,
        token: Optional[str] = None,
    ) -> None:
        if not _LITELLM_AVAILABLE:
            raise ImportError("litellm is required: pip install litellm")
        super().__init__()
        self._client = ProxyClient(base_url=base_url, token=token)
        self._model = model
        self._raise_on_error = raise_on_error

    async def async_pre_call_hook(
        self,
        user_api_key_dict: Any,
        cache: Any,
        data: Dict[str, Any],
        call_type: str,
    ) -> Dict[str, Any]:
        if call_type in _PRECALL_TYPES and isinstance(data, dict):
            loop = asyncio.get_running_loop()
            await loop.run_in_executor(
                None,
                lambda: compress_request_data(
                    data,
                    client=self._client,
                    model=self._model,
                    raise_on_error=self._raise_on_error,
                ),
            )
        return data
