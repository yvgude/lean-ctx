"""Agent-Task-Benchmark v1 harness (GL #493) — shared helpers.

Measures task success rate and cost per solved task for an agentic coding
workload (SWE-bench Verified subset), with and without lean-ctx, under the
pre-registered protocol in ../PROTOCOL.md.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

BENCH_ROOT = Path(__file__).resolve().parent.parent

ARMS = ("native", "leanctx")


def load_config() -> dict:
    return json.loads((BENCH_ROOT / "config.json").read_text())


def load_tasks_lock() -> list:
    lock = BENCH_ROOT / "tasks.lock.json"
    if not lock.exists():
        raise SystemExit(
            "tasks.lock.json missing — generate it once with: python -m swebench_harness.select_tasks"
        )
    return json.loads(lock.read_text())["instances"]


def canonical_dumps(obj) -> str:
    """Stable JSON for hashing: sorted keys, no float surprises, no whitespace drift."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_jsonl(path: Path) -> list:
    rows = []
    with path.open() as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows
