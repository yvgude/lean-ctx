#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check the Engine and colocated SDK contract before compiling release artifacts.

The monorepo release tests the SDK from the same checkout as the Engine. This
gate never downloads another repository or treats unavailable evidence as a pass.
The Apache host export has neither `sdk/` nor `ee/`: it is reported as an
Engine-only checkout, whose release check is the Engine's own Agent Tools gate.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
SDK_CONSTANT_NAME = "SUPPORTED_AGENT_TOOLS_ENGINE_VERSION"
SDK_CONSTANT_PATH = "sdk/src/leanctx_sdk/agent.py"
ENGINE_GATE = "scripts/verify-engine-agent-tools.py"
# The matrix of (interface, schema, transport, engine) pairs the SDK accepts a
# hello for is validated by ONE module. It lies next to this gate and, byte
# for byte, next to the SDK's own checks; this gate uses its own copy (it also
# runs where no SDK exists) and refuses a checkout whose SDK copy differs.
SDK_VALIDATOR = "agent_tools_compatibility.py"
SDK_VALIDATOR_COPY = "sdk/scripts/agent_tools_compatibility.py"


def _compatibility():
    path = Path(__file__).with_name(SDK_VALIDATOR)
    spec = importlib.util.spec_from_file_location("agent_tools_compatibility", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, path.read_bytes()


def verify(root: Path) -> dict:
    manifest = tomllib.loads((root / "rust/Cargo.toml").read_text(encoding="utf-8"))
    engine = manifest["package"]["version"]
    workflow = (root / ".github/workflows/release.yml").read_text(encoding="utf-8")
    if ENGINE_GATE not in workflow:
        raise ValueError("release workflow must run the Engine's own Agent Tools gate")
    # The workflow is the same file in every checkout, so its rules hold in
    # every checkout: no external SDK repository, the colocated SDK check named.
    if "repository: Thinkery-AG/leanctx-sdk" in workflow or "sdk-agent-tools" in workflow:
        raise ValueError("release workflow still uses an external SDK checkout")
    if "PYTHONPATH=sdk/src" not in workflow or "sdk/scripts/verify_agent_context_e2e.py" not in workflow:
        raise ValueError("release workflow must verify the SDK in the same checkout")
    # The Apache host export withholds `sdk/` together with `ee/`. There is no
    # SDK to couple there; the Engine's own gate above is its release check.
    # A checkout that carries commercial sources without the SDK is incomplete,
    # not Engine-only, and stays a failure (the read below raises).
    if not (root / "sdk").exists() and not (root / "ee").exists():
        return {
            "schema_version": "leanctx.sdk-engine-coupling/v2",
            "engine_version": engine,
            "sdk_source": None,
            "same_checkout": False,
            "coupled": False,
            "engine_only": True,
        }
    source = (root / SDK_CONSTANT_PATH).read_bytes()
    values = []
    for node in ast.parse(source).body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == SDK_CONSTANT_NAME
            for target in node.targets
        ):
            if not isinstance(node.value, ast.Constant) or not isinstance(node.value.value, str):
                raise ValueError("SDK Engine version must be a literal string")
            values.append(node.value.value)
    if len(values) != 1:
        raise ValueError("SDK must declare exactly one Engine version constant")
    if values[0] != engine:
        raise ValueError(f"SDK requires Engine {values[0]}, but the checkout builds {engine}")
    # The constant is the pair kept from before the matrix; what a hello is
    # accepted for is the matrix. The whole pair this checkout builds — the
    # interface, schema and transport the contract speaks with this Engine
    # version — has to be a row of a valid matrix that all six packages
    # mirror. An Engine version that merely appears in some row is not that.
    compatibility, validator = _compatibility()
    if (root / SDK_VALIDATOR_COPY).read_bytes() != validator:
        raise ValueError("SDK compatibility validator differs from the release gate's copy")
    checked = compatibility.check(root / "sdk", engine_version=engine)
    matrix = root / "sdk" / compatibility.MATRIX
    return {
        "sdk_matrix": f"sdk/{compatibility.MATRIX}",
        "sdk_matrix_sha256": hashlib.sha256(matrix.read_bytes()).hexdigest(),
        "sdk_validator_sha256": hashlib.sha256(validator).hexdigest(),
        "sdk_admitted_pairs": checked["pairs"],
        "sdk_built_pair": checked["built_pair"],
        "schema_version": "leanctx.sdk-engine-coupling/v2",
        "engine_version": engine,
        "sdk_source": SDK_CONSTANT_PATH,
        "sdk_source_sha256": hashlib.sha256(source).hexdigest(),
        "sdk_supported_engine_version": values[0],
        "same_checkout": True,
        "coupled": True,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        report = verify(ROOT)
    except (OSError, ValueError, SyntaxError, KeyError, TypeError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(report, sort_keys=True))
    elif report.get("engine_only"):
        print(f"OK: Engine-only checkout {report['engine_version']} (no SDK, no commercial sources); "
              "the Engine's own Agent Tools gate is its release check")
    else:
        print(f"OK: colocated SDK and Engine agree on {report['engine_version']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
