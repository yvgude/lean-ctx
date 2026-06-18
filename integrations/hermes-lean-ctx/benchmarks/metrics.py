"""Benchmark metrics: token savings, compaction latency, needle recall.

All metrics are computed from the real engine output (no estimation of the
result itself). ``verbatim_recall`` measures how many needle facts survive *in
the compacted window* — the figure that decides whether the model can answer
without a tool call. lean-ctx additionally exposes the dropped detail through
its recall tools (see the live tests); that recoverability is a property the
verbatim metric deliberately does not credit, so the comparison stays fair.
"""

from __future__ import annotations

import time
from typing import Any, Dict, List

from hermes_lean_ctx.tokens import count_messages_tokens, normalize_content_value

Message = Dict[str, Any]


def _blob(messages: List[Message]) -> str:
    return "\n".join(normalize_content_value(m.get("content")) for m in messages)


def verbatim_recall(compacted: List[Message], needles: List[str]) -> float:
    """Fraction of needle facts whose exact text remains in the window."""
    if not needles:
        return 1.0
    blob = _blob(compacted)
    hits = sum(1 for needle in needles if needle in blob)
    return hits / len(needles)


def measure(
    adapter: Any,
    messages: List[Message],
    needles: List[str],
    *,
    repeats: int = 1,
) -> Dict[str, Any]:
    """Run ``adapter.compress`` and return a metrics row (averaged latency)."""
    before = count_messages_tokens(messages)
    reps = max(1, repeats)
    t0 = time.perf_counter()
    out = adapter.compress(messages, None)
    for _ in range(reps - 1):
        out = adapter.compress(messages, None)
    latency_ms = (time.perf_counter() - t0) / reps * 1000.0
    after = count_messages_tokens(out)
    saved = max(0, before - after)
    return {
        "engine": adapter.name,
        "note": adapter.note,
        "input_messages": len(messages),
        "output_messages": len(out),
        "input_tokens": before,
        "output_tokens": after,
        "saved_tokens": saved,
        "savings_pct": round(100.0 * saved / before, 2) if before else 0.0,
        "verbatim_recall": round(verbatim_recall(out, needles), 4),
        "compress_latency_ms": round(latency_ms, 2),
    }
