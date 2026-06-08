"""Framework adapters for lean-ctx tools (EPIC 12.6).

Thin, optional integrations that expose the lean-ctx tool surface to popular
agent frameworks. Each framework is an *optional* dependency, imported lazily —
installing the SDK pulls in none of them. The OpenAI adapter is a pure
transformation and needs no extra package.

    from leanctx import LeanCtxClient
    from leanctx.adapters import to_openai_tools, to_langchain_tools

    client = LeanCtxClient("http://127.0.0.1:8080")
    tools = to_openai_tools(client)
"""

from __future__ import annotations

from ._common import ToolSpec, coerce_arguments, normalized_tool_specs
from .crewai import to_crewai_tools
from .langchain import to_langchain_tools
from .llamaindex import to_llamaindex_tools
from .openai import run_openai_tool_call, to_openai_tools

__all__ = [
    "ToolSpec",
    "normalized_tool_specs",
    "coerce_arguments",
    "to_openai_tools",
    "run_openai_tool_call",
    "to_langchain_tools",
    "to_llamaindex_tools",
    "to_crewai_tools",
]
