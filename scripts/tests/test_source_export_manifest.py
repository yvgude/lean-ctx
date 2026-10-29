#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Isolated Git-tree tests for source export manifest binding."""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


def _project_root() -> Path:
    for parent in Path(__file__).resolve().parents:
        if (parent / ".git").exists():
            return parent
    raise RuntimeError("cannot locate the project Git root")


ROOT = _project_root()
AUDIT_PATH = Path(
    os.environ.get("LEANCTX_IP_PROVENANCE_AUDIT", ROOT / "scripts" / "ip-provenance-audit.py")
).resolve()
os.environ["LEANCTX_IP_PROVENANCE_AUDIT"] = str(AUDIT_PATH)
SCRIPT = Path(__file__).resolve().parents[1] / "source-export-manifest.py"
_SPEC = importlib.util.spec_from_file_location("source_export_manifest_target", SCRIPT)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"cannot load manifest validator from {SCRIPT}")
TOOL = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(TOOL)
GIT = shutil.which("git")
SOURCE_BYTES = b"// SPDX-License-" b"Identifier: Apache-2.0\npub fn admitted() {}\n"


def _git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    env = os.environ.copy()
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    return subprocess.run(
        [GIT or "git", "-c", f"core.hooksPath={os.devnull}", "-c", "commit.gpgsign=false", "-C", str(root), *args],
        check=check,
        capture_output=True,
        env=env,
        timeout=30,
    )


def _write(root: Path, relative: str, raw: bytes) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(raw)


def _commit(root: Path, message: str) -> str:
    _git(root, "add", "--all")
    _git(
        root,
        "-c",
        "user.name=Source export test",
        "-c",
        "user.email=source-export-test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        message,
    )
    return _git(root, "rev-parse", "HEAD").stdout.decode().strip()


class SourceExportManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.source = base / "source"
        self.target = base / "target"
        self.source.mkdir()
        self.target.mkdir()
        _git(self.source, "init", "--quiet")
        _git(self.target, "init", "--quiet")
        _write(self.source, "src/lib.rs", SOURCE_BYTES)
        _write(self.source, "src/other.rs", SOURCE_BYTES.replace(b"admitted", b"other"))
        _write(self.source, "LICENSE", b"Apache License 2.0 test evidence\n")
        _write(self.target, "ee/lib.rs", SOURCE_BYTES)
        self.source_commit = _commit(self.source, "source baseline")
        self.target_commit = _commit(self.target, "candidate baseline")
        self.plan = {
            "schema": "leanctx.source-export-plan/v1",
            "target_origin": "engine-candidate",
            "managed_target_roots": ["ee"],
            "sources": [{
                "id": "engine",
                "origin": "gitlab-project-5",
                "ref": self.source_commit,
            }],
            "files": [{
                "source_id": "engine",
                "source_path": "src/lib.rs",
                "target_path": "ee/lib.rs",
                "source_group": "existing_apache_core",
                "spdx": "Apache-2.0",
                "approval_ref": "review:source-lib",
            }],
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def capture(self, plan: dict | None = None) -> dict:
        return TOOL._capture(
            copy.deepcopy(self.plan if plan is None else plan),
            [f"engine={self.source}"],
        )

    def approvals_for(self, manifest: dict, status: str = "approved") -> dict:
        sources = {row["id"]: row for row in manifest["sources"]}
        rows = []
        for entry in manifest["files"]:
            source = sources[entry["source_id"]]
            rows.append({
                "ref": entry["approval_ref"],
                "status": status,
                "source_id": entry["source_id"],
                "source_origin": source["origin"],
                "source_commit": source["commit"],
                "target_origin": manifest["target_origin"],
                "source_path": entry["source_path"],
                "source_sha256": entry["source_sha256"],
                "spdx": entry["spdx"],
                "spdx_basis": entry["spdx_basis"],
                "spdx_claim": entry["spdx_claim"],
                "spdx_evidence_path": entry["spdx_evidence_path"],
                "spdx_evidence_sha256": entry["spdx_evidence_sha256"],
                "source_group": entry["source_group"],
                "target_path": entry["target_path"],
            })
        return {"schema": TOOL.APPROVAL_SCHEMA, "approvals": rows}

    def verify(
        self,
        manifest: dict,
        approvals: dict,
        target_ref: str | None = None,
        target_origin: str = "engine-candidate",
    ) -> dict:
        ledger_path = Path(self.temp.name) / "approvals.json"
        ledger_path.write_text(json.dumps(approvals), encoding="utf-8")
        return TOOL.verify_manifest(
            manifest,
            ledger_path,
            [f"engine={self.source}"],
            self.target,
            target_origin,
            target_ref or self.target_commit,
        )

    def test_json_duplicate_keys_are_rejected(self) -> None:
        path = Path(self.temp.name) / "duplicate.json"
        path.write_text('{"schema":"first","schema":"second"}', encoding="utf-8")
        with self.assertRaises(TOOL.ExportError):
            TOOL._read_json(path)

    def test_json_reads_are_bounded_and_output_create_is_exclusive(self) -> None:
        too_large = Path(self.temp.name) / "large.json"
        too_large.write_bytes(b'{"ok":true}')
        with self.assertRaises(TOOL.ExportError):
            TOOL._read_json(too_large, limit=4)

        symlink = Path(self.temp.name) / "linked.json"
        symlink.symlink_to(too_large)
        with self.assertRaises(TOOL.ExportError):
            TOOL._read_json(symlink)

        existing = Path(self.temp.name) / "existing.json"
        existing.write_text("preserve", encoding="utf-8")
        with self.assertRaises(TOOL.ExportError):
            TOOL._write_json(existing, {"candidate": True})
        self.assertEqual(existing.read_text(encoding="utf-8"), "preserve")

    def test_manifest_hashes_and_validation_use_exact_pinned_blobs(self) -> None:
        manifest = self.capture()
        self.assertEqual(manifest["files"][0]["source_sha256"], hashlib.sha256(SOURCE_BYTES).hexdigest())
        self.assertEqual(manifest["files"][0]["spdx_claim"], "observed_header")

        # A dirty source and target worktree must not be mixed into either pinned ref.
        _write(self.source, "src/lib.rs", b"dirty source bytes")
        _write(self.target, "ee/lib.rs", b"dirty candidate bytes")
        report = self.verify(manifest, self.approvals_for(manifest))
        self.assertEqual(report["result"], "PASS")
        self.assertEqual(report["target"]["commit"], self.target_commit)
        self.assertEqual(report["source_files_verified"], 1)

    def test_moving_source_ref_is_rejected(self) -> None:
        _git(self.source, "branch", "moving", self.source_commit)
        plan = copy.deepcopy(self.plan)
        plan["sources"][0]["ref"] = "moving"
        manifest = self.capture(plan)
        approvals = self.approvals_for(manifest)

        _write(self.source, "src/lib.rs", SOURCE_BYTES.replace(b"admitted", b"changed"))
        moved_to = _commit(self.source, "move source ref")
        _git(self.source, "update-ref", "refs/heads/moving", moved_to)
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, approvals)

    def test_ref_and_digest_mismatch_fail_closed(self) -> None:
        manifest = self.capture()
        changed = copy.deepcopy(manifest)
        changed["sources"][0]["commit"] = "0" * 40
        with self.assertRaises(TOOL.ExportError):
            self.verify(changed, self.approvals_for(changed))

        changed = copy.deepcopy(manifest)
        changed["files"][0]["source_sha256"] = "0" * 64
        with self.assertRaises(TOOL.ExportError):
            self.verify(changed, self.approvals_for(changed))

    def test_approval_is_required_and_bound_to_the_exact_row(self) -> None:
        manifest = self.capture()
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, {"schema": TOOL.APPROVAL_SCHEMA, "approvals": []})
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, self.approvals_for(manifest, status="pending"))

        approvals = self.approvals_for(manifest)
        approvals["approvals"][0]["source_sha256"] = "0" * 64
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, approvals)

        for field, value in (
            ("source_origin", "different-source"),
            ("target_origin", "different-target"),
            ("spdx_basis", "referenced-source-file"),
            ("spdx_claim", "plan_declared"),
            ("spdx_evidence_path", "LICENSE"),
            ("spdx_evidence_sha256", "0" * 64),
        ):
            approvals = self.approvals_for(manifest)
            approvals["approvals"][0][field] = value
            with self.assertRaises(TOOL.ExportError):
                self.verify(manifest, approvals)

        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, self.approvals_for(manifest), target_origin="different-target")

        no_ref = copy.deepcopy(manifest)
        no_ref["files"][0]["approval_ref"] = None
        with self.assertRaises(TOOL.ExportError):
            self.verify(no_ref, {"schema": TOOL.APPROVAL_SCHEMA, "approvals": []})

    def test_path_escape_and_target_collision_are_rejected(self) -> None:
        escaped = copy.deepcopy(self.plan)
        escaped["files"][0]["target_path"] = "../outside.rs"
        with self.assertRaises(TOOL.ExportError):
            self.capture(escaped)

        outside = copy.deepcopy(self.plan)
        outside["managed_target_roots"] = ["docs"]
        with self.assertRaises(TOOL.ExportError):
            self.capture(outside)

        collision = copy.deepcopy(self.plan)
        second = copy.deepcopy(collision["files"][0])
        second.update({
            "source_path": "src/other.rs",
            "approval_ref": "review:other",
        })
        collision["files"].append(second)
        with self.assertRaises(TOOL.ExportError):
            self.capture(collision)

    def test_missing_or_mismatched_target_file_is_rejected(self) -> None:
        manifest = self.capture()
        _git(self.target, "rm", "--quiet", "ee/lib.rs")
        missing_commit = _commit(self.target, "remove mapped file")
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, self.approvals_for(manifest), missing_commit)

        _write(self.target, "ee/lib.rs", b"not the admitted source bytes")
        changed_commit = _commit(self.target, "change mapped file")
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, self.approvals_for(manifest), changed_commit)

    def test_spdx_header_cannot_be_reclassified_as_plan_declared(self) -> None:
        manifest = self.capture()
        entry = manifest["files"][0]
        entry["spdx_basis"] = "referenced-source-file"
        entry["spdx_claim"] = "plan_declared"
        entry["spdx_evidence_path"] = "LICENSE"
        entry["spdx_evidence_sha256"] = hashlib.sha256(b"Apache License 2.0 test evidence\n").hexdigest()
        with self.assertRaises(TOOL.ExportError):
            self.verify(manifest, self.approvals_for(manifest))

    def test_headerless_spdx_is_marked_as_plan_declared_with_evidence_digest(self) -> None:
        plain = b"legacy source with no SPDX header\n"
        _write(self.source, "src/legacy.rs", plain)
        _write(self.target, "ee/legacy.rs", plain)
        source_commit = _commit(self.source, "headerless source")
        target_commit = _commit(self.target, "headerless target")
        plan = copy.deepcopy(self.plan)
        plan["sources"][0]["ref"] = source_commit
        plan["files"].append({
            "source_id": "engine",
            "source_path": "src/legacy.rs",
            "target_path": "ee/legacy.rs",
            "source_group": "ee/intelligence",
            "spdx": "Apache-2.0",
            "license_evidence_path": "LICENSE",
            "approval_ref": "review:legacy",
        })
        manifest = self.capture(plan)
        entry = next(row for row in manifest["files"] if row["source_path"] == "src/legacy.rs")
        self.assertEqual(entry["spdx_basis"], "referenced-source-file")
        self.assertEqual(entry["spdx_claim"], "plan_declared")
        self.assertEqual(entry["spdx_evidence_sha256"], hashlib.sha256(b"Apache License 2.0 test evidence\n").hexdigest())
        report = self.verify(manifest, self.approvals_for(manifest), target_commit)
        self.assertEqual(report["result"], "PASS")


if __name__ == "__main__":
    unittest.main()
