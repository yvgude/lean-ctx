"""Token counting utilities.

Uses ``tiktoken`` (``cl100k_base``) when available to stay aligned with the
host's accounting, and falls back to a deterministic char-based estimate
otherwise. Deterministic output keeps compaction prompt-cache friendly.
"""

from __future__ import annotations

import logging
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

_CHARS_PER_TOKEN = 4
_encoder = None
_encoder_checked = False


def _get_encoder():
    """Lazily load the tiktoken ``cl100k_base`` encoder (once)."""
    global _encoder, _encoder_checked
    if _encoder_checked:
        return _encoder
    _encoder_checked = True
    try:
        import tiktoken

        _encoder = tiktoken.get_encoding("cl100k_base")
    except Exception:  # pragma: no cover - depends on optional dep
        logger.debug("tiktoken unavailable; using char-based token estimate")
    return _encoder


def normalize_content_value(content: Any) -> str:
    """Flatten an OpenAI ``content`` value (str or content-part list) to text."""
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: List[str] = []
        for part in content:
            if isinstance(part, str):
                parts.append(part)
            elif isinstance(part, dict):
                text = part.get("text")
                if isinstance(text, str):
                    parts.append(text)
        return "\n".join(parts)
    if isinstance(content, dict):
        text = content.get("text")
        return text if isinstance(text, str) else ""
    return str(content)


def count_tokens(text: str) -> int:
    """Count tokens in a string."""
    if not text:
        return 0
    enc = _get_encoder()
    if enc is not None:
        try:
            return len(enc.encode(text))
        except Exception:  # pragma: no cover - encoder edge cases
            pass
    return len(text) // _CHARS_PER_TOKEN + 1


def count_message_tokens(msg: Dict[str, Any]) -> int:
    """Estimate tokens for one OpenAI-format message (content + tool calls)."""
    total = 4  # role + per-message overhead
    total += count_tokens(normalize_content_value(msg.get("content")))
    for tc in msg.get("tool_calls") or []:
        if isinstance(tc, dict):
            fn = tc.get("function", {}) or {}
            total += count_tokens(str(fn.get("name", "")))
            total += count_tokens(str(fn.get("arguments", "")))
            total += 3  # per-call overhead
    return total


def count_messages_tokens(messages: List[Dict[str, Any]]) -> int:
    """Estimate total tokens for a message list."""
    return sum(count_message_tokens(m) for m in messages)
