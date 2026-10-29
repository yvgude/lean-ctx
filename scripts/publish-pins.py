#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Derive the release pins that `publish-gate.py` checks every publisher against.

A sibling of `pinned-evidence.py` with the same rule: it decides nothing a
reviewer owns. It only records, for one release candidate, the digests of the
inputs it is handed — export receipt, publish manifest, gate tools, the Engine
version the tree builds, the SDK contract, the isolated-build evidence, a
content review if one is used, and the artifacts. It refuses when an input is
missing or does not belong to this candidate; it never fills a gap.

The file it writes is not yet trusted. It becomes the release authority's
statement when its SHA-256 (printed here) is set in the protected release
environment by the person who approves the release; the publishers then pass
that value to `publish-gate.py --pins-sha256`.

Without `--write` nothing is written: exit 0 when the file on disk equals what
is derived, 1 when it differs or is absent, 2 when an input is refused.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tomllib

SCRIPTS = Path(__file__).resolve().parent
PINS_SCHEMA = "leanctx.publish-pins/v1"
BUILD_EVIDENCE_SCHEMA = "leanctx.export-build-evidence/v1"
GATE_TOOLS = ("public-export.py", "export-secret-gate.py", "check-source-layout.py",
              "ip-provenance-audit.py", "export-offline-build.py")


class Refused(Exception):
    pass


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_export_tool(tools_dir: Path):
    spec = importlib.util.spec_from_file_location("public_export", tools_dir / "public-export.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def derive(args: argparse.Namespace) -> dict:
    tools = {}
    for name in GATE_TOOLS:
        path = args.tools_dir / name
        if not path.is_file() or path.is_symlink():
            raise Refused(f"gate tool is missing: {name}")
        tools[name] = sha256_file(path)
    receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
    if receipt.get("manifest_sha256") != sha256_file(args.manifest):
        raise Refused("the receipt was not produced with this publish manifest")
    if receipt.get("tool_sha256") != tools["public-export.py"]:
        raise Refused("the receipt was not produced by this export tool")
    export_tool = load_export_tool(args.tools_dir)
    receipt_digest = export_tool.receipt_sha256(receipt)
    try:
        # The candidate must really be the receipt's tree before anything is pinned to it.
        export_tool.verify(args.tree, args.manifest, receipt, pinned_receipt_sha256=receipt_digest)
    except export_tool.ExportError as error:
        raise Refused(f"the tree is not the receipt's export: {error}") from error
    engine = tomllib.loads((args.tree / "rust/Cargo.toml").read_text(encoding="utf-8"))
    engine_version = engine["package"]["version"]

    if args.no_sdk == (args.sdk_contract is not None):
        raise Refused("say exactly one of --sdk-contract FILE or --no-sdk")
    sdk = None if args.no_sdk else {"contract_sha256": sha256_file(args.sdk_contract)}

    if args.build_evidence is None:
        raise Refused("no isolated offline build evidence: nothing to pin for R05")
    build = json.loads(args.build_evidence.read_text(encoding="utf-8"))
    if (build.get("schema") != BUILD_EVIDENCE_SCHEMA or build.get("result") != "PASS"
            or build.get("tree_sha256") != receipt["tree_sha256"]
            or build.get("receipt_sha256") != receipt_digest
            or build.get("producer_sha256") != tools["export-offline-build.py"]
            or build.get("engine_version") != engine_version):
        raise Refused("build evidence is not a passed isolated build of this tree by this producer")

    if args.no_content_review == (args.content_review is not None):
        raise Refused("say exactly one of --content-review FILE or --no-content-review")
    review = None if args.no_content_review else sha256_file(args.content_review)

    artifacts = {}
    for item in args.artifact:
        name, separator, path = item.partition("=")
        if not separator or not name or name in artifacts:
            raise Refused(f"artifact must be NAME=PATH, each name once: {item!r}")
        artifacts[name] = sha256_file(Path(path))
    if not artifacts and not args.no_artifacts:
        raise Refused("no artifact named; say --no-artifacts if this release ships none")

    return {
        "schema": PINS_SCHEMA,
        "source_commit": receipt["source_commit"],
        "manifest_sha256": receipt["manifest_sha256"],
        "receipt_sha256": receipt_digest,
        "tools": tools,
        "engine_version": engine_version,
        "sdk": sdk,
        "evidence": {"R05-offline-build": sha256_file(args.build_evidence)},
        "reviews": {"R04-content-review": review},
        "artifacts": artifacts,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--tools-dir", type=Path, default=SCRIPTS)
    parser.add_argument("--sdk-contract", type=Path)
    parser.add_argument("--no-sdk", action="store_true")
    parser.add_argument("--build-evidence", type=Path)
    parser.add_argument("--content-review", type=Path)
    parser.add_argument("--no-content-review", action="store_true")
    parser.add_argument("--artifact", action="append", default=[], metavar="NAME=PATH")
    parser.add_argument("--no-artifacts", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    try:
        pins = derive(args)
    except (Refused, OSError, ValueError, KeyError, tomllib.TOMLDecodeError) as error:
        print(f"publish-pins: refused: {error}", file=sys.stderr)
        return 2
    rendered = (json.dumps(pins, indent=1, sort_keys=True) + "\n").encode("utf-8")
    digest = hashlib.sha256(rendered).hexdigest()
    current = args.output.read_bytes() if args.output.is_file() else None
    if args.write and current != rendered:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(rendered)
        print(f"WROTE {args.output}")
    elif current != rendered:
        print(f"DIFFERS {args.output}")
        return 1
    print(f"publish-pins: sha256 {digest} — to be set by the release authority, not by this tool")
    return 0


if __name__ == "__main__":
    sys.exit(main())
