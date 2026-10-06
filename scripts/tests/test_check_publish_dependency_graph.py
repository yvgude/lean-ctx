# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from importlib import util
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "check-publish-dependency-graph.py"
SPEC = util.spec_from_file_location("publish_dependency_graph", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GATE = util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


class PublishDependencyGraphTest(unittest.TestCase):
    def run_gate(self, root_manifest: str, child_manifest: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text(root_manifest, encoding="utf-8")
            child = root / "child"
            child.mkdir()
            (child / "Cargo.toml").write_text(child_manifest, encoding="utf-8")
            (root / "src").mkdir()
            (root / "src" / "lib.rs").write_text("", encoding="utf-8")
            (child / "src").mkdir()
            (child / "src" / "lib.rs").write_text("", encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), "--workspace-root", str(root)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_accepts_versioned_publishable_dependency(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[package]
name = "root"
version = "1.0.0"
[dependencies]
child = { path = "child", version = "1.0.0" }
""",
            '[package]\nname = "child"\nversion = "1.0.0"\n',
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_path_only_dependency(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[package]
name = "root"
version = "1.0.0"
[dependencies]
child = { path = "child" }
""",
            '[package]\nname = "child"\nversion = "1.0.0"\n',
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("lacks a crates.io version", result.stderr)

    def test_rejects_non_publishable_dependency(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[package]
name = "root"
version = "1.0.0"
[target.'cfg(unix)'.build-dependencies]
renamed = { package = "child", path = "child", version = "1.0.0" }
""",
            '[package]\nname = "child"\nversion = "1.0.0"\npublish = false\n',
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("depends on non-publishable package child", result.stderr)

    def test_ignores_dependencies_of_private_packages(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[package]
name = "root"
version = "1.0.0"
publish = false
[dependencies]
child = { path = "child" }
""",
            '[package]\nname = "child"\nversion = "1.0.0"\npublish = false\n',
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ignores_dev_only_path_dependency(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[package]
name = "root"
version = "1.0.0"
[dev-dependencies]
child = { path = "child" }
""",
            '[package]\nname = "child"\nversion = "1.0.0"\npublish = false\n',
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_resolves_workspace_inherited_dependency(self) -> None:
        result = self.run_gate(
            """
[workspace]
members = [".", "child"]
[workspace.dependencies]
child = { path = "child", version = "1.0.0" }
[package]
name = "root"
version = "1.0.0"
[dependencies]
child.workspace = true
""",
            '[package]\nname = "child"\nversion = "1.0.0"\npublish = false\n',
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("depends on non-publishable package child", result.stderr)

    def test_rejects_missing_dependency_list(self) -> None:
        metadata = {
            "workspace_members": ["root 1.0.0"],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": None,
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "has no dependency list"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_missing_path_requirement(self) -> None:
        metadata = {
            "workspace_members": ["root 1.0.0"],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": None,
                    "dependencies": [{"name": "child", "path": "/tmp/child"}],
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "has no requirement"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_omitted_workspace_member(self) -> None:
        metadata = {
            "workspace_members": ["missing 1.0.0"],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": None,
                    "dependencies": [],
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "omitted workspace members"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_malformed_publish_field(self) -> None:
        metadata = {
            "workspace_members": ["root 1.0.0"],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": "not-a-list-or-null",
                    "dependencies": [],
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "publish field"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_duplicate_manifest_path(self) -> None:
        package = {
            "name": "root",
            "manifest_path": "/tmp/root/Cargo.toml",
            "publish": None,
            "dependencies": [],
        }
        metadata = {
            "workspace_members": ["root 1.0.0"],
            "packages": [
                {**package, "id": "root 1.0.0"},
                {**package, "id": "shadow 1.0.0"},
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "duplicate manifest path"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_malformed_workspace_member(self) -> None:
        metadata = {
            "workspace_members": [None],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": None,
                    "dependencies": [],
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "workspace members"):
                GATE.validate(Path("/tmp/root"))

    def test_rejects_empty_dependency_object(self) -> None:
        metadata = {
            "workspace_members": ["root 1.0.0"],
            "packages": [
                {
                    "id": "root 1.0.0",
                    "name": "root",
                    "manifest_path": "/tmp/root/Cargo.toml",
                    "publish": None,
                    "dependencies": [{}],
                }
            ],
        }
        with mock.patch.object(GATE, "cargo_metadata", return_value=metadata):
            with self.assertRaisesRegex(ValueError, "dependency has no name"):
                GATE.validate(Path("/tmp/root"))


if __name__ == "__main__":
    unittest.main()
