"""Extract plain text from an MCP tool result.

Mirrors `toolText.ts` / `tool_text.rs` so all SDKs flatten tool results the same
way: concatenate the ``text`` of every ``{"type": "text", "text": ...}`` content
block. Non-text blocks are ignored; a bare string result is returned as-is.
"""

from __future__ import annotations

from typing import Any


def tool_result_to_text(result: Any) -> str:
    """Flatten an MCP tool result into a single text string."""
    if result is None:
        return ""
    if isinstance(result, str):
        return result
    if isinstance(result, dict):
        content = result.get("content")
        if isinstance(content, list):
            parts = []
            for block in content:
                if (
                    isinstance(block, dict)
                    and block.get("type") == "text"
                    and isinstance(block.get("text"), str)
                ):
                    parts.append(block["text"])
            return "".join(parts)
    return ""
