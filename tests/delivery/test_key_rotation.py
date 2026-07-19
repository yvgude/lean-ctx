import base64
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from test_verify_delivery_manifest import FIXTURE, ROOT, VERIFIER


TRUST = VERIFIER.TRUST
OLD_ROOT = ROOT / "tests/delivery/valid/release-trust-root.json"
OLD_RECEIPT = ROOT / "tests/delivery/valid/signature.bundle.json"
NEW_SEED = bytes(range(32))


class KeyRotationTests(unittest.TestCase):
    def setUp(self):
        self.directory_context = tempfile.TemporaryDirectory(dir=ROOT)
        self.directory = Path(self.directory_context.name)
        public = TRUST.public_from_seed(NEW_SEED)
        self.new_root = self.directory / "new-trust-root.json"
        self.new_root.write_bytes(TRUST.canonical_json({
            "algorithm": "Ed25519",
            "key_id": "sha256:" + hashlib.sha256(public).hexdigest(),
            "public_key": base64.b64encode(public).decode(),
        }))

    def tearDown(self):
        self.directory_context.cleanup()

    def root_reference(self, path):
        _, key_id = TRUST.read_public_key(path)
        return {
            "path": str(path.relative_to(ROOT)),
            "sha256": "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest(),
            "key_id": key_id,
        }

    def write_plan(self, transition, old_path=OLD_ROOT, new_path=None):
        path = self.directory / "rotation-plan.json"
        path.write_bytes(TRUST.canonical_json({
            "schema_version": "leanctx.release-key-rotation/v1",
            "old_trust_root": self.root_reference(old_path),
            "new_trust_root": self.root_reference(new_path or self.new_root),
            "transition": transition,
        }))
        return path

    def manifest(self):
        return json.loads(FIXTURE.read_bytes())

    def old_receipt(self):
        return json.loads(OLD_RECEIPT.read_bytes())

    def new_receipt(self, manifest=None):
        manifest = manifest or self.manifest()
        payload = TRUST.canonical_json(TRUST.promotion_payload(manifest))
        public = TRUST.public_from_seed(NEW_SEED)
        return {
            "schema_version": "leanctx.release-signature/v1",
            "algorithm": "Ed25519",
            "key_id": "sha256:" + hashlib.sha256(public).hexdigest(),
            "payload_sha256": hashlib.sha256(payload).hexdigest(),
            "signature": base64.b64encode(
                TRUST.ed25519_sign(payload, NEW_SEED)).decode(),
        }

    def overlap(self):
        return {"activation": "complete", "overlap": "active",
                "revocation": "pending"}

    def test_overlap_accepts_old_and_new_and_returns_content_only_evidence(self):
        plan = self.write_plan(self.overlap())
        old_evidence = TRUST.verify_rotation(
            self.manifest(), self.old_receipt(), plan, ROOT)
        new_evidence = TRUST.verify_rotation(
            self.manifest(), self.new_receipt(), plan, ROOT)
        self.assertEqual(old_evidence["accepted_role"], "old")
        self.assertEqual(new_evidence["accepted_role"], "new")
        self.assertEqual(set(old_evidence), {
            "schema_version", "rotation_plan_id", "accepted_role",
            "trust_root_id", "key_id"})
        self.assertTrue(old_evidence["rotation_plan_id"].startswith("sha256:"))
        self.assertNotIn("path", old_evidence)

    def test_pre_activation_accepts_only_old_key(self):
        plan = self.write_plan({
            "activation": "pending", "overlap": "inactive",
            "revocation": "not-started"})
        TRUST.verify_rotation(self.manifest(), self.old_receipt(), plan, ROOT)
        with self.assertRaisesRegex(TRUST.TrustError, "not allowed"):
            TRUST.verify_rotation(
                self.manifest(), self.new_receipt(), plan, ROOT)

    def test_post_revocation_accepts_only_new_key(self):
        plan = self.write_plan({
            "activation": "complete", "overlap": "complete",
            "revocation": "old-key-revoked"})
        with self.assertRaisesRegex(TRUST.TrustError, "not allowed"):
            TRUST.verify_rotation(
                self.manifest(), self.old_receipt(), plan, ROOT)
        TRUST.verify_rotation(self.manifest(), self.new_receipt(), plan, ROOT)

    def test_receipt_key_id_selects_role_before_signature_verification(self):
        plan = self.write_plan(self.overlap())
        receipt = self.old_receipt()
        receipt["key_id"] = self.root_reference(self.new_root)["key_id"]
        with self.assertRaisesRegex(
                TRUST.TrustError, "signature verification failed"):
            TRUST.verify_rotation(self.manifest(), receipt, plan, ROOT)

    def test_closed_transition_allowlist_rejects_mixed_state(self):
        plan = self.write_plan({
            "activation": "pending", "overlap": "active",
            "revocation": "old-key-revoked"})
        with self.assertRaisesRegex(TRUST.TrustError, "unsupported"):
            TRUST.read_rotation_plan(plan, ROOT)

    def test_rejects_identical_old_and_new_roots(self):
        plan = self.write_plan(self.overlap(), new_path=OLD_ROOT)
        with self.assertRaisesRegex(TRUST.TrustError, "must be distinct"):
            TRUST.read_rotation_plan(plan, ROOT)

    def test_rejects_root_file_or_key_content_id_drift(self):
        plan = self.write_plan(self.overlap())
        value = json.loads(plan.read_bytes())
        value["new_trust_root"]["sha256"] = "sha256:" + "0" * 64
        plan.write_bytes(TRUST.canonical_json(value))
        with self.assertRaisesRegex(TRUST.TrustError, "file content ID mismatch"):
            TRUST.read_rotation_plan(plan, ROOT)
        value["new_trust_root"]["sha256"] = self.root_reference(
            self.new_root)["sha256"]
        value["new_trust_root"]["key_id"] = value["old_trust_root"]["key_id"]
        plan.write_bytes(TRUST.canonical_json(value))
        with self.assertRaisesRegex(TRUST.TrustError, "key content ID mismatch"):
            TRUST.read_rotation_plan(plan, ROOT)

    def test_rejects_noncanonical_or_oversized_plan(self):
        plan = self.write_plan(self.overlap())
        plan.write_text(json.dumps(json.loads(plan.read_bytes()), indent=2))
        with self.assertRaisesRegex(TRUST.TrustError, "not canonical"):
            TRUST.read_rotation_plan(plan, ROOT)
        plan.write_bytes(b" " * (TRUST.MAX_ROTATION_PLAN_BYTES + 1))
        with self.assertRaisesRegex(TRUST.TrustError, "exceeds"):
            TRUST.read_rotation_plan(plan, ROOT)

    def test_rejects_symlinked_trust_root(self):
        link = self.directory / "linked-root.json"
        link.symlink_to(self.new_root)
        plan = self.write_plan(self.overlap(), new_path=link)
        with self.assertRaisesRegex(TRUST.TrustError, "symlink"):
            TRUST.read_rotation_plan(plan, ROOT)

    def test_manifest_verifier_accepts_rotation_plan_and_requires_one_policy(self):
        plan = self.write_plan(self.overlap())
        VERIFIER.verify(FIXTURE, ROOT, rotation_plan=plan)
        with self.assertRaisesRegex(VERIFIER.InvalidManifest, "exactly one"):
            VERIFIER.verify(FIXTURE, ROOT)
        with self.assertRaisesRegex(VERIFIER.InvalidManifest, "exactly one"):
            VERIFIER.verify(FIXTURE, ROOT, OLD_ROOT, plan)


if __name__ == "__main__":
    unittest.main()
