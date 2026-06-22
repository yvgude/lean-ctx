"""Drop-in ``compress(messages, model)`` over the local lean-ctx proxy.

Posts a chat-style ``messages`` array to the daemon's deterministic
``POST /v1/compress`` endpoint and returns the rewritten messages. Only text
payloads are compressed; images, tool-call blocks and ids pass through
untouched, and the output is byte-stable for provider prompt caching.

Stdlib-only (``urllib``) so ``pip install lean-ctx-sdk`` pulls in no transitive
dependencies.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

from . import discovery
from .errors import LeanCtxAuthError, LeanCtxConnectionError, LeanCtxError

Message = Dict[str, Any]

_DEFAULT_TIMEOUT = 30.0


@dataclass
class CompressResult:
    """Result of a ``/v1/compress`` call: rewritten messages plus savings."""

    messages: List[Message]
    stats: Dict[str, Any] = field(default_factory=dict)

    @property
    def original_tokens(self) -> int:
        return int(self.stats.get("original_tokens", 0))

    @property
    def compressed_tokens(self) -> int:
        return int(self.stats.get("compressed_tokens", 0))

    @property
    def saved_tokens(self) -> int:
        return int(self.stats.get("saved_tokens", 0))

    @property
    def saved_pct(self) -> float:
        return float(self.stats.get("saved_pct", 0.0))


class ProxyClient:
    """Reusable client for the local lean-ctx proxy ``/v1/compress`` endpoint.

    Endpoint and token are auto-discovered (env → config → UID/data-dir) and may
    be overridden explicitly for CI or remote proxies.
    """

    def __init__(
        self,
        base_url: Optional[str] = None,
        token: Optional[str] = None,
        timeout: float = _DEFAULT_TIMEOUT,
    ) -> None:
        self.base_url = discovery.resolve_base_url(base_url)
        self.token = discovery.resolve_token(token)
        self.timeout = timeout

    def compress(
        self,
        messages: List[Message],
        model: Optional[str] = None,
    ) -> CompressResult:
        """Compress ``messages`` and return the rewritten list plus stats."""
        if not isinstance(messages, list):
            raise TypeError("messages must be a list of chat-message dicts")

        payload: Dict[str, Any] = {"messages": messages}
        if model:
            payload["model"] = model

        data = self._post("/v1/compress", payload)
        out = data.get("messages")
        if not isinstance(out, list):
            raise LeanCtxError("malformed /v1/compress response: 'messages' missing")
        stats = data.get("stats")
        return CompressResult(messages=out, stats=stats if isinstance(stats, dict) else {})

    def resolve_reference(self, reference_id: str) -> str:
        """Return the original content behind a lean-ctx reference id.

        lean-ctx replaces oversized omitted content with a durable reference; this
        fetches it back via ``GET /v1/references/{id}``. Raises :class:`LeanCtxError`
        when the reference is unknown or expired.
        """
        if not reference_id:
            raise ValueError("reference_id must be a non-empty string")
        quoted = urllib.parse.quote(reference_id, safe="")
        request = urllib.request.Request(f"{self.base_url}/v1/references/{quoted}", method="GET")
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        return self._send(request).decode("utf-8")

    def _post(self, path: str, payload: Dict[str, Any]) -> Dict[str, Any]:
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(f"{self.base_url}{path}", data=body, method="POST")
        request.add_header("Content-Type", "application/json")
        if self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        raw = self._send(request)
        try:
            return json.loads(raw.decode("utf-8"))
        except (ValueError, TypeError) as exc:
            raise LeanCtxError(f"invalid JSON response from {request.full_url}: {exc}") from exc

    def _send(self, request: urllib.request.Request) -> bytes:
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.read()
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", "replace").strip()
            if exc.code in (401, 403):
                raise LeanCtxAuthError(
                    f"proxy rejected the request (HTTP {exc.code}). "
                    "Set LEAN_CTX_PROXY_TOKEN or pass token=…"
                ) from exc
            if exc.code == 404:
                raise LeanCtxError(
                    f"{request.full_url} not found (HTTP 404): {detail}"
                ) from exc
            raise LeanCtxError(
                f"{request.get_method()} {request.full_url} failed (HTTP {exc.code}): {detail}"
            ) from exc
        except urllib.error.URLError as exc:
            raise LeanCtxConnectionError(
                f"could not reach the lean-ctx proxy at {self.base_url} ({exc.reason}). "
                "Is the daemon running? Try: lean-ctx proxy enable"
            ) from exc


def compress(
    messages: List[Message],
    model: Optional[str] = None,
    *,
    base_url: Optional[str] = None,
    token: Optional[str] = None,
    timeout: float = _DEFAULT_TIMEOUT,
) -> List[Message]:
    """Compress a chat ``messages`` array, returning the rewritten messages.

    Drop-in parity with library-style gateways::

        from lean_ctx import compress
        messages = compress(messages, model="claude-sonnet-4")

    For token-savings stats, use :class:`ProxyClient` directly::

        from lean_ctx import ProxyClient
        result = ProxyClient().compress(messages)
        print(result.saved_pct)
    """
    client = ProxyClient(base_url=base_url, token=token, timeout=timeout)
    return client.compress(messages, model=model).messages
