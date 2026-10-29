#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Fail a release build whose binary exceeds its recorded size ceiling.

    check-binary-budget.py ARTIFACT BINARY [--budget product/binary-budget.json]

Artifacts without a ceiling are reported only, so a new target never blocks a
release before it has been measured.
"""

import argparse
import json
import sys
from pathlib import Path

DEFAULT_BUDGET = Path(__file__).resolve().parent.parent / "product" / "binary-budget.json"


def check(artifact: str, binary: Path, budget_path: Path) -> int:
    size = binary.stat().st_size
    entry = json.loads(budget_path.read_text(encoding="utf-8"))["artifacts"].get(artifact)
    if entry is None:
        print(f"binary-budget: {artifact}: {size} bytes (no ceiling recorded)")
        return 0
    limit = int(entry["max_bytes"])
    print(f"binary-budget: {artifact}: {size} of {limit} bytes ({100 * size / limit:.1f} %)")
    if size > limit:
        print(
            f"binary-budget: {artifact} exceeds its ceiling by {size - limit} bytes; "
            f"shrink the build or raise max_bytes in {budget_path.name} with a reason",
            file=sys.stderr,
        )
        return 1
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("artifact")
    parser.add_argument("binary", type=Path)
    parser.add_argument("--budget", type=Path, default=DEFAULT_BUDGET)
    args = parser.parse_args(argv)
    return check(args.artifact, args.binary, args.budget)


if __name__ == "__main__":
    sys.exit(main())
