# SPDX-License-Identifier: Apache-2.0
"""CI diff check (mz6/mz14 T4): the bundled model catalog equals the vendored
Hive catalog. Runs in the v4-provenance job through `unittest discover`."""

from __future__ import annotations

import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from importlib import util
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "sync-model-catalog.py"
SPEC = util.spec_from_file_location("sync_model_catalog", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
SYNC = util.module_from_spec(SPEC)
SPEC.loader.exec_module(SYNC)


def check() -> int:
    with mock.patch.object(sys, "argv", ["sync-model-catalog.py", "--check"]):
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            return SYNC.main()


class ModelCatalogBundleTest(unittest.TestCase):
    def test_bundle_equals_the_vendored_hive_catalog(self) -> None:
        self.assertEqual(check(), 0)

    def test_a_drifted_bundle_or_edited_vendored_copy_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bundle = Path(tmp) / "model_catalog.json"
            bundle.write_text(SYNC.BUNDLE.read_text(encoding="utf-8").replace('"CN"', '"US"'), encoding="utf-8")
            with mock.patch.object(SYNC, "BUNDLE", bundle), mock.patch.object(SYNC, "REPO", Path(tmp)):
                self.assertEqual(check(), 1)
            vendored = Path(tmp) / "model_catalog.hive.toml"
            vendored.write_bytes(SYNC.VENDORED.read_bytes() + b"\n# hand edit\n")
            with mock.patch.object(SYNC, "VENDORED", vendored), mock.patch.object(SYNC, "REPO", Path(tmp)):
                self.assertEqual(check(), 1)


if __name__ == "__main__":
    unittest.main()
