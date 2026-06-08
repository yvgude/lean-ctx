"""OpenAI function-calling adapter (EPIC 12.6).

Pure transformation — no `openai` package required. Turns the lean-ctx tool
surface into OpenAI ``tools=[...]`` specs and executes the tool calls the model
returns. Works with both the Chat Completions and Responses tool-call shapes.
"""

from __future__ import annotations

from typing import Any, Dict, List

from ..client import LeanCtxClient
from ._common import coerce_arguments, normalized_tool_specs


def to_openai_tools(client: LeanCtxClient) -> List[Dict[str, Any]]:
    """Return lean-ctx tools as OpenAI function-tool specs."""
    return [
        {
            "type": "function",
            "function": {
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            },
        }
        for spec in normalized_tool_specs(client)
    ]


def run_openai_tool_call(client: LeanCtxClient, tool_call: Any) -> str:
    """Execute one OpenAI tool call and return the tool's text result.

    Accepts either the dict shape (``{"function": {"name", "arguments"}}``) or
    an SDK object exposing ``.function.name`` / ``.function.arguments``.
    """
    function = tool_call.get("function") if isinstance(tool_call, dict) else getattr(tool_call, "function", None)
    if function is None:
        raise ValueError("tool_call has no 'function'")
    name = function.get("name") if isinstance(function, dict) else getattr(function, "name", None)
    if not name:
        raise ValueError("tool_call.function has no 'name'")
    raw_args = (
        function.get("arguments") if isinstance(function, dict) else getattr(function, "arguments", None)
    )
    return client.call_tool_text(name, coerce_arguments(raw_args))
