"""Compute endpoints (PROTOCOL.md §5) and emit the verifiable result artifact.

Inputs: per-run meta.json files + the official SWE-bench evaluation reports.
Output: runs/<id>/result-v1.json (canonical, self-hashing) + markdown summary.

Usage:
    python -m swebench_harness.report --run-id v1 \
        --eval-report native=claude-code-native.v1-native.json \
        --eval-report leanctx=claude-code-leanctx.v1-leanctx.json
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

from . import ARMS, BENCH_ROOT, canonical_dumps, load_config, load_tasks_lock, sha256_file, sha256_text


def arm_metrics(run_root: Path, arm: str, instances: list, resolved_ids: set) -> dict:
    metas = []
    for inst in instances:
        meta_file = run_root / inst["instance_id"] / arm / "meta.json"
        if meta_file.exists():
            metas.append(json.loads(meta_file.read_text()))
    n = len(metas)
    resolved = sum(1 for m in metas if m["instance_id"] in resolved_ids)
    usable = [m for m in metas if not m.get("usage_missing")]
    total_cost = round(sum(m.get("total_cost_usd") or 0.0 for m in usable), 4)
    input_tokens = [m["input_tokens"] for m in usable if m.get("input_tokens") is not None]
    output_tokens = [m["output_tokens"] for m in usable if m.get("output_tokens") is not None]
    return {
        "n_run": n,
        "resolved": resolved,
        "resolved_rate": round(resolved / n, 4) if n else None,
        "usage_missing_runs": n - len(usable),
        "total_cost_usd": total_cost,
        "cost_per_resolved_usd": round(total_cost / resolved, 4) if resolved else None,
        "median_input_tokens": statistics.median(input_tokens) if input_tokens else None,
        "median_output_tokens": statistics.median(output_tokens) if output_tokens else None,
        "median_wall_time_seconds": statistics.median(m["wall_time_seconds"] for m in metas) if metas else None,
        "timed_out_runs": sum(1 for m in metas if m.get("timed_out")),
        "resolved_instance_ids": sorted(m["instance_id"] for m in metas if m["instance_id"] in resolved_ids),
    }


def load_resolved_ids(report_path: Path) -> set:
    report = json.loads(report_path.read_text())
    ids = report.get("resolved_ids")
    if ids is None:
        sys.exit(f"{report_path}: no resolved_ids field — pass the official run_evaluation report")
    return set(ids)


def render_markdown(result: dict) -> str:
    lines = [
        "# Agent-Task-Benchmark v1 — result",
        "",
        f"run_id `{result['run_id']}` · N={result['n_tasks']} (SWE-bench Verified subset) · protocol sha256 `{result['protocol_sha256'][:16]}…`",
        "",
        "| endpoint | native | leanctx |",
        "|---|---|---|",
    ]
    rows = [
        ("resolved", "resolved"),
        ("resolved rate", "resolved_rate"),
        ("total cost (USD)", "total_cost_usd"),
        ("cost / resolved task (USD)", "cost_per_resolved_usd"),
        ("median billed input tokens", "median_input_tokens"),
        ("median output tokens", "median_output_tokens"),
        ("median wall time (s)", "median_wall_time_seconds"),
        ("timed-out runs", "timed_out_runs"),
    ]
    for label, key in rows:
        lines.append(f"| {label} | {result['arms']['native'][key]} | {result['arms']['leanctx'][key]} |")
    lines += ["", f"artifact sha256: `{result['artifact_sha256']}`", ""]
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--eval-report", action="append", required=True,
                    metavar="ARM=PATH", help="official evaluation report per arm")
    args = ap.parse_args()

    reports = {}
    for spec in args.eval_report:
        arm, _, path = spec.partition("=")
        if arm not in ARMS or not path:
            sys.exit(f"bad --eval-report '{spec}' (expected ARM=PATH)")
        reports[arm] = Path(path)
    if set(reports) != set(ARMS):
        sys.exit(f"need eval reports for both arms: {ARMS}")

    cfg = load_config()
    instances = load_tasks_lock()
    run_root = BENCH_ROOT / cfg["runs_dir"] / args.run_id

    result = {
        "benchmark": "lean-ctx agent-task v1 (GL #493)",
        "run_id": args.run_id,
        "n_tasks": len(instances),
        "protocol_sha256": sha256_file(BENCH_ROOT / "PROTOCOL.md"),
        "tasks_lock_sha256": sha256_file(BENCH_ROOT / "tasks.lock.json"),
        "arms": {
            arm: arm_metrics(run_root, arm, instances, load_resolved_ids(reports[arm]))
            for arm in ARMS
        },
    }
    result["artifact_sha256"] = sha256_text(canonical_dumps(result))

    out_json = run_root / "result-v1.json"
    out_json.write_text(canonical_dumps(result) + "\n")
    out_md = run_root / "result-v1.md"
    out_md.write_text(render_markdown(result))
    print(render_markdown(result))
    print(f"wrote {out_json} + {out_md}")
    print("sign it:  ssh-keygen -Y sign -f ~/.ssh/id_ed25519 -n lean-ctx-bench " + str(out_json))


if __name__ == "__main__":
    main()
