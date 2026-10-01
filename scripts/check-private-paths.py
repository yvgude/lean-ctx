#!/usr/bin/env python3
"""Fail when a path that must stay off the public mirror is tracked.

`.github-ignore` is the single list of paths that never belong in this
repository. Two ways it can be defeated are checked here:

1. A denied path is already tracked. The pre-push hook only inspects files a
   push adds, so a file committed before its path was denied stays public
   forever unless the tracked tree itself is checked.
2. A path marked confidential in `.gitignore` is missing from `.github-ignore`.
   `.gitignore` only stops `git add`; a forced add or an older commit still
   publishes the file, and nothing else would notice.

Usage: check-private-paths.py [repo-root]
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import Iterable, List

# `.gitignore` sections whose entries are confidential, by their header line.
CONFIDENTIAL_GITIGNORE_SECTIONS = ("Internal strategy docs (confidential)",)


def _entries(lines: Iterable[str]) -> List[str]:
    out = []
    for raw in lines:
        line = raw.strip()
        if line and not line.startswith("#"):
            out.append(line)
    return out


def denied_paths(github_ignore: str) -> List[str]:
    return [line.lstrip("/") for line in _entries(github_ignore.splitlines())]


def confidential_gitignore_entries(gitignore: str) -> List[str]:
    """Entries of the confidential sections, without glob-only patterns."""
    entries: List[str] = []
    active = False
    for raw in gitignore.splitlines():
        line = raw.strip()
        if line.startswith("# ──"):
            active = any(name in line for name in CONFIDENTIAL_GITIGNORE_SECTIONS)
            continue
        if not active or not line or line.startswith("#"):
            continue
        if "*" in line:
            continue
        entries.append(line.lstrip("/"))
    return entries


def _covered(path: str, denied: Iterable[str]) -> bool:
    for entry in denied:
        if entry.endswith("/"):
            if path == entry.rstrip("/") or path.startswith(entry):
                return True
        elif path == entry:
            return True
    return False


def find_violations(github_ignore: str, gitignore: str, tracked: Iterable[str]) -> List[str]:
    denied = denied_paths(github_ignore)
    findings = []
    for entry in confidential_gitignore_entries(gitignore):
        if not _covered(entry, denied):
            findings.append(
                f"[drift] confidential .gitignore entry {entry!r} is missing from .github-ignore"
            )
    for path in sorted(tracked):
        if _covered(path, denied):
            findings.append(f"[tracked] {path} is listed in .github-ignore but tracked")
    return findings


def main(argv: List[str]) -> int:
    root = Path(argv[1] if len(argv) > 1 else Path(__file__).resolve().parents[1])
    try:
        github_ignore = (root / ".github-ignore").read_text(encoding="utf-8")
        gitignore = (root / ".gitignore").read_text(encoding="utf-8")
        tracked = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z"],
            check=True,
            capture_output=True,
        ).stdout.decode("utf-8").split("\0")
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"check-private-paths: cannot read repository state: {error}", file=sys.stderr)
        return 2
    findings = find_violations(github_ignore, gitignore, [t for t in tracked if t])
    for finding in findings:
        print(finding)
    if findings:
        print("Private paths must not be tracked in the public repository. See .github-ignore.")
        return 1
    print("check-private-paths: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
