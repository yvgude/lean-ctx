#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""One release decision for every publisher of a public export.

Each publisher (binaries, crates, npm, language SDKs) asks the same question
before it ships: is this tree the reviewed export, and did the gates pass on
exactly this tree? This tool answers it from one trusted input, the pins
file, which the release authority supplies from outside the tree:

  - the source commit, publish manifest, export receipt and every gate tool
    are accepted only at their pinned SHA-256;
  - the tree must be exactly the pinned receipt's tree;
  - the Engine version in the tree, and the SDK contract when an SDK ships,
    must be the pinned pair;
  - the local gates are *run again here on this tree* — their earlier reports
    are not read, so an old, differently scoped or hand-written report cannot
    stand in; only exit 0 counts (REVIEW_REQUIRED is not a pass);
  - evidence that cannot be reproduced here (the isolated offline build) is
    accepted only as the exact pinned file, and only if it names this tree;
  - every artifact must match its pin, none missing, none extra.

Any failed, missing or unpinned item refuses the release. The tool publishes
nothing, signs nothing and changes no pin.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

PINS_SCHEMA = "leanctx.publish-pins/v1"
REPORT_SCHEMA = "leanctx.publish-gate/v1"
BUILD_EVIDENCE_SCHEMA = "leanctx.export-build-evidence/v1"
SHA256 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
EXPORT_TOOL = "public-export.py"
BUILD_PRODUCER = "export-offline-build.py"
# Gates run here on the tree: (id, tool, arguments after the tree is known).
def _secret_gate_arguments(tree: Path, out: Path, pins: dict, args: argparse.Namespace) -> list[str]:
    arguments = ["--tree", str(tree), "--output", str(out / "r04.json")]
    if pins["reviews"]["R04-content-review"] is not None:
        if args.content_review is None:
            raise Refusal("the pins name a content review; none was supplied")
        arguments += ["--content-review", str(args.content_review), "--content-review-sha256",
                      pins["reviews"]["R04-content-review"], "--source-commit", pins["source_commit"]]
    elif args.content_review is not None:
        raise Refusal("a content review was supplied but the pins name none")
    return arguments


LOCAL_GATES = (
    ("R04-secrets-tree", "export-secret-gate.py", _secret_gate_arguments),
    ("R02-community-closure", "check-source-layout.py",
     lambda tree, out, pins, args: ["--root", str(tree), "--profile", "community"]),
    ("R03-provenance", "ip-provenance-audit.py",
     lambda tree, out, pins, args: ["--root", str(tree), "--export", "--release-strict",
                                    "--output", str(out / "r03.json")]),
)
GATE_TIMEOUT_SECONDS = 1800


class Refusal(Exception):
    pass


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_pins(path: Path) -> dict:
    pins = json.loads(path.read_text(encoding="utf-8"))
    expected = {"schema", "source_commit", "manifest_sha256", "receipt_sha256", "tools",
                "engine_version", "sdk", "evidence", "reviews", "artifacts"}
    if not isinstance(pins, dict) or set(pins) != expected or pins["schema"] != PINS_SCHEMA:
        raise Refusal("pins: unknown shape or schema")
    if not COMMIT.fullmatch(str(pins["source_commit"])):
        raise Refusal("pins: source_commit must be a full commit id")
    digests = [pins["manifest_sha256"], pins["receipt_sha256"]]
    for name in ("tools", "evidence", "artifacts"):
        if not isinstance(pins[name], dict):
            raise Refusal(f"pins: {name} must be a table of name → sha256")
        digests += list(pins[name].values())
    if not all(isinstance(d, str) and SHA256.fullmatch(d) for d in digests):
        raise Refusal("pins: every digest must be a lowercase SHA-256")
    required_tools = {EXPORT_TOOL, BUILD_PRODUCER} | {tool for _, tool, _ in LOCAL_GATES}
    if set(pins["tools"]) != required_tools:
        raise Refusal(f"pins: tools must be exactly {sorted(required_tools)}")
    if set(pins["evidence"]) != {"R05-offline-build"}:
        raise Refusal("pins: evidence must pin exactly R05-offline-build")
    reviews = pins["reviews"]
    if (not isinstance(reviews, dict) or set(reviews) != {"R04-content-review"}
            or not (reviews["R04-content-review"] is None
                    or SHA256.fullmatch(str(reviews["R04-content-review"])))):
        raise Refusal("pins: reviews must name R04-content-review as a digest or null")
    if not isinstance(pins["engine_version"], str) or not pins["engine_version"]:
        raise Refusal("pins: engine_version is required")
    sdk = pins["sdk"]
    if sdk is not None and (
        not isinstance(sdk, dict) or set(sdk) != {"contract_sha256"}
        or not SHA256.fullmatch(str(sdk["contract_sha256"]))
    ):
        raise Refusal("pins: sdk is null (no SDK ships) or {contract_sha256}")
    return pins


def pinned_tool(tools_dir: Path, name: str, pins: dict) -> Path:
    path = tools_dir / name
    if not path.is_file() or path.is_symlink() or sha256_file(path) != pins["tools"][name]:
        raise Refusal(f"tool is missing or not at its pinned digest: {name}")
    return path


def load_export_tool(path: Path):
    spec = importlib.util.spec_from_file_location("public_export_pinned", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def decide(args: argparse.Namespace) -> list[dict]:
    """Every check in order; the first refusal of a check is recorded, the rest still run."""
    checks: list[dict] = []

    def check(identity: str, action) -> bool:
        try:
            detail = action()
            checks.append({"id": identity, "ok": True, "detail": detail or ""})
            return True
        except (Refusal, OSError, ValueError, KeyError, tomllib.TOMLDecodeError) as error:
            checks.append({"id": identity, "ok": False, "detail": str(error)})
            return False

    # The pins are the trust anchor; their own digest comes from the protected
    # release environment, not from a file next to the tree.
    if sha256_file(args.pins) != args.pins_sha256:
        raise Refusal("pins: file is not the one the release environment pins")
    pins = load_pins(args.pins)
    state: dict = {}

    def export_binding() -> str:
        if sha256_file(args.manifest) != pins["manifest_sha256"]:
            raise Refusal("publish manifest is not the pinned one")
        tool = load_export_tool(pinned_tool(args.tools_dir, EXPORT_TOOL, pins))
        receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
        if receipt.get("source_commit") != pins["source_commit"]:
            raise Refusal("receipt names another source commit than the pinned one")
        try:
            tool.verify(args.tree, args.manifest, receipt,
                        pinned_receipt_sha256=pins["receipt_sha256"])
        except tool.ExportError as error:
            raise Refusal(f"tree is not the pinned export: {error}") from error
        state["tree_sha256"] = receipt["tree_sha256"]
        return f"tree {receipt['tree_sha256']}"

    bound = check("export-binding", export_binding)

    def engine_version() -> str:
        manifest = tomllib.loads((args.tree / "rust/Cargo.toml").read_text(encoding="utf-8"))
        found = manifest["package"]["version"]
        if found != pins["engine_version"]:
            raise Refusal(f"tree builds Engine {found}, pinned is {pins['engine_version']}")
        return found

    def sdk_pair() -> str:
        if pins["sdk"] is None:
            if args.sdk_contract is not None:
                raise Refusal("an SDK contract was supplied but the pins say no SDK ships")
            return "no SDK in this release"
        if args.sdk_contract is None:
            raise Refusal("the pins name an SDK contract; none was supplied")
        if sha256_file(args.sdk_contract) != pins["sdk"]["contract_sha256"]:
            raise Refusal("SDK contract is not the pinned one")
        contract = json.loads(args.sdk_contract.read_text(encoding="utf-8"))
        # Today's contract pairs one Engine version; a tested-pairs list replaces it later.
        paired = contract.get("tested_engine_versions") or [contract["engine_version"]]
        if pins["engine_version"] not in paired:
            raise Refusal(f"SDK contract pairs Engine {paired}, not {pins['engine_version']}")
        return f"SDK contract pairs Engine {pins['engine_version']}"

    # Without the binding, nothing below would be about the reviewed tree.
    if bound:
        check("engine-version", engine_version)
    else:
        checks.append({"id": "engine-version", "ok": False, "detail": "not evaluated: tree is unbound"})
    check("sdk-pair", sdk_pair)

    def publication_index() -> str:
        """The gates below read what git tracks. The tree must be the
        publication clone — a git worktree whose index holds every file of the
        export and nothing else — or an ignored or unadded file would be
        published without having been audited."""
        if not bound:
            raise Refusal("not evaluated: tree is unbound")
        def git(*arguments: str) -> bytes:
            done = subprocess.run(["git", "-C", str(args.tree), *arguments], capture_output=True,
                                  check=False)
            if done.returncode != 0:
                raise Refusal("tree is not a git worktree: the provenance gate needs the "
                              "publication clone with every file added")
            return done.stdout
        if Path(git("rev-parse", "--show-toplevel").decode().strip()).resolve() != args.tree.resolve():
            raise Refusal("tree is not the root of its git worktree")
        indexed = {}
        for record in git("ls-files", "-s", "-z").split(b"\x00"):
            if record:
                meta, name = record.split(b"\t", 1)
                indexed[name.decode("utf-8")] = meta.split()[1].decode()
        receipt = json.loads(args.receipt.read_text(encoding="utf-8"))
        expected = {row["path"]: row["blob"] for row in receipt["files"]}
        extra = {path: blob for path, blob in indexed.items() if path not in expected}
        allowed_extra = {args.receipt.name} | {row["path"] for row in receipt["generated"]}
        if set(expected) - set(indexed) or set(extra) - allowed_extra:
            raise Refusal("git index and export receipt name different files")
        if any(indexed[path] != blob for path, blob in expected.items()):
            raise Refusal("git index holds other content than the export receipt")
        return f"index holds exactly the {len(expected)} exported files"

    indexed_ok = check("publication-index", publication_index)

    for identity, tool_name, arguments in LOCAL_GATES:
        def run(tool_name=tool_name, arguments=arguments) -> str:
            if not bound or not indexed_ok:
                raise Refusal("not run: tree is not the bound publication clone")
            tool = pinned_tool(args.tools_dir, tool_name, pins)
            args.work.mkdir(parents=True, exist_ok=True)
            done = subprocess.run([sys.executable, "-B", str(tool),
                                   *arguments(args.tree, args.work, pins, args)],
                                  capture_output=True, text=True, check=False,
                                  timeout=GATE_TIMEOUT_SECONDS)
            if done.returncode != 0:
                # The gate's own stdout line is its verdict; stderr may quote paths only.
                verdict = (done.stdout.strip().splitlines() or ["no output"])[-1][:200]
                raise Refusal(f"gate exited {done.returncode}: {verdict}")
            return "run here on this tree: exit 0"
        check(identity, run)

    def offline_build() -> str:
        if args.build_evidence is None:
            raise Refusal("no isolated offline build evidence supplied")
        if sha256_file(args.build_evidence) != pins["evidence"]["R05-offline-build"]:
            raise Refusal("build evidence is not the pinned file")
        evidence = json.loads(args.build_evidence.read_text(encoding="utf-8"))
        if evidence.get("schema") != BUILD_EVIDENCE_SCHEMA or evidence.get("result") != "PASS":
            raise Refusal("build evidence is not a passed isolated offline build")
        if not bound or evidence.get("tree_sha256") != state["tree_sha256"]:
            raise Refusal("build evidence is about another tree")
        if evidence.get("receipt_sha256") != pins["receipt_sha256"]:
            raise Refusal("build evidence is bound to another export receipt")
        if evidence.get("producer_sha256") != pins["tools"][BUILD_PRODUCER]:
            raise Refusal("build evidence was not written by the pinned producer")
        if evidence.get("engine_version") != pins["engine_version"]:
            raise Refusal("build evidence is for another Engine version")
        return f"pinned evidence names this tree; toolchain: {evidence['toolchain']['cargo']}"

    check("R05-offline-build", offline_build)

    def artifacts() -> str:
        supplied = {}
        for item in args.artifact:
            name, separator, path = item.partition("=")
            if not separator or name in supplied:
                raise Refusal(f"artifact must be NAME=PATH, each name once: {item!r}")
            supplied[name] = Path(path)
        if set(supplied) != set(pins["artifacts"]):
            missing = sorted(set(pins["artifacts"]) - set(supplied))
            extra = sorted(set(supplied) - set(pins["artifacts"]))
            raise Refusal(f"artifact set differs from the pins (missing {missing}, unpinned {extra})")
        for name, path in supplied.items():
            if sha256_file(path) != pins["artifacts"][name]:
                raise Refusal(f"artifact is not at its pinned digest: {name}")
        return f"{len(supplied)} artifact(s) at their pins"

    check("artifacts", artifacts)
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--pins", type=Path, required=True,
                        help="trusted release pins, supplied from outside the tree")
    parser.add_argument("--pins-sha256", required=True,
                        help="digest of the pins file, from the protected release environment")
    parser.add_argument("--content-review", type=Path,
                        help="release-approving content review of open secret findings, if pinned")
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--tools-dir", type=Path, default=Path(__file__).resolve().parent,
                        help="gate tools; each is used only at its pinned digest")
    parser.add_argument("--sdk-contract", type=Path)
    parser.add_argument("--build-evidence", type=Path)
    parser.add_argument("--artifact", action="append", default=[], metavar="NAME=PATH")
    parser.add_argument("--work", type=Path, required=True, help="scratch directory for gate output")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        checks = decide(args)
    except (Refusal, OSError, ValueError) as error:
        checks = [{"id": "pins", "ok": False, "detail": str(error)}]
    allowed = bool(checks) and all(item["ok"] for item in checks)
    report = {"schema": REPORT_SCHEMA, "result": "ALLOW" if allowed else "REFUSE", "checks": checks,
              "pins_sha256": sha256_file(args.pins) if args.pins.is_file() else None,
              "scope": "a release decision for this tree and these artifacts; publishes nothing"}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    for item in checks:
        print(f"publish-gate: {'ok    ' if item['ok'] else 'REFUSE'} {item['id']}: {item['detail']}")
    print(f"publish-gate: {report['result']}")
    return 0 if allowed else 1


if __name__ == "__main__":
    sys.exit(main())
