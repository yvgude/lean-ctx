#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Exercise the compatibility CLI; classification regressions live with its checker."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / "scripts/license-policy-check.py"


class LicensePolicyTest(unittest.TestCase):
    def run_check(self, *args):
        return subprocess.run([sys.executable, str(CHECKER), *args], capture_output=True, text=True, timeout=30)

    def test_cli_uses_the_canonical_classification_result(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            canonical = subprocess.run([
                sys.executable, str(ROOT / "scripts/ip-provenance-audit.py"),
                "--root", str(ROOT), "--output", str(report),
            ], capture_output=True, text=True, timeout=30)
            result = self.run_check("--root", str(ROOT))
            self.assertEqual(result.returncode, canonical.returncode)
            expected = json.loads(report.read_text())["errors"]
            self.assertEqual(result.stderr.splitlines(), ["license-policy: " + error for error in expected])

    def test_canonical_matrix_option_is_retained(self):
        result = self.run_check("--root", str(ROOT), "--matrix", str(ROOT / "LICENSE_MATRIX.toml"))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_external_matrix_cannot_replace_the_grant_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            matrix = Path(temporary) / "override.toml"
            matrix.write_text('version = 1\n')
            result = self.run_check("--root", str(ROOT), "--matrix", str(matrix))
            self.assertEqual(result.returncode, 1)
            self.assertIn("external overrides are not accepted", result.stderr)

    def test_missing_repository_fails_without_traceback(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_check("--root", str(Path(temporary) / "missing"))
            self.assertEqual(result.returncode, 1)
            self.assertIn("repository root is not a directory", result.stderr)
            self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
