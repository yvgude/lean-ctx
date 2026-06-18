"""Deterministic long-context benchmark corpus with embedded *needle* facts.

This is benchmark **input data** (a reproducible synthetic stress transcript) —
not a stand-in for a real API or real application data. For a real-dataset run,
pass a transcript JSON via :func:`load_transcript` (e.g. a LOCOMO conversation
exported to ``{"messages": [...]}``).

Determinism: the corpus is a pure function of its parameters (SHA-256 derived
pseudo-text, no randomness), so benchmark numbers are reproducible.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any, Dict, List, Tuple

Message = Dict[str, Any]

_VOCAB = (
    "context engine token budget recall summary graph index session memory "
    "compaction tool agent window retrieval embedding latency prompt cache "
    "deterministic offload knowledge semantic ledger handoff"
).split()


def _det_text(seed: str, words: int) -> str:
    """Deterministic pseudo-text from a seed (reproducible, no randomness)."""
    out: List[str] = []
    h = hashlib.sha256(seed.encode()).hexdigest()
    for i in range(words):
        h = hashlib.sha256(f"{h}{i}".encode()).hexdigest()
        out.append(_VOCAB[int(h[:8], 16) % len(_VOCAB)])
    return " ".join(out)


def build_corpus(
    *,
    turns: int = 200,
    needles: int = 12,
    words_per_msg: int = 60,
) -> Tuple[List[Message], List[str]]:
    """Return ``(messages, needle_facts)``.

    ``needle_facts`` are unique fact strings placed at the *start* of selected
    early user turns (the region a compactor must summarize). A faithful engine
    keeps them recoverable; a lossy one drops them.
    """
    msgs: List[Message] = [
        {"role": "system", "content": "You are a senior engineer pairing on lean-ctx."}
    ]
    needle_facts: List[str] = []
    needle_every = max(1, turns // max(1, needles))
    for i in range(turns):
        if i % needle_every == 0 and len(needle_facts) < needles:
            token = hashlib.sha256(str(i).encode()).hexdigest()[:12]
            fact = (
                f"NEEDLE-{len(needle_facts):03d}: the deploy token for shard "
                f"{len(needle_facts)} is {token}"
            )
            needle_facts.append(fact)
            user = f"Remember this exactly — {fact}. Also: {_det_text(f'u{i}', words_per_msg)}"
        else:
            user = f"Question {i}: {_det_text(f'u{i}', words_per_msg)}"
        msgs.append({"role": "user", "content": user})
        msgs.append({"role": "assistant", "content": f"Answer {i}: {_det_text(f'a{i}', words_per_msg)}"})
    return msgs, needle_facts


def load_transcript(path: str) -> List[Message]:
    """Load a real transcript: a JSON list of messages or ``{"messages": [...]}``."""
    with open(path, "r", encoding="utf-8") as fh:
        data = json.load(fh)
    if isinstance(data, dict) and isinstance(data.get("messages"), list):
        return data["messages"]
    if isinstance(data, list):
        return data
    raise ValueError("transcript must be a list of messages or {messages: [...]}")
