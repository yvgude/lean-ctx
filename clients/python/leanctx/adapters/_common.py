"""Shared helpers for framework adapters (EPIC 12.6).

All adapters normalize lean-ctx tools into a common [`ToolSpec`] and reuse the
same SDK call path (`call_tool_text`), so every framework integration behaves
identically and stays correct as the tool surface evolves.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Callable, Dict, List

from ..client import LeanCtxClient


@dataclass
class ToolSpec:
    """A framework-neutral description of one lean-ctx tool."""

    name: str
    description: str
    parameters: Dict[str, Any]  # JSON Schema for the arguments object


def _default_schema() -> Dict[str, Any]:
    return {"type": "object", "properties": {}}


def normalized_tool_specs(client: LeanCtxClient, *, limit: int = 500) -> List[ToolSpec]:
    """Fetch and normalize the server's tool surface into [`ToolSpec`]s."""
    listing = client.list_tools(limit=limit)
    tools = listing.get("tools", []) if isinstance(listing, dict) else []
    specs: List[ToolSpec] = []
    for entry in tools:
        if not isinstance(entry, dict):
            continue
        name = entry.get("name")
        if not isinstance(name, str) or not name:
            continue
        description = entry.get("description")
        if not isinstance(description, str):
            description = ""
        schema = (
            entry.get("input_schema")
            or entry.get("inputSchema")
            or entry.get("parameters")
            or _default_schema()
        )
        if not isinstance(schema, dict):
            schema = _default_schema()
        specs.append(ToolSpec(name=name, description=description, parameters=schema))
    return specs


def coerce_arguments(raw: Any) -> Dict[str, Any]:
    """Coerce tool-call arguments (JSON string or dict) into a dict."""
    if raw is None:
        return {}
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, str):
        text = raw.strip()
        if not text:
            return {}
        parsed = json.loads(text)
        return parsed if isinstance(parsed, dict) else {}
    return {}


def make_json_runner(client: LeanCtxClient, name: str) -> Callable[[str], str]:
    """A `(arguments: str) -> str` runner used by string-input frameworks.

    The single JSON-string argument is the most portable shape across framework
    versions and avoids brittle per-tool signature synthesis.
    """

    def run(arguments: str = "") -> str:
        return client.call_tool_text(name, coerce_arguments(arguments))

    run.__name__ = name
    run.__doc__ = f"Invoke the lean-ctx tool '{name}' with a JSON object string."
    return run
