"""LangChain adapter (EPIC 12.6).

Exposes lean-ctx tools as LangChain `Tool`s. The framework is an optional
dependency, imported lazily so the SDK has no hard coupling.
"""

from __future__ import annotations

from typing import Any, List

from ..client import LeanCtxClient
from ._common import make_json_runner, normalized_tool_specs


def to_langchain_tools(client: LeanCtxClient) -> List[Any]:
    """Return lean-ctx tools as `langchain_core.tools.Tool` instances.

    Each tool accepts a JSON object string of arguments — the most portable
    shape across LangChain versions.
    """
    try:
        from langchain_core.tools import Tool
    except ImportError as exc:  # pragma: no cover - exercised only without dep
        raise ImportError(
            "LangChain is required for this adapter: pip install langchain-core"
        ) from exc

    tools: List[Any] = []
    for spec in normalized_tool_specs(client):
        runner = make_json_runner(client, spec.name)
        description = (
            f"{spec.description} "
            "Input: a JSON object string matching this tool's argument schema."
        ).strip()
        tools.append(Tool(name=spec.name, description=description, func=runner))
    return tools
