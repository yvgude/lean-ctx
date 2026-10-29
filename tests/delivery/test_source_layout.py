# SPDX-License-Identifier: Apache-2.0
"""Prevent hidden remote dependencies or duplicate Core implementations."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("source_layout", ROOT / "scripts/check-source-layout.py")
layout_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(layout_check)


class SourceLayoutTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.layout = {"schema": "leanctx.source-layout/v1", "cargo_entrypoints": ["Cargo.toml"],
                       "core_manifest": "Cargo.toml", "protocol_manifest": "protocol/Cargo.toml",
                       "native_storage_manifest": "storage/Cargo.toml",
                       "commercial_core_manifest": "commercial/Cargo.toml"}
        self.base = '[package]\nname="engine"\nversion="1.0.0"\n[workspace]\nmembers=["protocol","storage","commercial"]\n'
        self.write("Cargo.toml", self.base)
        for path, name, version in [("protocol", "lean-ctx-protocol", "0.3.0"),
                                    ("storage", "leanctx-native-storage", "0.1.0"),
                                    ("commercial", "commercial-core", "0.1.0")]:
            self.write(path + "/Cargo.toml", f'[package]\nname="{name}"\nversion="{version}"\n')

    def write(self, path, text):
        p = self.root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def test_valid_shared_closure(self):
        self.assertEqual(len(layout_check.verify(self.root, self.layout)["manifests"]), 4)

    def test_root_package_can_be_an_explicit_workspace_member(self):
        self.write("Cargo.toml", self.base.replace('members=[', 'members=[".",'))
        self.assertEqual(len(layout_check.verify(self.root, self.layout)["manifests"]), 4)

    def test_target_specific_git_dependency_fails(self):
        self.write("Cargo.toml", self.base + '[target.\'cfg(windows)\'.dependencies]\nsecret={git="ssh://example.invalid/private.git"}\n')
        with self.assertRaisesRegex(ValueError, "Git dependency"):
            layout_check.verify(self.root, self.layout)

    def test_escaping_path_fails(self):
        with tempfile.TemporaryDirectory() as outside:
            Path(outside, "Cargo.toml").write_text('[package]\nname="outside"\n')
            self.write("Cargo.toml", self.base + f'[dependencies]\noutside={{path="{outside}"}}\n')
            with self.assertRaisesRegex(ValueError, "escapes"):
                layout_check.verify(self.root, self.layout)

    def test_second_core_implementation_fails(self):
        self.write("other/Cargo.toml", '[package]\nname="commercial-core"\nversion="0.1.0"\n')
        self.write("Cargo.toml", self.base + '[dependencies]\nother={path="other"}\n')
        with self.assertRaisesRegex(ValueError, "duplicate shared"):
            layout_check.verify(self.root, self.layout)

    def test_second_team_company_contracts_implementation_fails(self):
        # The private Team/Company contracts are a shared component like the
        # commercial core: one implementation, wherever a second one appears.
        self.layout["team_company_contracts_manifest"] = "contracts/Cargo.toml"
        contracts = '[package]\nname="leanctx-team-company-contracts"\nversion="0.1.0"\n'
        self.write("contracts/Cargo.toml", contracts)
        self.write("Cargo.toml", self.base + '[dependencies]\nc={path="contracts"}\n')
        self.assertIn("leanctx-team-company-contracts",
                      layout_check.verify(self.root, self.layout)["shared_components"])
        # Referenced second copy, under the same package name.
        self.write("service/vendor/Cargo.toml", contracts)
        self.write("Cargo.toml", self.base + '[dependencies]\nc={path="contracts"}\n'
                   'again={path="service/vendor", package="leanctx-team-company-contracts"}\n')
        with self.assertRaisesRegex(ValueError, "duplicate shared implementation for leanctx-team-company-contracts"):
            layout_check.verify(self.root, self.layout)
        # Renamed package that still builds the same crate identifier.
        self.write("service/vendor/Cargo.toml",
                   '[package]\nname="decisions-fork"\nversion="9.0.0"\n'
                   '[lib]\nname="leanctx_team_company_contracts"\n')
        self.write("Cargo.toml", self.base + '[dependencies]\nc={path="contracts"}\n'
                   'again={path="service/vendor", package="decisions-fork"}\n')
        with self.assertRaisesRegex(ValueError, "duplicate shared implementation"):
            layout_check.verify(self.root, self.layout)
        # Unreferenced copy: only the tree scan sees it.
        self.write("Cargo.toml", self.base + '[dependencies]\nc={path="contracts"}\n')
        self.assertTrue(layout_check.verify(self.root, self.layout))
        with self.assertRaisesRegex(ValueError, "service/vendor/Cargo.toml"):
            layout_check.verify(self.root, self.layout, tree=True)

    def test_older_vendored_or_renamed_copy_of_a_shared_crate_fails(self):
        # Neither an older version, a vendor directory nor a new package name
        # around the same crate identifier makes a second core acceptable.
        for copy in ('[package]\nname="lean-ctx-protocol"\nversion="0.1.1"\n',
                     '[package]\nname="protocol-compat"\nversion="9.0.0"\n[lib]\nname="lean_ctx_protocol"\n'):
            with self.subTest(copy=copy):
                self.write("service/vendor/lean-ctx-protocol/Cargo.toml", copy)
                self.write("Cargo.toml", self.base
                           + '[dependencies]\nold={path="service/vendor/lean-ctx-protocol"}\n')
                with self.assertRaisesRegex(ValueError, "service/vendor.*duplicate shared"):
                    layout_check.verify(self.root, self.layout)
                with self.assertRaisesRegex(ValueError, "duplicate shared"):
                    layout_check.verify(self.root, self.layout, "community")

    def test_tree_scan_finds_a_copy_no_manifest_references(self):
        self.write("service/vendor/lean-ctx-protocol/Cargo.toml",
                   '[package]\nname="lean-ctx-protocol"\nversion="0.1.1"\n')
        self.write("target/package/lean-ctx-protocol/Cargo.toml",
                   '[package]\nname="lean-ctx-protocol"\nversion="0.3.0"\n')
        self.assertTrue(layout_check.verify(self.root, self.layout)["passed"])
        with self.assertRaisesRegex(ValueError, "service/vendor.*in the tree"):
            layout_check.verify(self.root, self.layout, tree=True)

    def test_stale_git_lock_fails(self):
        self.write("Cargo.lock", 'version=4\n[[package]]\nname="old"\nsource="git+ssh://example.invalid/private.git"\n')
        with self.assertRaisesRegex(ValueError, "lockfile retains"):
            layout_check.verify(self.root, self.layout)

    def test_git_patch_is_not_a_portable_source(self):
        self.write("Cargo.toml", self.base + '[patch.crates-io]\npatched={git="https://example.invalid/private.git"}\n')
        with self.assertRaisesRegex(ValueError, "Git dependency"):
            layout_check.verify(self.root, self.layout)

    def test_community_closure_does_not_require_commercial_source(self):
        self.write("Cargo.toml", self.base.replace(',"commercial"', ''))
        (self.root / "commercial/Cargo.toml").unlink()
        result = layout_check.verify(self.root, self.layout, "community")
        self.assertEqual(len(result["manifests"]), 3)

    def test_public_projection_names_no_private_source_and_checks_community_only(self):
        self.layout.update({"cargo_entrypoints": ["Cargo.toml", "ee/service/Cargo.toml", "sdk/rust/Cargo.toml"],
                            "team_company_contracts_manifest": "ee/contracts/Cargo.toml",
                            "private_operations_root": "docs/internal", "sdk_root": "sdk"})
        public = layout_check.project(self.layout)
        self.assertNotIn("ee/", repr(public))
        self.assertNotIn("sdk", repr(public))
        self.assertNotIn("docs/internal", repr(public))
        self.write("Cargo.toml", self.base.replace(',"commercial"', ''))
        (self.root / "commercial/Cargo.toml").unlink()
        self.assertTrue(layout_check.verify(self.root, public, "community")["passed"])
        with self.assertRaisesRegex(ValueError, "community profile only"):
            layout_check.verify(self.root, public, "product")
        with self.assertRaisesRegex(ValueError, "already a projection"):
            layout_check.project(public)

    def test_community_cannot_depend_on_ee_even_via_an_alias(self):
        self.write("ee/extra/Cargo.toml", '[package]\nname="innocent-alias"\nversion="1.0.0"\n')
        self.write("Cargo.toml", self.base + '[dependencies]\nextra={path="ee/extra"}\n')
        with self.assertRaisesRegex(ValueError, "Community depends"):
            layout_check.verify(self.root, self.layout, "community")


if __name__ == "__main__":
    unittest.main()
