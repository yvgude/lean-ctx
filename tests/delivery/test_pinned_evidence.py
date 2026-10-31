# SPDX-License-Identifier: Apache-2.0
"""Proof obligations of scripts/pinned-evidence.py.

Three kinds of evidence:

* a self-contained miniature tree whose pins start as zeros: what the producer
  writes is accepted by the real verifier functions;
* drift, repair, idempotence and the refusals, on that tree;
* byte reproduction of the committed pin files on trees materialised from git
  objects (skipped, with the reason, when the objects are not available).
"""

import base64
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def load_verifiers(scripts, tag):
    """Fresh instances of the four verifiers, independent of the producer's."""
    evidence = load(f"pe_{tag}_evidence", scripts / "verify-delivery-evidence.py")
    return types.SimpleNamespace(
        gate=load(f"pe_{tag}_gate", scripts / "history-policy-gate.py"),
        ocla=load(f"pe_{tag}_ocla", scripts / "verify-ocla-contract-suite.py"),
        manifest=load(f"pe_{tag}_manifest", scripts / "verify-delivery-manifest.py"),
        evidence=evidence,
    )


PRODUCER = load("pinned_evidence_producer", ROOT / "scripts/pinned-evidence.py")
VERIFIERS = load_verifiers(ROOT / "scripts", "checkout")

POLICY = PRODUCER.POLICY
PACK = PRODUCER.PACK
CONTRACT = PRODUCER.EVIDENCE_CONTRACT
FIXTURE = PRODUCER.FIXTURE_MANIFEST
RECEIPT = PRODUCER.FIXTURE_RECEIPT
TRUST_ROOT = PRODUCER.FIXTURE_TRUST_ROOT
REPORT = "security/evidence/full-history-baseline-v1.json"
ZERO = "0" * 64
# The tiny tree uses the repository's real SEC001 rule: the gate validates the
# rule's full contract, and no fixture value in this tree matches it.
REPO_SECRET_RULE = json.loads((ROOT / POLICY).read_text())["secret_rule"]

# RFC 8032 section 7.1 TEST 2, public key only (no seed exists in this file):
# a valid Ed25519 point that is not the TEST 1 key.
RFC8032_TEST2_PUBLIC = bytes.fromhex(
    "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"
)

BASE_COMMIT = "a5be77de9fd572dd5afc4ff3522dd2622e83f362"
SNAPSHOT_REF = "refs/codex-snapshots/foreign-311/integration-branch-20261006T1234Z"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def put(root, relative, data):
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data if isinstance(data, bytes) else data.encode())


def pretty(value):
    return (json.dumps(value, indent=2) + "\n").encode()


def trust_root_bytes(public):
    return VERIFIERS.manifest.canonical_json(
        {
            "algorithm": "Ed25519",
            "key_id": "sha256:" + sha256(public),
            "public_key": base64.b64encode(public).decode(),
        }
    )


def snapshot(root):
    return {
        str(path.relative_to(root)): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file() and not path.is_symlink() and ".git" not in path.relative_to(root).parts
    }


def run(command, root, **options):
    out = io.StringIO()
    return PRODUCER.run(command, root, out=out, **options), out.getvalue()


def verify_all(root, verifiers=VERIFIERS):
    """Every real verifier entry point of the two chains; raises on rejection."""
    policy = verifiers.gate.load_policy(root / POLICY)
    verifiers.gate.validate_scanner_sources(root, policy)
    verifiers.gate.validate_baseline(root, policy)
    verifiers.ocla.validate_contract_pack(root)
    verifiers.manifest.verify(root / FIXTURE, root, root / TRUST_ROOT)
    verifiers.evidence.verify(root / CONTRACT, root)


def build_miniature_tree(root):
    """A complete, tiny tree whose every pin is a zero digest."""

    def git(*args):
        return subprocess.run(
            ["git", "-C", str(root), *args], capture_output=True, text=True, check=True
        ).stdout

    git("init", "-q")
    git("config", "gc.auto", "0")
    git("config", "user.email", "test@leanctx.invalid")
    git("config", "user.name", "LeanCTX Test")
    put(root, "README.md", "public\n")
    git("add", "-A")
    git("commit", "-q", "-m", "baseline")
    base = git("rev-parse", "HEAD").strip()

    gate, evidence, manifest_verifier = VERIFIERS.gate, VERIFIERS.evidence, VERIFIERS.manifest

    # History chain.
    sources = ["scripts/history-policy-gate.py", "tests/security/test_history_policy_gate.py"]
    for relative in sources:
        put(root, relative, f"# scanner source {relative}\n")
    put(
        root,
        POLICY,
        pretty(
            {
                "schema_version": "leanctx.history-policy/v1",
                "limits": {"max_commits": 50, "max_objects": 500, "max_findings": 20, "command_timeout_seconds": 10},
                "forbidden_paths": [{"id": "IP001", "prefix": "private/"}],
                "secret_rule": REPO_SECRET_RULE,
                "policy_source_path": POLICY,
                "scanner_source_paths": [POLICY, *sources],
                "scanner_source_sha256": {relative: ZERO for relative in sources},
                "baseline": {"commit": base, "report": REPORT, "report_sha256": ZERO},
            }
        ),
    )
    put(
        root,
        REPORT,
        gate.canonical(
            {
                "schema_version": "leanctx.full-history-evidence/v1",
                "audited_commit": base,
                "policy_sha256": ZERO,
                "scanner_versions": gate.BASELINE_SCANNER_VERSIONS,
                "counts": {"commits": 1, "objects": 3, "findings": 0},
                "audit_status": "rotation-and-rewrite-decision-pending",
                "current_tree_finding_ids": [],
                "finding_set_sha256": sha256(gate.canonical([])),
                "findings": [],
            }
        ),
    )

    # Contract pack.
    artifacts = sorted(VERIFIERS.ocla.CONTRACT_PACK_ARTIFACTS)
    for relative in artifacts:
        put(root, relative, f"artifact {relative}\n")
    for name in VERIFIERS.ocla.SCHEMA_MIRRORS:
        put(root, f"clients/rust/lean-ctx-client/contracts/{name}", (root / "docs/contracts" / name).read_bytes())
    put(
        root,
        PACK,
        pretty(
            {
                "artifacts": [{"path": relative, "sha256": ZERO} for relative in artifacts],
                "compatibility": {"policy": "N-and-N-minus-1", "supported": ["3.0.0", "2.0.0"]},
                "schema_version": "leanctx.contract-pack/v1",
                "version": "3.0.0",
            }
        ),
    )

    # Delivery fixture.
    canonical = manifest_verifier.canonical_json
    component = {"name": "lean-ctx", "version": "1.2.3"}
    source = {"repository": "https://example.invalid/lean-ctx", "commit": "1" * 40}
    image = {"reference": "registry.example.invalid/lean-ctx", "digest": "sha256:" + "2" * 64}
    valid = PRODUCER.FIXTURE_DIR
    put(root, valid + "sbom.cdx.json", canonical({"bomFormat": "CycloneDX", "metadata": {"component": component}}))
    put(
        root,
        valid + "provenance.slsa.json",
        canonical(
            {
                "_type": "https://in-toto.io/Statement/v1",
                "predicateType": "https://slsa.dev/provenance/v1",
                "predicate": {"buildDefinition": {"externalParameters": {"source": source}}},
                "subject": [{"name": image["reference"], "digest": {"sha256": "2" * 64}}],
            }
        ),
    )
    put(root, valid + "vulnerability-report.json", canonical({"artifactName": image["reference"]}))
    put(root, TRUST_ROOT, trust_root_bytes(PRODUCER.RFC8032_TEST1_PUBLIC))
    put(root, "docs/releases/migration-1.0.md", "# migration\n")
    put(root, RECEIPT, b"{}\n")
    put(
        root,
        FIXTURE,
        canonical(
            {
                "schema_version": "leanctx.delivery/v1",
                "component": component,
                "source": source,
                "image": image,
                "configuration": {"schema_version": "1", "migration": "docs/releases/migration-1.0.md"},
                "contracts": {"pack_version": "0.0.0", "pack_digest": "sha256:" + ZERO},
                "evidence": {
                    "sbom": {"path": valid + "sbom.cdx.json", "sha256": ZERO},
                    "provenance": {"path": valid + "provenance.slsa.json", "sha256": ZERO},
                    "vulnerability_report": {"path": valid + "vulnerability-report.json", "sha256": ZERO},
                    "signature": {"path": RECEIPT, "sha256": ZERO},
                },
            }
        ),
    )

    # Delivery evidence contract, built from the verifier's own closed tables.
    checklist = []
    for stage in evidence.STAGE_ORDER:
        spec = evidence.STAGE_EVIDENCE[stage]
        if spec["evidence_class"].startswith("rust"):
            body = "#[test]\n" + spec["selector"] + " {\n}\n"
        else:
            body = "class DeliveryRehearsalTests:\n    " + spec["selector"] + "\n        pass\n"
        put(root, spec["path"], body)
        checklist.append(
            {
                "stage": stage,
                "evidence": {**spec, "sha256": ZERO},
                "external_acceptance_required": True,
                "local_status": "offline-rehearsal" if stage == "rollback" else "component-evidence",
            }
        )
    fixed = {
        "schema": ("docs/contracts/delivery-manifest-v1.schema.json", "https://leanctx.dev/contracts/delivery-manifest-v1.schema.json"),
        "verifier": ("scripts/verify-delivery-manifest.py", "def verify(manifest_path, root, trust_root=None, rotation_plan=None):"),
    }
    for relative, selector in fixed.values():
        put(root, relative, selector + "\n")
    put(root, ".github/CODEOWNERS", "* @yvgude\n")
    for _, relative, _ in evidence.PUBLISH_CHANNELS:
        put(root, relative, "name: Release\n" if relative.endswith("/release.yml") else f"name: {relative}\n")
    for _, relative, selector, _ in evidence.VERSION_GATES:
        put(root, relative, selector + "\n    return 0\n")
    put(
        root,
        CONTRACT,
        evidence.canonical_json(
            {
                "checklist": checklist,
                "delivery_manifest": {
                    "fixture": {"path": FIXTURE, "selector": '"schema_version":"leanctx.delivery/v1"', "sha256": ZERO},
                    "schema": {"path": fixed["schema"][0], "selector": fixed["schema"][1], "sha256": ZERO},
                    "trust_root": {"path": TRUST_ROOT, "selector": '"algorithm":"Ed25519"', "sha256": ZERO},
                    "verifier": {"path": fixed["verifier"][0], "selector": fixed["verifier"][1], "sha256": ZERO},
                },
                "owner": {"handle": "@yvgude", "source": {"path": ".github/CODEOWNERS", "selector": "* @yvgude", "sha256": ZERO}},
                "release": {
                    "publish_channels": [
                        {"id": identifier, "source_path": relative, "source_sha256": ZERO, "trigger_pattern": trigger}
                        for identifier, relative, trigger in evidence.PUBLISH_CHANNELS
                    ],
                    "targets": [
                        {"artifact": artifact, "certification": "not-asserted", "runner": runner, "target": target}
                        for artifact, target, runner in evidence.TARGETS
                    ],
                    "version_gates": [
                        {"id": identifier, "source": {"path": relative, "selector": selector, "sha256": ZERO}, "workflow_selector": workflow}
                        for identifier, relative, selector, workflow in evidence.VERSION_GATES
                    ],
                    "workflow": {"path": ".github/workflows/release.yml", "selector": "name: Release\n", "sha256": ZERO},
                    "workflow_tag_glob": "v[0-9]*",
                },
                "requirement_ids": ["BC-04", "EN-05", "RG-07", "RG-11"],
                "schema_version": "leanctx.delivery-evidence/v1",
                "scope": {
                    "customer_delivery_acceptance": False,
                    "external_operational_acceptance": False,
                    "local_contract_consistency": True,
                    "os_certification": False,
                    "zero_downtime_verified": False,
                },
                "status": "partial",
            }
        ),
    )
    return base


class MiniatureTree(unittest.TestCase):
    """Base: a miniature tree; ``pinned`` leaves it with producer-written pins."""

    pinned = True

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.outside = Path(self.temp.name).resolve() / "outside"
        self.root = Path(self.temp.name).resolve() / "tree"
        self.outside.mkdir()
        self.root.mkdir()
        self.base = build_miniature_tree(self.root)
        if self.pinned:
            code, output = run("all", self.root, write=True)
            self.assertEqual(code, 0, output)

    def tearDown(self):
        try:
            self.temp.cleanup()
        except OSError:
            shutil.rmtree(self.temp.name, ignore_errors=True)

    def json(self, relative):
        return json.loads((self.root / relative).read_bytes())

    def assert_refused(self, command, needle, **options):
        before = snapshot(self.root)
        code, output = run(command, self.root, write=True, **options)
        self.assertEqual(code, 2, output)
        self.assertIn("REFUSED", output)
        self.assertIn(needle, output)
        self.assertEqual(snapshot(self.root), before, "a refused run must write nothing")
        return output


class ProducedPinsAreAcceptedByTheRealVerifiers(MiniatureTree):
    pinned = False

    def test_zero_pins_are_rejected_then_produced_pins_are_accepted(self):
        with self.assertRaises(VERIFIERS.gate.GateError):
            VERIFIERS.gate.validate_baseline(self.root, VERIFIERS.gate.load_policy(self.root / POLICY))
        with self.assertRaises(VERIFIERS.ocla.SuiteError):
            VERIFIERS.ocla.validate_contract_pack(self.root)
        with self.assertRaises(VERIFIERS.manifest.InvalidManifest):
            VERIFIERS.manifest.verify(self.root / FIXTURE, self.root, self.root / TRUST_ROOT)
        with self.assertRaises(VERIFIERS.evidence.InvalidDeliveryEvidence):
            VERIFIERS.evidence.verify(self.root / CONTRACT, self.root)

        before = snapshot(self.root)
        code, output = run("all", self.root)
        self.assertEqual(code, 1, output)
        self.assertEqual(snapshot(self.root), before, "check mode must not write")

        code, output = run("all", self.root, write=True)
        self.assertEqual(code, 0, output)
        verify_all(self.root)
        changed = {name for name, data in snapshot(self.root).items() if before.get(name) != data}
        self.assertEqual(changed, {POLICY, REPORT, PACK, FIXTURE, RECEIPT, CONTRACT})

        code, output = run("all", self.root)
        self.assertEqual(code, 0, output)
        self.assertNotIn("DRIFT", output)

    def test_each_command_alone_in_dependency_order(self):
        for command in ("contract-pack", "delivery-fixture", "delivery-evidence", "history-policy", "baseline-reattest"):
            code, output = run(command, self.root)
            self.assertEqual(code, 1, f"{command}: {output}")
            code, output = run(command, self.root, write=True)
            self.assertEqual(code, 0, f"{command}: {output}")
            code, output = run(command, self.root)
            self.assertEqual(code, 0, f"{command}: {output}")
        verify_all(self.root)

    def test_command_line_entry_point(self):
        script = str(ROOT / "scripts/pinned-evidence.py")
        check = subprocess.run([sys.executable, script, "all", "--root", str(self.root)], capture_output=True, text=True)
        self.assertEqual(check.returncode, 1, check.stdout + check.stderr)
        self.assertIn("DRIFT " + PACK, check.stdout)
        write = subprocess.run([sys.executable, script, "all", "--root", str(self.root), "--write"], capture_output=True, text=True)
        self.assertEqual(write.returncode, 0, write.stdout + write.stderr)
        verify_all(self.root)
        again = subprocess.run([sys.executable, script, "all", "--root", str(self.root)], capture_output=True, text=True)
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)

    def test_receipt_is_the_one_the_existing_fixture_test_derives(self):
        run("all", self.root, write=True)
        trust = VERIFIERS.manifest.TRUST
        manifest = self.json(FIXTURE)
        payload = trust.canonical_json(trust.promotion_payload(manifest))
        self.assertEqual(
            self.json(RECEIPT),
            {
                "algorithm": "Ed25519",
                "key_id": "sha256:" + sha256(PRODUCER.RFC8032_TEST1_PUBLIC),
                "payload_sha256": sha256(payload),
                "schema_version": "leanctx.release-signature/v1",
                "signature": base64.b64encode(trust.ed25519_sign(payload, PRODUCER.RFC8032_TEST1_SEED)).decode(),
            },
        )
        pack = self.json(PACK)
        self.assertEqual(manifest["contracts"], {
            "pack_digest": "sha256:" + sha256(VERIFIERS.manifest.canonical_json(pack)),
            "pack_version": pack["version"],
        })


class DriftThenRepair(MiniatureTree):
    def drift_then_repair(self, changed, command, field, also_changed=(), replacement=None):
        if replacement is None:
            replacement = (self.root / changed).read_bytes() + b"\n"
        (self.root / changed).write_bytes(replacement)
        with self.assertRaises(Exception):
            verify_all(self.root)
        before = snapshot(self.root)
        code, output = run(command, self.root)
        self.assertEqual(code, 1, output)
        self.assertIn(field, output)
        self.assertEqual(snapshot(self.root), before, "check mode must not write")

        code, output = run("all", self.root, write=True)
        self.assertEqual(code, 0, output)
        verify_all(self.root)
        first = snapshot(self.root)
        rewritten = {name for name, data in first.items() if before[name] != data}
        self.assertEqual(rewritten, set(also_changed))

        code, output = run("all", self.root, write=True)
        self.assertEqual(code, 0, output)
        self.assertNotIn("WROTE", output)
        self.assertEqual(snapshot(self.root), first, "a second write must be byte-identical")

    def test_scanner_source(self):
        report_before = self.json(REPORT)
        policy_before = self.json(POLICY)
        self.drift_then_repair(
            "scripts/history-policy-gate.py",
            "history-policy",
            'scanner_source_sha256["scripts/history-policy-gate.py"]',
            also_changed={POLICY, REPORT},
        )
        report, policy = self.json(REPORT), self.json(POLICY)
        self.assertNotEqual(report["policy_sha256"], report_before["policy_sha256"])
        self.assertEqual(report["policy_sha256"], VERIFIERS.gate.policy_fingerprint(policy))
        self.assertEqual({**report, "policy_sha256": None}, {**report_before, "policy_sha256": None})
        self.assertEqual(report["audited_commit"], self.base)
        self.assertEqual(policy["baseline"]["commit"], self.base)
        self.assertEqual(policy["scanner_source_paths"], policy_before["scanner_source_paths"])
        self.assertEqual(list(policy["scanner_source_sha256"]), list(policy_before["scanner_source_sha256"]))
        self.assertEqual(policy["baseline"]["report_sha256"], sha256((self.root / REPORT).read_bytes()))

    def test_baseline_reattest_names_the_fingerprint_and_refuses_stale_sources(self):
        with open(self.root / "scripts/history-policy-gate.py", "ab") as handle:
            handle.write(b"\n")
        code, output = run("baseline-reattest", self.root)
        self.assertEqual(code, 2, output)
        self.assertIn("run history-policy first", output)
        code, output = run("history-policy", self.root, write=True)
        self.assertEqual(code, 0, output)
        code, output = run("baseline-reattest", self.root)
        self.assertEqual(code, 1, output)
        self.assertIn("policy_sha256", output)
        self.assertIn("baseline.report_sha256", output)

    def test_contract_pack_artifact(self):
        pack_before = self.json(PACK)
        self.drift_then_repair(
            "docs/contracts/README.md",
            "contract-pack",
            "sha256 (docs/contracts/README.md)",
            also_changed={PACK, FIXTURE, RECEIPT, CONTRACT},
        )
        pack = self.json(PACK)
        self.assertEqual([item["path"] for item in pack["artifacts"]], [item["path"] for item in pack_before["artifacts"]])
        self.assertEqual(pack["version"], pack_before["version"])
        self.assertEqual(pack["compatibility"], pack_before["compatibility"])

    def test_fixture_evidence(self):
        sbom = PRODUCER.FIXTURE_DIR + "sbom.cdx.json"
        receipt_before = self.json(RECEIPT)
        self.drift_then_repair(
            sbom,
            "delivery-fixture",
            f"evidence.sbom.sha256 ({sbom})",
            also_changed={FIXTURE, RECEIPT, CONTRACT},
            replacement=VERIFIERS.manifest.canonical_json({**self.json(sbom), "serialNumber": "urn:uuid:changed"}),
        )
        receipt = self.json(RECEIPT)
        self.assertNotEqual(receipt["signature"], receipt_before["signature"])
        self.assertEqual(receipt["key_id"], receipt_before["key_id"])

    def test_pack_bytes_that_are_not_the_derived_form_are_reported(self):
        with open(self.root / PACK, "ab") as handle:
            handle.write(b"\n")
        code, output = run("contract-pack", self.root)
        self.assertEqual(code, 1, output)
        self.assertIn("<serialisation>", output)

    def test_delivery_evidence_source(self):
        self.drift_then_repair(
            ".github/CODEOWNERS",
            "delivery-evidence",
            "owner.source.sha256 (.github/CODEOWNERS)",
            also_changed={CONTRACT},
        )

    def test_publish_workflow_source(self):
        self.drift_then_repair(
            ".github/workflows/publish-clients.yml",
            "delivery-evidence",
            "release.publish_channels[1].source_sha256 (.github/workflows/publish-clients.yml)",
            also_changed={CONTRACT},
        )


class Refusals(MiniatureTree):
    def test_missing_scanner_source_is_an_error_not_a_dropped_path(self):
        (self.root / "tests/security/test_history_policy_gate.py").unlink()
        output = self.assert_refused("history-policy", "tests/security/test_history_policy_gate.py")
        self.assertIn("not dropped", output)
        self.assert_refused("all", "tests/security/test_history_policy_gate.py")

    def test_missing_pack_artifact(self):
        (self.root / "docs/contracts/DEPRECATION.md").unlink()
        self.assert_refused("contract-pack", "docs/contracts/DEPRECATION.md")

    def test_missing_delivery_evidence_source(self):
        (self.root / "scripts/check-release-tag.py").unlink()
        self.assert_refused("delivery-evidence", "scripts/check-release-tag.py")

    def test_missing_fixture_evidence(self):
        (self.root / PRODUCER.FIXTURE_DIR / "sbom.cdx.json").unlink()
        self.assert_refused("delivery-fixture", "sbom.cdx.json")

    def test_trust_root_that_is_not_the_test_key_is_never_signed_for(self):
        put(self.root, TRUST_ROOT, trust_root_bytes(RFC8032_TEST2_PUBLIC))
        VERIFIERS.manifest.TRUST.read_public_key(self.root / TRUST_ROOT)  # a valid trust root
        self.assert_refused("delivery-fixture", "not the RFC 8032 TEST 1 public key")
        self.assert_refused("all", "not the RFC 8032 TEST 1 public key")

    def test_the_only_seed_is_the_published_test_vector_and_cannot_be_supplied(self):
        existing = load("pe_existing_manifest_test", ROOT / "tests/delivery/test_verify_delivery_manifest.py")
        self.assertEqual(PRODUCER.RFC8032_TEST1_SEED, existing.TEST_RELEASE_SEED)
        self.assertEqual(PRODUCER.RFC8032_TEST1_PUBLIC, existing.TEST_RELEASE_PUBLIC)
        self.assertEqual(PRODUCER.test_seed(), existing.TEST_RELEASE_SEED)
        with mock.patch.object(PRODUCER, "RFC8032_TEST1_SEED", bytes(32)):
            with self.assertRaises(PRODUCER.Refusal):
                PRODUCER.test_seed()
        for option in ("--seed", "--key", "--trust-root"):
            with self.assertRaises(SystemExit), mock.patch("sys.stderr", io.StringIO()):
                PRODUCER.main(["delivery-fixture", "--root", str(self.root), option, "value"])

    def test_fixture_receipt_outside_the_fixture_directory(self):
        manifest = self.json(FIXTURE)
        put(self.root, "docs/receipt.json", (self.root / RECEIPT).read_bytes())
        manifest["evidence"]["signature"]["path"] = "docs/receipt.json"
        put(self.root, FIXTURE, VERIFIERS.manifest.canonical_json(manifest))
        self.assert_refused("delivery-fixture", "fixture receipt path is not " + RECEIPT)

    def test_output_path_refuses_everything_outside_root_and_scope(self):
        (self.outside / "pin.json").write_bytes(b"{}\n")
        (self.root / "security/linked").symlink_to(self.outside, target_is_directory=True)
        (self.root / "security/link.json").symlink_to(self.outside / "pin.json")
        for relative, scope in (
            ("../outside/pin.json", ""),
            (str(self.outside / "pin.json"), ""),
            ("security/../../outside/pin.json", "security/"),
            ("security/linked/pin.json", "security/"),
            ("security/link.json", "security/"),
            ("security/not-there.json", "security/"),
            (POLICY, PRODUCER.FIXTURE_DIR),
        ):
            with self.subTest(relative=relative, scope=scope):
                with self.assertRaises(PRODUCER.Refusal):
                    PRODUCER.output_path(self.root, relative, scope)
                with self.assertRaises(PRODUCER.Refusal):
                    PRODUCER.Tree(self.root).stage(relative, b"{}\n", scope)
        self.assertEqual(PRODUCER.output_path(self.root, POLICY, "security/"), self.root / POLICY)

    def test_baseline_report_outside_root_is_not_written(self):
        outside = self.outside / "report.json"
        outside.write_bytes((self.root / REPORT).read_bytes())
        policy = self.json(POLICY)
        policy["baseline"]["report"] = "../outside/report.json"
        put(self.root, POLICY, pretty(policy))
        self.assert_refused("baseline-reattest", "unsafe")
        self.assertEqual(outside.read_bytes(), (self.root / REPORT).read_bytes())

    def test_baseline_report_symlink_is_not_followed(self):
        outside = self.outside / "report.json"
        original = (self.root / REPORT).read_bytes()
        outside.write_bytes(original)
        (self.root / REPORT).unlink()
        (self.root / REPORT).symlink_to(outside)
        with open(self.root / "scripts/history-policy-gate.py", "ab") as handle:
            handle.write(b"\n")
        code, output = run("all", self.root, write=True)
        self.assertEqual(code, 2, output)
        self.assertIn("symlink", output)
        self.assertEqual(outside.read_bytes(), original)

    def test_baseline_commit_and_path_set_are_never_moved(self):
        policy = self.json(POLICY)
        policy["baseline"]["commit"] = "3" * 40
        put(self.root, POLICY, pretty(policy))
        self.assert_refused("baseline-reattest", "owner decision")
        code, output = run("history-policy", self.root)
        self.assertEqual(code, 0, output)  # the commit is carried, not corrected

    def test_pack_artifact_set_is_not_derived(self):
        pack = self.json(PACK)
        removed = pack["artifacts"].pop()
        put(self.root, PACK, pretty(pack))
        output = self.assert_refused("contract-pack", "the set is not derived")
        self.assertIn(removed["path"], output)

        pack["artifacts"].append(removed)
        put(self.root, "docs/contracts/extra.md", "extra\n")
        pack["artifacts"].append({"path": "docs/contracts/extra.md", "sha256": ZERO})
        put(self.root, PACK, pretty(pack))
        self.assert_refused("contract-pack", "docs/contracts/extra.md")

    def test_pack_version_moves_only_when_asked(self):
        code, output = run("contract-pack", self.root, pack_version="3.0.1")
        self.assertEqual(code, 1, output)
        self.assertIn('version: "3.0.0" -> "3.0.1"', output)
        code, output = run("all", self.root, write=True, pack_version="3.0.1")
        self.assertEqual(code, 0, output)
        pack = self.json(PACK)
        self.assertEqual(pack["version"], "3.0.1")
        self.assertEqual(pack["compatibility"]["supported"], ["3.0.1", "2.0.0"])
        self.assertEqual(self.json(FIXTURE)["contracts"]["pack_version"], "3.0.1")
        verify_all(self.root)
        code, output = run("all", self.root)
        self.assertEqual(code, 0, output)  # without the option the version on disk is carried
        self.assertEqual(self.json(PACK)["version"], "3.0.1")
        self.assert_refused("contract-pack", "contract_pack_version_invalid", pack_version="next")
        with self.assertRaises(SystemExit), mock.patch("sys.stderr", io.StringIO()):
            PRODUCER.main(["history-policy", "--root", str(self.root), "--pack-version", "3.0.1"])

    def test_unparseable_pin_file_is_refused(self):
        put(self.root, PACK, b"<<<<<<< ours\n{}\n=======\n{}\n>>>>>>> theirs\n")
        self.assert_refused("contract-pack", "JSONDecodeError")


def git_available(revision):
    try:
        return subprocess.run(
            ["git", "-C", str(ROOT), "cat-file", "-e", revision + "^{commit}"], capture_output=True
        ).returncode == 0
    except OSError:
        return False


def git_bytes(revision, relative):
    result = subprocess.run(["git", "-C", str(ROOT), "show", f"{revision}:{relative}"], capture_output=True)
    return result.stdout if result.returncode == 0 else None


def referenced_paths(value):
    if isinstance(value, dict):
        for key in ("path", "source_path"):
            if isinstance(value.get(key), str):
                yield value[key]
        for nested in value.values():
            yield from referenced_paths(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from referenced_paths(nested)


VERIFIER_SCRIPTS = (
    "scripts/history-policy-gate.py",
    "scripts/verify-ocla-contract-suite.py",
    "scripts/verify-delivery-manifest.py",
    "scripts/verify-delivery-evidence.py",
)


def materialise(revision, root):
    """Write the pin files of ``revision`` and everything they name; read-only on git."""
    policy = json.loads(git_bytes(revision, POLICY))
    pack = json.loads(git_bytes(revision, PACK))
    manifest = json.loads(git_bytes(revision, FIXTURE))
    contract = json.loads(git_bytes(revision, CONTRACT))
    report = policy["baseline"]["report"]
    wanted = {POLICY, PACK, FIXTURE, RECEIPT, TRUST_ROOT, CONTRACT, report, "rust/Cargo.toml", *VERIFIER_SCRIPTS}
    wanted.update(policy["scanner_source_sha256"])
    wanted.update(referenced_paths(pack))
    wanted.update(f"clients/rust/lean-ctx-client/contracts/{name}" for name in VERIFIERS.ocla.SCHEMA_MIRRORS)
    wanted.update(referenced_paths(manifest))
    wanted.add(manifest["configuration"]["migration"])
    wanted.update(referenced_paths(contract))
    absent = []
    for relative in sorted(wanted):
        data = git_bytes(revision, relative)
        if data is None:
            absent.append(relative)
        else:
            put(root, relative, data)
    return report, absent


# The two chains that load the history policy through the current gate.
HISTORY_CHAINS = ("history-policy", "baseline-reattest")


def predates_secret_rule_contract(root):
    """True when the tree's committed history policy uses an SEC001 rule older than
    the contract the current gate enforces (the gate validates the whole rule)."""
    try:
        PRODUCER.GATE.load_policy(root / POLICY)
    except PRODUCER.GATE.GateError as error:
        return "contract" in str(error)
    return False


class CommittedPinsAreReproducedByteForByte(unittest.TestCase):
    """On a tree whose pinned inputs are unchanged the producer changes nothing.

    A historic tree whose history policy predates the current SEC001 rule
    contract cannot be re-derived for the two history chains: there the
    producer must refuse them, never call the tree clean and write nothing,
    while the other chains are still reproduced byte for byte.
    """

    def assert_history_chains_refused(self):
        for command in HISTORY_CHAINS:
            with self.subTest(command=command, expect="refused: older SEC001 contract"):
                _, refusals = PRODUCER.derive(command, self.root)
                self.assertTrue(refusals and "contract" in " ".join(refusals), refusals)

    def setUp(self):
        if not (ROOT / ".git").exists():
            self.skipTest("not a git checkout: committed pin files cannot be read from git objects")
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.addCleanup(self.temp.cleanup)
        self.git_dir = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True,
        ).stdout.strip()

    def chains(self, revision):
        """Run each of the tree's OWN verifiers; return {command: rejection or None}."""
        report, absent = materialise(revision, self.root)
        own = load_verifiers(self.root / "scripts", "tree_" + revision[-8:].replace("/", "_"))
        policy = own.gate.load_policy(self.root / POLICY)

        def baseline():
            # The gate ends with `git cat-file -e <baseline commit>`; the
            # materialised tree is not a repository, so point git at the object
            # store the tree came from. Nothing in the gate is stubbed.
            with mock.patch.dict(os.environ, {"GIT_DIR": self.git_dir}):
                own.gate.validate_baseline(self.root, policy)

        checks = {
            "contract-pack": lambda: own.ocla.validate_contract_pack(self.root),
            "delivery-fixture": lambda: own.manifest.verify(self.root / FIXTURE, self.root, self.root / TRUST_ROOT),
            "delivery-evidence": lambda: own.evidence.verify(self.root / CONTRACT, self.root),
            "history-policy": lambda: own.gate.validate_scanner_sources(self.root, policy),
            "baseline-reattest": baseline,
        }
        verdicts = {}
        for command, check in checks.items():
            try:
                check()
                verdicts[command] = None
            except (ValueError, RuntimeError) as error:
                verdicts[command] = f"{type(error).__name__}: {error}"
        return report, absent, verdicts

    def assert_reproduced(self, revision, command):
        tree, refusals = PRODUCER.derive(command, self.root)
        self.assertEqual(refusals, [], command)
        self.assertTrue(tree.staged, command)
        for relative, derived in tree.staged.items():
            committed = git_bytes(revision, relative)
            self.assertEqual(
                sha256(derived), sha256(committed),
                f"{command}: {relative} is not reproduced byte for byte at {revision}: "
                f"{PRODUCER.drift(tree).get(relative)}",
            )

    def test_base_commit(self):
        if not git_available(BASE_COMMIT):
            self.skipTest(f"git object {BASE_COMMIT} unavailable (shallow or foreign clone)")
        _, absent, verdicts = self.chains(BASE_COMMIT)
        self.assertEqual(absent, [])
        self.assertEqual(verdicts, dict.fromkeys(verdicts), "the base commit's own verifiers accept its pins")
        older = predates_secret_rule_contract(self.root)
        for command in verdicts:
            if older and command in HISTORY_CHAINS:
                continue
            with self.subTest(command=command):
                self.assert_reproduced(BASE_COMMIT, command)
        before = snapshot(self.root)
        if older:
            self.assert_history_chains_refused()
            code, output = run("all", self.root, write=True)
            self.assertEqual(code, 2, output)
            self.assertIn("contract", output)
            self.assertEqual(snapshot(self.root), before, "a refused run writes nothing")
            return
        code, output = run("all", self.root)
        self.assertEqual(code, 0, output)
        self.assertNotIn("DRIFT", output)
        code, output = run("all", self.root, write=True)
        self.assertEqual(code, 0, output)
        self.assertEqual(snapshot(self.root), before)

    def test_snapshot_ref_agrees_with_its_own_verifiers(self):
        """Second, independent tree. Its pins are not assumed to verify.

        Per chain: where the tree's own verifier accepts the committed pins the
        producer must reproduce them byte for byte; where it rejects them the
        producer must not call the tree clean.
        """
        if not git_available(SNAPSHOT_REF):
            self.skipTest(f"git ref {SNAPSHOT_REF} unavailable (local snapshot ref, not in a clone)")
        _, absent, verdicts = self.chains(SNAPSHOT_REF)
        older = predates_secret_rule_contract(self.root)
        accepted = [command for command, rejection in verdicts.items() if rejection is None]
        rejected = {command: rejection for command, rejection in verdicts.items() if rejection}
        self.assertTrue(accepted, verdicts)
        for command in accepted:
            if older and command in HISTORY_CHAINS:
                continue
            with self.subTest(command=command):
                self.assert_reproduced(SNAPSHOT_REF, command)
        if older:
            self.assert_history_chains_refused()
        code, output = run("all", self.root)
        clean = not rejected and not older
        self.assertEqual(code == 0, clean, f"absent={absent} verdicts={verdicts} older={older}\n{output}")
        if os.environ.get("PINNED_EVIDENCE_REPORT"):
            sys.stderr.write(f"\n[snapshot {SNAPSHOT_REF}] absent={absent}\nverdicts={verdicts}\nexit={code}\n{output}")


if __name__ == "__main__":
    unittest.main()
