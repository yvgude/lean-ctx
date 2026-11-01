# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import unittest
from importlib import util
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check-package-versions.py"
SPEC = util.spec_from_file_location("check_package_versions", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GATE = util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)

CHANGELOG = """# Changelog

## [Unreleased]

## [3.12.0] — 2026-10-28

## [3.11.4] — 2026-10-11

## [3.10.5] — 2026-09-27
"""


class AurPkgbuildCouplingTest(unittest.TestCase):
    """The AUR recipe may trail the engine by one release, never more."""

    def test_engine_version_and_the_release_before_it_are_allowed(self) -> None:
        self.assertEqual(GATE.aur_allowed_versions("3.12.1", CHANGELOG), ["3.12.1", "3.12.0"])
        self.assertEqual(GATE.aur_allowed_versions("3.12.0", CHANGELOG), ["3.12.0", "3.11.4"])

    def test_an_older_recipe_is_drift(self) -> None:
        self.assertNotIn("3.10.5", GATE.aur_allowed_versions("3.12.0", CHANGELOG))

    def test_pkgver_is_read_from_the_recipe(self) -> None:
        self.assertEqual(GATE.aur_pkgver("pkgname=lean-ctx\npkgver=3.12.0\npkgrel=1\n"), "3.12.0")
        self.assertIsNone(GATE.aur_pkgver("pkgver=$(cat VERSION)\n"))

    def test_the_committed_recipe_is_in_range(self) -> None:
        root = SCRIPT.parents[1]
        allowed = GATE.aur_allowed_versions(
            GATE.engine_version(), (root / "CHANGELOG.md").read_text(encoding="utf-8")
        )
        pkgbuild = (root / GATE.AUR_PKGBUILD).read_text(encoding="utf-8")
        self.assertIn(GATE.aur_pkgver(pkgbuild), allowed)


if __name__ == "__main__":
    unittest.main()
