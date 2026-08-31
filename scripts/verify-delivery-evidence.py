#!/usr/bin/env python3
"""Verify the canonical, local-only delivery evidence ledger fail closed."""

import argparse
import hashlib
import importlib.util
import json
import re
import sys
from pathlib import Path


_SPEC = importlib.util.spec_from_file_location(
    "delivery_manifest_verifier", Path(__file__).with_name("verify-delivery-manifest.py")
)
MANIFEST = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(MANIFEST)

MAX_CONTRACT_BYTES = 512 * 1024
MAX_SOURCE_BYTES = 8 * 1024 * 1024


class InvalidDeliveryEvidence(ValueError):
    """Evidence ledger makes a claim not backed by the checked-in source."""


def canonical_json(value):
    return MANIFEST.canonical_json(value)


def _fail(message):
    raise InvalidDeliveryEvidence(message)


def _exact_keys(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        _fail(f"{label} must contain exactly {sorted(keys)}")


STAGE_EVIDENCE = {
    "setup": {
        "evidence_class": "rust-integration-test",
        "harness_command": "cargo test --manifest-path rust/Cargo.toml --test main suite::setup_ci_smoke::setup_bootstrap_doctor_status_json_smoke -- --exact --test-threads=1",
        "path": "rust/tests/suite/setup_ci_smoke.rs",
        "selector": "fn setup_bootstrap_doctor_status_json_smoke()",
    },
    "doctor": {
        "evidence_class": "rust-integration-test",
        "harness_command": "cargo test --manifest-path rust/Cargo.toml --test main suite::onboard_doctor_clean::onboard_yes_leaves_doctor_fully_green -- --exact",
        "path": "rust/tests/suite/onboard_doctor_clean.rs",
        "selector": "fn onboard_yes_leaves_doctor_fully_green()",
    },
    "upgrade": {
        "evidence_class": "rust-unit-test",
        "harness_command": "cargo test --manifest-path rust/Cargo.toml --lib doctor::migrate::tests::contract_outcome_reports_frozen_set -- --exact",
        "path": "rust/src/doctor/migrate.rs",
        "selector": "fn contract_outcome_reports_frozen_set()",
    },
    "rollback": {
        "evidence_class": "python-unittest",
        "harness_command": "python3 tests/delivery/test_rehearse_delivery.py DeliveryRehearsalTests.test_rehearses_verified_candidate_and_rollback_without_deployment",
        "path": "tests/delivery/test_rehearse_delivery.py",
        "selector": "def test_rehearses_verified_candidate_and_rollback_without_deployment(self):",
    },
    "uninstall": {
        "evidence_class": "rust-integration-test",
        "harness_command": "cargo test --manifest-path rust/Cargo.toml --test main suite::cli_characterization::uninstall_dry_run_exits_zero -- --exact",
        "path": "rust/tests/suite/cli_characterization.rs",
        "selector": "fn uninstall_dry_run_exits_zero()",
    },
}
STAGE_ORDER = ("setup", "doctor", "upgrade", "rollback", "uninstall")
TARGETS = (
    ("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-gnu", "ubuntu-22.04"),
    ("x86_64-unknown-linux-gnu-cuda", "x86_64-unknown-linux-gnu", "ubuntu-22.04"),
    ("aarch64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "ubuntu-22.04"),
    ("x86_64-unknown-linux-musl", "x86_64-unknown-linux-musl", "ubuntu-22.04"),
    ("aarch64-unknown-linux-musl", "aarch64-unknown-linux-musl", "ubuntu-22.04"),
    ("x86_64-apple-darwin", "x86_64-apple-darwin", "macos-latest"),
    ("aarch64-apple-darwin", "aarch64-apple-darwin", "macos-latest"),
    ("x86_64-pc-windows-msvc", "x86_64-pc-windows-msvc", "windows-latest"),
    ("x86_64-pc-windows-gnu", "x86_64-pc-windows-gnu", "windows-latest"),
)
PUBLISH_CHANNELS = (
    ("engine-release", ".github/workflows/release.yml", "v[0-9]*"),
    ("client-release", ".github/workflows/publish-clients.yml", "client-v[0-9]*"),
)
VERSION_GATES = (
    ("release-tag", "scripts/check-release-tag.py", "def verify_tag(tag: str, root: Path) -> str:", "python3 scripts/check-release-tag.py \"${GITHUB_REF_NAME}\""),
    ("sdk-surface", "scripts/check-sdk-surface.py", "def main() -> int:", "python3 scripts/check-sdk-surface.py"),
    ("package-version", "scripts/check-package-versions.py", "def main() -> int:", "python3 scripts/check-package-versions.py"),
)


def _read(root, relative, limit, label):
    try:
        path = MANIFEST.confined_file(root, relative)
        raw = MANIFEST._regular_file(path, limit, label)
    except MANIFEST.InvalidManifest as error:
        _fail(str(error))
    return path, raw


def _source_hash(raw):
    return hashlib.sha256(raw).hexdigest()


def verify_executable_selector(stage, source, selector):
    if stage not in STAGE_EVIDENCE:
        _fail("unknown lifecycle stage")
    if not isinstance(source, bytes) or not isinstance(selector, str):
        _fail("executable selector has invalid type")
    text = source.decode("utf-8", "strict")
    expected = STAGE_EVIDENCE[stage]["selector"]
    if selector != expected:
        _fail("evidence selector is not the staged executable selector")
    if STAGE_EVIDENCE[stage]["evidence_class"].startswith("rust"):
        function = re.escape(selector.rstrip().removesuffix("{").rstrip())
        match = re.search(r"(?m)^\s*" + function + r"\s*\{", text)
        if match is None:
            _fail("selector is not an executable Rust test")
        before = text[:match.start()]
        if re.search(r"(?m)^\s*#\[test\]\s*$", before) is None:
            _fail("selector has no Rust test attribute")
        return
    method = re.escape(selector.rstrip(":"))
    if re.search(r"(?m)^\s*" + method + r"\s*:", text) is None:
        _fail("selector is not an executable unittest method")


def verify_ref(root, value, label):
    if not isinstance(value, dict) or not {"path", "selector", "sha256"}.issubset(value):
        _fail(f"{label} has no safe evidence reference")
    _, raw = _read(root, value["path"], MAX_SOURCE_BYTES, f"{label} evidence")
    expected = STAGE_EVIDENCE.get(label)
    if expected is None:
        _fail("unknown lifecycle stage")
    _exact_keys(value, {"path", "selector", "sha256", "evidence_class", "harness_command"}, label)
    for field in ("path", "selector", "evidence_class", "harness_command"):
        if value[field] != expected[field]:
            _fail(f"{label} {field} is caller-controlled or drifted")
    if value["sha256"] != _source_hash(raw):
        _fail(f"{label} evidence digest mismatch")
    verify_executable_selector(label, raw, value["selector"])


def _fixed_ref(root, value, label, path, selector):
    _exact_keys(value, {"path", "selector", "sha256"}, label)
    if value["path"] != path or value["selector"] != selector:
        _fail(f"{label} identity drift")
    _, raw = _read(root, path, MAX_SOURCE_BYTES, label)
    if value["sha256"] != _source_hash(raw):
        _fail(f"{label} digest mismatch")
    if selector.encode("utf-8") not in raw:
        _fail(f"{label} selector is absent")


def _verify_checklist(root, checklist):
    if not isinstance(checklist, list) or len(checklist) != len(STAGE_ORDER):
        _fail("checklist must contain every lifecycle stage exactly once")
    for expected_stage, step in zip(STAGE_ORDER, checklist):
        _exact_keys(step, {"stage", "evidence", "external_acceptance_required", "local_status"}, "checklist stage")
        if step["stage"] != expected_stage or step["external_acceptance_required"] is not True:
            _fail("checklist lifecycle order or acceptance gate drifted")
        expected_status = "offline-rehearsal" if expected_stage == "rollback" else "component-evidence"
        if step["local_status"] != expected_status:
            _fail("checklist local status drifted")
        verify_ref(root, step["evidence"], expected_stage)


def _verify_delivery_manifest_section(root, value):
    _exact_keys(value, {"fixture", "schema", "trust_root", "verifier"}, "delivery manifest evidence")
    _fixed_ref(root, value["fixture"], "delivery manifest fixture", "tests/delivery/valid/delivery-manifest.json", '"schema_version":"leanctx.delivery/v1"')
    _fixed_ref(root, value["schema"], "delivery manifest schema", "docs/contracts/delivery-manifest-v1.schema.json", "https://leanctx.dev/contracts/delivery-manifest-v1.schema.json")
    _fixed_ref(root, value["trust_root"], "delivery trust root", "tests/delivery/valid/release-trust-root.json", '"algorithm":"Ed25519"')
    _fixed_ref(root, value["verifier"], "delivery manifest verifier", "scripts/verify-delivery-manifest.py", "def verify(manifest_path, root, trust_root=None, rotation_plan=None):")


def _verify_release(root, release):
    _exact_keys(release, {"publish_channels", "targets", "version_gates", "workflow", "workflow_tag_glob"}, "release")
    if release["workflow_tag_glob"] != "v[0-9]*":
        _fail("release workflow tag glob drifted")
    _fixed_ref(root, release["workflow"], "release workflow", ".github/workflows/release.yml", "name: Release\n")
    if not isinstance(release["targets"], list) or len(release["targets"]) != len(TARGETS):
        _fail("release targets drifted")
    for target, expected in zip(release["targets"], TARGETS):
        _exact_keys(target, {"artifact", "certification", "runner", "target"}, "release target")
        artifact, platform, runner = expected
        if target != {"artifact": artifact, "certification": "not-asserted", "runner": runner, "target": platform}:
            _fail("release target makes an unsupported platform certification or runner claim")
    if not isinstance(release["publish_channels"], list) or len(release["publish_channels"]) != len(PUBLISH_CHANNELS):
        _fail("release publish channels drifted")
    for channel, expected in zip(release["publish_channels"], PUBLISH_CHANNELS):
        _exact_keys(channel, {"id", "source_path", "source_sha256", "trigger_pattern"}, "publish channel")
        identifier, path, trigger = expected
        if channel["id"] != identifier or channel["source_path"] != path or channel["trigger_pattern"] != trigger:
            _fail("publish channel identity drifted")
        _, raw = _read(root, path, MAX_SOURCE_BYTES, "publish workflow")
        if channel["source_sha256"] != _source_hash(raw):
            _fail("publish workflow digest mismatch")
    if not isinstance(release["version_gates"], list) or len(release["version_gates"]) != len(VERSION_GATES):
        _fail("release version gates drifted")
    for gate, expected in zip(release["version_gates"], VERSION_GATES):
        _exact_keys(gate, {"id", "source", "workflow_selector"}, "version gate")
        identifier, path, selector, workflow_selector = expected
        if gate["id"] != identifier or gate["workflow_selector"] != workflow_selector:
            _fail("version gate identity drifted")
        _fixed_ref(root, gate["source"], f"version gate {identifier}", path, selector)


def verify(contract_path, root):
    root = Path(root).resolve(strict=True)
    try:
        relative = str(Path(contract_path).relative_to(root)) if Path(contract_path).is_absolute() else str(contract_path)
    except ValueError:
        _fail("delivery evidence contract escapes repository root")
    _, raw = _read(root, relative, MAX_CONTRACT_BYTES, "delivery evidence contract")
    try:
        contract = json.loads(raw)
    except (TypeError, ValueError, UnicodeDecodeError) as error:
        _fail(f"delivery evidence contract is not valid JSON: {error}")
    if canonical_json(contract) != raw:
        _fail("delivery evidence contract is not canonical JSON")
    _exact_keys(contract, {"checklist", "delivery_manifest", "owner", "release", "requirement_ids", "schema_version", "scope", "status"}, "delivery evidence contract")
    if contract["schema_version"] != "leanctx.delivery-evidence/v1" or contract["status"] != "partial" or contract["requirement_ids"] != ["BC-04", "EN-05", "RG-07", "RG-11"]:
        _fail("delivery evidence contract has unsupported status or requirements")
    _exact_keys(contract["scope"], {"customer_delivery_acceptance", "external_operational_acceptance", "local_contract_consistency", "os_certification", "zero_downtime_verified"}, "delivery scope")
    if contract["scope"] != {"customer_delivery_acceptance": False, "external_operational_acceptance": False, "local_contract_consistency": True, "os_certification": False, "zero_downtime_verified": False}:
        _fail("delivery scope makes unsupported operational claims")
    _exact_keys(contract["owner"], {"handle", "source"}, "delivery owner")
    if contract["owner"]["handle"] != "@yvgude":
        _fail("delivery owner drifted")
    _fixed_ref(root, contract["owner"]["source"], "delivery owner source", ".github/CODEOWNERS", "* @yvgude")
    _verify_checklist(root, contract["checklist"])
    _verify_delivery_manifest_section(root, contract["delivery_manifest"])
    _verify_release(root, contract["release"])
    return contract


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("contract")
    parser.add_argument("--root", required=True)
    arguments = parser.parse_args(argv)
    try:
        verify(arguments.contract, arguments.root)
    except InvalidDeliveryEvidence as error:
        print(f"delivery evidence rejected: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
