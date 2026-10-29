#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build and test a public export offline and record what really happened.

This is the producer of the isolated-build evidence that `publish-pins.py`
pins and `publish-gate.py` requires. It runs a fixed list of steps — the
caller cannot choose or reorder them — as real processes in the export's
`rust/` directory, with cargo's network access switched off and a scrubbed
environment, and writes one evidence file:

  - the tree is checked against the pinned export receipt before the first
    step and again after the last: a build that changes its sources fails;
  - every step is recorded with its command, exit code, duration and the
    SHA-256 of its full log; the first failing step ends the run;
  - the toolchain is recorded as it identifies itself, with the digest of the
    cargo executable that was run;
  - the built binary must report the version the tree declares.

`result` is PASS only if every step exited 0 and the tree is unchanged. A FAIL
is written too, so a failed run leaves evidence instead of nothing.

Offline here means cargo is told not to use the network (`--offline`,
`--locked`, `CARGO_NET_OFFLINE`); dependencies must already be in the cargo
home. It is not an operating-system network jail, and the evidence says so.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import tomllib

SCRIPTS = Path(__file__).resolve().parent
SCHEMA = "leanctx.export-build-evidence/v1"
BINARY = "lean-ctx"
# (id, cargo arguments, timeout in seconds). Fixed: evidence from another list is not R05.
STEPS = (
    ("resolve-offline", ["metadata", "--offline", "--locked", "--format-version", "1", "--quiet"], 600),
    ("build-release", ["build", "--offline", "--locked", "--release", "--bin", BINARY], 7200),
    ("test-lib", ["test", "--offline", "--locked", "--lib"], 7200),
)
PASSED_ENVIRONMENT = ("PATH", "HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR", "SYSTEMROOT")


class Refused(Exception):
    pass


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def load_export_tool(tools_dir: Path):
    spec = importlib.util.spec_from_file_location("public_export", tools_dir / "public-export.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def environment(args: argparse.Namespace) -> dict[str, str]:
    env = {name: os.environ[name] for name in PASSED_ENVIRONMENT if name in os.environ}
    env.update({"CARGO_HOME": str(args.cargo_home), "CARGO_TARGET_DIR": str(args.target_dir),
                "CARGO_NET_OFFLINE": "true", "CARGO_TERM_COLOR": "never"})
    return env


def identify(command: list[str], env: dict[str, str]) -> str:
    done = subprocess.run(command, capture_output=True, text=True, env=env, check=False, timeout=60)
    if done.returncode != 0:
        raise Refused(f"toolchain does not identify itself: {' '.join(command[:1])}")
    return done.stdout.strip()


def run_step(identity: str, command: list[str], cwd: Path, env: dict[str, str], timeout: int,
             log: Path) -> dict:
    started = time.monotonic()
    with log.open("wb") as handle:
        try:
            code = subprocess.run(command, cwd=cwd, env=env, stdout=handle,
                                  stderr=subprocess.STDOUT, check=False, timeout=timeout).returncode
        except subprocess.TimeoutExpired:
            code = None
    return {"id": identity, "command": command, "exit_code": code,
            "timed_out": code is None, "seconds": round(time.monotonic() - started, 1),
            "log": log.name, "log_sha256": sha256_file(log), "log_bytes": log.stat().st_size}


def produce(args: argparse.Namespace) -> dict:
    tree = args.tree.resolve()
    for name in ("target_dir", "log_dir"):
        inside = getattr(args, name).resolve()
        if inside == tree or tree in inside.parents:
            raise Refused(f"--{name.replace('_', '-')} must be outside the export tree")
    # Not resolved: a toolchain manager's `cargo` is a link to its launcher and
    # must be started under the name `cargo`.
    cargo = args.cargo.absolute()
    if not cargo.is_file() or not (cargo.parent / "rustc").is_file():
        raise Refused("cargo executable (with rustc beside it) not found")
    export_tool = load_export_tool(args.tools_dir)
    receipt = json.loads(args.receipt.read_text(encoding="utf-8"))

    def tree_is_the_receipt() -> str | None:
        try:
            export_tool.verify(tree, args.manifest, receipt,
                               pinned_receipt_sha256=args.receipt_sha256)
            return None
        except export_tool.ExportError as error:
            return str(error)

    if problem := tree_is_the_receipt():
        raise Refused(f"the tree is not the pinned export: {problem}")
    declared = tomllib.loads((tree / "rust/Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    env = environment(args)
    args.log_dir.mkdir(parents=True, exist_ok=True)
    args.target_dir.mkdir(parents=True, exist_ok=True)
    evidence = {
        "schema": SCHEMA,
        "source_commit": receipt["source_commit"],
        "receipt_sha256": args.receipt_sha256,
        "tree_sha256": receipt["tree_sha256"],
        "producer_sha256": sha256_file(Path(__file__)),
        "engine_version": declared,
        # rustc beside the cargo that runs: the pair a toolchain installs together.
        "toolchain": {"cargo": identify([str(cargo), "--version"], env),
                      "rustc": identify([str(cargo.parent / "rustc"), "--version", "--verbose"], env),
                      "cargo_path": str(cargo), "cargo_sha256": sha256_file(cargo)},
        "isolation": "cargo offline and locked; scrubbed environment; no OS network jail",
        "environment": sorted(env),
        "steps": [],
    }
    for identity, arguments, timeout in STEPS:
        step = run_step(identity, [str(cargo), *arguments], tree / "rust", env, timeout,
                        args.log_dir / f"{identity}.log")
        evidence["steps"].append(step)
        if step["exit_code"] != 0:
            break
    built = args.target_dir / "release" / (BINARY + (".exe" if os.name == "nt" else ""))
    binary = {"path": f"release/{built.name}", "sha256": None, "reported_version": None}
    if all(step["exit_code"] == 0 for step in evidence["steps"]) and len(evidence["steps"]) == len(STEPS):
        if built.is_file():
            binary["sha256"] = sha256_file(built)
            done = subprocess.run([str(built), "--version"], capture_output=True, text=True, env=env,
                                  check=False, timeout=60)
            binary["reported_version"] = done.stdout.strip()[:200] if done.returncode == 0 else None
    evidence["binary"] = binary
    evidence["tree_unchanged_after_build"] = tree_is_the_receipt() is None
    reported = (binary["reported_version"] or "").split()
    evidence["result"] = "PASS" if (
        len(evidence["steps"]) == len(STEPS)
        and all(step["exit_code"] == 0 for step in evidence["steps"])
        and evidence["tree_unchanged_after_build"]
        and binary["sha256"] is not None
        and declared in reported
    ) else "FAIL"
    return evidence


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--receipt-sha256", required=True, help="the pinned export receipt digest")
    parser.add_argument("--tools-dir", type=Path, default=SCRIPTS)
    parser.add_argument("--cargo", type=Path, required=True, help="the cargo executable to run")
    parser.add_argument("--cargo-home", type=Path, required=True,
                        help="cargo home holding the already fetched dependencies")
    parser.add_argument("--target-dir", type=Path, required=True, help="build output, outside the tree")
    parser.add_argument("--log-dir", type=Path, required=True, help="step logs, outside the tree")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        evidence = produce(args)
    except (Refused, OSError, ValueError, KeyError, tomllib.TOMLDecodeError,
            subprocess.TimeoutExpired) as error:
        print(f"export-offline-build: refused: {error}", file=sys.stderr)
        return 2
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    for step in evidence["steps"]:
        print(f"export-offline-build: {step['id']}: exit {step['exit_code']} in {step['seconds']} s")
    print(f"export-offline-build: {evidence['result']}")
    return 0 if evidence["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
