"""Best-effort model -> context-window presets.

Only a fallback: Hermes passes the real ``context_length`` to
``update_model``; these published windows are used when it does not. Matching
is by case-insensitive substring, longest key first (so ``gpt-4o-mini`` is not
shadowed by ``gpt-4``).
"""

from __future__ import annotations

from typing import Optional

# Conservative, widely-documented context windows.
_PRESETS = {
    "claude": 200_000,
    "claude-3": 200_000,
    "claude-4": 200_000,
    "gpt-4o": 128_000,
    "gpt-4.1": 1_000_000,
    "gpt-4-turbo": 128_000,
    "gpt-4": 8_192,
    "gpt-3.5": 16_385,
    "o1": 200_000,
    "o3": 200_000,
    "hermes": 128_000,
    "llama-3.1": 128_000,
    "llama-3": 8_192,
    "qwen2.5": 128_000,
    "deepseek": 128_000,
    "mistral": 32_768,
    "gemini-1.5": 1_000_000,
    "gemini": 1_000_000,
}


def context_length_for(model: Optional[str]) -> Optional[int]:
    """Return a known context window for ``model``, or ``None`` if unknown."""
    if not model:
        return None
    needle = model.lower()
    for key in sorted(_PRESETS, key=len, reverse=True):
        if key in needle:
            return _PRESETS[key]
    return None
