"""Run the context-engine benchmark and print a comparison table.

Usage (from the plugin directory)::

    python benchmarks/run.py --turns 400 --needles 16 --context-length 8000

Only lean-ctx runs out of the box; install Hermes / hermes-lcm to include the
competitors (they are auto-detected). Results are written to
``benchmarks/results/`` (gitignored).
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional

# Allow direct execution (`python benchmarks/run.py`) without installation.
_PLUGIN_DIR = Path(__file__).resolve().parent.parent
_REPO_ROOT = _PLUGIN_DIR.parent.parent
_PKG = "hermes_lean_ctx"


def _bootstrap_package() -> None:
    """Make the hyphenated plugin dir importable as ``hermes_lean_ctx``.

    Mirrors the test conftest so the runner works standalone, and wires the
    monorepo ``leanctx`` SDK onto the path when it is checked out alongside.
    """
    clients_python = _REPO_ROOT / "clients" / "python"
    if clients_python.is_dir() and str(clients_python) not in sys.path:
        sys.path.insert(0, str(clients_python))
    if _PKG in sys.modules:
        return
    spec = importlib.util.spec_from_file_location(
        _PKG,
        str(_PLUGIN_DIR / "__init__.py"),
        submodule_search_locations=[str(_PLUGIN_DIR)],
    )
    if not spec or not spec.loader:  # pragma: no cover - packaging accident
        raise ImportError("cannot locate hermes_lean_ctx package")
    module = importlib.util.module_from_spec(spec)
    sys.modules[_PKG] = module
    spec.loader.exec_module(module)


_bootstrap_package()

from hermes_lean_ctx.benchmarks import corpus as _corpus  # noqa: E402
from hermes_lean_ctx.benchmarks import engines as _engines  # noqa: E402
from hermes_lean_ctx.benchmarks import metrics as _metrics  # noqa: E402

_COLUMNS = [
    ("engine", "engine", 18),
    ("note", "mode", 14),
    ("savings_pct", "savings%", 9),
    ("saved_tokens", "saved", 10),
    ("verbatim_recall", "recall", 7),
    ("compress_latency_ms", "ms", 9),
    ("output_messages", "out_msgs", 9),
]


def run_benchmark(
    *,
    turns: int = 200,
    needles: int = 12,
    words_per_msg: int = 60,
    context_length: int = 8_000,
    repeats: int = 1,
    base_url: Optional[str] = None,
    token: Optional[str] = None,
    transcript_path: Optional[str] = None,
) -> Dict[str, Any]:
    """Run all available engines over the corpus; return a results dict."""
    if transcript_path:
        messages = _corpus.load_transcript(transcript_path)
        needle_facts: List[str] = []
    else:
        messages, needle_facts = _corpus.build_corpus(
            turns=turns, needles=needles, words_per_msg=words_per_msg
        )

    rows: List[Dict[str, Any]] = []
    skipped: List[Dict[str, str]] = []
    for adapter in _engines.discover_adapters(
        context_length=context_length, base_url=base_url, token=token
    ):
        if not adapter.available:
            skipped.append({"engine": adapter.name, "reason": adapter.note})
            continue
        rows.append(_metrics.measure(adapter, messages, needle_facts, repeats=repeats))

    return {
        "params": {
            "turns": turns,
            "needles": len(needle_facts),
            "words_per_msg": words_per_msg,
            "context_length": context_length,
            "repeats": repeats,
            "transcript_path": transcript_path,
        },
        "results": rows,
        "skipped": skipped,
    }


def format_table(report: Dict[str, Any]) -> str:
    header = "  ".join(label.ljust(width) for _, label, width in _COLUMNS)
    lines = [header, "-" * len(header)]
    for row in report["results"]:
        lines.append(
            "  ".join(str(row.get(key, "")).ljust(width) for key, _, width in _COLUMNS)
        )
    for skip in report["skipped"]:
        lines.append(f"(skipped {skip['engine']}: {skip['reason']})")
    return "\n".join(lines)


def _write_results(report: Dict[str, Any]) -> Path:
    out_dir = _PLUGIN_DIR / "benchmarks" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "latest.json"
    out_path.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
    return out_path


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="hermes-lean-ctx context-engine benchmark")
    parser.add_argument("--turns", type=int, default=200)
    parser.add_argument("--needles", type=int, default=12)
    parser.add_argument("--words-per-msg", type=int, default=60)
    parser.add_argument("--context-length", type=int, default=8_000)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--base-url", default=os.environ.get("LEANCTX_BASE_URL"))
    parser.add_argument("--token", default=os.environ.get("LEANCTX_TOKEN"))
    parser.add_argument("--transcript", default=None, help="Path to a real transcript JSON")
    parser.add_argument("--no-write", action="store_true", help="Do not write results JSON")
    args = parser.parse_args(argv)

    report = run_benchmark(
        turns=args.turns,
        needles=args.needles,
        words_per_msg=args.words_per_msg,
        context_length=args.context_length,
        repeats=args.repeats,
        base_url=args.base_url,
        token=args.token,
        transcript_path=args.transcript,
    )
    print(format_table(report))
    if not args.no_write:
        path = _write_results(report)
        print(f"\nwrote {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
