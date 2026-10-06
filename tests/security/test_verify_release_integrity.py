# SPDX-License-Identifier: Apache-2.0
import hashlib
import importlib.util
import io
import json
import tempfile
import tarfile
import unittest
import sys
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
SPEC = importlib.util.spec_from_file_location(
    "release_integrity", ROOT / "scripts/verify-release-integrity.py"
)
INTEGRITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INTEGRITY)


class ReleaseIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name)
        self.artifact = self.directory / "lean-ctx-test.tar.gz"
        self.payload = b"release artifact\n"
        with tarfile.open(self.artifact, "w:gz") as archive:
            info = tarfile.TarInfo("lean-ctx")
            info.size = len(self.payload)
            archive.addfile(info, io.BytesIO(self.payload))
        self.write_fixture()

    def tearDown(self):
        self.temp.cleanup()

    def write_fixture(self):
        artifact_hash = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        sums = f"{artifact_hash}  {self.artifact.name}\n".encode()
        (self.directory / "SHA256SUMS").write_bytes(sums)
        (self.directory / "SBOM.cdx.json").write_text(json.dumps({
            "bomFormat": "CycloneDX", "specVersion": "1.5",
            "components": [{"type": "application", "name": "lean-ctx", "version": "3.9.14"}],
        }))
        manifest = {
            "schema_version": "leanctx.release-manifest/v1",
            "tag": "v3.9.14",
            "commit": "a" * 40,
            "artifacts": {self.artifact.name: {
                "sha256": artifact_hash,
                "size": self.artifact.stat().st_size,
                "payload_sha256": hashlib.sha256(self.payload).hexdigest(),
            }},
            "sbom_sha256": hashlib.sha256((self.directory / "SBOM.cdx.json").read_bytes()).hexdigest(),
            "checksums_sha256": hashlib.sha256(sums).hexdigest(),
        }
        (self.directory / "release-manifest.json").write_text(json.dumps(manifest))

    def test_valid_manifest_and_artifacts_verify(self):
        report = INTEGRITY.verify_release("v3.9.14", self.directory)
        self.assertTrue(report["verified"])
        self.assertEqual(report["errors"], [])

    def test_manifest_schema_validation_rejects_unknown_field(self):
        manifest = json.loads((self.directory / "release-manifest.json").read_text())
        manifest["unexpected"] = True
        with self.assertRaises(INTEGRITY.GateError):
            INTEGRITY.validate_manifest(manifest)

    def test_artifact_digest_mismatch_fails(self):
        self.artifact.write_bytes(b"tampered\n")
        report = INTEGRITY.verify_release("v3.9.14", self.directory)
        self.assertFalse(report["verified"])
        self.assertIn("artifact digest mismatch", report["errors"][0])

    def test_payload_digest_mismatch_fails(self):
        manifest = json.loads((self.directory / "release-manifest.json").read_text())
        manifest["artifacts"][self.artifact.name]["payload_sha256"] = "b" * 64
        (self.directory / "release-manifest.json").write_text(json.dumps(manifest))
        report = INTEGRITY.verify_release("v3.9.14", self.directory)
        self.assertFalse(report["verified"])
        self.assertIn("payload digest mismatch", report["errors"][0])

    def test_outer_symlink_artifact_fails_closed(self):
        outside = self.directory / "outside.tar.gz"
        self.artifact.replace(outside)
        self.artifact.symlink_to(outside)
        report = INTEGRITY.verify_release("v3.9.14", self.directory)
        self.assertFalse(report["verified"])
        self.assertIn("missing release file", report["errors"][0])

    def test_source_suffix_masquerade_requires_payload_digest(self):
        manifest = json.loads((self.directory / "release-manifest.json").read_text())
        details = manifest["artifacts"].pop(self.artifact.name)
        details.pop("payload_sha256")
        manifest["artifacts"]["lean-ctx-other-source.tar.gz"] = details
        with self.assertRaisesRegex(INTEGRITY.GateError, "omits payload digest"):
            INTEGRITY.validate_manifest(manifest)

    def test_checksum_parser_handles_gnu_format_and_rejects_paths(self):
        digest = "b" * 64
        self.assertEqual(INTEGRITY.parse_checksums(f"{digest} *asset.tar.gz\n".encode()),
                         {"asset.tar.gz": digest})
        with self.assertRaises(INTEGRITY.GateError):
            INTEGRITY.parse_checksums(f"{digest}  ../asset.tar.gz\n".encode())
        for name in ("a b.tar.gz", "a\\b.tar.gz", ".", "..", "a\tb", "é.tar.gz"):
            with self.subTest(name=name), self.assertRaises(INTEGRITY.GateError):
                INTEGRITY.parse_checksums(f"{digest}  {name}\n".encode())

    def test_sbom_parser_accepts_cyclonedx_and_rejects_empty_components(self):
        sbom = {"bomFormat": "CycloneDX", "components": [{"name": "lean-ctx"}]}
        self.assertEqual(INTEGRITY.parse_sbom(json.dumps(sbom).encode()), sbom)
        with self.assertRaises(INTEGRITY.GateError):
            INTEGRITY.parse_sbom(b'{"bomFormat":"CycloneDX","components":[]}')

    def test_download_uses_http_and_fetches_checksum_listed_artifacts(self):
        digest = "c" * 64
        responses = {
            "SHA256SUMS": f"{digest}  lean-ctx-test.tar.gz\n".encode(),
            "SBOM.cdx.json": b'{"bomFormat":"CycloneDX","components":[{"name":"lean-ctx"}]}',
            "release-manifest.json": b"{}\n",
            "lean-ctx-test.tar.gz": b"archive\n",
        }

        def fake_urlopen(request, timeout):
            name = request.full_url.rsplit("/", 1)[1]
            return io.BytesIO(responses[name])

        with mock.patch.object(INTEGRITY.urllib.request, "urlopen", side_effect=fake_urlopen):
            report = INTEGRITY.download_release("v3.9.14", self.directory / "download", "owner/repo")
        self.assertEqual(report["downloaded"], [
            "SHA256SUMS", "SBOM.cdx.json", "release-manifest.json", "lean-ctx-test.tar.gz"
        ])
        self.assertEqual((self.directory / "download" / "lean-ctx-test.tar.gz").read_bytes(), b"archive\n")

    def test_download_replaces_symlink_without_writing_its_target(self):
        target = self.directory / "outside"
        target.write_bytes(b"unchanged")
        destination = self.directory / "download" / "asset"
        destination.parent.mkdir()
        destination.symlink_to(target)
        with mock.patch.object(
            INTEGRITY.urllib.request, "urlopen", return_value=io.BytesIO(b"download")
        ):
            INTEGRITY.download_file("https://example.invalid/asset", destination)
        self.assertEqual(target.read_bytes(), b"unchanged")
        self.assertFalse(destination.is_symlink())
        self.assertEqual(destination.read_bytes(), b"download")


if __name__ == "__main__":
    unittest.main()
