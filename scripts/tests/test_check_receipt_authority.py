# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import tempfile
import unittest
from importlib import util
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-receipt-authority.py"
SPEC = util.spec_from_file_location("receipt_authority", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GATE = util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class ReceiptAuthorityTest(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path]:
        directory = tempfile.TemporaryDirectory()
        root = Path(directory.name)
        for relative in GATE.LEGACY_CONSTRUCTORS:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("", encoding="utf-8")
        return directory, root

    def test_accepts_isolated_legacy_definitions(self) -> None:
        directory, root = self.fixture()
        with directory:
            self.assertEqual(GATE.validate(root), [])

    def test_rejects_new_legacy_constructor(self) -> None:
        directory, root = self.fixture()
        with directory:
            path = root / "src/new_writer.rs"
            path.write_text("let receipt = ExecutionReceiptV1 { };", encoding="utf-8")
            self.assertIn("unauthorized legacy receipt constructor", GATE.validate(root)[0])

    def test_rejects_legacy_builder_caller(self) -> None:
        directory, root = self.fixture()
        with directory:
            path = root / "src/new_caller.rs"
            path.write_text('ReceiptBuilder::new("task");', encoding="utf-8")
            self.assertIn("production caller", GATE.validate(root)[0])

    def test_rejects_legacy_arm_caller(self) -> None:
        directory, root = self.fixture()
        with directory:
            path = root / "src/caller.rs"
            path.write_text('arm_to_receipt(&arm, "run", "plan");', encoding="utf-8")
            self.assertIn("legacy arm_to_receipt", GATE.validate(root)[0])


if __name__ == "__main__":
    unittest.main()
