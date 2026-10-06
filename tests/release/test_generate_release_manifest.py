# SPDX-License-Identifier: Apache-2.0

import hashlib
import importlib.util
import io
import json
import tarfile
import tempfile
import unittest
import zipfile
import sys
import subprocess
from pathlib import Path


SCRIPT = Path(__file__).parents[2] / "scripts" / "generate-release-manifest.py"
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location("generate_release_manifest", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)
INTEGRITY_SCRIPT = Path(__file__).parents[2] / "scripts" / "verify-release-integrity.py"
INTEGRITY_SPEC = importlib.util.spec_from_file_location(
    "verify_release_integrity", INTEGRITY_SCRIPT
)
INTEGRITY = importlib.util.module_from_spec(INTEGRITY_SPEC)
assert INTEGRITY_SPEC.loader is not None
INTEGRITY_SPEC.loader.exec_module(INTEGRITY)


class GenerateReleaseManifestTest(unittest.TestCase):
    def metadata(self, root):
        from release_inventory import SUPPLEMENTAL_SOURCES
        for source in SUPPLEMENTAL_SOURCES.values():
            path = root / source
            path.parent.mkdir(parents=True, exist_ok=True)
            if not path.exists():
                path.write_text("retained license/notice fixture\n")

    def test_generated_manifest_verifies_round_trip(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive_path = root / "lean-ctx-linux.tar.gz"
            payload = b"binary"
            with tarfile.open(archive_path, "w:gz") as archive:
                info = tarfile.TarInfo("lean-ctx")
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            archive_hash = hashlib.sha256(archive_path.read_bytes()).hexdigest()
            wheel = root / "thinkery_leanctx_engine-1.2.3-py3-none-manylinux_2_35_x86_64.whl"
            with zipfile.ZipFile(wheel, "w") as archive:
                archive.writestr("thinkery_leanctx_engine/bin/lean-ctx", payload)
            wheel_hash = hashlib.sha256(wheel.read_bytes()).hexdigest()
            (root / "SHA256SUMS").write_text(
                f"{archive_hash}  {archive_path.name}\n{wheel_hash}  {wheel.name}\n"
            )
            (root / "SBOM.cdx.json").write_text(json.dumps({
                "bomFormat": "CycloneDX",
                "components": [{"type": "application", "name": "lean-ctx"}],
            }))
            self.metadata(root)
            manifest = MODULE.generate(root, "v1.2.3", "a" * 40)
            self.assertEqual(manifest["artifacts"][wheel.name]["payload_sha256"], hashlib.sha256(payload).hexdigest())
            (root / "release-manifest.json").write_text(json.dumps(manifest))

            report = INTEGRITY.verify_release("v1.2.3", root)
            self.assertTrue(report["verified"], report["errors"])
            from release_inventory import SIGNED_ENVELOPE, SUPPLEMENTAL_SOURCES, upload_paths
            checksums = INTEGRITY.parse_checksums((root / "SHA256SUMS").read_bytes())
            self.assertEqual(set(checksums), set(manifest["artifacts"]))
            self.assertTrue(SUPPLEMENTAL_SOURCES.keys() <= checksums.keys())
            legacy_name = "LICENSES-LeanCTX-Commercial-Source-License-2.0.txt"
            self.assertNotIn(legacy_name, manifest["artifacts"])
            self.assertEqual(manifest, MODULE.generate(root, "v1.2.3", "a" * 40))
            with self.assertRaises(ValueError):
                upload_paths(root, manifest["artifacts"])
            for name in SIGNED_ENVELOPE:
                if not (root / name).exists():
                    (root / name).write_bytes(b"test-envelope-not-a-real-signature")
            self.assertEqual(set(upload_paths(root, manifest["artifacts"])),
                             set(checksums) | set(SIGNED_ENVELOPE))
            cli = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root), "--tag", "v1.2.3",
                 "--commit", "a" * 40], capture_output=True, text=True, check=False,
            )
            self.assertEqual(cli.returncode, 0, cli.stderr)
            upload = subprocess.run(
                [sys.executable, str(INTEGRITY_SCRIPT), "upload-list", "--tag", "v1.2.3",
                 "--dir", str(root)], capture_output=True, text=True, check=False,
            )
            self.assertEqual(upload.returncode, 0, upload.stdout + upload.stderr)
            self.assertEqual(set(upload.stdout.splitlines()),
                              {str(root / name) for name in upload_paths(root, manifest["artifacts"])})

            # Legacy inventories remain checksum-verifiable but cannot be
            # republished by the new uploader; this is not signature validation.
            historical = json.loads(json.dumps(manifest))
            legacy_bytes = b"legacy terms"
            (root / legacy_name).write_bytes(legacy_bytes)
            historical["artifacts"][legacy_name] = {
                "kind": "supplemental", "source_path": "LICENSES/LeanCTX-Commercial-Source-License-2.0.txt",
                "sha256": hashlib.sha256(legacy_bytes).hexdigest(), "size": len(legacy_bytes),
            }
            INTEGRITY.validate_manifest(historical)
            sums = "".join(f"{entry['sha256']}  {name}\n" for name, entry in historical["artifacts"].items())
            (root / "SHA256SUMS").write_text(sums)
            historical["checksums_sha256"] = hashlib.sha256(sums.encode()).hexdigest()
            (root / "release-manifest.json").write_text(json.dumps(historical))
            self.assertTrue(INTEGRITY.verify_release("v1.2.3", root)["verified"])
            refused = subprocess.run(
                [sys.executable, str(INTEGRITY_SCRIPT), "upload-list", "--tag", "v1.2.3",
                 "--dir", str(root)], capture_output=True, text=True, check=False,
            )
            self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
            self.assertIn("verification-only, not publishable", refused.stdout)
            with self.assertRaisesRegex(ValueError, "verification-only"):
                upload_paths(root, historical["artifacts"])
            historical["artifacts"][legacy_name]["source_path"] = "private/terms.txt"
            with self.assertRaises(INTEGRITY.GateError):
                INTEGRITY.validate_manifest(historical)

            # Neither a forged kind nor mixed legacy metadata can bypass payload checks.
            for edit in ("kind", "mixed", "source", "missing", "sbom"):
                malformed = json.loads(json.dumps(manifest))
                if edit == "kind":
                    entry = malformed["artifacts"][archive_path.name]
                    entry.update(kind="supplemental", source_path="LICENSE.md")
                    entry.pop("payload_sha256")
                elif edit == "mixed":
                    malformed["artifacts"][archive_path.name].pop("kind")
                elif edit == "source":
                    malformed["artifacts"]["LICENSE.md"]["source_path"] = "elsewhere"
                elif edit == "missing":
                    malformed["artifacts"].pop("THIRD_PARTY_NOTICES")
                else:
                    malformed["artifacts"]["SBOM.cdx.json"]["sha256"] = "f" * 64
                with self.subTest(edit=edit), self.assertRaises(INTEGRITY.GateError):
                    INTEGRITY.validate_manifest(malformed)

            # A pre-existing different flat file is a collision, not an overwrite target.
            flat = root / "LICENSES-Apache-2.0.txt"
            flat.write_text("different bytes")
            with self.assertRaisesRegex(ValueError, "collision"):
                MODULE.generate(root, "v1.2.3", "a" * 40)
            self.assertEqual(flat.read_text(), "different bytes")

    def test_binary_archives_bind_extracted_payloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            unix_payload = b"unix-binary"
            with tarfile.open(root / "lean-ctx-linux.tar.gz", "w:gz") as archive:
                info = tarfile.TarInfo("lean-ctx")
                info.size = len(unix_payload)
                archive.addfile(info, io.BytesIO(unix_payload))
            windows_payload = b"windows-binary"
            with zipfile.ZipFile(root / "lean-ctx-windows.zip", "w") as archive:
                archive.writestr("lean-ctx.exe", windows_payload)
            with tarfile.open(root / "lean-ctx-1.2.3-source.tar.gz", "w:gz") as archive:
                info = tarfile.TarInfo("lean-ctx-1.2.3/README.md")
                info.size = 0
                archive.addfile(info, io.BytesIO())
            (root / "SHA256SUMS").write_bytes(b"checksums")
            (root / "SBOM.cdx.json").write_bytes(b"{}")

            self.metadata(root)

            manifest = MODULE.generate(root, "v1.2.3", "a" * 40)
            artifacts = manifest["artifacts"]
            self.assertEqual(artifacts["lean-ctx-linux.tar.gz"]["payload_sha256"], hashlib.sha256(unix_payload).hexdigest())
            self.assertEqual(artifacts["lean-ctx-windows.zip"]["payload_sha256"], hashlib.sha256(windows_payload).hexdigest())
            self.assertNotIn("payload_sha256", artifacts["lean-ctx-1.2.3-source.tar.gz"])
            json.dumps(manifest)

    def test_ambiguous_payload_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive_path = Path(temporary) / "lean-ctx-bad.zip"
            with zipfile.ZipFile(archive_path, "w") as archive:
                archive.writestr("a/lean-ctx.exe", b"one")
                archive.writestr("b/lean-ctx.exe", b"two")
            with self.assertRaisesRegex(ValueError, "exactly one"):
                MODULE.payload_digest(archive_path)

    def test_companion_wheel_requires_one_expected_regular_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wheel = root / "thinkery_leanctx_engine-1.2.3-py3-none-win_amd64.whl"
            for names in [
                ["unrelated/bin/lean-ctx.exe"],
                ["thinkery_leanctx_engine/bin/lean-ctx", "thinkery_leanctx_engine/bin/lean-ctx.exe"],
            ]:
                with zipfile.ZipFile(wheel, "w") as archive:
                    for name in names:
                        archive.writestr(name, b"binary")
                with self.assertRaises(ValueError):
                    MODULE.payload_digest(wheel)
                with self.assertRaises(INTEGRITY.GateError):
                    INTEGRITY.payload_sha256(wheel)
            info = zipfile.ZipInfo("thinkery_leanctx_engine/bin/lean-ctx.exe")
            info.create_system = 3
            info.external_attr = 0o120777 << 16
            with zipfile.ZipFile(wheel, "w") as archive:
                archive.writestr(info, b"target")
            with self.assertRaises(ValueError):
                MODULE.payload_digest(wheel)
            with self.assertRaises(INTEGRITY.GateError):
                INTEGRITY.payload_sha256(wheel)

    def test_nested_or_symlink_payload_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            nested = root / "lean-ctx-nested.zip"
            with zipfile.ZipFile(nested, "w") as archive:
                archive.writestr("bin/lean-ctx.exe", b"nested")
            with self.assertRaisesRegex(ValueError, "exactly one"):
                MODULE.payload_digest(nested)

            linked = root / "lean-ctx-linked.tar.gz"
            with tarfile.open(linked, "w:gz") as archive:
                info = tarfile.TarInfo("lean-ctx")
                info.type = tarfile.SYMTYPE
                info.linkname = "elsewhere"
                archive.addfile(info)
            with self.assertRaisesRegex(ValueError, "exactly one"):
                MODULE.payload_digest(linked)

    def test_only_exact_source_name_may_omit_payload_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            masquerade = root / "lean-ctx-other-source.tar.gz"
            with tarfile.open(masquerade, "w:gz") as archive:
                info = tarfile.TarInfo("lean-ctx")
                info.size = 1
                archive.addfile(info, io.BytesIO(b"x"))
            (root / "SHA256SUMS").write_bytes(b"checksums")
            (root / "SBOM.cdx.json").write_bytes(b"{}")
            self.metadata(root)
            manifest = MODULE.generate(root, "v1.2.3", "a" * 40)
            self.assertIn(
                "payload_sha256", manifest["artifacts"][masquerade.name]
            )

    def test_outer_symlink_artifact_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outside = root / "outside.zip"
            with zipfile.ZipFile(outside, "w") as archive:
                archive.writestr("lean-ctx.exe", b"binary")
            (root / "lean-ctx-linked.zip").symlink_to(outside)
            (root / "SHA256SUMS").write_bytes(b"checksums")
            (root / "SBOM.cdx.json").write_bytes(b"{}")
            with self.assertRaisesRegex(ValueError, "must not be a symlink"):
                MODULE.generate(root, "v1.2.3", "a" * 40)


if __name__ == "__main__":
    unittest.main()
