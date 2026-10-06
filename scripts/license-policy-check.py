#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Compatibility CLI for the canonical license/provenance checker."""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
import sys


def validate(root: Path, matrix_path: Path, export: bool = False) -> list[str]:
    root = root.resolve()
    if matrix_path.resolve() != (root / "LICENSE_MATRIX.toml").resolve():
        return ["the repository LICENSE_MATRIX.toml is the licensing authority; external overrides are not accepted"]
    spec = importlib.util.spec_from_file_location(
        "leanctx_canonical_provenance", Path(__file__).with_name("ip-provenance-audit.py")
    )
    if spec is None or spec.loader is None:
        return ["canonical provenance checker is unavailable"]
    checker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checker)
    # Development policy checks preserve pending-approval warnings. Release
    # workflows separately invoke the same checker with --release-strict.
    return checker.run_audit(root, None, False, export)["errors"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--matrix", type=Path, help="must resolve to the selected repository's LICENSE_MATRIX.toml")
    parser.add_argument(
        "--export",
        action="store_true",
        help="history-free export: verify historical provenance against legal/provenance-manifest.json",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        problems = validate(root, args.matrix or root / "LICENSE_MATRIX.toml", args.export)
    except (OSError, ValueError, ImportError) as error:
        problems = [f"policy inspection failed: {error}"]
    if problems:
        for problem in problems:
            print(f"license-policy: {problem}", file=sys.stderr)
        return 1
    print("license-policy: PASS (canonical provenance audit)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
