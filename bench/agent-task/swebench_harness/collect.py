"""Assemble per-arm predictions.jsonl for the official SWE-bench evaluation.

Usage:
    python -m swebench_harness.collect --run-id v1
Then evaluate each arm (docker required):
    python -m swebench.harness.run_evaluation \
        --dataset_name princeton-nlp/SWE-bench_Verified \
        --predictions_path runs/v1/predictions-native.jsonl \
        --run_id v1-native --max_workers 4
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from . import ARMS, BENCH_ROOT, load_config, load_tasks_lock


def collect_arm(run_root: Path, arm: str, instances: list) -> Path:
    out_path = run_root / f"predictions-{arm}.jsonl"
    rows, missing = [], []
    for inst in instances:
        iid = inst["instance_id"]
        patch_file = run_root / iid / arm / "model_patch.diff"
        if not patch_file.exists():
            missing.append(iid)
            continue
        rows.append({
            "instance_id": iid,
            "model_name_or_path": f"claude-code-{arm}",
            "model_patch": patch_file.read_text(),
        })
    with out_path.open("w") as fh:
        for row in rows:
            fh.write(json.dumps(row) + "\n")
    print(f"{arm}: {len(rows)} predictions -> {out_path}" + (f" (missing: {', '.join(missing)})" if missing else ""))
    return out_path


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--run-id", required=True)
    args = ap.parse_args()

    cfg = load_config()
    run_root = BENCH_ROOT / cfg["runs_dir"] / args.run_id
    instances = load_tasks_lock()
    for arm in ARMS:
        collect_arm(run_root, arm, instances)


if __name__ == "__main__":
    main()
