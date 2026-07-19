import base64
import copy
import hashlib
import importlib.util
import json
import secrets
import shutil
import tempfile
import unittest
from pathlib import Path

SOURCE_ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "delivery_rehearsal", SOURCE_ROOT / "scripts/rehearse-delivery.py"
)
REHEARSAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REHEARSAL)
TRUST = REHEARSAL.VERIFIER.TRUST


class DeliveryRehearsalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.directory = Path(cls.temporary.name)
        cls.root = cls.directory
        source_pack_path = SOURCE_ROOT / "docs/contracts/ocla-contract-pack-v1.json"
        pack = json.loads(source_pack_path.read_bytes())
        contract_sources = [source_pack_path]
        contract_sources.extend(SOURCE_ROOT / artifact["path"] for artifact in pack["artifacts"])
        contract_sources.append(SOURCE_ROOT / "docs/releases/migration-1.0.md")
        for source in contract_sources:
            destination = cls.root / source.relative_to(SOURCE_ROOT)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        cls.seed = secrets.token_bytes(32)
        public = TRUST.public_from_seed(cls.seed)
        cls.trust_root = cls.directory / "release-trust-root.json"
        cls.trust_root.write_bytes(
            REHEARSAL.canonical_json(
                {
                    "algorithm": "Ed25519",
                    "key_id": "sha256:" + hashlib.sha256(public).hexdigest(),
                    "public_key": base64.b64encode(public).decode(),
                }
            )
        )
        cls.previous = cls.create_release(
            "previous", "3.9.11", "1" * 40, "lean-ctx", "https://github.com/yvgude/lean-ctx"
        )
        cls.candidate = cls.create_release(
            "candidate", "4.0.0", "2" * 40, "lean-ctx", "https://github.com/yvgude/lean-ctx"
        )
        cls.other_component = cls.create_release(
            "other-component", "3.9.10", "3" * 40, "lean-ctx-other", "https://github.com/yvgude/lean-ctx"
        )
        cls.other_repository = cls.create_release(
            "other-repository", "3.9.10", "4" * 40, "lean-ctx", "https://github.com/example/lean-ctx"
        )
        cls.plan = {
            "schema_version": "leanctx.deployment-rehearsal/v1",
            "candidate": cls.candidate,
            "previous": cls.previous,
            "rollback": {"target_manifest_sha256": cls.previous["manifest"]["sha256"]},
        }

    @classmethod
    def tearDownClass(cls):
        cls.seed = b""
        cls.temporary.cleanup()

    @classmethod
    def create_release(cls, label, version, commit, component_name, repository):
        image_path = cls.directory / f"{label}-oci-image-manifest.json"
        image_path.write_bytes(
            REHEARSAL.canonical_json(
                {
                    "schemaVersion": 2,
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "config": {
                        "mediaType": "application/vnd.oci.image.config.v1+json",
                        "digest": "sha256:" + hashlib.sha256(label.encode()).hexdigest(),
                        "size": len(label),
                    },
                    "layers": [],
                }
            )
        )
        image_digest = hashlib.sha256(image_path.read_bytes()).hexdigest()
        image_reference = "ghcr.io/yvgude/lean-ctx"

        sbom_path = cls.directory / f"{label}-sbom.cdx.json"
        sbom_path.write_bytes(
            REHEARSAL.canonical_json(
                {
                    "bomFormat": "CycloneDX",
                    "specVersion": "1.5",
                    "metadata": {"component": {"name": component_name, "version": version}},
                }
            )
        )
        provenance_path = cls.directory / f"{label}-provenance.slsa.json"
        provenance_path.write_bytes(
            REHEARSAL.canonical_json(
                {
                    "_type": "https://in-toto.io/Statement/v1",
                    "predicateType": "https://slsa.dev/provenance/v1",
                    "subject": [
                        {"name": image_reference, "digest": {"sha256": image_digest}}
                    ],
                    "predicate": {
                        "buildDefinition": {
                            "externalParameters": {
                                "source": {"repository": repository, "commit": commit}
                            }
                        }
                    },
                }
            )
        )
        vulnerability_path = cls.directory / f"{label}-vulnerability-report.json"
        vulnerability_path.write_bytes(
            REHEARSAL.canonical_json({"artifactName": image_reference, "matches": []})
        )
        signature_path = cls.directory / f"{label}-signature.bundle.json"

        pack_path = cls.root / "docs/contracts/ocla-contract-pack-v1.json"
        pack = json.loads(pack_path.read_bytes())
        migration_path = cls.root / "docs/releases/migration-1.0.md"
        manifest = {
            "schema_version": "leanctx.delivery/v1",
            "component": {"name": component_name, "version": version},
            "source": {"repository": repository, "commit": commit},
            "image": {"reference": image_reference, "digest": "sha256:" + image_digest},
            "configuration": {
                "schema_version": "1",
                "migration": "docs/releases/migration-1.0.md",
            },
            "contracts": {
                "pack_version": pack["version"],
                "pack_digest": "sha256:"
                + hashlib.sha256(REHEARSAL.canonical_json(pack)).hexdigest(),
            },
            "evidence": {
                "sbom": cls.artifact(sbom_path),
                "provenance": cls.artifact(provenance_path),
                "signature": {
                    "path": str(signature_path.relative_to(cls.root)),
                    "sha256": "0" * 64,
                },
                "vulnerability_report": cls.artifact(vulnerability_path),
            },
        }
        payload = TRUST.canonical_json(TRUST.promotion_payload(manifest))
        receipt = {
            "schema_version": "leanctx.release-signature/v1",
            "algorithm": "Ed25519",
            "key_id": "sha256:"
            + hashlib.sha256(TRUST.public_from_seed(cls.seed)).hexdigest(),
            "payload_sha256": hashlib.sha256(payload).hexdigest(),
            "signature": base64.b64encode(TRUST.ed25519_sign(payload, cls.seed)).decode(),
        }
        signature_path.write_bytes(REHEARSAL.canonical_json(receipt))
        manifest["evidence"]["signature"] = cls.artifact(signature_path)
        manifest_path = cls.directory / f"{label}-delivery-manifest.json"
        manifest_path.write_bytes(REHEARSAL.canonical_json(manifest))
        return {
            "manifest": cls.artifact(manifest_path),
            "image": cls.artifact(image_path),
            "migration": cls.artifact(migration_path),
            "configuration_schema_version": "1",
        }

    @classmethod
    def artifact(cls, path):
        return {
            "path": str(path.relative_to(cls.root)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }

    def write_plan(self, plan, name="plan.json", canonical=True):
        path = self.directory / name
        content = REHEARSAL.canonical_json(plan) if canonical else json.dumps(plan, indent=2).encode()
        path.write_bytes(content)
        return str(path.relative_to(self.root))

    def rehearse(self, plan):
        return REHEARSAL.rehearse(
            self.write_plan(plan), self.root, str(self.trust_root.relative_to(self.root))
        )

    def assert_rejected(self, mutate, name):
        plan = copy.deepcopy(self.plan)
        mutate(plan)
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.rehearse(
                self.write_plan(plan, name),
                self.root,
                str(self.trust_root.relative_to(self.root)),
            )

    def test_rehearses_verified_candidate_and_rollback_without_deployment(self):
        evidence = self.rehearse(copy.deepcopy(self.plan))
        self.assertEqual(evidence["status"], "passed")
        self.assertEqual(evidence["rehearsal_kind"], "hermetic-local-no-deployment")
        self.assertEqual(
            evidence["trust_root_sha256"],
            hashlib.sha256(self.trust_root.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            [transition["phase"] for transition in evidence["transitions"]],
            ["previous-active", "candidate-active", "previous-restored"],
        )
        self.assertTrue(all(transition["scope"] == "in-memory-simulation" for transition in evidence["transitions"]))
        self.assertEqual(
            evidence["transitions"][0]["manifest_sha256"],
            evidence["transitions"][2]["manifest_sha256"],
        )

    def test_evidence_is_deterministic_for_same_plan(self):
        plan_path = self.write_plan(copy.deepcopy(self.plan), "deterministic-plan.json")
        trust_path = str(self.trust_root.relative_to(self.root))
        first = REHEARSAL.rehearse(plan_path, self.root, trust_path)
        second = REHEARSAL.rehearse(plan_path, self.root, trust_path)
        self.assertEqual(REHEARSAL.canonical_json(first), REHEARSAL.canonical_json(second))

    def test_rejects_candidate_manifest_digest_mismatch(self):
        self.assert_rejected(
            lambda value: value["candidate"]["manifest"].update({"sha256": "0" * 64}),
            "bad-manifest-digest.json",
        )

    def test_rejects_image_not_bound_to_manifest(self):
        self.assert_rejected(
            lambda value: value["candidate"].update({"image": copy.deepcopy(self.previous["image"])}),
            "bad-image-binding.json",
        )

    def test_rejects_configuration_schema_mismatch(self):
        self.assert_rejected(
            lambda value: value["candidate"].update({"configuration_schema_version": "2"}),
            "bad-config.json",
        )

    def test_rejects_migration_digest_mismatch(self):
        self.assert_rejected(
            lambda value: value["candidate"]["migration"].update({"sha256": "f" * 64}),
            "bad-migration.json",
        )

    def test_rejects_migration_path_not_bound_to_manifest(self):
        wrong_path = self.root / "docs/contracts/ocla-contract-pack-v1.json"
        self.assert_rejected(
            lambda value: value["candidate"].update({"migration": self.artifact(wrong_path)}),
            "bad-migration-path.json",
        )

    def test_rejects_rollback_target_not_bound_to_previous_manifest(self):
        self.assert_rejected(
            lambda value: value["rollback"].update(
                {"target_manifest_sha256": value["candidate"]["manifest"]["sha256"]}
            ),
            "bad-rollback-target.json",
        )

    def test_rejects_same_candidate_and_previous(self):
        def mutate(value):
            value["previous"] = copy.deepcopy(value["candidate"])
            value["rollback"]["target_manifest_sha256"] = value["candidate"]["manifest"]["sha256"]

        self.assert_rejected(mutate, "same-release.json")

    def test_rejects_component_discontinuity(self):
        def mutate(value):
            value["previous"] = copy.deepcopy(self.other_component)
            value["rollback"]["target_manifest_sha256"] = self.other_component["manifest"]["sha256"]

        self.assert_rejected(mutate, "other-component.json")

    def test_rejects_repository_discontinuity(self):
        def mutate(value):
            value["previous"] = copy.deepcopy(self.other_repository)
            value["rollback"]["target_manifest_sha256"] = self.other_repository["manifest"]["sha256"]

        self.assert_rejected(mutate, "other-repository.json")

    def test_rejects_symlink_in_artifact_path(self):
        link = self.directory / "linked"
        link.symlink_to(self.directory, target_is_directory=True)
        plan = copy.deepcopy(self.plan)
        plan["candidate"]["image"]["path"] = "linked/candidate-oci-image-manifest.json"
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.rehearse(
                self.write_plan(plan, "symlink.json"),
                self.root,
                str(self.trust_root.relative_to(self.root)),
            )

    def test_rejects_noncanonical_plan(self):
        path = self.write_plan(copy.deepcopy(self.plan), "noncanonical.json", canonical=False)
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.rehearse(
                path, self.root, str(self.trust_root.relative_to(self.root))
            )

    def test_rejects_oversized_plan(self):
        oversized = {"padding": "x" * REHEARSAL.MAX_PLAN_BYTES}
        path = self.write_plan(oversized, "oversized.json")
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.rehearse(
                path, self.root, str(self.trust_root.relative_to(self.root))
            )

    def test_rejects_symlink_output(self):
        target = self.directory / "target.json"
        target.write_text("occupied")
        link = self.directory / "output.json"
        link.symlink_to(target)
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.confined_output(
                self.root, str(link.relative_to(self.root))
            )

    def test_writes_new_canonical_evidence_and_rejects_overwrite(self):
        evidence = self.rehearse(copy.deepcopy(self.plan))
        relative = "evidence.json"
        output = REHEARSAL.confined_output(self.root, relative)
        REHEARSAL.write_new(output, REHEARSAL.canonical_json(evidence))
        self.assertEqual(output.read_bytes(), REHEARSAL.canonical_json(evidence))
        with self.assertRaises(REHEARSAL.InvalidRehearsal):
            REHEARSAL.confined_output(self.root, relative)


if __name__ == "__main__":
    unittest.main()
