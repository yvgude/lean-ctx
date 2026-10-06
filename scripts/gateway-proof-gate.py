#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Gateway proof gate: the 60 acceptance scenarios and 26 bypass paths.

docs/contracts/gateway-proof-v1/registry.json maps every scenario and path to
the Engine tests that prove it, to external evidence, or marks it open. This
gate keeps that map honest:

  check    every scenario 1..60 and every bypass path appears exactly once;
           every `proven` entry names tests, and every named test exists
  run      additionally runs exactly the named tests and requires them green
  release  additionally fails while any entry is `open`

Usage: python3 scripts/gateway-proof-gate.py [--run] [--release]
Run from the repository root; cargo is invoked in rust/.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "docs/contracts/gateway-proof-v1/registry.json"
BYPASS_PATHS = [
    "ctx_call", "ctx_read full", "ctx_read raw", "fresh reads", "line reads",
    "alternate read modes", "ctx_expand", "ctx_retrieve", "archive", "CCR", "tee",
    "session cache", "memory", "knowledge", "search", "graph", "provider cache",
    "MCP bridge", "nested dispatch", "cross-agent handoff", "snapshot restore",
    "proxy in-band expansion", "OpenAI rail", "Anthropic rail", "Gemini rail",
    "WebSocket path",
]
STATUSES = {"proven", "external", "open"}
TARGETS = {"lib": ["--lib"], "main": ["--test", "main"]}


def fail(message):
    print(f"gateway-proof: {message}", file=sys.stderr)
    sys.exit(1)


def validate(registry):
    acceptance = registry.get("acceptance", [])
    ids = [entry.get("id") for entry in acceptance]
    if sorted(ids) != list(range(1, 61)):
        fail("acceptance must list scenarios 1..60 exactly once")
    paths = [entry.get("id") for entry in registry.get("bypass", [])]
    if sorted(paths) != sorted(BYPASS_PATHS):
        fail("bypass must list the 26 bypass paths exactly once")
    tests = {}
    for entry in [*acceptance, *registry["bypass"]]:
        status = entry.get("status")
        if status not in STATUSES:
            fail(f"{entry['id']}: unknown status {status!r}")
        named = entry.get("tests", [])
        if status == "proven" and not named:
            fail(f"{entry['id']}: proven without tests")
        if status == "external" and not entry.get("evidence"):
            fail(f"{entry['id']}: external without evidence")
        if status == "open" and not entry.get("note"):
            fail(f"{entry['id']}: open without a note saying what is missing")
        for test in named:
            target, _, name = test.partition(":")
            if target not in TARGETS or not name:
                fail(f"{entry['id']}: test {test!r} must be lib:<path> or main:<path>")
            tests.setdefault(target, set()).add(name)
    return tests


def listed(target):
    result = subprocess.run(
        ["cargo", "test", *TARGETS[target], "--", "--list"],
        cwd=ROOT / "rust", capture_output=True, text=True, check=False,
    )
    if result.returncode != 0:
        fail(f"cargo test {target} --list failed:\n{result.stderr[-2000:]}")
    return {line[: -len(": test")] for line in result.stdout.splitlines() if line.endswith(": test")}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run", action="store_true", help="run the named tests")
    parser.add_argument("--release", action="store_true", help="fail while any entry is open")
    args = parser.parse_args()
    registry = json.loads(REGISTRY.read_text())
    tests = validate(registry)
    for target, names in tests.items():
        missing = sorted(names - listed(target))
        if missing:
            fail(f"{len(missing)} named {target} test(s) do not exist: {missing}")
    if args.run:
        for target, names in tests.items():
            result = subprocess.run(
                ["cargo", "test", *TARGETS[target], "--", "--exact", *sorted(names)],
                cwd=ROOT / "rust", check=False,
            )
            if result.returncode != 0:
                fail(f"named {target} tests failed")
    entries = [*registry["acceptance"], *registry["bypass"]]
    counts = {status: sum(entry["status"] == status for entry in entries) for status in sorted(STATUSES)}
    open_entries = [str(entry["id"]) for entry in entries if entry["status"] == "open"]
    print(f"gateway-proof: {counts} ({sum(len(n) for n in tests.values())} named tests)")
    if open_entries:
        print("gateway-proof: open: " + ", ".join(open_entries))
    if args.release and open_entries:
        fail("release requires every scenario and bypass path proven or externally evidenced")


if __name__ == "__main__":
    main()
