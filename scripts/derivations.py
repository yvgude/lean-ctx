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
  worktrees of the clone). Run it once per clone: `make setup` (or `make setup-merge-drivers`).
  The driver runs the script of the tree being merged and falls back to git's marked text
  merge where that tree has no script, so backports to older lines stay safe.
* git calls `derivations.py merge <strategy> %O %A %B %P` only when both sides changed a file:
  - `lineset`   three-way merge of a list of lines, hunk by hunk: a change only one side made
                is applied; where both sides touched the same or adjacent base lines or
                inserted at the same place, identical changes merge once, deletions merge,
                inserts of disjoint plain lines into one gap keep both (two changelog entries,
                two manifest paths), and anything else is a conflict: an edit next to a
                deletion, or an insert that opens a section (a TOML table header, a changelog
                heading), a multi-line string or an unclosed bracket, which would move the other
                side's lines into it.
                A line is never dropped because the other side added the same text elsewhere,
                so structured blocks (a TOML table, a dependency list) both sides appended
                conflict instead of merging into broken syntax (CHANGELOG,
                PUBLISH_MANIFEST.toml, the scope register).
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
* `derivations.py check [--since REV] [--commit REV]` is the guard (ECCP W5-02): every registered
  derived file whose inputs (or itself) changed between REV and the commit must equal its
  regeneration at that commit. It runs each file's non-writing check in a temporary worktree of
  the commit (`--in-place`: in this checkout, for a clean CI clone), names every file that
  differs and exits 1. `.githooks/pre-push` runs it for the pushed commit.
* `derivations.py write-publish-manifest [--check]` names new public files in `new_paths` of
  PUBLISH_MANIFEST.toml (19 of 136 red v4-provenance jobs, 05.-11.11.2026), but only a file
  that LICENSE_MATRIX.toml classifies as public Apache source and that declares
  `SPDX-License-Identifier: Apache-2.0` itself; it drops names of files that are gone. Anything
  else (private by policy, vendored, a fixture or a file without the header) is refused path by
  path with the decision it needs. `regenerate` and `check` run it for every change.
* `derivations.py repin-frozen MANIFEST PATH...` re-pins named entries of a contract pack's
  frozen-hashes.json. A frozen hash is a reviewed contract promise, so nothing re-pins it
  automatically; the verifier's failure message prints this command for a reviewed change.
"""

from __future__ import annotations

import argparse
import difflib
import fnmatch
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import traceback
from pathlib import Path

STRATEGIES = ("lineset", "json", "cargolock", "regenerate")
DRIVER_NAMES = {s: f"leanctx-{s}" for s in STRATEGIES}
# `merge` exits 0 (merged) or 1 (conflict, markers written). Anything else means the driver did
# not run: no script in this tree (a backport or a branch cut before the registry), no python3,
# an older script. git would then record a conflict without markers and keep ours unchanged, so
# a later `git add` would silently drop theirs. The installed driver therefore runs the script
# from the tree being merged (git starts drivers at the worktree root) and falls back to git's
# own marked text merge whenever it exits with anything but 0, or with 1 but no markers in the
# file: an uncaught Python exception also exits 1 (the script of a branch cut between the
# registry and this check catches only some errors), and that is a crash, not a conflict.
EXIT_DRIVER_FAILED = 3
DRIVER = (
    "sh -c '"
    'if [ -f scripts/derivations.py ]; then python3 scripts/derivations.py merge "$0" "$@"; s=$?; '
    '[ "$s" -eq 0 ] && exit 0; '
    '[ "$s" -eq 1 ] && grep -q "^<<<<<<< " "$2" && exit 1; '
    'echo "derivations: merge driver failed (exit $s); text merge follows" >&2; fi; '
    'git merge-file -L ours -L base -L theirs "$2" "$1" "$3"'
    "' {strategy} %O %A %B %P"
)


class Conflict(Exception):
    """The strategy cannot merge without a decision; git's text merge takes over."""


# ---------------------------------------------------------------------------------------------
# lineset


def _hunks(base: list[str], side: list[str]) -> list[tuple[int, int, list[str]]]:
    """The side's changes against base: (first base line, end base line, replacement lines)."""
    matcher = difflib.SequenceMatcher(None, base, side, autojunk=False)
    return [(i1, i2, side[j1:j2]) for tag, i1, i2, j1, j2 in matcher.get_opcodes() if tag != "equal"]


def _overlap(a: tuple[int, int, list[str]], b: tuple[int, int, list[str]]) -> bool:
    """Two hunks touch the same base lines, insert into the same gap, or one inserts into or at
    either edge of lines the other removed or replaced. Adjacent changes conflict as in git's own
    text merge: an insert right after a deleted block belongs to that block (a key of a deleted
    TOML table, a detail line of a dropped changelog entry), and merging it would attach it to
    whatever precedes the block."""
    (a1, a2, _), (b1, b2, _) = a, b
    if a1 == a2 and b1 == b2:
        return a1 == b1
    if a1 == a2:
        return b1 <= a1 <= b2
    if b1 == b2:
        return a1 <= b1 <= a2
    return max(a1, b1) <= min(a2, b2)


def _resolve(ours: tuple[int, int, list[str]], theirs: tuple[int, int, list[str]]) -> tuple[int, int, list[str]]:
    """One hunk from each side on the same base lines or gap."""
    if ours == theirs:
        return ours
    (o1, o2, onew), (t1, t2, tnew) = ours, theirs
    if not onew and not tnew:  # both deleted lines: delete the union
        return min(o1, t1), max(o2, t2), []
    if o1 == o2 == t1 == t2:
        # Both inserted into the same gap. Two disjoint sets of plain entries (two changelog
        # lines, two manifest paths, two register rows) are independent: keep both. Shared
        # non-blank lines mean the inserts are structured blocks (a TOML table, a dependency
        # list) that cannot be interleaved or deduplicated safely. An insert that opens a section
        # or a bracket it does not close (or closes one it did not open) changes where the lines
        # after it belong: concatenated, the other side's lines would land in that section (a key
        # meant for `[a]` under the `[b]` ours appended, a path in the wrong list). Both are a
        # decision for a person.
        if _structural(onew) or _structural(tnew):
            raise Conflict(f"both sides inserted before base line {o1 + 1} and one opens or closes a section")
        if not {line for line in onew if line.strip()} & {line for line in tnew if line.strip()}:
            return o1, o2, onew + tnew
        raise Conflict(f"both sides inserted overlapping blocks before base line {o1 + 1}")
    raise Conflict(f"both sides changed base lines {min(o1, t1) + 1}-{max(o2, t2)} differently")


# A TOML table or array-of-tables header, or a Markdown heading (a changelog section). TOML
# comments match the heading pattern too; they only make a merge conflict that could have merged.
_SECTION = re.compile(r"^\s*(\[\[?[^\[\]]*\]\]?\s*(#.*)?|#+(\s.*)?)$")
_QUOTED = re.compile(r'"(?:[^"\\\n]|\\.)*"')
# Quoted TOML keys (basic and literal strings) may hold brackets: `[t."x]y"]` is a header.
_KEY_STRINGS = re.compile(r"\"(?:[^\"\\\n]|\\.)*\"|'[^'\n]*'")
# A TOML multi-line string delimiter. Lines between two of them are string content, not
# entries: an insert holding one opens or closes a string around the other side's lines.
_MULTILINE_STRING = ('"""', "'''")


def _structural(lines: list[str]) -> bool:
    """The insert starts a section or touches a multi-line string, or its brackets do not
    balance within itself."""
    depth = 0
    for line in lines:
        if any(delimiter in line for delimiter in _MULTILINE_STRING):
            return True
        if _SECTION.match(_KEY_STRINGS.sub('""', line.rstrip("\n"))):
            return True
        for char in _QUOTED.sub("", line):
            if char in "[{":
                depth += 1
            elif char in "]}":
                depth -= 1
                if depth < 0:
                    return True
    return depth != 0


def merge_lines(base: list[str], ours: list[str], theirs: list[str]) -> list[str]:
    """Three-way merge of a list of lines, hunk by hunk against base.

    A hunk only one side made is applied. Where both sides changed the same base lines or
    inserted into the same gap, only identical changes, deletions, or inserts of disjoint plain
    lines (no section header, no multi-line string delimiter, balanced brackets) merge;
    everything else raises Conflict. A line is never dropped because the other side added the
    same text somewhere else, so structural lines (`]`, `[[generated]]`) survive.
    """
    tagged = [(h, 0) for h in _hunks(base, ours)] + [(h, 1) for h in _hunks(base, theirs)]
    tagged.sort(key=lambda item: (item[0][0], item[0][1], item[1]))
    resolved: list[tuple[int, int, list[str]]] = []
    for hunk, side in tagged:
        partners = [h for h, s in tagged if s != side and _overlap(hunk, h)]
        if not partners:
            resolved.append(hunk)
        elif side == 0:
            if len(partners) > 1 or sum(_overlap(partners[0], h) for h, s in tagged if s == 0) > 1:
                raise Conflict(f"both sides changed base lines near {hunk[0] + 1} differently")
            resolved.append(_resolve(hunk, partners[0]))
    resolved.sort(key=lambda h: (h[0], h[1]))
    result: list[str] = []
    position = 0
    for i1, i2, new in resolved:
        if i1 < position:
            raise Conflict(f"both sides changed base lines near {i1 + 1} differently")
        result.extend(base[position:i1])
        result.extend(new)
        position = i2
    result.extend(base[position:])
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
    except Exception as exc:  # noqa: BLE001 - any failure must end in git's marked text merge
        reason = exc if isinstance(exc, Conflict) else f"{type(exc).__name__}: {exc}"
        print(f"derivations: {name}: {strategy} merge needs a decision ({reason}); text merge follows",
              file=sys.stderr)
        # git's own three-way text merge, markers included; it exits with the number of
        # conflicts, or negative (255) when it could not merge at all.
        status = subprocess.run(["git", "merge-file", "-L", "ours", "-L", "base", "-L", "theirs",
                                 ours_path, base_path, theirs_path]).returncode
        return 0 if status == 0 else 1 if 0 < status < 128 else EXIT_DRIVER_FAILED
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
    # Any added or removed file can need a `new_paths` change.
    ("PUBLISH_MANIFEST.toml", ("*",)),
]

PUBLISH_MANIFEST = "PUBLISH_MANIFEST.toml"
PUBLIC_EXPORT = "scripts/public-export.py"
IP_AUDIT = "scripts/ip-provenance-audit.py"
LICENSE_MATRIX = "LICENSE_MATRIX.toml"


def _commands(path: str, root: Path) -> tuple[list[str] | None, list[list[str]]] | None:
    """(check, writers) for a derived file: a passing check leaves the file alone, which keeps
    the legal approval of THIRD_PARTY_NOTICES; otherwise the writers run until one succeeds."""
    if path.endswith("Cargo.lock"):
        return None, [_cargo_lock_command(path), _cargo_lock_command(path, offline=False)]
    if path == PUBLISH_MANIFEST:
        if not (root / PUBLIC_EXPORT).is_file():
            return None
        write = [sys.executable, "scripts/derivations.py", "write-publish-manifest"]
        return [*write, "--check"], [write]
    if path == "SBOM.cdx.json":
        return ["bash", "scripts/generate-sbom.sh", "--check"], [["bash", "scripts/generate-sbom.sh"]]
    if path == "THIRD_PARTY_NOTICES":
        script = [sys.executable, "scripts/generate-third-party-notices.py", "--root", "."]
        return [*script, "--check"], [[sys.executable, "scripts/derivations.py", "write-notices"]]
    if path == "ee/delivery/FEATURE_SCOPE_REGISTER.csv":
        command = _scope_register_command(root)
        return None if command is None else (None, [command])
    return None


NOTICE_ASSETS = "legal/third-party-assets.toml"
_INPUTS_TABLE = re.compile(r"^\[inputs\]\n(.*?)(?=^\[|\Z)", re.M | re.S)
_INPUT_LINE = re.compile(r'^([a-z0-9_]+) = "([^"]*)"$', re.M)

def refresh_notice_input_digests(root: Path) -> list[str]:
    """Re-pin the input digests in the [inputs] table of the notice asset manifest.

    `cargo_lock_sha256` and its siblings pin the bytes the notices were generated from; they
    are not approval evidence (legal/third-party-notices-approval.toml binds the generated
    file, whose status falls back to PENDING APPROVAL). Every Cargo.lock change made them
    stale, `--write` refused with "digest drift" and the pins were edited by hand (61 of 136
    red v4-provenance jobs, 05.-11.11.2026). An input that still fails its own check (an SBOM
    whose recorded Cargo.lock digest is stale) keeps the generator failing closed. Returns the
    keys it changed."""
    path = root / NOTICE_ASSETS
    if not path.is_file():
        return []
    text = path.read_text(encoding="utf-8")
    table = _INPUTS_TABLE.search(text)
    if table is None:
        return []
    values = dict(_INPUT_LINE.findall(table.group(1)))
    body, changed = table.group(1), []
    for key, rel in sorted(values.items()):
        digest_key = f"{key}_sha256"
        if key.endswith("_sha256") or digest_key not in values:
            continue
        source = (root / rel).resolve()
        if root.resolve() not in source.parents or not source.is_file():
            raise SystemExit(f"derivations: {NOTICE_ASSETS}: input {key} is not a repository file")
        actual = hashlib.sha256(source.read_bytes()).hexdigest()
        if actual != values[digest_key]:
            body = re.sub(rf'^{digest_key} = "[^"]*"$', f'{digest_key} = "{actual}"', body, flags=re.M)
            changed.append(digest_key)
    if changed:
        path.write_text(text[:table.start(1)] + body + text[table.end(1):], encoding="utf-8")
    return changed

def cmd_write_notices(root: Path) -> int:
    for key in refresh_notice_input_digests(root):
        print(f"derivations: {NOTICE_ASSETS}: re-pinned {key}", file=sys.stderr)
    script = [sys.executable, "scripts/generate-third-party-notices.py", "--root", ".", "--write"]
    return subprocess.run(script, cwd=root).returncode

# ---------------------------------------------------------------------------------------------
# write-publish-manifest: `new_paths` of PUBLISH_MANIFEST.toml.
#
# A selected file that the public baseline lacks is refused by scripts/public-export.py until
# `new_paths` names it, "after its provenance and licence evidence were reviewed". The name is
# evidence, so it is written here only where the evidence is in the file and the policy files:
# LICENSE_MATRIX.toml gives exactly one rule, that rule is public Apache source (not private,
# commercial, free-runtime, third-party or a header-less fixture) and the file itself declares
# `SPDX-License-Identifier: Apache-2.0`. The name still shows in the merge request diff. Every
# other new file is refused with the decision it needs: `exclude` it (private), add the header
# (own work) or name it by hand after a review.

_NEW_PATHS = re.compile(r"^new_paths = \[\n(.*?)^\]$", re.M | re.S)
_NAME_LINE = re.compile(r'^\s*"([^"\\]+)",\s*(#.*)?$')
EXIT_UNAVAILABLE = 2  # the check cannot run here (baseline commit not fetched)


def _load_script(root: Path, rel: str, name: str):
    spec = importlib.util.spec_from_file_location(name, root / rel)
    if spec is None or spec.loader is None:
        raise SystemExit(f"derivations: cannot load {rel}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _worktree_files(root: Path) -> list[str]:
    """Tracked and untracked (not ignored) files that exist in the worktree."""
    names = _git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").split("\0")
    return sorted({n for n in names if n and os.path.lexists(root / n)})


def classify_new_public_file(root: Path, path: str, matrix: dict, audit) -> str | None:
    """None when the file may be named automatically, else the reason it needs a decision."""
    if '"' in path or "\\" in path or any(ord(c) < 32 for c in path):
        return "its name cannot be written as a plain TOML string; name it by hand"
    target = root / path
    if target.is_symlink() or not target.is_file():
        return "not a regular file; list it in `exclude`"
    rules = [rule for rule in matrix.get("rules", []) if audit.matches(path, rule)]
    if len(rules) != 1:
        names = ", ".join(str(rule.get("name")) for rule in rules) or "none"
        return f"{LICENSE_MATRIX} has {len(rules)} rules for it ({names}); classify it there first"
    rule = rules[0]
    klass, license_name = str(rule.get("class")), rule.get("license")
    if (rule.get("forbidden_public") or klass in audit.APACHE_HOST_PRIVATE_CLASSES
            or path in audit.APACHE_HOST_PRIVATE_ARTIFACTS):
        return f"private by {LICENSE_MATRIX} rule {rule.get('name')} ({klass}); add it to `exclude`"
    if klass == "third_party" or license_name != "apache":
        return (f"{LICENSE_MATRIX} rule {rule.get('name')} ({klass}, licence {license_name}): third-party "
                "provenance and notices need a review; name it by hand after that review")
    if rule.get("fixture"):
        return f"fixture of rule {rule.get('name')} carries no licence header; name it by hand after a review"
    expected = matrix.get("licenses", {}).get(license_name, {}).get("spdx")
    with target.open("rb") as handle:
        head = handle.read(64 * 1024)
    header, _ = audit.spdx_header(head)
    if header != expected:
        return (f"it does not declare `SPDX-License-Identifier: {expected}` in its first 40 lines: add the "
                "header if it is own work for the public tree, add it to `exclude` if it is private, or "
                "name it by hand after a review")
    return None


def publish_manifest_plan(root: Path) -> tuple[list[str], list[str], list[tuple[str, str]]]:
    """(names to add, names to drop, refused (path, reason)) for `new_paths`."""
    export = _load_script(root, PUBLIC_EXPORT, "public_export")
    try:
        manifest = export.Manifest(root / PUBLISH_MANIFEST)
    except export.ExportError as exc:
        raise SystemExit(f"derivations: {PUBLISH_MANIFEST}: {exc}") from exc
    if manifest.baseline_commit is None:
        return [], [], []
    probe = subprocess.run(["git", "-C", str(root), "cat-file", "-e", manifest.baseline_commit + "^{commit}"],
                           capture_output=True)
    if probe.returncode != 0:
        print(f"derivations: {PUBLISH_MANIFEST}: baseline {manifest.baseline_commit} is not in this clone; "
              "fetch it (git fetch origin) and run again", file=sys.stderr)
        raise SystemExit(EXIT_UNAVAILABLE)
    published = set(_git(root, "ls-tree", "-r", "-z", "--name-only", "--full-tree",
                         manifest.baseline_commit).split("\0")) - {""}
    new = {path for path in _worktree_files(root) if manifest.selects(path)} - published
    named = set(manifest.new_paths)
    drop = sorted(named - new)
    add, refused = [], []
    missing = sorted(new - named)
    if missing:
        audit = _load_script(root, IP_AUDIT, "ip_provenance_audit")
        matrix = tomllib.loads((root / LICENSE_MATRIX).read_text(encoding="utf-8"))
        for path in missing:
            prefix = manifest.forbidden_prefix(path)
            reason = (f"selected under forbidden prefix {prefix}: fix `include`/`exclude`" if prefix
                      else classify_new_public_file(root, path, matrix, audit))
            if reason is None:
                add.append(path)
            else:
                refused.append((path, reason))
    return add, drop, refused


def rewrite_new_paths(text: str, add: list[str], drop: list[str]) -> str:
    """Drop the named entry lines and insert each new name before the first entry that sorts
    after it (above that entry's comment), so names from parallel branches rarely share a gap."""
    block = _NEW_PATHS.search(text)
    if block is None:
        raise SystemExit(f"derivations: {PUBLISH_MANIFEST}: no `new_paths = [` ... `]` block")
    lines = block.group(1).splitlines(keepends=True)
    gone = set(drop)
    lines = [line for line in lines if not ((m := _NAME_LINE.match(line.rstrip("\n"))) and m.group(1) in gone)]
    for path in sorted(add):
        index = len(lines)
        for i, line in enumerate(lines):
            m = _NAME_LINE.match(line.rstrip("\n"))
            if m and m.group(1) > path:
                index = i
                break
        if index < len(lines):
            while index > 0 and lines[index - 1].lstrip().startswith("#"):
                index -= 1
        lines.insert(index, f'  "{path}",\n')
    return text[:block.start(1)] + "".join(lines) + text[block.end(1):]


def cmd_write_publish_manifest(root: Path, check: bool) -> int:
    add, drop, refused = publish_manifest_plan(root)
    for path, reason in refused:
        print(f"derivations: {PUBLISH_MANIFEST}: {path}: not named automatically: {reason}", file=sys.stderr)
    if check:
        for path in add:
            print(f"derivations: {PUBLISH_MANIFEST}: {path} is new to the public tree and not in `new_paths`",
                  file=sys.stderr)
        for path in drop:
            print(f"derivations: {PUBLISH_MANIFEST}: `new_paths` names {path}, which is not a new selected file",
                  file=sys.stderr)
        return 1 if add or drop or refused else 0
    if add or drop:
        path = root / PUBLISH_MANIFEST
        path.write_text(rewrite_new_paths(path.read_text(encoding="utf-8"), add, drop), encoding="utf-8")
        after_add, after_drop, _ = publish_manifest_plan(root)
        if after_add or after_drop:
            raise SystemExit(f"derivations: {PUBLISH_MANIFEST}: rewrite left `new_paths` inconsistent "
                             f"(add {after_add[:3]}, drop {after_drop[:3]}); fix it by hand")
        for name in add:
            print(f"derivations: {PUBLISH_MANIFEST}: named {name} (Apache-2.0 header, public matrix rule)",
                  file=sys.stderr)
        for name in drop:
            print(f"derivations: {PUBLISH_MANIFEST}: dropped {name} (no longer a new selected file)",
                  file=sys.stderr)
    if refused:
        print(f"derivations: {PUBLISH_MANIFEST}: {len(refused)} new public file(s) need a decision (above)",
              file=sys.stderr)
        return 1
    return 0


# ---------------------------------------------------------------------------------------------
# repin-frozen: the explicit, per-file re-pin of a contract pack's frozen-hashes.json.

_FRAGMENT_KEYS = {"ci_fragment_sha256": "ci_fragment", "registry_fragment_sha256": "registry_fragment"}


def cmd_repin_frozen(root: Path, manifest_rel: str, paths: list[str]) -> int:
    """Re-pin each named entry, never one that is not named: a frozen hash is a reviewed contract
    promise. Fragment pins hash the lines the pack's verifier selects (its own functions)."""
    manifest_path = (root / manifest_rel).resolve()
    if root.resolve() not in manifest_path.parents or manifest_path.name != "frozen-hashes.json":
        raise SystemExit(f"derivations: repin-frozen: {manifest_rel} is not a frozen-hashes.json in this repository")
    text = manifest_path.read_text(encoding="utf-8")
    manifest = json.loads(text)
    if json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n" != text:
        raise SystemExit(f"derivations: repin-frozen: {manifest_rel} is not in canonical form (indent 2, sorted)")
    verifier = None
    contract = manifest.get("contract")
    if isinstance(contract, str) and re.fullmatch(r"[a-z0-9-]+", contract) and (
            root / f"scripts/verify-{contract}.py").is_file():
        verifier = f"scripts/verify-{contract}.py"
    failed = 0
    for rel in paths:
        source = root / rel
        if not source.is_file():
            print(f"derivations: repin-frozen: {rel} is not a file", file=sys.stderr)
            failed += 1
            continue
        if rel in manifest.get("sha256", {}):
            table, digest = "sha256", hashlib.sha256(source.read_bytes()).hexdigest()
        else:
            table = next((key for key in _FRAGMENT_KEYS if rel in manifest.get(key, {})), None)
            if table is None or verifier is None:
                print(f"derivations: repin-frozen: {rel} is not pinned in {manifest_rel}", file=sys.stderr)
                failed += 1
                continue
            module = _load_script(root, verifier, "frozen_verifier")
            lines = getattr(module, _FRAGMENT_KEYS[table])(source.read_text(encoding="utf-8"))
            digest = module.ci_fragment_digest(lines)
        if manifest[table][rel] != digest:
            manifest[table][rel] = digest
            print(f"derivations: repin-frozen: {table}[{rel}] = {digest}; name the contract review in the commit",
                  file=sys.stderr)
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
                             encoding="utf-8")
    return 1 if failed else 0


def _git(root: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True, text=True).stdout


def _run(command: list[str], root: Path) -> int:
    if shutil.which(command[0]) is None and not Path(command[0]).is_file():
        print(f"derivations: {command[0]} not found", file=sys.stderr)
        return 127
    return subprocess.run(command, cwd=root, stdout=subprocess.DEVNULL).returncode


def _selected(changed: set[str], tracked: list[str]):
    """Registered derived files (in registry order) whose inputs, or which themselves, changed."""
    has_export = PUBLIC_EXPORT in tracked
    for pattern, inputs in REGISTRY:
        for path in (p for p in tracked if fnmatch.fnmatchcase(p, pattern)):
            if path == PUBLISH_MANIFEST and not has_export:
                continue
            directory = path.rsplit("/", 1)[0] + "/" if "/" in path else ""
            globs = [g.replace("{dir}", directory) for g in inputs]
            if path in changed or any(fnmatch.fnmatchcase(c, g) for c in changed for g in globs):
                yield path


def cmd_regenerate(root: Path, since: str, dry_run: bool) -> int:
    changed = set(_git(root, "diff", "--name-only", since).split())
    changed |= set(_git(root, "ls-files", "--others", "--exclude-standard").split())
    tracked = _git(root, "ls-files").split()
    failed = 0
    for path in _selected(changed, tracked):
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


# ---------------------------------------------------------------------------------------------
# check: the guard. A derived file in a change must equal its regeneration (ECCP W5-02).

CURRENT, STALE, UNAVAILABLE = "current", "stale", "unavailable"
_LOCKED_REFUSAL = re.compile(r"--locked|lock file .* needs to be updated", re.I)


def _verdict(path: str, root: Path) -> tuple[str, str]:
    """(CURRENT | STALE | UNAVAILABLE, detail) without writing the file."""
    if path.endswith("Cargo.lock"):
        if shutil.which("cargo") is None:
            return UNAVAILABLE, "cargo not found"
        detail = ""
        for offline in (True, False):
            command = [*_cargo_lock_command(path, offline), "--locked"]
            proc = subprocess.run(command, cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            if proc.returncode == 0:
                return CURRENT, ""
            detail = proc.stderr.strip().splitlines()[-1] if proc.stderr.strip() else f"exit {proc.returncode}"
            if _LOCKED_REFUSAL.search(proc.stderr):
                return STALE, detail
        return UNAVAILABLE, detail
    if path == "SBOM.cdx.json":
        missing = [tool for tool in ("cargo", "cargo-deny", "cargo-cyclonedx") if shutil.which(tool) is None]
        if missing:
            return UNAVAILABLE, " and ".join(missing) + " not found"
        command = ["bash", "scripts/generate-sbom.sh", "--check"]
    elif path == "THIRD_PARTY_NOTICES":
        command = [sys.executable, "scripts/generate-third-party-notices.py", "--root", ".", "--check"]
    elif path == PUBLISH_MANIFEST:
        command = [sys.executable, "scripts/derivations.py", "write-publish-manifest", "--check"]
    elif path == "ee/delivery/FEATURE_SCOPE_REGISTER.csv":
        writer = _scope_register_command(root)
        if writer is None:
            return UNAVAILABLE, "ee/ci/scope-register.yml not found"
        command = [*writer, "--check"]
    else:
        return UNAVAILABLE, "no check registered"
    if not (root / command[1]).is_file():
        return UNAVAILABLE, f"{command[1]} not found"
    proc = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if proc.returncode == 0:
        return CURRENT, ""
    lines = [line for line in proc.stdout.strip().splitlines() if line.strip()]
    if path == PUBLISH_MANIFEST and proc.returncode == EXIT_UNAVAILABLE:
        return UNAVAILABLE, lines[-1] if lines else "baseline commit not available"
    return STALE, lines[-1] if lines else f"exit {proc.returncode}"


def _default_since(root: Path, commit: str) -> str:
    proc = subprocess.run(["git", "-C", str(root), "merge-base", commit, "@{upstream}"], capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("derivations: check: no upstream branch; pass --since REV (e.g. origin/integration/3.12.3)")
    return proc.stdout.strip()


def cmd_check(root: Path, since: str | None, commit: str, in_place: bool, strict: bool, all_files: bool) -> int:
    commit = _git(root, "rev-parse", "--verify", f"{commit}^{{commit}}").strip()
    tracked = _git(root, "ls-tree", "-r", "--name-only", commit).split()
    if all_files:
        changed = set(tracked)
    else:
        since = since or _default_since(root, commit)
        changed = set(_git(root, "diff", "--name-only", "--no-renames", since, commit).split())
    selected = list(_selected(changed, tracked))
    if not selected:
        print("derivations: check: no derived file affected", file=sys.stderr)
        return 0
    if in_place:
        head = _git(root, "rev-parse", "HEAD").strip()
        dirty = _git(root, "status", "--porcelain").strip()  # untracked files can feed a regeneration
        if head != commit or dirty:
            print(f"derivations: check --in-place needs a clean checkout of {commit[:12]}", file=sys.stderr)
            return 2
        return _report(selected, root, strict)
    with tempfile.TemporaryDirectory(prefix="derivations-check-") as tmp:
        tree = Path(tmp) / "tree"
        _git(root, "worktree", "add", "--detach", "--quiet", str(tree), commit)
        try:
            return _report(selected, tree, strict)
        finally:
            subprocess.run(["git", "-C", str(root), "worktree", "remove", "--force", str(tree)], capture_output=True)
            subprocess.run(["git", "-C", str(root), "worktree", "prune"], capture_output=True)


def _report(selected: list[str], root: Path, strict: bool) -> int:
    stale, unavailable = [], []
    for path in selected:
        verdict, detail = _verdict(path, root)
        suffix = f" ({detail})" if detail else ""
        if verdict == STALE:
            stale.append(path)
            print(f"derivations: check: {path} differs from its regeneration{suffix}", file=sys.stderr)
        elif verdict == UNAVAILABLE:
            unavailable.append(path)
            print(f"derivations: check: {path} not checked{suffix}", file=sys.stderr)
        else:
            print(f"derivations: check: {path} current", file=sys.stderr)
    if stale:
        print("derivations: check failed: " + " ".join(stale), file=sys.stderr)
        print("derivations: run `python3 scripts/derivations.py regenerate --since <base>` and commit the result",
              file=sys.stderr)
        return 1
    if unavailable and strict:
        print("derivations: check failed (--strict): not checked: " + " ".join(unavailable), file=sys.stderr)
        return 1
    return 0


def cmd_install(root: Path) -> int:
    for strategy, name in DRIVER_NAMES.items():
        _git(root, "config", f"merge.{name}.name", f"LeanCTX derivations: {strategy}")
        _git(root, "config", f"merge.{name}.driver", DRIVER.format(strategy=strategy))
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
    w = sub.add_parser("write-notices", help="re-pin the notice input digests, then write THIRD_PARTY_NOTICES")
    w.add_argument("--repo", type=Path, default=Path("."))
    p = sub.add_parser("write-publish-manifest",
                       help="name new public Apache files in new_paths of PUBLISH_MANIFEST.toml, refuse the rest")
    p.add_argument("--repo", type=Path, default=Path("."))
    p.add_argument("--check", action="store_true", help="write nothing; exit 1 when new_paths needs a change")
    f = sub.add_parser("repin-frozen", help="re-pin named entries of a frozen-hashes.json after a contract review")
    f.add_argument("--repo", type=Path, default=Path("."))
    f.add_argument("manifest", help="path of the pack's frozen-hashes.json")
    f.add_argument("paths", nargs="+", help="pinned files to re-pin (each one explicitly)")
    c = sub.add_parser("check", help="fail when a derived file differs from its regeneration (W5-02 guard)")
    c.add_argument("--repo", type=Path, default=Path("."))
    c.add_argument("--commit", default="HEAD", help="commit to check (default HEAD)")
    c.add_argument("--since", help="base revision (default: merge base with the upstream branch)")
    c.add_argument("--all", action="store_true", help="check every registered derived file")
    c.add_argument("--in-place", action="store_true", help="check this clean checkout instead of a temporary worktree")
    c.add_argument("--strict", action="store_true", help="a file whose check cannot run (tool missing) fails too")
    args = ap.parse_args()
    if args.cmd == "merge":
        return cmd_merge(args.strategy, args.base, args.ours, args.theirs, args.path)
    root = Path(_git(args.repo, "rev-parse", "--show-toplevel").strip())
    if args.cmd == "install":
        return cmd_install(root)
    if args.cmd == "write-notices":
        return cmd_write_notices(root)
    if args.cmd == "write-publish-manifest":
        return cmd_write_publish_manifest(root, args.check)
    if args.cmd == "repin-frozen":
        return cmd_repin_frozen(root, args.manifest, args.paths)
    if args.cmd == "check":
        return cmd_check(root, args.since, args.commit, args.in_place, args.strict, args.all)
    return cmd_regenerate(root, args.since, args.dry_run)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:  # noqa: BLE001 - a crash must never read as "conflict, markers written"
        traceback.print_exc()
        sys.exit(EXIT_DRIVER_FAILED)
