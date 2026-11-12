#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Bisect hint for a red post-merge run (tier 2 of the tiered CI, owner 11.11.).

Merge-request pipelines run only the tests a change affects
(scripts/ci/mr_quick.py); the full suite runs after the merge on
integration/* (v4-runtime). When that run is red, this prints, and writes to
--out, the last green commit (the newest warm-farm generation whose commit is
an ancestor of HEAD: only a green post-merge run publishes one), the
first-parent merges since it (the suspects), the failing tests of the JUnit
report and a ready `git bisect` command over exactly those tests.

Read-only and best effort: it never changes the job's verdict.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

GEN_RE = re.compile(r"^(\d{6})-([0-9a-f]{12})$")
MAX_TESTS = 20


def git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", "-c", f"safe.directory={repo}", "-C", str(repo), *args],
                          capture_output=True, text=True, check=False)


def ref_slug(name: str) -> str:
    """GitLab's CI_COMMIT_REF_SLUG rule (as scripts/ci/warm_farm.py)."""
    return re.sub(r"[^a-z0-9]", "-", name.lower())[:63].strip("-")


def last_green(farm: Path | None, project: str, branch: str, repo: Path, head: str) -> tuple[str, str] | None:
    """(commit, generation name) of the newest generation that is an ancestor
    of head and not head itself, whatever toolchain or paths built it."""
    if farm is None:
        return None
    gens = farm / ref_slug(project) / ref_slug(branch) / "gens"
    try:
        names = sorted((p.name for p in gens.iterdir() if GEN_RE.match(p.name)), reverse=True)
    except OSError:
        return None
    for name in names:
        try:
            meta = json.loads((gens / name / ".warm-farm.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        commit = meta.get("commit", "") if isinstance(meta, dict) else ""
        if not re.fullmatch(r"[0-9a-f]{40}", commit) or commit == head:
            continue
        if git(repo, "merge-base", "--is-ancestor", commit, head).returncode == 0:
            return commit, name
    return None


def failing_tests(junit: Path) -> list[tuple[str, str]]:
    """(binary id, test name) of every failed or errored test case."""
    try:
        root = ET.parse(junit).getroot()
    except (OSError, ET.ParseError):
        return []
    out = []
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                out.append((case.get("classname") or suite.get("name") or "", case.get("name") or ""))
    return sorted(set(out))


def report(repo: Path, farm: Path | None, project: str, branch: str, junit: Path) -> str:
    head = git(repo, "rev-parse", "HEAD").stdout.strip()
    lines = [f"== post-merge RED on {branch} at {head}"]
    green = last_green(farm, project, branch, repo, head)
    if green:
        commit, gen = green
        lines.append(f"last green: {commit} (warm-farm generation {gen})")
        rng = f"{commit}..{head}"
    else:
        lines.append("last green: unknown (no warm-farm generation is an ancestor); suspects since HEAD^1")
        commit, rng = "", f"{head}^1..{head}"
    log = git(repo, "log", "--first-parent", "--reverse", "--format=%H %s", rng).stdout.splitlines()
    lines.append(f"suspects (first-parent, oldest first): {len(log)}")
    lines.extend(f"  {entry}" for entry in log)
    tests = failing_tests(junit)
    lines.append(f"failing tests: {len(tests)}")
    lines.extend(f"  {bid} {name}" for bid, name in tests[:MAX_TESTS])
    if len(tests) > MAX_TESTS:
        lines.append(f"  ... and {len(tests) - MAX_TESTS} more (runtime-evidence/junit.xml)")
    if commit and tests:
        expr = " | ".join(f"(binary_id({bid}) & test(={name}))" for bid, name in tests[:MAX_TESTS])
        lines.append("bisect (from the repository root):")
        lines.append(f"  git bisect start --first-parent {head} {commit}")
        lines.append("  git bisect run sh -c 'cd rust && cargo +stable nextest run --locked --workspace "
                     "--features \"$(python3 ../scripts/ci/mr_quick.py --repo .. --print-test-features)\" "
                     f"--profile ci -E \"{expr}\"'")
    elif len(log) == 1:
        lines.append(f"one suspect: {log[0].split(' ', 1)[0]}")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo", required=True, type=Path)
    ap.add_argument("--farm", default="", help="the warm farm mount; empty when absent")
    ap.add_argument("--project", required=True)
    ap.add_argument("--branch", required=True)
    ap.add_argument("--junit", required=True, type=Path)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args(argv)
    farm = Path(args.farm) if args.farm else None
    text = report(args.repo.resolve(), farm, args.project, args.branch, args.junit)
    sys.stderr.write(text)
    if args.out:
        try:
            args.out.write_text(text, encoding="utf-8")
        except OSError as exc:
            sys.stderr.write(f"postmerge-bisect: cannot write {args.out}: {exc}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
