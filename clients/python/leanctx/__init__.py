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
from .ocla import (
    AGENT_ENVELOPE_SCHEMA_VERSION,
    CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
    MAX_OCLA_WIRE_BYTES,
    OCLA_API_VERSION,
    AgentEnvelopeV1,
    CanonicalTokenEnvelopeV1,
    OclaGatewayAdmissibilityError,
    OclaRequestContext,
    OclaWireError,
    TokenBalanceV1,
    decode_agent_envelope,
    decode_canonical_token_envelope,
    verify_agent_gateway_admissibility,
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
    "OCLA_API_VERSION",
    "CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION",
    "AGENT_ENVELOPE_SCHEMA_VERSION",
    "MAX_OCLA_WIRE_BYTES",
    "OclaWireError",
    "OclaGatewayAdmissibilityError",
    "OclaRequestContext",
    "TokenBalanceV1",
    "CanonicalTokenEnvelopeV1",
    "AgentEnvelopeV1",
    "decode_canonical_token_envelope",
    "decode_agent_envelope",
    "verify_agent_gateway_admissibility",
    "__version__",
]
