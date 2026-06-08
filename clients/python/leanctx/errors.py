"""Error types for the lean-ctx Python client."""

from __future__ import annotations

from typing import Any, Optional


class LeanCtxError(Exception):
    """Base class for all lean-ctx client errors."""


class LeanCtxConfigError(LeanCtxError):
    """Raised for invalid client configuration or arguments (no I/O performed)."""


class LeanCtxTransportError(LeanCtxError):
    """Raised when the request never produced an HTTP response (network/DNS/TLS)."""


class LeanCtxHTTPError(LeanCtxError):
    """Raised for a non-2xx HTTP response from the server.

    Carries enough structured detail (status, method, url, server error code and
    body) for programmatic handling, mirroring the Rust/TS SDK error shape.
    """

    def __init__(
        self,
        *,
        status: int,
        method: str,
        url: str,
        message: str,
        error_code: Optional[str] = None,
        body: Any = None,
    ) -> None:
        super().__init__(message)
        self.status = status
        self.method = method
        self.url = url
        self.message = message
        self.error_code = error_code
        self.body = body

    def __str__(self) -> str:  # pragma: no cover - trivial
        code = f" [{self.error_code}]" if self.error_code else ""
        return f"HTTP {self.status} {self.method} {self.url}{code}: {self.message}"
