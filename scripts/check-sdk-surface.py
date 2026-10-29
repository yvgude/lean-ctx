#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Guard one canonical SDK source tree and reject competing package identities.

The consolidated checkout owns sdk/. The Apache-only export can omit that
separately licensed tree; neither form may introduce a second SDK package.
This checks source identity, not registry publication or usage rights.
"""

from __future__ import annotations

import configparser
import json
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Only the exact canonical manifest may claim a current SDK identity.
RESERVED_SDK_NAMES = {
    "thinkery-leanctx-sdk",
    "thinkery_leanctx_sdk",
    "lean-ctx-sdk",
    "lean_ctx_sdk",
    "leanctx-sdk",
    "leanctx_sdk",
    "lean-ctx-python",
    "@thinkery/leanctx-sdk",
}

CANONICAL_MANIFESTS = {
    "sdk/setup.cfg": "thinkery-leanctx-sdk",
    "sdk/packages/typescript/package.json": "@thinkery/leanctx-sdk",
    "sdk/packages/rust/Cargo.toml": "thinkery-leanctx-sdk",
}

SURFACE_DOC = pathlib.Path("docs/reference/sdk-surface.md")
SDK_PACKAGE = "thinkery-leanctx-sdk"

# Vendored, archived and example trees are not product surface.
SKIP_PARTS = {
    "node_modules",
    "target",
    "vendor",
    "_archive",
    ".git",
    ".worktrees",
    "dist",
    "build",
}

FAILURES: list[str] = []


def skipped(path: pathlib.Path) -> bool:
    return any(part in SKIP_PARTS for part in path.parts)


def manifests(filename: str):
    for directory, dirs, files in os.walk(ROOT, followlinks=False):
        dirs[:] = [name for name in dirs if name not in SKIP_PARTS
                   and not (pathlib.Path(directory) / name).is_symlink()]
        if filename in files:
            yield pathlib.Path(directory) / filename


def check_identity(path: pathlib.Path, name: str | None) -> None:
    relative = path.relative_to(ROOT).as_posix()
    expected = CANONICAL_MANIFESTS.get(relative)
    if expected is not None:
        if name != expected:
            FAILURES.append(f"{relative}: expected canonical SDK identity {expected!r}, got {name!r}")
    elif name in RESERVED_SDK_NAMES:
        FAILURES.append(f"{relative}: duplicate or retired SDK identity {name!r}; use sdk/.")


def check_npm_manifests() -> None:
    for path in manifests("package.json"):
        if skipped(path.relative_to(ROOT)):
            continue
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            FAILURES.append(f"{path.relative_to(ROOT)}: invalid package manifest: {error}")
            continue
        name = data.get("name")
        check_identity(path, name)


def check_python_manifests() -> None:
    for path in manifests("pyproject.toml"):
        if skipped(path.relative_to(ROOT)):
            continue
        match = re.search(
            r'^name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"), re.MULTILINE
        )
        check_identity(path, match.group(1) if match else None)
    for path in manifests("setup.cfg"):
        config = configparser.ConfigParser(interpolation=None)
        config.read(path, encoding="utf-8")
        check_identity(path, config.get("metadata", "name", fallback=None))


def check_cargo_manifests() -> None:
    for path in manifests("Cargo.toml"):
        if skipped(path.relative_to(ROOT)):
            continue
        match = re.search(
            r'^name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"), re.MULTILINE
        )
        check_identity(path, match.group(1) if match else None)


def check_surface_doc() -> None:
    doc = ROOT / SURFACE_DOC
    if not doc.is_file():
        FAILURES.append(
            f"{SURFACE_DOC} is missing — the single-SDK answer needs one written home."
        )
        return
    text = doc.read_text(encoding="utf-8")
    if SDK_PACKAGE not in text:
        FAILURES.append(f"{SURFACE_DOC} does not name {SDK_PACKAGE}.")
    if (ROOT / "sdk").exists():
        if "sdk/" not in text:
            FAILURES.append(f"{SURFACE_DOC} does not name the canonical sdk/ source tree.")
        for relative in CANONICAL_MANIFESTS:
            if not (ROOT / relative).is_file():
                FAILURES.append(f"canonical SDK manifest is missing: {relative}")


def main() -> int:
    FAILURES.clear()
    check_npm_manifests()
    check_python_manifests()
    check_cargo_manifests()
    check_surface_doc()

    if FAILURES:
        print("SDK surface guard failed:", file=sys.stderr)
        for failure in FAILURES:
            print(f"  - {failure}", file=sys.stderr)
        print(f"\nSee {SURFACE_DOC} for what each published artifact is.", file=sys.stderr)
        return 1

    location = "sdk/" if (ROOT / "sdk").exists() else "separate licensed source export"
    print(f"OK: one SDK ({SDK_PACKAGE}, {location}); no duplicate SDK identity")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
