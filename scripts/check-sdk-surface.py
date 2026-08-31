#!/usr/bin/env python3
"""Guard the single-SDK surface.

There is exactly one LeanCTX SDK: `thinkery-leanctx-sdk`, developed in
Thinkery-AG/leanctx-sdk and published to PyPI. It is a clean-room
implementation that consumes the engine only through its public CLI wire
boundary, so it deliberately does NOT live in this repository.

Everything this repository publishes is something else — the engine, the
extension contracts, thin HTTP clients, install wrappers — and this gate exists
so none of it can drift back into calling itself an SDK. That drift is what
produced the state this guard replaces: a second Python SDK on PyPI
(`lean-ctx-python`), a Node package holding the name `lean-ctx-sdk`, and a
release gate that asserted the in-repo package was "the one canonical Python
SDK v1 surface".

Two checks, both cheap and both about names rather than behaviour:

1. No packaging manifest in this repository may declare one of the reserved SDK
   names, or describe itself as *the* SDK.
2. `docs/reference/sdk-surface.md` must exist and must name the external SDK, so
   the answer to "which SDK do I use?" has exactly one written home.

No third-party dependencies — standard library only.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Names that belong to the one SDK, or that would re-create the ambiguity this
# guard exists to prevent. A manifest in THIS repository may not claim them.
RESERVED_SDK_NAMES = {
    "thinkery-leanctx-sdk",
    "thinkery_leanctx_sdk",
    "lean-ctx-sdk",
    "lean_ctx_sdk",
    "leanctx-sdk",
    "leanctx_sdk",
    "lean-ctx-python",
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


def check_npm_manifests() -> None:
    for path in ROOT.rglob("package.json"):
        if skipped(path.relative_to(ROOT)):
            continue
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        name = data.get("name")
        if name in RESERVED_SDK_NAMES:
            FAILURES.append(
                f"{path.relative_to(ROOT)} declares name {name!r}, which is reserved "
                f"for the one SDK ({SDK_PACKAGE}). Rename it after what it actually is."
            )


def check_python_manifests() -> None:
    for path in ROOT.rglob("pyproject.toml"):
        if skipped(path.relative_to(ROOT)):
            continue
        match = re.search(
            r'^name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"), re.MULTILINE
        )
        if match and match.group(1) in RESERVED_SDK_NAMES:
            FAILURES.append(
                f"{path.relative_to(ROOT)} declares name {match.group(1)!r}, which is "
                f"reserved for the one SDK ({SDK_PACKAGE})."
            )


def check_cargo_manifests() -> None:
    for path in ROOT.rglob("Cargo.toml"):
        if skipped(path.relative_to(ROOT)):
            continue
        match = re.search(
            r'^name\s*=\s*"([^"]+)"', path.read_text(encoding="utf-8"), re.MULTILINE
        )
        if match and match.group(1) in RESERVED_SDK_NAMES:
            FAILURES.append(
                f"{path.relative_to(ROOT)} declares name {match.group(1)!r}, which is "
                f"reserved for the one SDK ({SDK_PACKAGE}). The in-process facade is "
                f"`lean-ctx-embed`."
            )


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


def main() -> int:
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

    print(f"OK: one SDK ({SDK_PACKAGE}, external); no in-repo package claims the name")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
