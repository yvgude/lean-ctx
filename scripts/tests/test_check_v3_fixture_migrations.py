#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-v3-fixture-migrations.py"
SPEC = importlib.util.spec_from_file_location("check_v3_fixture_migrations", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class V3FixtureMigrationGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        fixture = self.root / "rust/tests/fixtures/v3/execution-receipt-v1.json"
        test = self.root / "rust/tests/v3_receipt_fixture_migration.rs"
        fixture.parent.mkdir(parents=True)
        test.parent.mkdir(parents=True, exist_ok=True)
        fixture.write_text("{}\n", encoding="utf-8")
        active_fixture = (
            self.root
            / "_archive/benchmarks/efficiency/task-spine-v1/tasks/task-001/execution_receipt.json"
        )
        active_fixture.parent.mkdir(parents=True)
        active_fixture.write_text("{}\n", encoding="utf-8")
        test.write_text("// test\n", encoding="utf-8")
        matrix = {
            "schema_version": 1,
            "fixtures": [{
                "id": "compatibility-fixture",
                "family": "execution_receipt_v1",
                "path": str(fixture.relative_to(self.root)),
                "test": str(test.relative_to(self.root)),
                "migration": "read_only_compatibility_projection",
                "authority": "non_authoritative",
                "canonical_successor": "ReceiptDocumentV1",
            }, {
                "id": "task-spine-001",
                "family": "execution_receipt_v1",
                "path": str(active_fixture.relative_to(self.root)),
                "test": str(test.relative_to(self.root)),
                "migration": "read_only_compatibility_projection",
                "authority": "non_authoritative",
                "canonical_successor": "ReceiptDocumentV1",
            }],
            "unsupported": [{"family": "unsigned", "reason": "cannot promote trust"}],
        }
        path = self.root / "docs/migration/v3-fixture-matrix-v1.json"
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps(matrix), encoding="utf-8")

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def test_accepts_complete_safe_matrix(self) -> None:
        self.assertEqual(MODULE.validate(self.root), [])

    def test_rejects_authority_promotion_and_missing_fixture(self) -> None:
        path = self.root / "docs/migration/v3-fixture-matrix-v1.json"
        matrix = json.loads(path.read_text(encoding="utf-8"))
        matrix["fixtures"][0]["authority"] = "authoritative"
        matrix["fixtures"][0]["path"] = "../escape.json"
        path.write_text(json.dumps(matrix), encoding="utf-8")
        errors = MODULE.validate(self.root)
        self.assertTrue(any("non-authoritative" in error for error in errors))
        self.assertTrue(any("missing or unsafe path" in error for error in errors))

    def test_rejects_missing_required_family_and_undocumented_unsupported(self) -> None:
        path = self.root / "docs/migration/v3-fixture-matrix-v1.json"
        matrix = json.loads(path.read_text(encoding="utf-8"))
        matrix["fixtures"] = []
        matrix["unsupported"] = []
        path.write_text(json.dumps(matrix), encoding="utf-8")
        errors = MODULE.validate(self.root)
        self.assertTrue(any("missing required fixture families" in error for error in errors))
        self.assertTrue(any("must be documented" in error for error in errors))

    def test_rejects_active_receipt_fixture_missing_from_matrix(self) -> None:
        path = self.root / "docs/migration/v3-fixture-matrix-v1.json"
        matrix = json.loads(path.read_text(encoding="utf-8"))
        matrix["fixtures"] = matrix["fixtures"][:1]
        path.write_text(json.dumps(matrix), encoding="utf-8")
        errors = MODULE.validate(self.root)
        self.assertTrue(any("active v3 receipt fixtures missing" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
