#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Derive the pinned evidence files from the sources they pin.

Five pinned files have a verifier and no producer; this script is that
producer. It never decides anything a reviewer owns: it recomputes digests for
exactly the paths a file already names, re-binds the files that depend on each
other, and serialises with the verifiers' own functions. A path set, a baseline
commit, a contract-pack version, a selector or a release target is never
changed here (the pack version only through an explicit ``--pack-version``).

Commands (each takes ``--root``; without ``--write`` nothing is written):

  history-policy     security/history-policy-v1.json  (scanner_source_sha256)
  baseline-reattest  the baseline report's policy_sha256 and the policy's
                     baseline.report_sha256; audited commit, findings and
                     counts are carried over unchanged
  contract-pack      docs/contracts/ocla-contract-pack-v1.json  (artifact digests)
  delivery-fixture   tests/delivery/valid/delivery-manifest.json and
                     signature.bundle.json, signed with the published
                     RFC 8032 section 7.1 TEST 1 seed and nothing else
  delivery-evidence  docs/contracts/delivery-evidence-v1.json  (source digests)
  all                contract-pack, delivery-fixture, delivery-evidence, then
                     history-policy, baseline-reattest

Exit status: 0 the files on disk equal what is derived (or were written),
1 drift (check mode; one line per differing field), 2 refused or failed.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import sys
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


# The verifiers are the single source of canonical form, hashing inputs and
# signature primitives. They are loaded from beside this script, never from
# ``--root``: the tree being pinned is data, not code to execute.
GATE = _load("pinned_evidence_history_gate", "history-policy-gate.py")
OCLA = _load("pinned_evidence_ocla_suite", "verify-ocla-contract-suite.py")
EVIDENCE = _load("pinned_evidence_delivery_evidence", "verify-delivery-evidence.py")
MANIFEST = EVIDENCE.MANIFEST
TRUST = MANIFEST.TRUST

POLICY = "security/history-policy-v1.json"
PACK = "docs/contracts/ocla-contract-pack-v1.json"
EVIDENCE_CONTRACT = "docs/contracts/delivery-evidence-v1.json"
FIXTURE_DIR = "tests/delivery/valid/"
FIXTURE_MANIFEST = FIXTURE_DIR + "delivery-manifest.json"
FIXTURE_RECEIPT = FIXTURE_DIR + "signature.bundle.json"
FIXTURE_TRUST_ROOT = FIXTURE_DIR + "release-trust-root.json"

# RFC 8032 section 7.1, TEST 1. Published, test-only, and the same vector as
# TEST_RELEASE_SEED in tests/delivery/test_verify_delivery_manifest.py. It is
# the only seed this script can sign with; there is no option to supply another.
RFC8032_TEST1_SEED = bytes.fromhex(
    "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
)
RFC8032_TEST1_PUBLIC = bytes.fromhex(
    "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
)
RFC8032_TEST1_EMPTY_SIGNATURE = bytes.fromhex(
    "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
    "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
)


class Refusal(ValueError):
    """The producer will not derive this; the reason needs a human decision."""


FAILURES = (
    Refusal,
    GATE.GateError,
    OCLA.SuiteError,
    MANIFEST.InvalidManifest,
    TRUST.TrustError,
    EVIDENCE.InvalidDeliveryEvidence,
    OSError,
    json.JSONDecodeError,
    UnicodeDecodeError,
    KeyError,
    TypeError,
)


def _sha256(data):
    return hashlib.sha256(data).hexdigest()


def _pretty(value):
    """Form of the two hand-readable pin files: 2-space indent, key order kept."""
    return (json.dumps(value, indent=2) + "\n").encode()


def output_path(root, relative, scope):
    """Resolve an output below ``root`` and ``scope``; refuse anything else.

    Outputs are existing regular files reached without symlinks. The producer
    re-derives pins of files that are already tracked; it creates none.
    """
    try:
        parts = MANIFEST._relative_parts(relative)
    except MANIFEST.InvalidManifest as error:
        raise Refusal(f"output {relative!r} refused: {error}") from error
    if not relative.startswith(scope):
        raise Refusal(f"output {relative!r} refused: outside {scope}")
    current = Path(root).resolve(strict=True)
    for part in parts:
        current = current / part
        if current.is_symlink():
            raise Refusal(f"output {relative!r} refused: symlink in path")
    if not current.is_file():
        raise Refusal(f"output {relative!r} refused: not an existing regular file")
    return current


class Tree:
    """Read view of ``--root`` plus the outputs derived so far in this run."""

    def __init__(self, root):
        self.root = Path(root).resolve(strict=True)
        self.staged = {}
        self.scopes = {}

    def disk(self, relative, limit=MANIFEST.MAX_ARTIFACT_BYTES):
        try:
            path = MANIFEST.confined_file(self.root, relative)
            return MANIFEST._regular_file(path, limit, relative)
        except MANIFEST.InvalidManifest as error:
            raise Refusal(f"{relative}: {error}") from error

    def read(self, relative, limit=MANIFEST.MAX_ARTIFACT_BYTES):
        if relative in self.staged:
            return self.staged[relative]
        return self.disk(relative, limit)

    def stage(self, relative, data, scope):
        output_path(self.root, relative, scope)
        self.staged[relative] = data
        self.scopes[relative] = scope


class _Raw:
    """Lets ``history-policy-gate.load_policy`` validate bytes already read."""

    def __init__(self, raw):
        self._raw = raw

    def read_bytes(self):
        return self._raw


def _policy(tree):
    policy = GATE.load_policy(_Raw(tree.read(POLICY)))
    if policy["policy_source_path"] != POLICY:
        raise Refusal(f"policy_source_path is not {POLICY}")
    return policy


def _scanner_source(tree, relative):
    if relative in tree.staged:
        return tree.staged[relative]
    try:
        return GATE.repository_file(tree.root, relative, "scanner source").read_bytes()
    except GATE.GateError as error:
        raise Refusal(
            f"{relative}: {error}; the listed scanner sources are an owner decision, "
            "a missing one is not dropped"
        ) from error


def derive_history_policy(tree, _options):
    policy = _policy(tree)
    digests = policy["scanner_source_sha256"]
    for relative in digests:
        digests[relative] = _sha256(_scanner_source(tree, relative))
    tree.stage(POLICY, _pretty(policy), "security/")


def derive_baseline_reattest(tree, _options):
    policy = _policy(tree)
    for relative, pinned in policy["scanner_source_sha256"].items():
        if _sha256(_scanner_source(tree, relative)) != pinned:
            raise Refusal(
                f"{relative}: scanner source digest in the policy is stale; "
                "run history-policy first"
            )
    baseline = policy["baseline"]
    relative = baseline["report"]
    if relative in tree.staged:
        raw = tree.staged[relative]
    else:
        try:
            raw = GATE.repository_file(tree.root, relative, "baseline report").read_bytes()
        except GATE.GateError as error:
            raise Refusal(f"{relative!r}: {error}") from error
    report = json.loads(raw)
    if (
        not isinstance(report, dict)
        or report.get("schema_version") != "leanctx.full-history-evidence/v1"
        or report.get("audited_commit") != baseline["commit"]
    ):
        raise Refusal(
            "baseline report is not bound to baseline.commit; a baseline rotation "
            "is an owner decision and a full audit, not a re-attestation"
        )
    report["policy_sha256"] = GATE.policy_fingerprint(policy)
    rendered = GATE.canonical(report)
    baseline["report_sha256"] = _sha256(rendered)
    tree.stage(relative, rendered, "security/")
    tree.stage(POLICY, _pretty(policy), "security/")


def derive_contract_pack(tree, options):
    pack = json.loads(tree.read(PACK))
    MANIFEST.verify_contract_pack_metadata(pack)
    listed = {artifact["path"] for artifact in pack["artifacts"]}
    if listed != OCLA.CONTRACT_PACK_ARTIFACTS:
        missing = sorted(OCLA.CONTRACT_PACK_ARTIFACTS - listed)
        unexpected = sorted(listed - OCLA.CONTRACT_PACK_ARTIFACTS)
        raise Refusal(
            "contract pack artifact set differs from the verifier's closed set "
            f"(missing {missing}, unexpected {unexpected}); the set is not derived"
        )
    for artifact in pack["artifacts"]:
        relative = artifact["path"]
        if relative in tree.staged:
            body = tree.staged[relative]
        else:
            try:
                body = OCLA.read_contract_artifact(tree.root, relative)
            except OCLA.SuiteError as error:
                raise Refusal(f"{relative}: {error}") from error
        artifact["sha256"] = _sha256(body)
    version = options.get("pack_version")
    if version is not None:
        major = OCLA.parse_semver(version, "contract_pack_version")[0]
        pack["version"] = version
        # The closed N/N-1 set verify_contract_pack_metadata requires for it.
        pack["compatibility"]["supported"] = [version] + (
            [f"{major - 1}.0.0"] if major > 1 else []
        )
        MANIFEST.verify_contract_pack_metadata(pack)
    tree.stage(PACK, _pretty(pack), "docs/contracts/")


def test_seed():
    """Return the RFC 8032 TEST 1 seed after checking it is that vector."""
    seed = RFC8032_TEST1_SEED
    if (
        TRUST.public_from_seed(seed) != RFC8032_TEST1_PUBLIC
        or TRUST.ed25519_sign(b"", seed) != RFC8032_TEST1_EMPTY_SIGNATURE
    ):
        raise Refusal("embedded seed is not the RFC 8032 TEST 1 vector")
    return seed


def derive_delivery_fixture(tree, _options):
    seed = test_seed()
    public, key_id = TRUST.read_public_key(
        MANIFEST.confined_file(tree.root, FIXTURE_TRUST_ROOT)
    )
    if public != RFC8032_TEST1_PUBLIC:
        raise Refusal(
            f"{FIXTURE_TRUST_ROOT} is not the RFC 8032 TEST 1 public key; "
            "this producer signs the test fixture only"
        )
    manifest = json.loads(tree.read(FIXTURE_MANIFEST, MANIFEST.MAX_MANIFEST_BYTES))
    MANIFEST._check_manifest(manifest)
    evidence = manifest["evidence"]
    if evidence["signature"].get("path") != FIXTURE_RECEIPT:
        raise Refusal(f"fixture receipt path is not {FIXTURE_RECEIPT}")
    pack = json.loads(tree.read(PACK))
    manifest["contracts"]["pack_digest"] = "sha256:" + _sha256(MANIFEST.canonical_json(pack))
    manifest["contracts"]["pack_version"] = pack["version"]
    for label in ("sbom", "provenance", "vulnerability_report"):
        evidence[label]["sha256"] = _sha256(tree.read(evidence[label]["path"]))
    payload = TRUST.canonical_json(TRUST.promotion_payload(manifest))
    receipt = MANIFEST.canonical_json(
        {
            "algorithm": "Ed25519",
            "key_id": key_id,
            "payload_sha256": _sha256(payload),
            "schema_version": "leanctx.release-signature/v1",
            "signature": base64.b64encode(TRUST.ed25519_sign(payload, seed)).decode("ascii"),
        }
    )
    evidence["signature"]["sha256"] = _sha256(receipt)
    tree.stage(FIXTURE_RECEIPT, receipt, FIXTURE_DIR)
    tree.stage(FIXTURE_MANIFEST, MANIFEST.canonical_json(manifest), FIXTURE_DIR)


def _repin(tree, value):
    """Recompute every digest reference of the delivery-evidence contract."""
    count = 0
    if isinstance(value, dict):
        for path_key, digest_key in (("path", "sha256"), ("source_path", "source_sha256")):
            if path_key in value and digest_key in value:
                if not isinstance(value[path_key], str):
                    raise Refusal("delivery evidence reference has no path")
                value[digest_key] = _sha256(
                    tree.read(value[path_key], EVIDENCE.MAX_SOURCE_BYTES)
                )
                count += 1
        for nested in value.values():
            count += _repin(tree, nested)
    elif isinstance(value, list):
        for nested in value:
            count += _repin(tree, nested)
    return count


def derive_delivery_evidence(tree, _options):
    contract = json.loads(tree.read(EVIDENCE_CONTRACT, EVIDENCE.MAX_CONTRACT_BYTES))
    if not _repin(tree, contract):
        raise Refusal("delivery evidence contract names no digest reference")
    tree.stage(EVIDENCE_CONTRACT, EVIDENCE.canonical_json(contract), "docs/contracts/")


# Each inner list is one dependency chain; the two chains of ``all`` share no file.
COMMANDS = {
    "history-policy": [[derive_history_policy]],
    "baseline-reattest": [[derive_baseline_reattest]],
    "contract-pack": [[derive_contract_pack]],
    "delivery-fixture": [[derive_delivery_fixture]],
    "delivery-evidence": [[derive_delivery_evidence]],
    "all": [
        [derive_contract_pack, derive_delivery_fixture, derive_delivery_evidence],
        [derive_history_policy, derive_baseline_reattest],
    ],
}


def derive(command, root, pack_version=None):
    """Return ``(tree, refusals)``: ``tree.staged`` holds the derived bytes by path."""
    tree = Tree(root)
    refusals = []
    for chain in COMMANDS[command]:
        for step in chain:
            try:
                step(tree, {"pack_version": pack_version})
            except FAILURES as error:
                name = step.__name__[len("derive_"):].replace("_", "-")
                refusals.append(f"{name}: {type(error).__name__}: {error}")
                break
    return tree, refusals


def _child(prefix, key):
    if isinstance(key, int):
        return f"{prefix}[{key}]"
    if key.replace("_", "a").isalnum():
        return f"{prefix}.{key}" if prefix else key
    return f"{prefix}[{json.dumps(key)}]"


def field_drift(old, new, prefix="", hint=""):
    """Yield one line per leaf that differs between two JSON values."""
    if isinstance(old, dict) and isinstance(new, dict):
        named = old.get("path", old.get("source_path"))
        inner = f" ({named})" if isinstance(named, str) else ""
        for key in list(old) + [key for key in new if key not in old]:
            if key not in new:
                yield f"{_child(prefix, key)}: removed"
            elif key not in old:
                yield f"{_child(prefix, key)}: added"
            else:
                yield from field_drift(old[key], new[key], _child(prefix, key), inner)
    elif isinstance(old, list) and isinstance(new, list) and len(old) == len(new):
        for index, (left, right) in enumerate(zip(old, new)):
            yield from field_drift(left, right, _child(prefix, index))
    elif old != new or type(old) is not type(new):
        yield f"{prefix}{hint}: {json.dumps(old)} -> {json.dumps(new)}"


def drift(tree):
    """Map each derived path that differs from disk to its differing fields."""
    result = {}
    for relative, derived in tree.staged.items():
        current = tree.disk(relative)
        if current == derived:
            continue
        try:
            lines = list(field_drift(json.loads(current), json.loads(derived)))
        except (ValueError, UnicodeDecodeError) as error:
            lines = [f"<file>: not valid JSON on disk ({error})"]
        result[relative] = lines or [
            "<serialisation>: values equal, bytes differ from the derived form"
        ]
    return result


def run(command, root, write=False, pack_version=None, out=None):
    out = out or sys.stdout
    try:
        tree, refusals = derive(command, root, pack_version)
        differing = drift(tree)
    except FAILURES as error:
        print(f"REFUSED {command}: {type(error).__name__}: {error}", file=out)
        return 2
    for relative in tree.staged:
        if relative in differing:
            print(f"DRIFT {relative}", file=out)
            for line in differing[relative]:
                print(f"  {line}", file=out)
        else:
            print(f"OK    {relative}", file=out)
    for refusal in refusals:
        print(f"REFUSED {refusal}", file=out)
    if refusals:
        print("nothing written: a step was refused", file=out)
        return 2
    if not write:
        return 1 if differing else 0
    for relative in differing:
        # Checked when staged; resolved again immediately before the write.
        output_path(tree.root, relative, tree.scopes[relative]).write_bytes(
            tree.staged[relative]
        )
        print(f"WROTE {relative}", file=out)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("command", choices=tuple(COMMANDS))
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--write", action="store_true", help="write the derived files")
    parser.add_argument(
        "--pack-version",
        help="set the contract pack version (contract-pack and all only); "
        "without it the version is never changed",
    )
    arguments = parser.parse_args(argv)
    if arguments.pack_version is not None and arguments.command not in ("contract-pack", "all"):
        parser.error("--pack-version applies to contract-pack and all only")
    return run(arguments.command, arguments.root, arguments.write, arguments.pack_version)


if __name__ == "__main__":
    raise SystemExit(main())
