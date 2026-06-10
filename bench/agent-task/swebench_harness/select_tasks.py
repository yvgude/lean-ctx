"""Deterministic task selection → tasks.lock.json (run exactly once; see PROTOCOL.md §2).

Selection rule (pre-registered): sort all SWE-bench-Verified instances by
instance_id; group by repo; visit repos in ascending name order round-robin,
taking the lexicographically first untaken instance per repo each cycle,
until N are selected. No seed, no randomness — fully reproducible from the
public dataset.

The lock embeds everything the runner needs (problem statement, base commit),
so benchmark runs do not depend on Hugging Face availability.
"""

from __future__ import annotations

import json
import sys
from collections import OrderedDict

from . import BENCH_ROOT, canonical_dumps, load_config, sha256_text


def select(instances: list, n: int) -> list:
    by_repo: "OrderedDict[str, list]" = OrderedDict()
    for inst in sorted(instances, key=lambda r: r["instance_id"]):
        by_repo.setdefault(inst["repo"], []).append(inst)

    picked = []
    while len(picked) < n:
        progressed = False
        for repo in sorted(by_repo):
            if by_repo[repo]:
                picked.append(by_repo[repo].pop(0))
                progressed = True
                if len(picked) == n:
                    break
        if not progressed:
            break
    return picked


def main() -> None:
    lock_path = BENCH_ROOT / "tasks.lock.json"
    if lock_path.exists():
        sys.exit(f"{lock_path} already exists — the v1 lock is frozen (PROTOCOL.md §2).")

    cfg = load_config()
    from datasets import load_dataset  # heavy import, only needed here

    ds = load_dataset(cfg["dataset"], split=cfg["split"])
    picked = select(list(ds), cfg["n_tasks"])

    keep = [
        "instance_id",
        "repo",
        "base_commit",
        "environment_setup_commit",
        "version",
        "problem_statement",
    ]
    instances = [{k: inst[k] for k in keep} for inst in picked]
    payload = {
        "dataset": cfg["dataset"],
        "split": cfg["split"],
        "selection_rule": "sorted-round-robin-by-repo (PROTOCOL.md §2)",
        "n": len(instances),
        "instances": instances,
    }
    text = canonical_dumps(payload)
    lock_path.write_text(text + "\n")
    print(f"wrote {lock_path} ({len(instances)} instances, sha256 {sha256_text(text)[:16]}…)")
    for inst in instances:
        print(f"  {inst['instance_id']}")


if __name__ == "__main__":
    main()
