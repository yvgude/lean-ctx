"""Native engine tool dispatch.

``get_tool_schemas()`` advertises lean-ctx's recall surface; ``handle_tool_call``
proxies the call to the daemon over ``/v1`` and returns the tool's text result.
"""

from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

from .config import LeanCtxConfig
from .schemas import ALL_SCHEMAS, TOOL_NAMES
from .transport import ToolGateway


def get_tool_schemas(config: LeanCtxConfig) -> List[Dict[str, Any]]:
    """Return engine tool schemas, or ``[]`` when tools are disabled."""
    if not config.enable_tools:
        return []
    return [dict(schema) for schema in ALL_SCHEMAS]


def _coerce_args(raw: Any) -> Dict[str, Any]:
    if raw is None:
        return {}
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, str):
        text = raw.strip()
        if not text:
            return {}
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}
    return {}


def handle_tool_call(
    gateway: ToolGateway,
    name: str,
    args: Any,
    **_: Any,
) -> str:
    """Dispatch one engine tool call and return a string result.

    Unknown tools and daemon failures return a clear error string rather than
    raising, so the agent loop is never broken by the engine.
    """
    if name not in TOOL_NAMES:
        return json.dumps({"error": f"Unknown tool: {name}"})
    arguments: Optional[Dict[str, Any]] = _coerce_args(args)
    text = gateway.call_text(name, arguments)
    if text is None:
        return json.dumps(
            {"error": f"lean-ctx daemon unavailable; '{name}' could not be executed."}
        )
    return text
