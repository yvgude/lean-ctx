"""CrewAI adapter (EPIC 12.6).

Exposes lean-ctx tools as CrewAI `BaseTool`s. The framework is an optional
dependency, imported lazily.
"""

from __future__ import annotations

from typing import Any, List

from ..client import LeanCtxClient
from ._common import coerce_arguments, normalized_tool_specs


def to_crewai_tools(client: LeanCtxClient) -> List[Any]:
    """Return lean-ctx tools as CrewAI `BaseTool` instances.

    Each tool takes a single ``arguments`` field: a JSON object string of the
    tool's arguments. This is portable across CrewAI versions and avoids
    synthesizing a distinct pydantic schema per tool.
    """
    try:
        from crewai.tools import BaseTool
        from pydantic import BaseModel, Field
    except ImportError as exc:  # pragma: no cover - exercised only without dep
        raise ImportError(
            "CrewAI is required for this adapter: pip install crewai"
        ) from exc

    class _ArgsSchema(BaseModel):
        arguments: str = Field(
            default="",
            description="JSON object string of the tool's arguments.",
        )

    class _LeanCtxTool(BaseTool):
        args_schema: type[BaseModel] = _ArgsSchema

        def __init__(self, tool_name: str, tool_description: str) -> None:
            super().__init__(name=tool_name, description=tool_description)
            self._tool_name = tool_name

        def _run(self, arguments: str = "") -> str:
            return client.call_tool_text(self._tool_name, coerce_arguments(arguments))

    tools: List[Any] = []
    for spec in normalized_tool_specs(client):
        description = (
            f"{spec.description} "
            "Input: a JSON object string matching this tool's argument schema."
        ).strip()
        tools.append(_LeanCtxTool(spec.name, description))
    return tools
