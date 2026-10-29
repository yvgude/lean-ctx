# SPDX-License-Identifier: Apache-2.0
"""SDK relocation must not allow duplicate packages or disguise a missing SDK."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("sdk_surface", ROOT / "scripts/check-sdk-surface.py")
surface = importlib.util.module_from_spec(spec)
spec.loader.exec_module(surface)


class SdkSurfaceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        original = surface.ROOT
        surface.ROOT = self.root
        self.addCleanup(setattr, surface, "ROOT", original)
        self.write("docs/reference/sdk-surface.md", "One SDK: thinkery-leanctx-sdk in sdk/.")
        self.write("sdk/setup.cfg", "[metadata]\nname = thinkery-leanctx-sdk\n")
        self.write("sdk/packages/typescript/package.json", '{"name":"@thinkery/leanctx-sdk"}')
        self.write("sdk/packages/rust/Cargo.toml", '[package]\nname = "thinkery-leanctx-sdk"\n')

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def check(self):
        surface.FAILURES.clear()
        surface.check_npm_manifests()
        surface.check_python_manifests()
        surface.check_cargo_manifests()
        surface.check_surface_doc()
        return surface.FAILURES[:]

    def test_canonical_sdk_names_allowed(self):
        self.assertEqual(self.check(), [])

    def test_duplicate_python_sdk_rejected(self):
        self.write("another/setup.cfg", "[metadata]\nname = thinkery-leanctx-sdk\n")
        self.assertTrue(any("duplicate" in item for item in self.check()))

    def test_duplicate_scoped_typescript_sdk_rejected(self):
        self.write("packages/other/package.json", '{"name":"@thinkery/leanctx-sdk"}')
        self.assertTrue(any("duplicate" in item for item in self.check()))

    def test_canonical_identity_cannot_be_renamed(self):
        self.write("sdk/packages/rust/Cargo.toml", '[package]\nname = "other"\n')
        self.assertTrue(any("expected canonical" in item for item in self.check()))

    def test_canonical_manifest_cannot_be_omitted(self):
        (self.root / "sdk/setup.cfg").unlink()
        self.assertTrue(any("manifest is missing" in item for item in self.check()))

    def test_canonical_rust_manifest_cannot_omit_package_identity(self):
        self.write("sdk/packages/rust/Cargo.toml", "[workspace]\n")
        self.assertTrue(any("expected canonical" in item for item in self.check()))

    def test_invalid_canonical_json_cannot_pass(self):
        self.write("sdk/packages/typescript/package.json", "{")
        self.assertTrue(any("invalid package manifest" in item for item in self.check()))


if __name__ == "__main__":
    unittest.main()
