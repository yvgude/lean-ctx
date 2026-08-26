#!/usr/bin/env python3
"""Fail closed when Git tracks internal documentation artifacts."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


INTERNAL_ARCHIVE = re.compile(r"^docs/internal[^/]*\.zip$", re.IGNORECASE)


def is_forbidden(path: str) -> bool:
    normalized = path.replace("\\", "/").lstrip("./")
    lowered = normalized.casefold()
    return (
        lowered == "docs/internal"
        or lowered.startswith("docs/internal/")
        or lowered == "docs/archive"
        or lowered.startswith("docs/archive/")
        or INTERNAL_ARCHIVE.fullmatch(normalized) is not None
    )


def tracked_paths(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    )
    return [path.decode("utf-8", "surrogateescape") for path in result.stdout.split(b"\0") if path]


def find_forbidden_tracked_paths(root: Path) -> list[str]:
    return sorted(path for path in tracked_paths(root) if is_forbidden(path))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()

    try:
        forbidden = find_forbidden_tracked_paths(args.root.resolve())
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"internal-artifact guard failed closed: {type(error).__name__}", file=sys.stderr)
        return 2

    if forbidden:
        print("forbidden tracked internal artifacts:", file=sys.stderr)
        for path in forbidden:
            print(f"- {path}", file=sys.stderr)
        return 1

    print("No forbidden internal artifacts are tracked.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
