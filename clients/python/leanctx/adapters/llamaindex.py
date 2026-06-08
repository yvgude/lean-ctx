"""LlamaIndex adapter (EPIC 12.6).

Exposes lean-ctx tools as LlamaIndex `FunctionTool`s. The framework is an
optional dependency, imported lazily.
"""

from __future__ import annotations

from typing import Any, List

from ..client import LeanCtxClient
from ._common import make_json_runner, normalized_tool_specs


def to_llamaindex_tools(client: LeanCtxClient) -> List[Any]:
    """Return lean-ctx tools as `llama_index.core.tools.FunctionTool` instances."""
    try:
        from llama_index.core.tools import FunctionTool
    except ImportError as exc:  # pragma: no cover - exercised only without dep
        raise ImportError(
            "LlamaIndex is required for this adapter: pip install llama-index-core"
        ) from exc

    tools: List[Any] = []
    for spec in normalized_tool_specs(client):
        runner = make_json_runner(client, spec.name)
        description = (
            f"{spec.description} "
            "Input: a JSON object string matching this tool's argument schema."
        ).strip()
        tools.append(
            FunctionTool.from_defaults(
                fn=runner, name=spec.name, description=description
            )
        )
    return tools
