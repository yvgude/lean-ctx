#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""CI entry point for the provenance checker tests.

The canonical suite lives in scripts/tests/test_ip_provenance_audit.py. The
earlier tests here targeted the first checker design (content-index lineage,
signature_names), which 3ee2119a23 replaced with the fail-closed audit; they
could no longer run against any checker revision. This module re-exports the
canonical cases so `python3 -m unittest tests/test_ip_provenance_audit.py`
exercises the checker that actually ships.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

_CANONICAL = Path(__file__).resolve().parents[1] / "scripts" / "tests" / "test_ip_provenance_audit.py"
_SPEC = importlib.util.spec_from_file_location("ip_provenance_audit_canonical_tests", _CANONICAL)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"cannot load canonical provenance tests from {_CANONICAL}")
_TESTS = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_TESTS)


def load_tests(loader: unittest.TestLoader, tests: unittest.TestSuite, pattern: str | None) -> unittest.TestSuite:
    return loader.loadTestsFromModule(_TESTS)


if __name__ == "__main__":
    unittest.main()
