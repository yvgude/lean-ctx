#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Local-free invariant v2: plan, licence and entitlement checks live only in core/hive.

    python3 scripts/check-local-free.py            # check, exit 1 on a violation
    python3 scripts/check-local-free.py --json     # machine-readable report
    python3 scripts/check-local-free.py --list     # every file with a gating symbol

Every tracked ``rust/src/**/*.rs`` file is scanned for gating symbols
(entitlements, plan or licence checks, the billing capability CLI) in its code;
comments and doc comments are not code and are skipped. A hit is allowed only
under ``rust/src/core/hive/`` or in a file listed in
``product/local-free-allowlist.toml`` with an owner (strand B or C) and a reason.
A listed file that no longer has a hit, or is no longer tracked, is a stale entry
and fails too, so the allowlist only shrinks. Contract:
``docs/contracts/local-free-invariant-v2.md``. Stdlib only, no network.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None

ROOT = Path(__file__).resolve().parents[1]
SCAN_ROOT = "rust/src"
HIVE_ROOT = "rust/src/core/hive/"
ALLOWLIST = "product/local-free-allowlist.toml"
OWNERS = ("B", "C")
# Test-only files carry fixtures of the gate, not the gate itself.
TEST_FILE = re.compile(r"(^|/)(tests?|fixtures)(/|$)|(^|[_/])tests?\.rs$")
GATING = {
    "entitlement": re.compile(r"entitlement", re.I),
    "require_pro": re.compile(r"\brequire_pro\b"),
    "capability_admitted": re.compile(r"\bcapability_admitted\b"),
    "signed_entitlements": re.compile(r"\bsigned_entitlements\b"),
    "billing capability": re.compile(r'billing capability|"billing",\s*"capability"'),
    "LEAN_CTX_PLAN": re.compile(r"\bLEAN_CTX_PLAN\b"),
    "LEAN_CTX_LICENSE": re.compile(r"\bLEAN_CTX_LICENSE\b"),
    "paid plan": re.compile(r"\bPlan::(?:Pro|Team|Enterprise)\b"),
    "minimum_plan": re.compile(r"\bminimum_plan\s*\("),
}


def strip_comments(text: str) -> str:
    """The code of a Rust source: line, doc and (nested) block comments blanked,
    string and char literals kept. Newlines are kept so line numbers hold."""
    out, i, n, depth = [], 0, len(text), 0
    while i < n:
        c = text[i]
        if depth:
            if text.startswith("/*", i):
                depth, i = depth + 1, i + 2
            elif text.startswith("*/", i):
                depth, i = depth - 1, i + 2
            else:
                out.append("\n" if c == "\n" else " ")
                i += 1
            continue
        if text.startswith("//", i):
            end = text.find("\n", i)
            i = n if end < 0 else end
            continue
        if text.startswith("/*", i):
            depth, i = 1, i + 2
            continue
        raw = re.match(r'b?r(#*)"', text[i:i + 260]) if c in "rb" and (i == 0 or not (
            text[i - 1].isalnum() or text[i - 1] == "_")) else None
        if raw:
            close = '"' + raw.group(1)
            end = text.find(close, i + raw.end())
            end = n if end < 0 else end + len(close)
            out.append(text[i:end])
            i = end
            continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append(text[i:j + 1])
            i = j + 1
            continue
        if c == "'":
            char = re.match(r"'(?:\\.[^']{0,8}|[^\\'])'", text[i:i + 12])
            if char:
                out.append(char.group(0))
                i += char.end()
                continue
        out.append(c)
        i += 1
    return "".join(out)


def tracked(root: Path) -> list[str]:
    try:
        result = subprocess.run(["git", "-C", str(root), "ls-files", "-z", "--", SCAN_ROOT],
                                capture_output=True, check=True)
        names = [name for name in result.stdout.decode("utf-8").split("\0") if name]
    except (OSError, subprocess.CalledProcessError):
        base = root / SCAN_ROOT
        names = [path.relative_to(root).as_posix() for path in base.rglob("*.rs")] if base.is_dir() else []
    return sorted(name for name in names if name.endswith(".rs") and not TEST_FILE.search(name)
                  and (root / name).is_file())


def hits(root: Path) -> dict:
    found: dict = {}
    for name in tracked(root):
        code = strip_comments((root / name).read_text(encoding="utf-8", errors="replace"))
        symbols = sorted(label for label, pattern in GATING.items() if pattern.search(code))
        if symbols:
            found[name] = symbols
    return found


def load_allowlist(root: Path, errors: list) -> dict:
    path = root / ALLOWLIST
    if not path.is_file():
        errors.append(f"{ALLOWLIST}: missing")
        return {}
    if tomllib is None:
        raise SystemExit("check-local-free.py needs Python 3.11 or newer (tomllib)")
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    if document.get("schema") != "leanctx.local-free-allowlist/v1":
        errors.append(f"{ALLOWLIST}: schema must be leanctx.local-free-allowlist/v1")
    entries: dict = {}
    for entry in document.get("file", []):
        name = entry.get("path", "")
        if name in entries:
            errors.append(f"{ALLOWLIST}: {name} listed twice")
        if entry.get("owner") not in OWNERS:
            errors.append(f"{ALLOWLIST}: {name} needs owner {' or '.join(OWNERS)}")
        if not str(entry.get("reason", "")).strip():
            errors.append(f"{ALLOWLIST}: {name} needs a reason")
        if name.startswith(HIVE_ROOT):
            errors.append(f"{ALLOWLIST}: {name} is under {HIVE_ROOT}, which needs no entry")
        entries[name] = entry
    return entries


def verify(root: Path) -> dict:
    errors: list = []
    found = hits(root)
    allowed = load_allowlist(root, errors)
    for name, symbols in found.items():
        if not name.startswith(HIVE_ROOT) and name not in allowed:
            errors.append(f"{name}: gating symbol(s) {', '.join(symbols)} outside {HIVE_ROOT} "
                          f"(move the check into core/hive or list the file in {ALLOWLIST})")
    for name in sorted(set(allowed) - set(found)):
        errors.append(f"{ALLOWLIST}: {name} has no gating symbol any more (or is gone); remove the entry")
    return {"schema": "leanctx.local-free-check/v1", "passed": not errors, "errors": errors,
            "hits": found, "allowlisted": len(allowed), "hive_root": HIVE_ROOT}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--list", action="store_true", help="print every file with a gating symbol")
    args = parser.parse_args()
    result = verify(args.root.resolve())
    if args.json:
        print(json.dumps(result, sort_keys=True))
    elif args.list:
        for name, symbols in result["hits"].items():
            print(f"{name}\t{', '.join(symbols)}")
    for error in result["errors"]:
        print(f"local-free: {error}", file=sys.stderr)
    if result["passed"] and not (args.json or args.list):
        print(f"local-free: PASS ({len(result['hits'])} files with gating symbols; "
              f"{result['allowlisted']} allowlisted, rest under {HIVE_ROOT})")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
