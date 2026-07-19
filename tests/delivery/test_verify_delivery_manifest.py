import copy
import base64
import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("delivery_verifier", ROOT / "scripts/verify-delivery-manifest.py")
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)
FIXTURE = ROOT / "tests/delivery/valid/delivery-manifest.json"
TRUST_ROOT = ROOT / "tests/delivery/valid/release-trust-root.json"
SIGNATURE_FIXTURE = ROOT / "tests/delivery/valid/signature.bundle.json"
TEST_RELEASE_SEED = bytes.fromhex(
    "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
)
TEST_RELEASE_PUBLIC = bytes.fromhex(
    "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
)
BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"


def equivalent_noncanonical_base64(value):
    padding = len(value) - len(value.rstrip("="))
    if padding not in (1, 2):
        raise AssertionError("fixture must use padded base64")
    index = len(value) - padding - 1
    sextet = BASE64_ALPHABET.index(value[index])
    replacement = BASE64_ALPHABET[sextet ^ 1]
    return value[:index] + replacement + value[index + 1:]


class DeliveryManifestTests(unittest.TestCase):
    def manifest(self):
        return json.loads(FIXTURE.read_text())

    def assert_rejected(self, mutate):
        manifest = self.manifest()
        mutate(manifest)
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(VERIFIER.canonical_json(manifest))
            handle.flush()
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.verify(Path(handle.name), ROOT, TRUST_ROOT)

    def test_valid_manifest(self):
        VERIFIER.verify(FIXTURE, ROOT, TRUST_ROOT)

    def test_contract_pack_declares_only_current_breaking_major(self):
        pack = json.loads((ROOT / "docs/contracts/ocla-contract-pack-v1.json").read_text())
        VERIFIER.verify_contract_pack_metadata(pack)
        self.assertEqual(pack["version"], "2.0.0")
        self.assertEqual(pack["compatibility"]["supported"], ["2.0.0"])
        self.assertNotIn("1.0.0", pack["compatibility"]["supported"])

        stale = copy.deepcopy(pack)
        stale["compatibility"]["supported"] = ["1.0.0"]
        with self.assertRaises(VERIFIER.InvalidManifest):
            VERIFIER.verify_contract_pack_metadata(stale)

    def test_valid_signature_fixture_is_deterministic_test_vector(self):
        public = VERIFIER.TRUST.public_from_seed(TEST_RELEASE_SEED)
        self.assertEqual(public, TEST_RELEASE_PUBLIC)
        key_id = "sha256:" + hashlib.sha256(public).hexdigest()
        trust_root = json.loads(TRUST_ROOT.read_text())
        self.assertEqual(
            trust_root,
            {
                "algorithm": "Ed25519",
                "key_id": key_id,
                "public_key": base64.b64encode(public).decode(),
            },
        )

        manifest = self.manifest()
        payload = VERIFIER.TRUST.canonical_json(
            VERIFIER.TRUST.promotion_payload(manifest)
        )
        expected_receipt = {
            "algorithm": "Ed25519",
            "key_id": key_id,
            "payload_sha256": hashlib.sha256(payload).hexdigest(),
            "schema_version": "leanctx.release-signature/v1",
            "signature": base64.b64encode(
                VERIFIER.TRUST.ed25519_sign(payload, TEST_RELEASE_SEED)
            ).decode(),
        }
        self.assertEqual(json.loads(SIGNATURE_FIXTURE.read_text()), expected_receipt)

    def test_rejects_parent_escape(self):
        with self.assertRaises(VERIFIER.InvalidManifest):
            VERIFIER.confined_file(ROOT, "tests/delivery/../delivery/valid/sbom.cdx.json")

    def test_rejects_internal_directory_symlink(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            directory = Path(directory)
            (directory / "linked").symlink_to(ROOT / "tests/delivery/valid", target_is_directory=True)
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.confined_file(ROOT, str((directory / "linked/sbom.cdx.json").relative_to(ROOT)))

    def test_rejects_symlink_escape(self):
        with tempfile.NamedTemporaryFile(dir="/private/tmp") as outside, tempfile.TemporaryDirectory(dir=ROOT) as directory:
            link = Path(directory) / "outside.json"
            link.symlink_to(outside.name)
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.confined_file(ROOT, str(link.relative_to(ROOT)))

    def test_rfc8032_ed25519_vector(self):
        seed = TEST_RELEASE_SEED
        public = TEST_RELEASE_PUBLIC
        signature = bytes.fromhex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
        self.assertEqual(VERIFIER.TRUST.public_from_seed(seed), public)
        self.assertEqual(VERIFIER.TRUST.ed25519_sign(b"", seed), signature)
        self.assertTrue(VERIFIER.TRUST.ed25519_verify(signature, b"", public))

    def test_rejects_cryptographically_invalid_signature(self):
        receipt = json.loads((ROOT / "tests/delivery/valid/signature.bundle.json").read_text())
        receipt["signature"] = "A" + receipt["signature"][1:]
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            receipt_path = Path(directory) / "signature.json"
            receipt_path.write_bytes(VERIFIER.canonical_json(receipt))
            manifest = self.manifest()
            manifest["evidence"]["signature"] = {
                "path": str(receipt_path.relative_to(ROOT)),
                "sha256": hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
            }
            manifest_path = Path(directory) / "manifest.json"
            manifest_path.write_bytes(VERIFIER.canonical_json(manifest))
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.verify(manifest_path, ROOT, TRUST_ROOT)

    def test_rejects_small_order_public_key(self):
        identity = b"\x01" + b"\x00" * 31
        value = {
            "algorithm": "Ed25519",
            "key_id": "sha256:" + hashlib.sha256(identity).hexdigest(),
            "public_key": base64.b64encode(identity).decode(),
        }
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(VERIFIER.canonical_json(value))
            handle.flush()
            with self.assertRaises(VERIFIER.TRUST.TrustError):
                VERIFIER.TRUST.read_public_key(Path(handle.name))

    def test_rejects_small_order_signature_r(self):
        receipt = json.loads((ROOT / "tests/delivery/valid/signature.bundle.json").read_text())
        signature = base64.b64decode(receipt["signature"])
        receipt["signature"] = base64.b64encode(b"\x01" + b"\x00" * 31 + signature[32:]).decode()
        self._assert_receipt_rejected(receipt)

    def test_rejects_noncanonical_trust_root_base64(self):
        value = json.loads(TRUST_ROOT.read_text())
        canonical = value["public_key"]
        value["public_key"] = equivalent_noncanonical_base64(canonical)
        self.assertEqual(base64.b64decode(value["public_key"]), base64.b64decode(canonical))
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(VERIFIER.canonical_json(value))
            handle.flush()
            with self.assertRaises(VERIFIER.TRUST.TrustError):
                VERIFIER.TRUST.read_public_key(Path(handle.name))

    def test_rejects_noncanonical_signature_base64(self):
        receipt = json.loads((ROOT / "tests/delivery/valid/signature.bundle.json").read_text())
        canonical = receipt["signature"]
        receipt["signature"] = equivalent_noncanonical_base64(canonical)
        self.assertEqual(base64.b64decode(receipt["signature"]), base64.b64decode(canonical))
        self._assert_receipt_rejected(receipt)

    def _assert_receipt_rejected(self, receipt):
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            receipt_path = Path(directory) / "signature.json"
            receipt_path.write_bytes(VERIFIER.canonical_json(receipt))
            manifest = self.manifest()
            manifest["evidence"]["signature"] = {
                "path": str(receipt_path.relative_to(ROOT)),
                "sha256": hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
            }
            manifest_path = Path(directory) / "manifest.json"
            manifest_path.write_bytes(VERIFIER.canonical_json(manifest))
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.verify(manifest_path, ROOT, TRUST_ROOT)

    def test_rejects_unknown_field(self):
        self.assert_rejected(lambda value: value.update({"approved": True}))

    def test_rejects_mutable_image(self):
        self.assert_rejected(lambda value: value["image"].update({"reference": "ghcr.io/yvgude/lean-ctx:latest"}))

    def test_rejects_wrong_contract_pack(self):
        self.assert_rejected(lambda value: value["contracts"].update({"pack_digest": "sha256:" + "0" * 64}))

    def test_rejects_wrong_evidence_digest(self):
        self.assert_rejected(lambda value: value["evidence"]["sbom"].update({"sha256": "0" * 64}))

    def test_rejects_provenance_for_other_commit(self):
        original = json.loads((ROOT / "tests/delivery/valid/provenance.slsa.json").read_text())
        changed = copy.deepcopy(original)
        changed["predicate"]["buildDefinition"]["externalParameters"]["source"]["commit"] = "0" * 40
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            path = Path(directory) / "provenance.json"
            path.write_bytes(VERIFIER.canonical_json(changed))
            manifest = self.manifest()
            manifest["evidence"]["provenance"] = {
                "path": str(path.relative_to(ROOT)),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
            manifest_path = Path(directory) / "manifest.json"
            manifest_path.write_bytes(VERIFIER.canonical_json(manifest))
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.verify(manifest_path, ROOT, TRUST_ROOT)

    def test_rejects_noncanonical_json(self):
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(json.dumps(self.manifest(), indent=2).encode())
            handle.flush()
            with self.assertRaises(VERIFIER.InvalidManifest):
                VERIFIER.verify(Path(handle.name), ROOT, TRUST_ROOT)


if __name__ == "__main__":
    unittest.main()
