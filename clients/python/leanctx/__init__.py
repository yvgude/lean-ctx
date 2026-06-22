"""lean-ctx Python SDK.

A thin, dependency-free client for the lean-ctx HTTP ``/v1`` contract. Mirrors
the TypeScript (`lean-ctx-client`) and Rust (`lean-ctx-client`) SDKs.
"""

from __future__ import annotations

from .client import LeanCtxClient
from .conformance import (
    COVERED_ROUTES,
    SUPPORTED_HTTP_CONTRACT_VERSIONS,
    ConformanceCheck,
    ConformanceScorecard,
    run_conformance,
)
from .errors import (
    LeanCtxConfigError,
    LeanCtxError,
    LeanCtxHTTPError,
    LeanCtxTransportError,
)
from .tool_text import tool_result_to_text

__version__ = "0.1.0"

__all__ = [
    "LeanCtxClient",
    "LeanCtxError",
    "LeanCtxConfigError",
    "LeanCtxTransportError",
    "LeanCtxHTTPError",
    "tool_result_to_text",
    "run_conformance",
    "ConformanceCheck",
    "ConformanceScorecard",
    "COVERED_ROUTES",
    "SUPPORTED_HTTP_CONTRACT_VERSIONS",
    "__version__",
]
