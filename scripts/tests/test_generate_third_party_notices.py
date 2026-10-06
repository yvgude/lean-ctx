#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Hermetic adversarial tests for the third-party notice generator."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "generate-third-party-notices.py"


def _load_generator():
    spec = importlib.util.spec_from_file_location("third_party_generator", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


GEN = _load_generator()


def _write(root: Path, relative: str, data: bytes | str) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)
    return path


def _sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _fixture(root: Path, *, asset_path: str = "asset.bin", license_name: str = "MIT") -> Path:
    lock = _write(root, "rust/Cargo.lock", b"[[package]]\n")
    sbom = _write(
        root,
        "SBOM.cdx.json",
        json.dumps(
            {
                "bomFormat": "CycloneDX",
                "specVersion": "1.3",
                "metadata": {
                    "properties": [
                        {
                            "name": "leanctx:cargo-lock-sha256",
                            "value": _sha(lock),
                        }
                    ]
                },
                "components": [
                    {"type": "library", "name": "fixture", "version": "1.0.0"}
                ],
            },
            sort_keys=True,
        ),
    )
    npm = _write(
        root,
        "packages/pi-lean-ctx/package-lock.json",
        json.dumps(
            {
                "packages": {
                    "node_modules/@modelcontextprotocol/sdk": {
                        "version": "1.30.0",
                        "resolved": "https://registry.npmjs.org/@modelcontextprotocol/sdk/-/sdk-1.30.0.tgz",
                        "integrity": "sha512-fixture",
                    }
                }
            },
            sort_keys=True,
        ),
    )
    asset = _write(root, asset_path, b"fixture asset\n")
    manifest = f"""schema = "leanctx.third-party-assets/v1"
version = 1
authority = "THIRD_PARTY_NOTICES"

[inputs]
cargo_lock = "rust/Cargo.lock"
cargo_lock_sha256 = "{_sha(lock)}"
sbom = "SBOM.cdx.json"
sbom_sha256 = "{_sha(sbom)}"
npm_lock = "packages/pi-lean-ctx/package-lock.json"
npm_lock_sha256 = "{_sha(npm)}"

[license_texts]
mit = "Fixture MIT license text"

[[assets]]
id = "fixture-asset"
path = "{asset_path}"
sha256 = "{_sha(asset)}"
version = "1.0.0"
source = "https://example.invalid/fixture"
spdx = "{license_name}"
license_text_path = "legal/third-party-assets.toml"
license_text_key = "license_texts.mit"
modified = false
evidence_paths = ["{asset_path}"]

[[assets]]
id = "mcp-sdk-bundle"
path = "packages/pi-lean-ctx/extensions/vendor/mcp-sdk.cjs"
sha256 = "{'0' * 64}"
version = "1.30.0"
source = "https://registry.npmjs.org/@modelcontextprotocol/sdk/-/sdk-1.30.0.tgz"
spdx = "MIT"
license_text_path = "legal/third-party-assets.toml"
license_text_key = "license_texts.mit"
generated = true
allow_missing_in_checkout = true
modified = false
evidence_paths = ["packages/pi-lean-ctx/package-lock.json"]
"""
    return _write(root, "legal/third-party-assets.toml", manifest)


class GeneratorUnitTests(unittest.TestCase):
    def test_sbom_component_versions_must_match_locked_inventory(self) -> None:
        # Matching the lock fingerprint alone must not bless stale components.
        for version, accepted in (("2.0.0", True), ("1.0.0", False)):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                lock = _write(root, "rust/Cargo.lock", (
                    'version = 3\n[[package]]\nname = "fixture"\nversion = "2.0.0"\n'
                    '[[package]]\nname = "dev-only"\nversion = "1.0.0"\n'
                ))
                document = {
                    "bomFormat": "CycloneDX",
                    "metadata": {"properties": [{
                        "name": "leanctx:cargo-lock-sha256", "value": _sha(lock),
                    }]},
                    "components": [{"type": "library", "name": "fixture", "version": version}],
                }
                sbom = _write(root, "SBOM.cdx.json", json.dumps(document))
                if accepted:
                    self.assertEqual(GEN._validate_sbom(root, _sha(lock)), (_sha(sbom), 1))
                else:
                    with self.assertRaisesRegex(GEN.GenerationError, "component absent from Cargo.lock"):
                        GEN._validate_sbom(root, _sha(lock))

    def test_live_generation_is_byte_deterministic_and_covers_surfaces(self) -> None:
        first, first_approved = GEN._generate(ROOT)
        second, second_approved = GEN._generate(ROOT)
        self.assertEqual(first, second)
        self.assertEqual(first_approved, second_approved)
        self.assertIn(b"dashboard-d3", first)
        self.assertIn(b"dashboard-chartjs", first)
        self.assertIn(b"font-inter", first)
        self.assertIn(b"font-jetbrains-mono", first)
        self.assertIn(b"font-space-grotesk", first)
        self.assertIn(b"vendored-rmcp", first)
        self.assertIn(b"Modification summary: LeanCTX carries the local JoinSet", first)
        self.assertIn(b"mcp-sdk-bundle", first)
        self.assertNotIn(str(ROOT).encode(), first)

    def test_cli_check_is_idempotent_and_never_networks(self) -> None:
        for _ in range(2):
            result = subprocess.run(
                [
                    sys.executable,
                    "scripts/generate-third-party-notices.py",
                    "--root",
                    ".",
                    "--check",
                ],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
        command = mock.Mock(
            returncode=0,
            stdout=json.dumps(
                {
                    "packages": [
                        {
                            "id": "registry+https://example/pkg#1.0.0",
                            "name": "pkg",
                            "version": "1.0.0",
                            "source": "registry+https://example",
                            "license": "MIT",
                            "repository": "https://example.invalid/pkg",
                            "targets": [],
                            "features": {},
                        }
                    ],
                    "resolve": {
                        "nodes": [
                            {
                                "id": "registry+https://example/pkg#1.0.0",
                                "deps": [],
                            }
                        ]
                    },
                }
            ).encode(),
        )
        with tempfile.TemporaryDirectory(prefix="third-party-metadata-") as name:
            root = Path(name)
            _write(root, "rust/Cargo.lock", b"[[package]]\n")
            with mock.patch.object(GEN.subprocess, "run", return_value=command) as run:
                GEN._run_metadata(root)
            argv = run.call_args.args[0]
            self.assertIn("--offline", argv)
            self.assertIn("--locked", argv)
            self.assertIn("--all-features", argv)

    def test_manifest_rejects_traversal_missing_and_digest_drift(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-manifest-") as name:
            root = Path(name)
            manifest_path = _fixture(root)
            text = manifest_path.read_text()
            _write(root, "outside.bin", b"outside")
            for mutation, expected in (
                ('path = "../outside.bin"', "unsafe relative path"),
                ('path = "missing.bin"', "file is missing"),
                ('sha256 = "' + ("0" * 64) + '"', "digest drift"),
            ):
                mutated = text
                if mutation.startswith("path ="):
                    mutated = mutated.replace('path = "asset.bin"', mutation, 1)
                else:
                    mutated = mutated.replace(
                        f'sha256 = "{_sha(root / "asset.bin")}"', mutation, 1
                    )
                _write(root, "legal/third-party-assets.toml", mutated)
                with self.assertRaisesRegex(GEN.GenerationError, expected):
                    GEN._load_manifest(root)
            _write(root, "legal/third-party-assets.toml", text)

    def test_manifest_rejects_lock_sbom_and_npm_input_drift(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-inputs-") as name:
            root = Path(name)
            manifest_path = _fixture(root)
            text = manifest_path.read_text()
            cases = (
                ("rust/Cargo.lock", b"[[package]]\ndrift\n", "cargo_lock: digest drift"),
                ("SBOM.cdx.json", b"{}", "sbom: digest drift"),
                ("packages/pi-lean-ctx/package-lock.json", b"{}", "npm_lock: digest drift"),
            )
            for relative, data, expected in cases:
                original = (root / relative).read_bytes()
                _write(root, relative, data)
                with self.assertRaisesRegex(GEN.GenerationError, expected):
                    GEN._load_manifest(root)
                _write(root, relative, original)
            _write(root, "legal/third-party-assets.toml", text)

    def test_manifest_rejects_duplicate_unknown_license_and_missing_text(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-manifest-") as name:
            root = Path(name)
            manifest_path = _fixture(root)
            text = manifest_path.read_text()
            duplicate = text.replace(
                'id = "mcp-sdk-bundle"', 'id = "fixture-asset"', 1
            )
            _write(root, "legal/third-party-assets.toml", duplicate)
            with self.assertRaisesRegex(GEN.GenerationError, "duplicate id"):
                GEN._load_manifest(root)
            unknown = text.replace('spdx = "MIT"', 'spdx = "Unknown-License"', 1)
            _write(root, "legal/third-party-assets.toml", unknown)
            with self.assertRaisesRegex(GEN.GenerationError, "unknown license"):
                GEN._load_manifest(root)
            malformed = text.replace('spdx = "MIT"', 'spdx = "MIT OR"', 1)
            _write(root, "legal/third-party-assets.toml", malformed)
            with self.assertRaisesRegex(GEN.GenerationError, "malformed SPDX"):
                GEN._load_manifest(root)
            missing = text.replace(
                'license_text_key = "license_texts.mit"',
                'license_text_key = "license_texts.missing"',
                1,
            )
            _write(root, "legal/third-party-assets.toml", missing)
            with self.assertRaisesRegex(GEN.GenerationError, "license text key"):
                GEN._load_manifest(root)

    def test_manifest_rejects_symlink_and_modified_without_statement(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-manifest-") as name:
            root = Path(name)
            manifest_path = _fixture(root)
            text = manifest_path.read_text()
            outside = root.parent / "third-party-outside.bin"
            outside.write_bytes(b"outside")
            link = root / "link.bin"
            link.symlink_to(outside)
            symlink_text = text.replace(
                'path = "asset.bin"', 'path = "link.bin"', 1
            ).replace(
                f'sha256 = "{_sha(root / "asset.bin")}"',
                f'sha256 = "{_sha(outside)}"',
                1,
            )
            _write(root, "legal/third-party-assets.toml", symlink_text)
            with self.assertRaisesRegex(GEN.GenerationError, "symlink"):
                GEN._load_manifest(root)
            link.unlink()
            text = text.replace("modified = false", "modified = true", 1)
            _write(root, "legal/third-party-assets.toml", text)
            with self.assertRaisesRegex(GEN.GenerationError, "modification_summary"):
                GEN._load_manifest(root)
            outside.unlink()

    def test_tree_digest_is_sorted_and_rejects_symlinks(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-tree-") as name:
            root = Path(name)
            tree = root / "tree"
            _write(root, "tree/b.txt", b"b")
            _write(root, "tree/a.txt", b"a")
            first = GEN._tree_digest(tree, "tree")
            _write(root, "tree/a-link", b"a")
            second = GEN._tree_digest(tree, "tree")
            self.assertNotEqual(first, second)
            (tree / "a-link").unlink()
            (tree / "link").symlink_to(tree / "a.txt")
            with self.assertRaisesRegex(GEN.GenerationError, "symlink"):
                GEN._tree_digest(tree, "tree")

    def test_coverage_rejects_unknown_shipped_asset(self) -> None:
        with tempfile.TemporaryDirectory(prefix="third-party-coverage-") as name:
            root = Path(name)
            _fixture(root)
            for directory in (
                "rust/src/dashboard/static/vendor",
                "rust/crates/vendor",
                "packages/pi-lean-ctx/extensions/vendor",
                "rust/src/dashboard/static/fonts",
            ):
                (root / directory).mkdir(parents=True, exist_ok=True)
            _write(root, "rust/src/dashboard/static/vendor/unlisted.js", b"unexpected\n")
            manifest, assets = GEN._load_manifest(root)
            del manifest
            with self.assertRaisesRegex(GEN.GenerationError, "missing asset manifest entry"):
                GEN._validate_coverage(root, assets)

    def test_content_digest_and_license_texts_are_deduplicated(self) -> None:
        raw = (ROOT / "THIRD_PARTY_NOTICES").read_bytes()
        text = raw.decode()
        digest = next(
            line.split(":", 1)[1].strip().removeprefix("sha256:")
            for line in text.splitlines()
            if line.startswith("Content-Digest:")
        )
        canonical = raw.replace(
            f"Content-Digest: sha256:{digest}".encode(),
            f"Content-Digest: sha256:{'0' * 64}".encode(),
            1,
        )
        self.assertEqual(hashlib.sha256(canonical).hexdigest(), digest)
        self.assertEqual(text.count("### OFL-1.1"), 1)
        self.assertIn("font-inter", text)
        self.assertIn("font-space-grotesk", text)

    def test_normal_provenance_checker_invokes_generator_path(self) -> None:
        result = subprocess.run(
            [sys.executable, "scripts/ip-provenance-audit.py", "--root", "."],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
