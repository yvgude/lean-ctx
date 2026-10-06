#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Durable stdlib tests for the IP provenance audit checker."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "ip-provenance-audit.py"
FIXTURES = ROOT / "scripts" / "tests" / "fixtures" / "ip-provenance"
WORKFLOW_ROOT = ROOT / ".github" / "workflows"
GIT = shutil.which("git")

_SPEC = importlib.util.spec_from_file_location("ip_provenance_audit_target", SCRIPT)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"cannot load checker from {SCRIPT}")
CHECKER = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(CHECKER)


def _git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    env = os.environ.copy()
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(
        ["git", "-C", str(root), *args],
        check=check,
        capture_output=True,
        env=env,
        timeout=30,
    )


def _write(root: Path, relative: str, data: bytes | str) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)
    return path


def _init_repo(root: Path) -> None:
    _git(root, "init", "--quiet")


def _commit(root: Path, message: str = "fixture") -> str:
    _git(root, "add", "--all")
    _git(
        root,
        "-c",
        "user.name=IP provenance test",
        "-c",
        "user.email=ip-provenance-test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        message,
    )
    return _git(root, "rev-parse", "HEAD").stdout.decode().strip()


def _current_matrix() -> dict[str, object]:
    return CHECKER.load_toml((ROOT / "LICENSE_MATRIX.toml").read_bytes())


def _workflow_jobs(text: str) -> dict[str, str]:
    """Extract top-level job blocks without adding a YAML dependency."""
    jobs_marker = re.search(r"^jobs:\s*$", text, re.MULTILINE)
    if jobs_marker is None:
        raise AssertionError("workflow has no top-level jobs mapping")

    jobs: dict[str, list[str]] = {}
    current: str | None = None
    for line in text[jobs_marker.end() :].splitlines():
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*(?:#.*)?$", line)
        if match:
            current = match.group(1)
            jobs[current] = []
        elif current is not None:
            jobs[current].append(line)
    return {name: "\n".join(lines) for name, lines in jobs.items()}


def _workflow_needs(job: str) -> set[str]:
    match = re.search(r"^    needs:\s*(.*?)\s*$", job, re.MULTILINE)
    if match is None:
        return set()
    return set(re.findall(r"[A-Za-z0-9_-]+", match.group(1)))


class WorkflowProvenanceGateTests(unittest.TestCase):
    UPLOAD_SHA = "ea165f8d65b6e75b540449e92b4886f43607fa02"
    CHECKER_TEST = "python3 -m unittest discover -s scripts/tests -v"
    AUDIT_COMMAND = "python3 scripts/ip-provenance-audit.py --root . --release-strict"

    def test_publish_jobs_require_unconditional_strict_gate(self) -> None:
        for filename, report_name in (
            ("publish-sdk.yml", "ip-provenance-publish-sdk-report"),
            ("publish-clients.yml", "ip-provenance-publish-clients-report"),
        ):
            jobs = _workflow_jobs((WORKFLOW_ROOT / filename).read_text(encoding="utf-8"))
            self.assertIn("ip-provenance-gate", jobs, filename)
            gate = jobs["ip-provenance-gate"]
            self.assertIn("timeout-minutes:", gate)
            self.assertNotRegex(gate, re.compile(r"^    if:", re.MULTILINE))
            self.assertIn(self.CHECKER_TEST, gate)
            self.assertIn(self.AUDIT_COMMAND, " ".join(gate.split()))
            self.assertNotIn("continue-on-error: true", gate)
            self.assertNotRegex(gate, re.compile(r"\|\|\s*true"))
            self.assertIn("fetch-depth: 0", gate)
            self.assertIn(
                f'--output "$RUNNER_TEMP/{report_name}.json"',
                " ".join(gate.split()),
            )
            self.assertIn("if: always()", gate)
            upload = re.search(
                r"^\s+uses: actions/upload-artifact@([^\s]+)", gate, re.MULTILINE
            )
            self.assertIsNotNone(upload, filename)
            self.assertEqual(upload.group(1), self.UPLOAD_SHA)
            self.assertIn(f"name: {report_name}", gate)
            self.assertIn(f"path: ${{{{ runner.temp }}}}/{report_name}.json", gate)

            for job_name, job in jobs.items():
                if job_name.startswith("publish-"):
                    self.assertIn("ip-provenance-gate", _workflow_needs(job), job_name)
                    self.assertNotIn("always()", job, job_name)
                    self.assertNotIn("continue-on-error: true", job, job_name)

            expected_publish_jobs = {
                "publish-sdk.yml": {"publish-npm", "publish-pypi"},
                "publish-clients.yml": {
                    "publish-npm",
                    "publish-pypi",
                    "publish-crates",
                },
            }
            self.assertEqual(
                {name for name in jobs if name.startswith("publish-")},
                expected_publish_jobs[filename],
            )

    def test_provenance_upload_actions_remain_sha_pinned(self) -> None:
        for filename in ("publish-sdk.yml", "publish-clients.yml", "release.yml"):
            workflow = (WORKFLOW_ROOT / filename).read_text(encoding="utf-8")
            pins = re.findall(
                r"^\s+uses: actions/upload-artifact@([^\s]+)",
                workflow,
                re.MULTILINE,
            )
            self.assertTrue(pins, filename)
            self.assertEqual(pins, [self.UPLOAD_SHA] * len(pins), filename)

    def test_sdk_publish_and_sbom_generation_fail_closed(self) -> None:
        sdk = (WORKFLOW_ROOT / "publish-sdk.yml").read_text(encoding="utf-8")
        pypi = _workflow_jobs(sdk)["publish-pypi"]
        self.assertIn('if [ -z "${TWINE_PASSWORD}" ]', pypi)
        self.assertIn("exit 1", pypi)
        self.assertNotIn("skipping PyPI publish", pypi)

        ci = (WORKFLOW_ROOT / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("cargo install cargo-cyclonedx --version 0.5.9 --locked", ci)
        self.assertIn("bash scripts/generate-sbom.sh --check", ci)
        self.assertIn(
            "python3 scripts/generate-third-party-notices.py --root . --check", ci
        )

    def test_release_uploads_post_build_report_before_release_creation(self) -> None:
        jobs = _workflow_jobs((WORKFLOW_ROOT / "release.yml").read_text(encoding="utf-8"))
        release = jobs["release"]
        normalized = " ".join(release.split())
        audit_index = release.index(self.AUDIT_COMMAND)
        upload_index = release.index("uses: actions/upload-artifact@", audit_index)
        manifest_index = release.index("python3 scripts/generate-release-manifest.py")
        sign_manifest_index = release.index("name: Cosign sign release manifest")
        github_release_index = release.index("name: Create GitHub Release")
        self.assertIn(
            '--output "$RUNNER_TEMP/ip-provenance-release-build-report.json"',
            normalized,
        )
        self.assertLess(audit_index, upload_index)
        self.assertLess(manifest_index, audit_index)
        self.assertLess(upload_index, sign_manifest_index)
        self.assertLess(upload_index, github_release_index)
        self.assertIn("if: always()", release[upload_index - 100 : upload_index])
        upload = re.search(
            r"^\s+uses: actions/upload-artifact@([^\s]+)",
            release[upload_index - 100 :],
            re.MULTILINE,
        )
        self.assertIsNotNone(upload)
        self.assertEqual(upload.group(1), self.UPLOAD_SHA)
        self.assertIn("name: ip-provenance-release-build-report", release)
        self.assertIn(
            "path: ${{ runner.temp }}/ip-provenance-release-build-report.json", release
        )
        self.assertIn("ip-provenance-gate", _workflow_needs(jobs["build"]))

    def test_grammar_addon_publication_requires_strict_gate(self) -> None:
        jobs = _workflow_jobs(
            (WORKFLOW_ROOT / "grammar-addons.yml").read_text(encoding="utf-8")
        )
        gate = jobs["ip-provenance-gate"]
        self.assertNotRegex(gate, re.compile(r"^    if:", re.MULTILINE))
        self.assertIn("fetch-depth: 0", gate)
        self.assertIn(self.CHECKER_TEST, gate)
        self.assertIn(self.AUDIT_COMMAND, " ".join(gate.split()))
        self.assertNotIn("continue-on-error: true", gate)
        self.assertNotRegex(gate, re.compile(r"\|\|\s*true"))
        self.assertIn("ip-provenance-gate", _workflow_needs(jobs["publish"]))
        self.assertIn("build", _workflow_needs(jobs["publish"]))
        self.assertIn("if: always()", gate)
        self.assertIn(f"actions/upload-artifact@{self.UPLOAD_SHA}", gate)

    def test_history_dependent_workflow_jobs_use_full_checkout(self) -> None:
        ci_jobs = _workflow_jobs((WORKFLOW_ROOT / "ci.yml").read_text(encoding="utf-8"))
        self.assertIn("fetch-depth: 0", ci_jobs["ip-provenance"])
        self.assertIn("fetch-depth: 0", ci_jobs["delivery-security-gate"])

        release_jobs = _workflow_jobs(
            (WORKFLOW_ROOT / "release.yml").read_text(encoding="utf-8")
        )
        self.assertIn("fetch-depth: 0", release_jobs["ip-provenance-gate"])
        self.assertIn("fetch-depth: 0", release_jobs["release"])

    @unittest.skipUnless(GIT, "git is required")
    def test_provenance_history_anchors_are_reachable(self) -> None:
        matrix = _current_matrix()
        for key in ("pre_v4_apache_cut",):
            subprocess.run(
                [GIT, "merge-base", "--is-ancestor", str(matrix[key]), "HEAD"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
        signature_ref = str(matrix["provenance"]["cla_v1_signature_ref"])
        subprocess.run(
            [GIT, "cat-file", "-e", f"{signature_ref}^{{commit}}"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )


class CheckerUnitTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory(prefix="ip-provenance-test-")
        self.root = Path(self.tempdir.name)

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def _matrix_validation_root(self) -> Path:
        matrix = (ROOT / "LICENSE_MATRIX.toml").read_bytes()
        _write(self.root, "LICENSE_MATRIX.toml", matrix)
        for path in (
            "LICENSES/Apache-2.0.txt",
            "LICENSES/LeanCTX-Commercial-Source-License-2.0.txt",
            "THIRD_PARTY_NOTICES",
        ):
            _write(self.root, path, "fixture license text\n")
        return self.root

    def _copy_fixture(self, relative: str, destination: str | None = None) -> Path:
        source = FIXTURES / relative
        self.assertTrue(source.is_file(), relative)
        return _write(self.root, destination or relative, source.read_bytes())
    def _notice_bytes(self, status: str) -> bytes:
        raw = (
            "# LeanCTX third-party notices\n"
            "Schema: leanctx.third-party-notices/v1\n"
            f"Status: {status}\n"
            "Content-Digest-Scope: SHA-256 of this file with the Content-Digest value zeroed\n"
            f"Content-Digest: sha256:{'0' * 64}\n"
            "Source inventory: fixture\n"
            "Generation command: fixture\n"
            "Approval contract: legal/third-party-notices-approval.toml\n"
        ).encode()
        digest = hashlib.sha256(raw).hexdigest()
        return raw.replace(
            f"Content-Digest: sha256:{'0' * 64}".encode(),
            f"Content-Digest: sha256:{digest}".encode(),
            1,
        )

    def test_required_inventory_declares_preserved_legal_artifacts(self) -> None:
        required = _current_matrix()["required_artifacts"]["paths"]
        for path in CHECKER.PRESERVED_PATHS:
            with self.subTest(path=path):
                self.assertEqual(required.count(path), 1)

    def test_apache_host_profile_retains_provenance_and_forbids_private_classes(self) -> None:
        matrix, errors = CHECKER.validate_matrix(ROOT, ROOT / "LICENSE_MATRIX.toml")
        self.assertEqual(errors, [])
        self.assertEqual(matrix["distribution"], "apache-host")
        self.assertEqual(set(matrix["licenses"]), {"apache", "third_party"})
        self.assertTrue(CHECKER.APACHE_HOST_REQUIRED_ARTIFACTS.issubset(matrix["required_artifacts"]["paths"]))
        variants = []
        unknown = copy.deepcopy(matrix)
        unknown["distribution"] = "skip-legal"
        variants.append(unknown)
        missing = copy.deepcopy(matrix)
        missing["required_artifacts"]["paths"].remove("CLA.md")
        variants.append(missing)
        sbom = copy.deepcopy(matrix)
        sbom["required_artifacts"]["release_paths"] = ["other.txt"]
        variants.append(sbom)
        commercial = copy.deepcopy(matrix)
        next(r for r in commercial["rules"] if r["class"] == "commercial")["forbidden_public"] = False
        variants.append(commercial)
        for value in variants:
            with self.subTest(value=value), mock.patch.object(CHECKER, "load_toml", return_value=value):
                _, failures = CHECKER.validate_matrix(ROOT, ROOT / "LICENSE_MATRIX.toml")
                self.assertTrue(failures)

    def test_apache_host_rejects_private_implementation_and_draft_reintroduction(self) -> None:
        matrix = _current_matrix()
        paths = ["rust/src/commercial/feature.py", "LEGAL_REVIEW_REQUIRED.md", "legal/cla/CLA-v2.md", "ee/LICENSE"]
        for path in paths:
            _write(self.root, path, "# SPDX-License-Identifier: Apache-2.0\n")
        errors: list[str] = []
        CHECKER.validate_paths(self.root, paths, matrix, matrix["rules"], set(), errors)
        for path in paths:
            self.assertIn(f"{path}: private_service paths are forbidden in public tree", errors)

    def test_required_artifact_inventory_rejects_missing_and_empty_files(self) -> None:
        matrix = {"required_artifacts": {"paths": list(CHECKER.PRESERVED_PATHS)}}
        for path in CHECKER.PRESERVED_PATHS:
            _write(self.root, path, "preserved fixture\n")
        for path in CHECKER.PRESERVED_PATHS:
            with self.subTest(path=path):
                target = self.root / path
                target.unlink()
                for missing in (True, False):
                    if not missing:
                        target.write_bytes(b"")
                    errors: list[str] = []
                    CHECKER.validate_artifacts(self.root, matrix, "WORKTREE", False, errors, [])
                    self.assertEqual(errors, [f"{path}: missing required artifact"])
                target.write_text("preserved fixture\n", encoding="utf-8")
        errors = []
        CHECKER.validate_artifacts(self.root, matrix, "WORKTREE", False, errors, [])
        self.assertEqual(errors, [])

    def test_ordered_matrix_classification_and_unclassified_failure(self) -> None:
        matrix = _current_matrix()
        paths = {
            "rust/src/commercial/feature.py":
                "# SPDX-License-Identifier: LicenseRef-LeanCTX-Commercial-Source-2.0\n",
            "rust/src/runtime.py": "# SPDX-License-Identifier: Apache-2.0\n",
            "scripts/tests/fixtures/ip-provenance/source.py": "fixture\n",
            "unmapped/orphan.py": "# SPDX-License-Identifier: Apache-2.0\n",
        }
        for path, content in paths.items():
            _write(self.root, path, content)

        rules = matrix["rules"]
        errors: list[str] = []
        rows, _, _ = CHECKER.validate_paths(
            self.root, sorted(paths), matrix, rules, set(), errors
        )
        by_path = {row["path"]: row for row in rows}
        self.assertEqual(by_path["rust/src/commercial/feature.py"]["rule"], "commercial-source")
        self.assertEqual(by_path["rust/src/runtime.py"]["rule"], "public-trust-core-runtime")
        self.assertEqual(
            by_path["scripts/tests/fixtures/ip-provenance/source.py"]["rule"],
            "test-fixtures",
        )
        self.assertNotIn("unmapped/orphan.py", by_path)
        self.assertIn(
            "unmapped/orphan.py: expected exactly one matrix rule (none)", errors
        )

    def test_spdx_expressions_headers_and_injection_rejection(self) -> None:
        for expression in (
            "Apache-2.0",
            "(MIT OR Apache-2.0) AND BSD-3-Clause",
            "MIT WITH LicenseRef-Example",
            "LicenseRef-Internal-Only",
        ):
            self.assertTrue(CHECKER.parse_spdx(expression), expression)
        for expression in (
            "",
            "Not-A-License",
            "Apache-2.0 OR",
            "MIT WITH",
            "Apache-2.0 && __import__('os')",
            "Apache-2.0; rm -rf /",
            "(MIT OR Apache-2.0",
        ):
            self.assertFalse(CHECKER.parse_spdx(expression), expression)

        valid_header, valid_values = CHECKER.spdx_header(
            b"# SPDX-License-Identifier: Apache-2.0\n"
        )
        self.assertEqual(valid_header, "Apache-2.0")
        self.assertEqual(valid_values, ["Apache-2.0"])
        for raw in (
            b"# SPDX-License-Identifier:\n",
            b"# SPDX-License-Identifier: Not-A-License\n",
            b"# SPDX-License-Identifier: Apache-2.0; __import__('os')\n",
        ):
            header, values = CHECKER.spdx_header(raw)
            self.assertTrue(values)
            self.assertTrue(header is None or not CHECKER.parse_spdx(header))

        header, values = CHECKER.spdx_header(
            b"# SPDX-License-Identifier: Apache-2.0\n"
            b"# SPDX-License-Identifier: MIT\n"
        )
        self.assertEqual(header, "Apache-2.0")
        self.assertEqual(values, ["Apache-2.0", "MIT"])
        self.assertNotEqual(values[0], values[1])
        matrix = _current_matrix()
        _write(
            self.root,
            "scripts/conflicting.py",
            b"# SPDX-License-Identifier: Apache-2.0\n"
            b"# SPDX-License-Identifier: MIT\n",
        )
        errors: list[str] = []
        CHECKER.validate_paths(
            self.root,
            ["scripts/conflicting.py"],
            matrix,
            matrix["rules"],
            set(),
            errors,
        )
        self.assertIn("scripts/conflicting.py: conflicting SPDX headers", errors)

    def test_path_root_symlink_and_ref_guards(self) -> None:
        self.assertTrue(CHECKER.safe_relpath("src/main.py"))
        for value in (
            "",
            "/absolute/path",
            "../escape",
            "a/../escape",
            "a//b",
            "a\\b",
            "a\x00b",
            "a\nb",
            "a\tb",
            "a\x7fb",
        ):
            self.assertFalse(CHECKER.safe_relpath(value), repr(value))

        self.assertTrue(CHECKER.safe_ref("refs/heads/main"))
        for value in (
            "",
            "../main",
            "/main",
            "refs/heads/main?bad",
            "refs/heads/main\n",
            "a..b",
            "a" * 201,
        ):
            self.assertFalse(CHECKER.safe_ref(value), repr(value))

        outside = self.root.parent / "ip-provenance-outside.txt"
        outside.write_text("outside\n", encoding="utf-8")
        link = self.root / "link.txt"
        try:
            link.symlink_to(outside)
        except OSError as exc:
            self.fail(f"symlink test requires a supported filesystem: {exc}")
        try:
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.relative_target(self.root, "link.txt")
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.relative_target(self.root, "../ip-provenance-outside.txt")
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.relative_target(self.root, "/etc/passwd")
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.git_blob(self.root, "../bad-ref", "file.txt")
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.git_blob(self.root, "refs/heads/main", "bad\x00path")
        finally:
            link.unlink(missing_ok=True)
            outside.unlink(missing_ok=True)

    def test_bounded_file_git_input_and_malformed_toml_json(self) -> None:
        bounded = _write(self.root, "small.bin", b"1234")
        with self.assertRaisesRegex(CHECKER.AuditError, "size limit"):
            CHECKER.read_bounded(bounded, 3)

        _write(self.root, "LICENSE_MATRIX.toml", "schema = [\n")
        _, errors = CHECKER.validate_matrix(self.root, self.root / "LICENSE_MATRIX.toml")
        self.assertIn("LICENSE_MATRIX.toml: invalid TOML or unreadable matrix", errors)

        _write(self.root, "SBOM.cdx.json", "{not-json")
        self.assertEqual(
            CHECKER.validate_sbom(self.root, "SBOM.cdx.json"),
            "SBOM.cdx.json: invalid JSON",
        )

        if not GIT:
            self.skipTest("git executable is required for git input bounds")
        _init_repo(self.root)
        _write(self.root, "one.txt", "one\n")
        _write(self.root, "two.txt", "two\n")
        _commit(self.root)
        with mock.patch.object(CHECKER, "MAX_GIT_OUTPUT_BYTES", 1):
            with self.assertRaisesRegex(CHECKER.AuditError, "git output"):
                CHECKER.git(self.root, "rev-parse", "--show-toplevel")
        with self.assertRaisesRegex(CHECKER.AuditError, "file count"):
            CHECKER.enumerate_paths(self.root, None, 1)

    def test_sbom_is_portable_and_bound_to_cargo_lock(self) -> None:
        lock = b"locked dependency graph\n"
        _write(self.root, "rust/Cargo.lock", lock)
        document = {
            "bomFormat": "CycloneDX",
            "specVersion": "1.3",
            "metadata": {
                "properties": [{
                    "name": "leanctx:cargo-lock-sha256",
                    "value": hashlib.sha256(lock).hexdigest(),
                }],
            },
            "components": [{
                "type": "library",
                "name": "example",
                "version": "1.0.0",
                "bom-ref": "path+file://./rust/crates/example#1.0.0",
            }],
        }
        _write(self.root, "SBOM.cdx.json", json.dumps(document))
        self.assertIsNone(CHECKER.validate_sbom(self.root, "SBOM.cdx.json"))

        absolute = copy.deepcopy(document)
        absolute["components"][0]["bom-ref"] = "path+file:///tmp/example#1.0.0"
        _write(self.root, "SBOM.cdx.json", json.dumps(absolute))
        self.assertIn(
            "absolute local file reference",
            CHECKER.validate_sbom(self.root, "SBOM.cdx.json"),
        )

        stale = copy.deepcopy(document)
        stale["metadata"]["properties"][0]["value"] = "0" * 64
        _write(self.root, "SBOM.cdx.json", json.dumps(stale))
        self.assertIn(
            "Cargo.lock digest is stale",
            CHECKER.validate_sbom(self.root, "SBOM.cdx.json"),
        )

        duplicate = copy.deepcopy(document)
        duplicate["metadata"]["properties"].append(
            copy.deepcopy(duplicate["metadata"]["properties"][0])
        )
        _write(self.root, "SBOM.cdx.json", json.dumps(duplicate))
        self.assertIn(
            "exactly one Cargo.lock digest",
            CHECKER.validate_sbom(self.root, "SBOM.cdx.json"),
        )

    def test_immutable_cla_source_hash_and_signer_count_pins(self) -> None:
        if not GIT:
            self.skipTest("git executable is required for provenance history")
        _init_repo(self.root)
        cla = b"CLA v1 source fixture\n"
        signature = b'{"signedContributors":[{"name":"Alice"},{"name":"Bob"}]}\n'
        for path, data in {
            "LICENSE": b"Apache baseline\n",
            "NOTICE": b"Notice baseline\n",
            "CLA.md": cla,
            "signatures/v1/cla.json": signature,
        }.items():
            _write(self.root, path, data)
        source_ref = _commit(self.root, "immutable source")
        _write(self.root, "legal/cla/CLA-v1-legacy.md", cla)
        _write(self.root, "LICENSES/Apache-2.0.txt", b"Apache baseline\n")
        matrix: dict[str, object] = {
            "pre_v4_apache_cut": source_ref,
            "required_artifacts": {"paths": [], "release_paths": []},
            "provenance": {
                "cla_v1_legacy_path": "legal/cla/CLA-v1-legacy.md",
                "cla_v1_source_path": "CLA.md",
                "cla_v1_source_ref": source_ref,
                "cla_v1_sha256": hashlib.sha256(cla).hexdigest(),
                "cla_v1_signature_ref": source_ref,
                "cla_v1_signature_path": "signatures/v1/cla.json",
                "cla_v1_signature_sha256": hashlib.sha256(signature).hexdigest(),
                "cla_v1_signature_count": 2,
            },
        }
        errors: list[str] = []
        CHECKER.validate_artifacts(self.root, matrix, source_ref, False, errors, [])
        self.assertEqual(errors, [])

        self._copy_fixture(
            "cla-drift/CLA-v1-legacy.md", "legal/cla/CLA-v1-legacy.md"
        )
        errors = []
        CHECKER.validate_artifacts(self.root, matrix, source_ref, False, errors, [])
        self.assertTrue(any("CLA-v1 digest drift" in error for error in errors), errors)
        _write(self.root, "legal/cla/CLA-v1-legacy.md", cla)

        wrong_hash = copy.deepcopy(matrix)
        wrong_hash["provenance"]["cla_v1_sha256"] = "0" * 64
        errors = []
        CHECKER.validate_artifacts(self.root, wrong_hash, source_ref, False, errors, [])
        self.assertTrue(any("CLA-v1 digest drift" in error for error in errors), errors)

        wrong_count = copy.deepcopy(matrix)
        wrong_count["provenance"]["cla_v1_signature_count"] = 1
        errors = []
        CHECKER.validate_artifacts(self.root, wrong_count, source_ref, False, errors, [])
        self.assertIn("CLA-v1 signature inventory signer count drift", errors)

        validation_root = self._matrix_validation_root()
        matrix_path = validation_root / "LICENSE_MATRIX.toml"
        original = matrix_path.read_text(encoding="utf-8")
        _, errors = CHECKER.validate_matrix(validation_root, matrix_path)
        self.assertEqual(errors, [])
        for old, new, expected in (
            (
                f'cla_v1_signature_ref = "{CHECKER.CLA_V1_SIGNATURE_REF}"',
                'cla_v1_signature_ref = "refs/heads/main"',
                "immutable source",
            ),
            (
                f'cla_v1_signature_sha256 = "{CHECKER.CLA_V1_SIGNATURE_SHA256}"',
                f'cla_v1_signature_sha256 = "{"0" * 64}"',
                "signature digest is stale",
            ),
            (
                f"cla_v1_signature_count = {CHECKER.CLA_V1_SIGNATURE_COUNT}",
                f"cla_v1_signature_count = {CHECKER.CLA_V1_SIGNATURE_COUNT - 1}",
                f"signer count is not {CHECKER.CLA_V1_SIGNATURE_COUNT}",
            ),
        ):
            self.assertIn(old, original)
            matrix_path.write_text(original.replace(old, new), encoding="utf-8")
            _, errors = CHECKER.validate_matrix(validation_root, matrix_path)
            self.assertTrue(any(expected in error for error in errors), errors)

    def _export_fixture(self) -> tuple[dict[str, object], str]:
        _init_repo(self.root)
        cla = b"CLA v1 source fixture\n"
        signature = b'{"signedContributors":[{"name":"Alice"},{"name":"Bob"}]}\n'
        for path, data in {
            "LICENSE": b"Apache baseline\n",
            "NOTICE": b"Notice baseline\n",
            "CLA.md": cla,
            "signatures/v1/cla.json": signature,
            "src/lib.rs": b"// SPDX-License-Identifier: Apache-2.0\n",
        }.items():
            _write(self.root, path, data)
        cut = _commit(self.root, "pre-v4 cut")
        _write(self.root, "legal/cla/CLA-v1-legacy.md", cla)
        _write(self.root, "LICENSES/Apache-2.0.txt", b"Apache baseline\n")
        matrix: dict[str, object] = {
            "pre_v4_apache_cut": cut,
            "required_artifacts": {"paths": [], "release_paths": []},
            "provenance": {
                "cla_v1_legacy_path": "legal/cla/CLA-v1-legacy.md",
                "cla_v1_source_path": "CLA.md",
                "cla_v1_source_ref": cut,
                "cla_v1_sha256": hashlib.sha256(cla).hexdigest(),
                "cla_v1_signature_ref": cut,
                "cla_v1_signature_path": "signatures/v1/cla.json",
                "cla_v1_signature_sha256": hashlib.sha256(signature).hexdigest(),
                "cla_v1_signature_count": 2,
            },
        }
        return matrix, cut

    def test_export_manifest_stands_in_for_history(self) -> None:
        if not GIT:
            self.skipTest("git executable is required for provenance history")
        matrix, cut = self._export_fixture()
        manifest = CHECKER.build_export_manifest(self.root, matrix)
        rendered = CHECKER.render_export_manifest(manifest)
        self.assertEqual(rendered, CHECKER.render_export_manifest(
            CHECKER.build_export_manifest(self.root, matrix)
        ))
        self.assertEqual(json.loads(rendered), manifest)

        git_history = CHECKER.GitHistory(self.root)
        exported = CHECKER.ManifestHistory(json.loads(rendered))
        self.assertEqual(exported.paths(cut), git_history.paths(cut))
        for path in ("LICENSE", "NOTICE", "CLA.md"):
            self.assertEqual(exported.digest(cut, path), git_history.digest(cut, path))
        self.assertEqual(
            exported.signatures(cut, "signatures/v1/cla.json").to_dict(),
            git_history.signatures(cut, "signatures/v1/cla.json").to_dict(),
        )

        errors: list[str] = []
        CHECKER.validate_artifacts(self.root, matrix, cut, False, errors, [], exported)
        self.assertEqual(errors, [])

        with self.assertRaises(CHECKER.AuditError):
            exported.paths("0" * 40)
        with self.assertRaises(CHECKER.AuditError):
            exported.digest("0" * 40, "LICENSE")
        with self.assertRaises(CHECKER.AuditError):
            exported.digest(cut, "src/lib.rs")

    def test_tampered_export_manifest_is_rejected(self) -> None:
        if not GIT:
            self.skipTest("git executable is required for provenance history")
        matrix, cut = self._export_fixture()
        manifest = CHECKER.build_export_manifest(self.root, matrix)

        widened = copy.deepcopy(manifest)
        widened["cut_paths"].append("src/added_after_cut.rs")
        with self.assertRaises(CHECKER.AuditError):
            CHECKER.ManifestHistory(widened).paths(cut)

        forged_license = copy.deepcopy(manifest)
        forged_license["blobs"][f"{cut}:LICENSE"] = "0" * 64
        errors: list[str] = []
        CHECKER.validate_artifacts(
            self.root, matrix, cut, False, errors, [], CHECKER.ManifestHistory(forged_license)
        )
        self.assertNotEqual(errors, [])

        forged_count = copy.deepcopy(manifest)
        forged_count["cla_v1_signatures"]["signer_count"] = 1
        errors = []
        CHECKER.validate_artifacts(
            self.root, matrix, cut, False, errors, [], CHECKER.ManifestHistory(forged_count)
        )
        self.assertIn("CLA-v1 signature inventory signer count drift", errors)

        _write(self.root, CHECKER.EXPORT_MANIFEST_PATH, "{}\n")
        with self.assertRaises(CHECKER.AuditError):
            CHECKER.load_export_manifest(self.root)

    def test_committed_export_manifest_must_match_history(self) -> None:
        if not GIT:
            self.skipTest("git executable is required for provenance history")
        matrix, _ = self._export_fixture()
        errors: list[str] = []
        CHECKER.check_export_manifest(self.root, matrix, errors)
        self.assertTrue(any("missing" in error for error in errors), errors)

        manifest = CHECKER.build_export_manifest(self.root, matrix)
        _write(self.root, CHECKER.EXPORT_MANIFEST_PATH, CHECKER.render_export_manifest(manifest))
        errors = []
        CHECKER.check_export_manifest(self.root, matrix, errors)
        self.assertEqual(errors, [])

        manifest["cut_paths"] = manifest["cut_paths"][:-1]
        _write(self.root, CHECKER.EXPORT_MANIFEST_PATH, CHECKER.render_export_manifest(manifest))
        errors = []
        CHECKER.check_export_manifest(self.root, matrix, errors)
        self.assertTrue(any("differs from what history derives" in error for error in errors), errors)

    def test_invalid_spdx_fixture_is_rejected(self) -> None:
        path = "source.py"
        fixture = self._copy_fixture("invalid-spdx/source.py", path)
        invalid_header, _ = CHECKER.spdx_header(fixture.read_bytes())
        self.assertEqual(invalid_header, "Not-A-License")
        self.assertFalse(CHECKER.parse_spdx(invalid_header))
        matrix = {
            "limits": {"max_file_bytes": 1024, "max_total_bytes": 4096},
            "licenses": {"apache": {"spdx": "Apache-2.0"}},
            "new_source_requires_spdx": True,
        }
        rules = [
            {
                "name": "source",
                "class": "open_source",
                "license": "apache",
                "include": [path],
            }
        ]
        errors: list[str] = []
        CHECKER.validate_paths(self.root, [path], matrix, rules, set(), errors)
        self.assertIn(f"{path}: invalid SPDX expression", errors)
        self.assertIn(f"{path}: missing or mismatched required SPDX header", errors)

    def test_overlap_and_unclassified_fixtures_fail_classification(self) -> None:
        overlap = "overlap/source.py"
        unclassified = "unclassified/source.py"
        self._copy_fixture(overlap)
        self._copy_fixture(unclassified)
        matrix = {
            "limits": {"max_file_bytes": 1024, "max_total_bytes": 4096},
            "licenses": {"apache": {"spdx": "Apache-2.0"}},
            "new_source_requires_spdx": True,
        }
        rules = [
            {"name": "first", "class": "open_source", "license": "apache", "include": ["overlap/**"]},
            {"name": "second", "class": "open_source", "license": "apache", "include": ["overlap/source.py"]},
        ]
        errors: list[str] = []
        rows, _, _ = CHECKER.validate_paths(
            self.root, [overlap, unclassified], matrix, rules, set(), errors
        )
        self.assertEqual(rows, [])
        self.assertIn(f"{overlap}: expected exactly one matrix rule (first,second)", errors)
        self.assertIn(f"{unclassified}: expected exactly one matrix rule (none)", errors)

    def test_generated_fixture_requires_generated_provenance_rule(self) -> None:
        path = "generated/output.txt"
        self._copy_fixture("generated-missing-provenance/output.txt", path)
        matrix = {"limits": {"max_file_bytes": 1024, "max_total_bytes": 4096}}
        rules = [{"name": "wrong", "class": "open_source", "include": ["generated/**"]}]
        errors: list[str] = []
        CHECKER.validate_paths(self.root, [path], matrix, rules, set(), errors)
        self.assertIn(f"{path}: generated path lacks generated provenance rule", errors)

    def test_vendor_fixture_requires_third_party_provenance_rule(self) -> None:
        path = "vendor/vendor.txt"
        self._copy_fixture("vendor-missing-notice/vendor.txt", path)
        matrix = {"limits": {"max_file_bytes": 1024, "max_total_bytes": 4096}}
        rules = [{"name": "wrong", "class": "open_source", "include": ["vendor/**"]}]
        errors: list[str] = []
        CHECKER.validate_paths(self.root, [path], matrix, rules, set(), errors)
        self.assertIn(f"{path}: vendor path lacks third-party provenance rule", errors)

    def test_pending_legal_fixture_warns_normally_and_blocks_strict(self) -> None:
        pending = self._copy_fixture(
            "legal-pending/LEGAL_REVIEW_REQUIRED.md", "LEGAL_REVIEW_REQUIRED.md"
        ).read_text(encoding="utf-8")
        _write(
            self.root,
            "LEGAL_REVIEW_REQUIRED.md",
            pending + "\n" + "\n".join(f"{field} pending" for field in CHECKER.DECISION_FIELDS),
        )
        for path in CHECKER.LEGAL_DRAFT_PATHS:
            if path != "LEGAL_REVIEW_REQUIRED.md":
                _write(self.root, path, "NON-OPERATIVE DRAFT\n")
        warnings: list[str] = []
        errors: list[str] = []
        CHECKER.validate_legal_files(self.root, False, warnings, errors)
        self.assertEqual(errors, [])
        self.assertIn("LEGAL_REVIEW_REQUIRED.md: unresolved legal marker", warnings)
        warnings = []
        errors = []
        CHECKER.validate_legal_files(self.root, True, warnings, errors)
        self.assertEqual(warnings, [])
        self.assertIn("LEGAL_REVIEW_REQUIRED.md: unresolved legal marker", errors)

    def _approved_legal_fixture(self) -> dict[str, object]:
        # Synthetic authority for checker tests only; never release approval.
        for path in CHECKER.LEGAL_DRAFT_PATHS:
            _write(self.root, path, "Synthetic approved test artifact\n")
        _write(self.root, "LEGAL_REVIEW_REQUIRED.md", "\n".join([
            "commercial_license_status: APPROVED", "cla_v2_status: APPROVED",
            "trademarks_status: APPROVED", "approving_counsel: Synthetic test counsel",
            "approval_evidence: fixture://not-real-approval", "effective_date: 2026-01-02",
        ]) + "\n")
        return {
            "schema": "leanctx.release-legal-approval/v1",
            "approving_counsel": "Synthetic test counsel",
            "approval_evidence": "fixture://not-real-approval", "approved_at": "2026-01-01",
            "artifacts": [{"path": path, "sha256": hashlib.sha256((self.root / path).read_bytes()).hexdigest()}
                          for path in CHECKER.LEGAL_DRAFT_PATHS],
        }

    def _write_legal_approval(self, approval: dict[str, object]) -> None:
        lines = [f"{key} = {json.dumps(value)}" for key, value in approval.items() if key != "artifacts"]
        for artifact in approval["artifacts"]:
            lines += ["[[artifacts]]", *(f"{key} = {json.dumps(value)}" for key, value in artifact.items())]
        _write(self.root, "legal/release-approval.toml", "\n".join(lines) + "\n")

    def test_approved_legal_transition_requires_exact_current_authority(self) -> None:
        approval = self._approved_legal_fixture()
        self._write_legal_approval(approval)
        for strict in (False, True):
            for fallback in (False, True):
                errors: list[str] = []
                with mock.patch.object(CHECKER, "tomllib", None if fallback else CHECKER.tomllib):
                    CHECKER.validate_legal_files(self.root, strict, [], errors)
                self.assertEqual(errors, [], (strict, fallback))
        (self.root / "TRADEMARKS.md").write_text("Changed after approval\n")
        errors = []
        CHECKER.validate_legal_files(self.root, True, [], errors)
        self.assertTrue(any("digest" in error for error in errors), errors)

    def test_operative_legal_documents_refuse_absent_or_invalid_approval(self) -> None:
        approval = self._approved_legal_fixture()
        errors: list[str] = []
        CHECKER.validate_legal_files(self.root, True, [], errors)
        self.assertTrue(errors)
        changes = [
            {"schema": "wrong"}, {"approving_counsel": "NOT RECORDED"},
            {"approval_evidence": "another-reference"}, {"approved_at": "2026-02-30"},
            {"artifacts": approval["artifacts"][:-1]},
            {"artifacts": approval["artifacts"] + [approval["artifacts"][0]]},
            {"artifacts": [{"path": "../escape", "sha256": "0" * 64}]},
        ]
        for change in changes:
            with self.subTest(change=change):
                self._write_legal_approval(dict(approval, **change))
                errors = []
                CHECKER.validate_legal_files(self.root, True, [], errors)
                self.assertTrue(errors)
        self._write_legal_approval(approval)
        _write(self.root, "TRADEMARKS.md", "NON-OPERATIVE DRAFT\n")
        errors = []
        CHECKER.validate_legal_files(self.root, True, [], errors)
        self.assertTrue(any("unresolved legal marker" in error for error in errors))

    def test_missing_notice_blocks_notice_validation(self) -> None:
        warnings: list[str] = []
        errors: list[str] = []
        self.assertIsNone(CHECKER.validate_notice(self.root, False, warnings, errors))
        self.assertEqual(warnings, [])
        self.assertIn("THIRD_PARTY_NOTICES: file is missing", errors)
    def test_notice_approval_is_exact_digest_bound_and_fail_closed(self) -> None:
        pending = self._notice_bytes("GENERATED - PENDING APPROVAL")
        _write(self.root, "THIRD_PARTY_NOTICES", pending)
        warnings: list[str] = []
        errors: list[str] = []
        self.assertEqual(
            CHECKER.validate_notice(self.root, False, warnings, errors),
            "GENERATED - PENDING APPROVAL",
        )
        self.assertEqual(errors, [])
        self.assertTrue(warnings)
        warnings = []
        errors = []
        CHECKER.validate_notice(self.root, True, warnings, errors)
        self.assertEqual(warnings, [])
        self.assertTrue(any("await exact-digest legal approval" in error for error in errors))

        approved = self._notice_bytes("GENERATED AND APPROVED")
        _write(self.root, "THIRD_PARTY_NOTICES", approved)
        errors = []
        CHECKER.validate_notice(self.root, True, [], errors)
        self.assertIn("approval evidence missing", " ".join(errors))

        approval = (
            "schema = \"leanctx.third-party-notices-approval/v1\"\n"
            f"approved_content_sha256 = \"{'0' * 64}\"\n"
            "approving_counsel = \"Counsel\"\n"
            "approval_evidence = \"review-record\"\n"
            "approved_at = \"2026-09-06\"\n"
        )
        _write(self.root, CHECKER.NOTICE_APPROVAL_PATH, approval)
        errors = []
        CHECKER.validate_notice(self.root, True, [], errors)
        self.assertIn("approval digest is stale", " ".join(errors))

        _write(
            self.root,
            CHECKER.NOTICE_APPROVAL_PATH,
            approval.replace("0" * 64, hashlib.sha256(approved).hexdigest()),
        )
        errors = []
        self.assertEqual(
            CHECKER.validate_notice(self.root, True, [], errors),
            "GENERATED AND APPROVED",
        )
        self.assertEqual(errors, [])

        mutated = approved.replace(b"Source inventory: fixture", b"Source inventory: drift")
        _write(self.root, "THIRD_PARTY_NOTICES", mutated)
        errors = []
        CHECKER.validate_notice(self.root, True, [], errors)
        self.assertIn("content digest is invalid", " ".join(errors))

    def test_notice_approval_accepts_exactly_one_owner_or_counsel(self) -> None:
        approved = self._notice_bytes("GENERATED AND APPROVED")
        _write(self.root, "THIRD_PARTY_NOTICES", approved)
        base = (
            "schema = \"leanctx.third-party-notices-approval/v1\"\n"
            f"approved_content_sha256 = \"{hashlib.sha256(approved).hexdigest()}\"\n"
            "approval_evidence = \"review-record\"\n"
            "approved_at = \"2026-09-28\"\n"
        )
        cases = {
            "approving_owner = \"Owner\"\n": None,
            "": "approving_counsel or approving_owner is missing",
            "approving_owner = \"  \"\n": "approving_counsel or approving_owner is missing",
            "approving_owner = \"Owner\"\napproving_counsel = \"Counsel\"\n": "exactly one approver",
        }
        for approver, expected in cases.items():
            with self.subTest(approver=approver):
                _write(self.root, CHECKER.NOTICE_APPROVAL_PATH, base + approver)
                errors: list[str] = []
                CHECKER.validate_notice(self.root, True, [], errors)
                if expected is None:
                    self.assertEqual(errors, [])
                else:
                    self.assertIn(expected, " ".join(errors))

    def test_traversal_fixture_cannot_escape_root(self) -> None:
        outside = self.root.parent / "ip-provenance-traversal-fixture.txt"
        shutil.copyfile(FIXTURES / "traversal/target.txt", outside)
        link = self.root / "escape.txt"
        try:
            link.symlink_to(outside)
            with self.assertRaises(CHECKER.AuditError):
                CHECKER.relative_target(self.root, "escape.txt")
        finally:
            link.unlink(missing_ok=True)
            outside.unlink(missing_ok=True)

    def test_positive_fixture_is_classified_without_errors(self) -> None:
        path = "docs/README.md"
        self._copy_fixture("positive/README.md", path)
        matrix = {"limits": {"max_file_bytes": 1024, "max_total_bytes": 4096}}
        rules = [{"name": "docs", "class": "open_source", "fixture": True, "include": ["docs/**"]}]
        errors: list[str] = []
        rows, _, _ = CHECKER.validate_paths(self.root, [path], matrix, rules, set(), errors)
        self.assertEqual(errors, [])
        self.assertEqual(rows[0]["rule"], "docs")

    def test_sorted_json_output_is_byte_deterministic(self) -> None:
        synthetic = CHECKER.make_report(
            None,
            False,
            ["a.py", "b.py"],
            [
                {"path": "a.py", "rule": "source", "class": "open_source"},
                {"path": "b.py", "rule": "source", "class": "open_source"},
            ],
            ["z-error", "a-error", "z-error"],
            ["z-warning", "a-warning", "z-warning"],
        )
        with tempfile.TemporaryDirectory(prefix="ip-provenance-reports-") as report_dir:
            first = Path(report_dir) / "first.json"
            second = Path(report_dir) / "second.json"
            with (
                mock.patch.object(CHECKER, "run_audit", return_value=synthetic),
                mock.patch("builtins.print"),
            ):
                for output in (first, second):
                    self.assertEqual(
                        CHECKER.main(["--root", str(self.root), "--output", str(output)]),
                        1,
                    )
            first_bytes = first.read_bytes()
            second_bytes = second.read_bytes()
            self.assertEqual(first_bytes, second_bytes)
            report = json.loads(first_bytes)
            self.assertEqual(report["errors"], sorted(report["errors"]))
            self.assertEqual(report["warnings"], sorted(report["warnings"]))
            paths = [row["path"] for row in report["classification"]]
            self.assertEqual(paths, sorted(paths))
            self.assertNotIn("timestamp", report)
            self.assertNotIn("random", report)
            self.assertEqual(
                first_bytes.decode(), json.dumps(report, indent=2, sort_keys=True) + "\n"
            )


if __name__ == "__main__":
    unittest.main()
