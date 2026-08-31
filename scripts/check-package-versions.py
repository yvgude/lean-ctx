#!/usr/bin/env python3
"""Engine <-> npm package version-coupling gate.

Every npm package that *ships the engine* (as opposed to an independently
versioned SDK, or the decoupled editor extension) MUST carry the exact engine
version. Otherwise a release silently skips publishing the stale package (npm
"already exists" -> continue-on-error) and `npx`/Pi users quietly fall a version
behind — the exact drift GH issue reports about the Pi extension came from.

This gate is the *enforced* counterpart of the DEPLOY_CHECKLIST "Version bumped
in ALL locations" step: it fails on every push/PR (CI) and blocks the release
build, so the engine and its wrapper packages can never diverge.

Source of truth:
  * engine version: rust/Cargo.toml [package] version (first `^version =`)

Coupled packages (must equal the engine version):
  * packages/pi-lean-ctx/package.json   — Pi Coding Agent extension
  * packages/lean-ctx-bin/package.json  — npx/npm binary wrapper

Deliberately excluded:
  * vscode-extension          — decoupled cadence (vscode-v* tags, own workflow)
  * the cookbook example client — not a product surface (private: true)

No third-party dependencies — standard library only.
"""

from __future__ import annotations

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# npm packages whose version must match the engine, relative to the repo root.
COUPLED_PACKAGES = [
    "packages/pi-lean-ctx/package.json",
    "packages/lean-ctx-bin/package.json",
]

FAILURES: list[str] = []


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def engine_version() -> str:
    """The [package] version from the workspace root Cargo.toml (first `^version =`)."""
    m = re.search(r'^version\s*=\s*"([^"]+)"', read("rust/Cargo.toml"), re.M)
    if not m:
        sys.exit("FATAL: [package] version not found in rust/Cargo.toml")
    return m.group(1)


def package_version(rel: str) -> str:
    return json.loads(read(rel))["version"]


def main() -> int:
    engine = engine_version()
    print(f"engine version (rust/Cargo.toml): {engine}")

    for rel in COUPLED_PACKAGES:
        try:
            got = package_version(rel)
        except (OSError, KeyError, json.JSONDecodeError) as e:
            FAILURES.append(f"{rel}: could not read version ({e})")
            continue
        status = "ok" if got == engine else "DRIFT"
        print(f"  {rel:<38} {got:<12} {status}")
        if got != engine:
            FAILURES.append(
                f"{rel} is {got} but the engine ships {engine} — bump it to {engine}"
            )

    for f in FAILURES:
        print(f"::error title=Package version drift::{f}")

    if FAILURES:
        print(
            f"\n{len(FAILURES)} package(s) out of sync with engine {engine}. "
            "Bump every coupled package.json to the engine version before releasing."
        )
        return 1

    print(f"\nOK: all {len(COUPLED_PACKAGES)} coupled packages match engine {engine}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
