import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "delivery_evidence_verifier", ROOT / "scripts/verify-delivery-evidence.py"
)
VERIFIER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFIER)
CONTRACT = ROOT / "docs/contracts/delivery-evidence-v1.json"
SCHEMA = ROOT / "docs/contracts/delivery-evidence-v1.schema.json"
PARITY = ROOT / "tests/fixtures/delivery_support_schema_parity_v1.json"


def parity_value_at(value, path):
    current = value
    for segment in path:
        current = current[segment]
    return current


def apply_parity_mutation(value, case):
    operation = case.get("op", "set")
    if operation in {"set", "copy"}:
        current = value
        for segment in case["path"][:-1]:
            current = current[segment]
        replacement = case["value"] if operation == "set" else copy.deepcopy(
            parity_value_at(value, case["source"])
        )
        current[case["path"][-1]] = replacement
        return
    array = parity_value_at(value, case["path"])
    if operation == "swap":
        left, right = case["indices"]
        array[left], array[right] = array[right], array[left]
    elif operation == "append_copy":
        array.append(copy.deepcopy(array[case["source_index"]]))
    elif operation == "remove":
        array.pop(case["index"])
    else:
        raise AssertionError(f"unknown parity operation {operation}")


class DeliveryEvidenceTests(unittest.TestCase):
    def contract(self):
        return json.loads(CONTRACT.read_text(encoding="utf-8"))

    def assert_rejected(self, mutate):
        value = self.contract()
        mutate(value)
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(VERIFIER.canonical_json(value))
            handle.flush()
            with self.assertRaises(VERIFIER.InvalidDeliveryEvidence):
                VERIFIER.verify(Path(handle.name), ROOT)

    def test_canonical_delivery_evidence_is_locally_consistent(self):
        VERIFIER.verify(CONTRACT, ROOT)

    def test_schema_is_strict_draft_2020_12(self):
        schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
        self.assertEqual(
            schema["$schema"], "https://json-schema.org/draft/2020-12/schema"
        )

        def assert_strict_objects(value):
            if isinstance(value, dict):
                if value.get("type") == "object":
                    self.assertIs(value.get("additionalProperties"), False)
                for nested in value.values():
                    assert_strict_objects(nested)
            elif isinstance(value, list):
                for nested in value:
                    assert_strict_objects(nested)

        assert_strict_objects(schema)

    def test_rejects_unknown_field(self):
        self.assert_rejected(lambda value: value.update({"certified": True}))

    def test_rejects_missing_owner(self):
        self.assert_rejected(lambda value: value.pop("owner"))

    def test_rejects_owner_digest_drift(self):
        self.assert_rejected(
            lambda value: value["owner"]["source"].update({"sha256": "0" * 64})
        )

    def test_rejects_missing_evidence_selector(self):
        self.assert_rejected(
            lambda value: value["checklist"][0]["evidence"].update(
                {"selector": "fn nonexistent_setup_evidence()"}
            )
        )

    def test_rejects_caller_chosen_evidence_class(self):
        self.assert_rejected(
            lambda value: value["checklist"][0]["evidence"].update(
                {"evidence_class": "documentation"}
            )
        )

    def test_rejects_caller_chosen_harness(self):
        self.assert_rejected(
            lambda value: value["checklist"][0]["evidence"].update(
                {"harness_command": "true"}
            )
        )

    def test_rejects_comment_as_executable_evidence(self):
        self.assert_rejected(
            lambda value: value["checklist"][0]["evidence"].update(
                {"selector": "// v5 (#1008 / GL #1144): edits route"}
            )
        )

    def test_executable_selector_proof_accepts_canonical_sources(self):
        value = self.contract()
        for step in value["checklist"]:
            evidence = step["evidence"]
            source = (ROOT / evidence["path"]).read_bytes()
            with self.subTest(stage=step["stage"]):
                VERIFIER.verify_executable_selector(
                    step["stage"], source, evidence["selector"]
                )

    def test_executable_selector_proof_rejects_rust_comment_deception(self):
        selector = VERIFIER.STAGE_EVIDENCE["setup"]["selector"]
        source = f"#[test]\n// {selector} {{\n".encode()
        with self.assertRaisesRegex(
            VERIFIER.InvalidDeliveryEvidence, "executable Rust test"
        ):
            VERIFIER.verify_executable_selector("setup", source, selector)

    def test_executable_selector_proof_rejects_python_comment_deception(self):
        selector = VERIFIER.STAGE_EVIDENCE["rollback"]["selector"]
        source = (
            "class DeliveryRehearsalTests(unittest.TestCase):\n"
            f"    # {selector}\n"
        ).encode()
        with self.assertRaisesRegex(
            VERIFIER.InvalidDeliveryEvidence, "executable unittest method"
        ):
            VERIFIER.verify_executable_selector("rollback", source, selector)

    def test_executable_selector_proof_requires_rust_test_attribute(self):
        selector = VERIFIER.STAGE_EVIDENCE["uninstall"]["selector"]
        source = f"{selector} {{\n}}\n".encode()
        with self.assertRaisesRegex(
            VERIFIER.InvalidDeliveryEvidence, "test attribute"
        ):
            VERIFIER.verify_executable_selector("uninstall", source, selector)

    def test_rejects_reordered_lifecycle(self):
        self.assert_rejected(
            lambda value: value["checklist"].__setitem__(
                0, value["checklist"][1]
            )
        )

    def test_rejects_duplicate_artifact_target(self):
        def mutate(value):
            value["release"]["targets"][1]["artifact"] = value["release"][
                "targets"
            ][0]["artifact"]

        self.assert_rejected(mutate)

    def test_rejects_workflow_target_drift(self):
        self.assert_rejected(
            lambda value: value["release"]["targets"][0].update(
                {"runner": "ubuntu-latest"}
            )
        )

    def test_rejects_os_certification_inferred_from_runner(self):
        self.assert_rejected(
            lambda value: value["release"]["targets"][5].update(
                {"certification": "certified"}
            )
        )

    def test_rejects_missing_publish_channel(self):
        self.assert_rejected(lambda value: value["release"]["publish_channels"].pop())

    def test_rejects_missing_lifecycle_stage(self):
        self.assert_rejected(lambda value: value["checklist"].pop())

    def test_rejects_missing_strict_release_tag_gate(self):
        self.assert_rejected(lambda value: value["release"]["version_gates"].pop(0))

    def test_rejects_operational_acceptance_claim(self):
        self.assert_rejected(
            lambda value: value["scope"].update(
                {"external_operational_acceptance": True}
            )
        )

    def test_rejects_noncanonical_json(self):
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(json.dumps(self.contract(), indent=2).encode("utf-8"))
            handle.flush()
            with self.assertRaises(VERIFIER.InvalidDeliveryEvidence):
                VERIFIER.verify(Path(handle.name), ROOT)

    def test_rejects_oversized_contract(self):
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".json") as handle:
            handle.write(b" " * (VERIFIER.MAX_CONTRACT_BYTES + 1))
            handle.flush()
            with self.assertRaises(VERIFIER.InvalidDeliveryEvidence):
                VERIFIER.verify(Path(handle.name), ROOT)

    def test_rejects_symlink_contract(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            link = Path(directory) / "contract.json"
            link.symlink_to(CONTRACT)
            with self.assertRaises(VERIFIER.InvalidDeliveryEvidence):
                VERIFIER.verify(link, ROOT)

    def test_rejects_oversized_evidence_before_read(self):
        with tempfile.NamedTemporaryFile(dir=ROOT, suffix=".rs") as handle:
            handle.write(b"x" * (VERIFIER.MAX_SOURCE_BYTES + 1))
            handle.flush()
            relative = str(Path(handle.name).relative_to(ROOT))
            with self.assertRaisesRegex(
                VERIFIER.InvalidDeliveryEvidence, "exceeds byte bound"
            ):
                VERIFIER.verify_ref(
                    ROOT,
                    {"path": relative, "selector": "x", "sha256": "0" * 64},
                    "oversized",
                )

    def test_rejects_symlink_evidence_before_open(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            link = Path(directory) / "source.rs"
            link.symlink_to(ROOT / "rust/tests/setup_ci_smoke.rs")
            with self.assertRaisesRegex(
                VERIFIER.InvalidDeliveryEvidence, "symlink path"
            ):
                VERIFIER.verify_ref(
                    ROOT,
                    {
                        "path": str(link.relative_to(ROOT)),
                        "selector": "fn setup_bootstrap_doctor_status_json_smoke()",
                        "sha256": "0" * 64,
                    },
                    "symlink",
                )

    def test_verifier_never_uses_unbounded_path_reads(self):
        with mock.patch.object(
            Path, "read_bytes", side_effect=AssertionError("unbounded read")
        ):
            VERIFIER.verify(CONTRACT, ROOT)

    def test_shared_schema_parity_mutations_are_rejected_by_verifier(self):
        fixture = json.loads(PARITY.read_text(encoding="utf-8"))
        for case in fixture["cases"]:
            if case["contract"] != "delivery":
                continue

            def mutate(value, case=case):
                apply_parity_mutation(value, case)

            with self.subTest(case=case["id"]):
                self.assert_rejected(mutate)


if __name__ == "__main__":
    unittest.main()
