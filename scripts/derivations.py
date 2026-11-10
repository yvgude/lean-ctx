#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Derivations registry: generated files are regenerated, list files merge as line sets.

Measured 10.11.2026 (Integration Engine X6, 85 MRs): 159 of 229 text conflicts came from one
generated file (the scope register), and generated files caused 39 % of the target merges in
agent branches. A generated file has no meaningful text merge, and a changelog or a manifest
list is a set of lines whose order between two branches does not matter. So:

* `.gitattributes` declares the merge strategy per path (`merge=leanctx-<strategy>`). It is the
  only place that says which strategy a file uses; git falls back to its normal text merge when
  the driver is not installed, so the declaration is safe in every clone.
* `derivations.py install` registers the drivers in the repository's git config (shared by all
  worktrees of the clone). Run it once per clone.
* git calls `derivations.py merge <strategy> %O %A %B %P` only when both sides changed a file:
  - `lineset`   three-way merge of a list of lines: both sides' insertions and deletions are
                applied, a line both sides added is kept once. Only a line both sides changed
                differently is a conflict (CHANGELOG, PUBLISH_MANIFEST.toml, the scope register).
  - `json`      three-way merge of a JSON object key by key; the same key set to two different
                values is a conflict, never a silent choice (frozen-hashes.json: a frozen hash
                is a reviewed promise and is never recomputed by a merge).
  - `cargolock` three-way merge of Cargo.lock `[[package]]` entries keyed by name, version and
                source; a package both sides changed is merged as a line set (its dependency
                list). Two new versions of one crate from the two sides are a conflict.
  - `regenerate` keeps the current side as a provisional result; `regenerate` below rebuilds
                the file from the merged inputs (SBOM, THIRD_PARTY_NOTICES).
  When a strategy finds a real conflict, git's own text merge runs and writes the usual markers.
* `derivations.py regenerate [--since REV]` rebuilds every registered derived file whose inputs
  (or itself) changed since REV (default ORIG_HEAD, which git sets on merge). Run it after a
  merge and commit the result; each file's CI `--check` gate fails if this was skipped.
"""

from __future__ import annotations

import argparse
import difflib
import fnmatch
import json
import re
import shutil
import subprocess
import sys
from collections import Counter
from pathlib import Path

STRATEGIES = ("lineset", "json", "cargolock", "regenerate")
DRIVER_NAMES = {s: f"leanctx-{s}" for s in STRATEGIES}


class Conflict(Exception):
    """The strategy cannot merge without a decision; git's text merge takes over."""


# ---------------------------------------------------------------------------------------------
# lineset


def _base_to_side(base: list[str], side: list[str]) -> dict[int, int]:
    """Map each base line index that survives unchanged in `side` to its index there."""
    mapping: dict[int, int] = {}
    matcher = difflib.SequenceMatcher(None, base, side, autojunk=False)
    for block in matcher.get_matching_blocks():
        for k in range(block.size):
            mapping[block.a + k] = block.b + k
    return mapping


def merge_lines(base: list[str], ours: list[str], theirs: list[str]) -> list[str]:
    """Apply theirs' changes against base onto ours, as a set of lines with stable positions."""
    in_ours = _base_to_side(base, ours)
    deleted_by_ours = {i for tag, i1, i2, _, _ in difflib.SequenceMatcher(None, base, ours, autojunk=False)
                       .get_opcodes() if tag == "delete" for i in range(i1, i2)}
    added_by_ours = Counter(ours) - Counter(base)
    deletions: set[int] = set()
    inserts: dict[int, list[str]] = {}
    matcher = difflib.SequenceMatcher(None, base, theirs, autojunk=False)
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            continue
        new = theirs[j1:j2]
        touched = [i for i in range(i1, i2) if i not in in_ours and base[i].strip()]
        if touched:
            # Both sides changed the same base lines. Both deleting them, or both writing the
            # same replacement, merges; edit against delete or two different edits do not.
            both_deleted = not new and all(i in deleted_by_ours for i in touched)
            same_edit = new and not any(i in deleted_by_ours for i in touched) and not (
                Counter(new) - added_by_ours)
            if not (both_deleted or same_edit):
                raise Conflict(f"both sides changed {base[touched[0]].strip()!r}")
        deletions.update(in_ours[i] for i in range(i1, i2) if i in in_ours)
        fresh = []
        for line in new:
            if line.strip() and added_by_ours[line] > 0:  # both sides added it: keep one
                added_by_ours[line] -= 1
            else:
                fresh.append(line)
        if not fresh:
            continue
        # Insert where base line i1 sits in ours; if ours dropped it, after the previous survivor.
        anchor = next((in_ours[i] for i in range(i1, len(base)) if i in in_ours), None)
        if anchor is None:
            anchor = len(ours)
        inserts.setdefault(anchor, []).extend(fresh)
    result: list[str] = []
    for index in range(len(ours) + 1):
        result.extend(inserts.get(index, []))
        if index < len(ours) and index not in deletions:
            result.append(ours[index])
    return result


def merge_lineset(base: str, ours: str, theirs: str) -> str:
    for text in (base, ours, theirs):
        if text and not text.endswith("\n"):
            raise Conflict("missing final newline")
    lines = [t.splitlines(keepends=True) for t in (base, ours, theirs)]
    return "".join(merge_lines(*lines))


# ---------------------------------------------------------------------------------------------
# json

_MISSING = object()


def _merge_value(base, ours, theirs, where: str):
    if ours == theirs:
        return ours
    if base == ours:
        return theirs
    if base == theirs:
        return ours
    if all(isinstance(v, dict) for v in (ours, theirs)) and (base is _MISSING or isinstance(base, dict)):
        base = {} if base is _MISSING else base
        merged = {}
        for key in list(ours) + [k for k in theirs if k not in ours]:
            value = _merge_value(base.get(key, _MISSING), ours.get(key, _MISSING), theirs.get(key, _MISSING),
                                 f"{where}.{key}")
            if value is not _MISSING:
                merged[key] = value
        return merged
    if all(isinstance(v, list) and all(isinstance(x, str) for x in v) for v in (ours, theirs)) and (
            base is _MISSING or isinstance(base, list)):
        lines = merge_lines(list(base) if isinstance(base, list) else [], ours, theirs)
        return list(dict.fromkeys(lines))
    raise Conflict(f"{where or '<root>'} changed differently on both sides")


def merge_json(base: str, ours: str, theirs: str) -> str:
    try:
        docs = [json.loads(t) if t.strip() else {} for t in (base, ours, theirs)]
    except json.JSONDecodeError as exc:
        raise Conflict(f"not JSON: {exc}") from exc
    style = None
    for sort_keys in (True, False):
        for indent in (2, 4):
            if json.dumps(docs[1], indent=indent, sort_keys=sort_keys, ensure_ascii=False) + "\n" == ours:
                style = (indent, sort_keys)
                break
        if style:
            break
    if style is None:
        raise Conflict("formatting of the current side is not reproducible")
    merged = _merge_value(*docs, "")
    return json.dumps(merged, indent=style[0], sort_keys=style[1], ensure_ascii=False) + "\n"


# ---------------------------------------------------------------------------------------------
# cargolock


def _lock_parts(text: str) -> tuple[str, dict[tuple[str, str, str], str]]:
    chunks = text.split("\n\n")
    head = [c for c in chunks if not c.startswith("[[package]]")]
    if any(c.startswith("[[") for c in head):
        raise Conflict("Cargo.lock has sections other than [[package]]")
    packages: dict[tuple[str, str, str], str] = {}
    for chunk in chunks:
        if chunk.startswith("[[package]]"):
            fields = dict(re.findall(r'^(name|version|source) = "([^"]*)"$', chunk, re.M))
            body = chunk.rstrip("\n") + "\n"
            packages[(fields.get("name", ""), fields.get("version", ""), fields.get("source", ""))] = body
    return "\n\n".join(c.rstrip("\n") for c in head if c.strip()) + "\n", packages


def _semver_key(version: str) -> tuple:
    core, _, pre = version.partition("-")
    nums = tuple(int(p) if p.isdigit() else 0 for p in core.split("."))
    # A release sorts after its pre-releases.
    return nums, pre == "", tuple((0, int(p)) if p.isdigit() else (1, p) for p in pre.split(".") if pre)


def merge_cargolock(base: str, ours: str, theirs: str) -> str:
    (bh, bp), (oh, op), (th, tp) = (_lock_parts(t) for t in (base, ours, theirs))
    head = _merge_value(bh, oh, th, "Cargo.lock header")
    merged: dict[tuple[str, str, str], str] = {}
    for key in set(bp) | set(op) | set(tp):
        b, o, t = bp.get(key, _MISSING), op.get(key, _MISSING), tp.get(key, _MISSING)
        if o is not _MISSING and t is not _MISSING and o != t and b not in (o, t):
            value = merge_lineset("" if b is _MISSING else b, o, t)
        else:
            value = _merge_value(b, o, t, f"package {key[0]} {key[1]}")
        if value is not _MISSING:
            merged[key] = value

    def versions(packages, name):
        return {k for k in packages if k[0] == name}

    for name in {k[0] for k in merged}:
        now = versions(merged, name)
        if now != versions(op, name) and now != versions(tp, name) and len(now) > max(
                len(versions(op, name)), len(versions(tp, name))):
            raise Conflict(f"both sides moved {name} to different versions")
    ordered = sorted(merged, key=lambda k: (k[0], _semver_key(k[1]), k[2]))
    return head + "".join("\n" + merged[k] for k in ordered)


MERGERS = {"lineset": merge_lineset, "json": merge_json, "cargolock": merge_cargolock,
           "regenerate": lambda base, ours, theirs: ours}


def cmd_merge(strategy: str, base_path: str, ours_path: str, theirs_path: str, name: str) -> int:
    try:
        texts = [Path(p).read_text(encoding="utf-8") for p in (base_path, ours_path, theirs_path)]
        merged = MERGERS[strategy](*texts)
    except (Conflict, UnicodeError) as exc:
        print(f"derivations: {name}: {strategy} merge needs a decision ({exc}); text merge follows",
              file=sys.stderr)
        # git's own three-way text merge, markers included; exit status = number of conflicts.
        return min(subprocess.run(["git", "merge-file", "-L", "ours", "-L", "base", "-L", "theirs",
                                   ours_path, base_path, theirs_path]).returncode, 1)
    Path(ours_path).write_text(merged, encoding="utf-8")
    if strategy == "regenerate":
        print(f"derivations: {name}: kept the current side; run scripts/derivations.py regenerate",
              file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------------------------
# regenerate: path pattern → inputs and the command that rebuilds it. Order matters: a later
# entry may read an earlier one (SBOM and notices read rust/Cargo.lock).


def _scope_register_command(root: Path) -> list[str] | None:
    ci = root / "ee/ci/scope-register.yml"
    if not ci.is_file():
        return None
    found = re.search(r"scope_register\.py --repo \. --baseline ([0-9a-f]{40})", ci.read_text(encoding="utf-8"))
    if not found:
        raise SystemExit("derivations: no --baseline in ee/ci/scope-register.yml")
    return [sys.executable, "ee/delivery/tools/scope_register.py", "--repo", ".", "--baseline", found.group(1),
            "--out", "ee/delivery/FEATURE_SCOPE_REGISTER.csv"]


def _cargo_lock_command(lock: str, offline: bool = True) -> list[str]:
    # Resolving (no --locked) completes and normalises the lock against the merged manifests;
    # a lock that is already current stays byte for byte.
    return ["cargo", "metadata", "--format-version", "1", *(["--offline"] if offline else []),
            "--manifest-path", str(Path(lock).parent / "Cargo.toml")]


REGISTRY: list[tuple[str, tuple[str, ...]]] = [
    ("*Cargo.lock", ("{dir}*Cargo.toml",)),
    ("SBOM.cdx.json", ("rust/Cargo.lock", "rust/*Cargo.toml", "rust/deny.toml", "scripts/generate-sbom.sh")),
    ("THIRD_PARTY_NOTICES", ("rust/Cargo.lock", "SBOM.cdx.json", "legal/third-party-assets.toml",
                             "packages/pi-lean-ctx/package-lock.json", "scripts/generate-third-party-notices.py")),
    ("ee/delivery/FEATURE_SCOPE_REGISTER.csv", ("rust/src/*", "rust/crates/*", "packages/*", "sdk/*", "ee/*",
                                                "ee/ci/scope-register.yml")),
]


def _commands(path: str, root: Path) -> tuple[list[str] | None, list[list[str]]] | None:
    """(check, writers) for a derived file: a passing check leaves the file alone, which keeps
    the legal approval of THIRD_PARTY_NOTICES; otherwise the writers run until one succeeds."""
    if path.endswith("Cargo.lock"):
        return None, [_cargo_lock_command(path), _cargo_lock_command(path, offline=False)]
    if path == "SBOM.cdx.json":
        return ["bash", "scripts/generate-sbom.sh", "--check"], [["bash", "scripts/generate-sbom.sh"]]
    if path == "THIRD_PARTY_NOTICES":
        script = [sys.executable, "scripts/generate-third-party-notices.py", "--root", "."]
        return [*script, "--check"], [[*script, "--write"]]
    if path == "ee/delivery/FEATURE_SCOPE_REGISTER.csv":
        command = _scope_register_command(root)
        return None if command is None else (None, [command])
    return None


def _git(root: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True, text=True).stdout


def _run(command: list[str], root: Path) -> int:
    if shutil.which(command[0]) is None and not Path(command[0]).is_file():
        print(f"derivations: {command[0]} not found", file=sys.stderr)
        return 127
    return subprocess.run(command, cwd=root, stdout=subprocess.DEVNULL).returncode


def cmd_regenerate(root: Path, since: str, dry_run: bool) -> int:
    changed = set(_git(root, "diff", "--name-only", since).split())
    changed |= set(_git(root, "ls-files", "--others", "--exclude-standard").split())
    tracked = _git(root, "ls-files").split()
    failed = 0
    for pattern, inputs in REGISTRY:
        for path in (p for p in tracked if fnmatch.fnmatchcase(p, pattern)):
            directory = path.rsplit("/", 1)[0] + "/" if "/" in path else ""
            globs = [g.replace("{dir}", directory) for g in inputs]
            if path not in changed and not any(fnmatch.fnmatchcase(c, g) for c in changed for g in globs):
                continue
            commands = _commands(path, root)
            if commands is None:
                continue
            check, writers = commands
            print(f"derivations: regenerate {path}: {' '.join(writers[0])}", file=sys.stderr)
            if dry_run:
                continue
            if check is not None and _run(check, root) == 0:
                print(f"derivations: {path}: current", file=sys.stderr)
                continue
            for writer in writers:
                status = _run(writer, root)
                if status == 0:
                    break
            else:
                print(f"derivations: {path}: regeneration failed (exit {status})", file=sys.stderr)
                failed += 1
    return 1 if failed else 0


def cmd_install(root: Path) -> int:
    for strategy, name in DRIVER_NAMES.items():
        _git(root, "config", f"merge.{name}.name", f"LeanCTX derivations: {strategy}")
        _git(root, "config", f"merge.{name}.driver",
             f"python3 scripts/derivations.py merge {strategy} %O %A %B %P")
    print("derivations: merge drivers installed: " + ", ".join(DRIVER_NAMES.values()), file=sys.stderr)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    m = sub.add_parser("merge", help="git merge driver (called by git)")
    m.add_argument("strategy", choices=STRATEGIES)
    m.add_argument("base")
    m.add_argument("ours")
    m.add_argument("theirs")
    m.add_argument("path", nargs="?", default="")
    i = sub.add_parser("install", help="register the merge drivers in this clone's git config")
    i.add_argument("--repo", type=Path, default=Path("."))
    r = sub.add_parser("regenerate", help="rebuild derived files whose inputs changed")
    r.add_argument("--repo", type=Path, default=Path("."))
    r.add_argument("--since", default="ORIG_HEAD", help="revision to compare the worktree with")
    r.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    if args.cmd == "merge":
        return cmd_merge(args.strategy, args.base, args.ours, args.theirs, args.path)
    root = Path(_git(args.repo, "rev-parse", "--show-toplevel").strip())
    if args.cmd == "install":
        return cmd_install(root)
    return cmd_regenerate(root, args.since, args.dry_run)


if __name__ == "__main__":
    sys.exit(main())
