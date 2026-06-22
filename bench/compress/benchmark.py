#!/usr/bin/env python3
"""Head-to-head compression benchmark: lean-ctx `/v1/compress` vs Headroom.

Runs both libraries over the *same* real corpus with the *same* tokenizer and
reports compression ratio + latency as JSON. Numbers are always measured, never
fabricated: a tool that is not installed/reachable is reported as
``available: false`` rather than guessed.

Prerequisites
-------------
* lean-ctx daemon with ``POST /v1/compress`` running (``lean-ctx dev-install``).
* Optional head-to-head: ``pip install headroom-ai``.
* Optional accurate token counts: ``pip install tiktoken`` (else char counts).

Usage
-----
    python bench/compress/benchmark.py                  # JSON to stdout
    python bench/compress/benchmark.py --corpus docs/   # custom corpus
    python bench/compress/benchmark.py --out report.json --model gpt-4o
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
PY_SDK = REPO_ROOT / "packages" / "python-lean-ctx"
if PY_SDK.is_dir():
    sys.path.insert(0, str(PY_SDK))

Message = Dict[str, Any]


def build_tokenizer(model: str) -> tuple[Callable[[str], int], str]:
    """Return ``(count_fn, name)``. Prefers tiktoken; falls back to chars."""
    try:
        import tiktoken

        try:
            enc = tiktoken.encoding_for_model(model)
        except KeyError:
            enc = tiktoken.get_encoding("o200k_base")
        return (lambda text: len(enc.encode(text)), enc.name)
    except Exception:
        return (len, "chars")


def iter_text(content: Any):
    """Yield every text payload inside an OpenAI/Anthropic message content."""
    if isinstance(content, str):
        yield content
    elif isinstance(content, list):
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "text" and isinstance(block.get("text"), str):
                yield block["text"]
            elif block.get("type") == "tool_result":
                yield from iter_text(block.get("content"))


def total_tokens(messages: List[Message], count: Callable[[str], int]) -> int:
    return sum(count(text) for msg in messages for text in iter_text(msg.get("content")))


def load_corpus(path: Path, max_files: int, max_bytes: int) -> List[Message]:
    """Build one user message per real text file under ``path`` (no fixtures)."""
    if not path.exists():
        raise SystemExit(f"corpus path does not exist: {path}")
    suffixes = {".md", ".rs", ".py", ".ts", ".txt", ".json", ".log", ".yaml", ".yml"}
    files = sorted(p for p in path.rglob("*") if p.is_file() and p.suffix in suffixes)
    messages: List[Message] = []
    for file in files:
        if len(messages) >= max_files:
            break
        try:
            text = file.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if len(text) > max_bytes:
            text = text[:max_bytes]
        if text.strip():
            messages.append({"role": "user", "content": text})
    if not messages:
        raise SystemExit(f"no readable text files found under {path}")
    return messages


def measure(
    label: str,
    compress: Callable[[List[Message]], List[Message]],
    messages: List[Message],
    count: Callable[[str], int],
) -> Dict[str, Any]:
    """Run one compressor once, returning measured tokens + latency."""
    original = total_tokens(messages, count)
    started = time.perf_counter()
    try:
        out = compress(messages)
    except Exception as exc:  # noqa: BLE001 - any failure is reported, not raised
        return {"available": False, "error": f"{type(exc).__name__}: {exc}"}
    latency_ms = round((time.perf_counter() - started) * 1000, 2)
    compressed = total_tokens(out, count)
    ratio = round(1 - compressed / original, 4) if original else 0.0
    return {
        "available": True,
        "original_tokens": original,
        "compressed_tokens": compressed,
        "tokens_saved": original - compressed,
        "ratio": ratio,
        "latency_ms": latency_ms,
    }


def lean_ctx_compressor(model: str) -> Optional[Callable[[List[Message]], List[Message]]]:
    try:
        from lean_ctx import compress as lc_compress
    except ImportError:
        return None
    return lambda messages: lc_compress(messages, model=model)


def headroom_compressor(model: str) -> Optional[Callable[[List[Message]], List[Message]]]:
    try:
        from headroom import compress as hr_compress
    except ImportError:
        return None
    return lambda messages: hr_compress(messages, model=model).messages


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", default=str(REPO_ROOT / "docs" / "reference"))
    parser.add_argument("--model", default="gpt-4o")
    parser.add_argument("--max-files", type=int, default=50)
    parser.add_argument("--max-bytes", type=int, default=200_000)
    parser.add_argument("--out", help="write the JSON report to this file")
    args = parser.parse_args()

    count, tokenizer = build_tokenizer(args.model)
    messages = load_corpus(Path(args.corpus), args.max_files, args.max_bytes)

    report: Dict[str, Any] = {
        "corpus": {
            "path": str(Path(args.corpus)),
            "messages": len(messages),
            "model": args.model,
            "tokenizer": tokenizer,
        },
    }

    lc = lean_ctx_compressor(args.model)
    report["lean_ctx"] = (
        measure("lean-ctx", lc, messages, count)
        if lc
        else {"available": False, "install": "pip install lean-ctx-sdk (and run the daemon)"}
    )

    hr = headroom_compressor(args.model)
    report["headroom"] = (
        measure("headroom", hr, messages, count)
        if hr
        else {"available": False, "install": "pip install headroom-ai"}
    )

    payload = json.dumps(report, indent=2)
    print(payload)
    if args.out:
        Path(args.out).write_text(payload + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
