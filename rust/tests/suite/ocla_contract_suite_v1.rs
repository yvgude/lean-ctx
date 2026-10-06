// SPDX-License-Identifier: Apache-2.0
//! Golden serde and JSON Schema fixtures for the eight V1 contract records.
//!
//! The canonical typed records are exported by `lean-ctx-protocol`.  The
//! Additive top-level fields are parsed, retained, and re-emitted by every
//! extensible V1 record.

use lean_ctx::core::conformance::{
    ConformanceResult, check_invocation_conformance, check_manifest_conformance,
};
use lean_ctx::core::ocla::invocation::CapabilityResult;
use lean_ctx_protocol::{
    AcceptedOutcomeV1, CapabilityManifestV1, ContextPlanProjectionV1, DecisionRecordV1,
    EvidenceRefV1, ExecutionPlanV1, ExecutionReceiptV1, KnowledgeObjectV1, TaskEnvelopeV1,
    ValidationError,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture_path(directory: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/ocla_contract_suite/v1")
        .join(directory)
        .join(name)
}

fn fixture_body(directory: &str, name: &str) -> String {
    let path = fixture_path(directory, name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn conformance_fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/ocla_contract_suite/v1/conformance")
        .join(name)
}

fn parse_json(directory: &str, name: &str) -> Value {
    let path = fixture_path(directory, name);
    serde_json::from_str(&fixture_body(directory, name))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn typed_round_trip<T, F>(directory: &str, validate: F)
where
    T: DeserializeOwned + Serialize + Debug + PartialEq,
    F: Fn(&T) -> Result<(), ValidationError>,
{
    let minimal: T = serde_json::from_str(&fixture_body(directory, "valid_minimal.json"))
        .unwrap_or_else(|error| panic!("{directory} minimal should deserialize: {error}"));
    validate(&minimal).unwrap_or_else(|error| panic!("{directory} minimal invalid: {error}"));
    let serialized = serde_json::to_string(&minimal)
        .unwrap_or_else(|error| panic!("{directory} minimal should serialize: {error}"));
    let decoded: T = serde_json::from_str(&serialized)
        .unwrap_or_else(|error| panic!("{directory} minimal reparse failed: {error}"));
    assert_eq!(minimal, decoded, "{directory} minimal round trip");

    let maximal: T = serde_json::from_str(&fixture_body(directory, "valid_maximal.json"))
        .unwrap_or_else(|error| panic!("{directory} maximal should deserialize: {error}"));
    validate(&maximal).unwrap_or_else(|error| panic!("{directory} maximal invalid: {error}"));
    let serialized = serde_json::to_string(&maximal)
        .unwrap_or_else(|error| panic!("{directory} maximal should serialize: {error}"));
    let decoded: T = serde_json::from_str(&serialized)
        .unwrap_or_else(|error| panic!("{directory} maximal reparse failed: {error}"));
    assert_eq!(maximal, decoded, "{directory} maximal round trip");

    assert!(
        serde_json::from_str::<T>(&fixture_body(directory, "invalid_missing_required.json"))
            .is_err(),
        "{directory} missing-required fixture must fail"
    );
    assert!(
        serde_json::from_str::<T>(&fixture_body(directory, "invalid_bad_enum.json")).is_err(),
        "{directory} bad-enum fixture must fail"
    );

    let unknown_json = parse_json(directory, "valid_unknown_field.json");
    assert!(
        unknown_json
            .as_object()
            .is_some_and(|object| object.keys().any(|key| key.starts_with("future_"))),
        "{directory} unknown fixture must contain an additive field"
    );
    let unknown: T = serde_json::from_value(unknown_json.clone())
        .unwrap_or_else(|error| panic!("{directory} unknown should deserialize: {error}"));
    validate(&unknown).unwrap_or_else(|error| panic!("{directory} unknown invalid: {error}"));
    let reemitted = serde_json::to_value(&unknown)
        .unwrap_or_else(|error| panic!("{directory} unknown should serialize: {error}"));
    for (key, value) in unknown_json
        .as_object()
        .expect("unknown fixture is an object")
        .iter()
        .filter(|(key, _)| key.starts_with("future_"))
    {
        assert_eq!(reemitted.get(key), Some(value), "{directory} retains {key}");
    }

    for bad_version in [
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!(1.5),
        serde_json::json!("1"),
    ] {
        let mut value = parse_json(directory, "valid_minimal.json");
        value["schema_version"] = bad_version;
        if let Ok(decoded) = serde_json::from_value::<T>(value) {
            assert!(
                validate(&decoded).is_err(),
                "{directory} must reject unsupported schema_version"
            );
        }
    }

    let mut missing_version = parse_json(directory, "valid_minimal.json");
    missing_version
        .as_object_mut()
        .expect("minimal fixture is an object")
        .remove("schema_version");
    assert!(
        serde_json::from_value::<T>(missing_version).is_err(),
        "{directory} must reject a missing schema_version"
    );
}

#[test]
fn task_envelope_v1_fixtures() {
    typed_round_trip::<TaskEnvelopeV1, _>("task-envelope", TaskEnvelopeV1::validate);
}

#[test]
fn execution_plan_v1_fixtures() {
    typed_round_trip::<ExecutionPlanV1, _>("execution-plan", ExecutionPlanV1::validate);
}

#[test]
fn context_plan_v1_fixtures() {
    typed_round_trip::<ContextPlanProjectionV1, _>(
        "context-plan",
        ContextPlanProjectionV1::validate,
    );
}

#[test]
fn execution_receipt_v1_fixtures() {
    typed_round_trip::<ExecutionReceiptV1, _>("execution-receipt", ExecutionReceiptV1::validate);
}

#[test]
fn accepted_outcome_v1_fixtures() {
    typed_round_trip::<AcceptedOutcomeV1, _>("accepted-outcome", AcceptedOutcomeV1::validate);
}

#[test]
fn capability_manifest_v1_fixtures() {
    typed_round_trip::<CapabilityManifestV1, _>(
        "capability-manifest",
        CapabilityManifestV1::validate,
    );
    let unknown: CapabilityManifestV1 = serde_json::from_str(&fixture_body(
        "capability-manifest",
        "valid_unknown_field.json",
    ))
    .expect("capability unknown field should deserialize");
    assert!(unknown.extra.contains_key("future_conformance_extension"));
}

#[test]
fn decision_record_v1_fixtures() {
    typed_round_trip::<DecisionRecordV1, _>("decision-record", DecisionRecordV1::validate);
}

#[test]
fn knowledge_object_v1_fixtures() {
    typed_round_trip::<KnowledgeObjectV1, _>("knowledge-object", KnowledgeObjectV1::validate);
    let unknown: KnowledgeObjectV1 = serde_json::from_str(&fixture_body(
        "knowledge-object",
        "valid_unknown_field.json",
    ))
    .expect("knowledge unknown field should deserialize");
    assert!(unknown.extra.contains_key("future_provenance"));
}

#[test]
fn public_contract_schemas_match_fixture_expectations() {
    let contracts = [
        ("task-envelope", "task-envelope-v1.schema.json"),
        ("context-plan", "context-plan-v1.schema.json"),
        ("execution-plan", "execution-plan-v1.schema.json"),
        ("execution-receipt", "execution-receipt-v1.schema.json"),
        ("accepted-outcome", "accepted-outcome-v1.schema.json"),
        ("decision-record", "decision-record-v1.schema.json"),
        ("capability-manifest", "capability-manifest-v1.schema.json"),
        ("knowledge-object", "knowledge-object-v1.schema.json"),
    ];
    let contract_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts");
    for (directory, schema_name) in contracts {
        let schema: Value = serde_json::from_str(
            &fs::read_to_string(contract_root.join(schema_name))
                .unwrap_or_else(|error| panic!("read {schema_name}: {error}")),
        )
        .unwrap_or_else(|error| panic!("parse {schema_name}: {error}"));
        let validator = jsonschema::validator_for(&schema)
            .unwrap_or_else(|error| panic!("compile {schema_name}: {error}"));
        for (name, should_pass) in [
            ("valid_minimal.json", true),
            ("valid_maximal.json", true),
            ("valid_unknown_field.json", true),
            ("invalid_missing_required.json", false),
            ("invalid_bad_enum.json", false),
        ] {
            let instance = parse_json(directory, name);
            assert_eq!(
                validator.is_valid(&instance),
                should_pass,
                "{schema_name} fixture {directory}/{name}"
            );
        }

        let mut missing_version = parse_json(directory, "valid_minimal.json");
        missing_version
            .as_object_mut()
            .expect("minimal fixture is an object")
            .remove("schema_version");
        assert!(
            !validator.is_valid(&missing_version),
            "{schema_name} must reject a missing schema_version"
        );
    }
}

#[test]
fn evidence_ref_schema_and_rust_validation_stay_in_parity() {
    let contracts = [
        (
            "accepted-outcome",
            "accepted-outcome-v1.schema.json",
            "evidence_refs",
        ),
        (
            "decision-record",
            "decision-record-v1.schema.json",
            "evidence_refs",
        ),
        ("context-plan", "context-plan-v1.schema.json", "evidence"),
        (
            "execution-receipt",
            "execution-receipt-v1.schema.json",
            "evidence_refs",
        ),
    ];
    let cases = [
        (
            "versionless legacy",
            serde_json::json!({
                "kind": "ProviderReceipt",
                "uri": "urn:legacy:receipt",
                "digest": "sha256:legacy-receipt-id",
                "signature_status": "Verified",
                "future_evidence_extension": {"kept": true}
            }),
            true,
        ),
        (
            "null legacy",
            serde_json::json!({
                "schema_version": null,
                "kind": "ProviderReceipt",
                "uri": "urn:null-legacy:receipt",
                "digest": "sha256:null-legacy-receipt-id",
                "signature_status": "Verified",
                "future_evidence_extension": {"kept": true}
            }),
            true,
        ),
        (
            "versioned canonical",
            serde_json::json!({
                "schema_version": 1,
                "kind": "ProviderReceipt",
                "uri": "urn:canonical:receipt",
                "digest": format!("sha256:{}", "a".repeat(64)),
                "signature_status": "Verified",
                "future_evidence_extension": {"kept": true}
            }),
            true,
        ),
        (
            "versioned malformed",
            serde_json::json!({
                "schema_version": 1,
                "kind": "ProviderReceipt",
                "uri": "urn:bad:receipt",
                "digest": "sha256:legacy-receipt-id",
                "signature_status": "Verified",
                "future_evidence_extension": {"kept": true}
            }),
            false,
        ),
        (
            "versionless control character",
            serde_json::json!({
                "kind": "ProviderReceipt",
                "uri": "urn:control:receipt",
                "digest": "legacy\u{0001}receipt-id",
                "signature_status": "Verified",
                "future_evidence_extension": {"kept": true}
            }),
            false,
        ),
    ];
    let contract_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts");
    for (directory, schema_name, evidence_field) in contracts {
        let schema: Value = serde_json::from_str(
            &fs::read_to_string(contract_root.join(schema_name))
                .unwrap_or_else(|error| panic!("read {schema_name}: {error}")),
        )
        .unwrap_or_else(|error| panic!("parse {schema_name}: {error}"));
        let validator = jsonschema::validator_for(&schema)
            .unwrap_or_else(|error| panic!("compile {schema_name}: {error}"));
        for (label, evidence, should_pass) in &cases {
            let typed: EvidenceRefV1 = serde_json::from_value(evidence.clone())
                .unwrap_or_else(|error| panic!("{schema_name} {label} typed parse: {error}"));
            if *should_pass {
                let extension = evidence
                    .get("future_evidence_extension")
                    .expect("passing evidence has extension");
                assert_eq!(
                    typed.extensions.get("future_evidence_extension"),
                    Some(extension),
                    "{schema_name} {label} typed extension retention"
                );
                let reemitted = serde_json::to_value(&typed)
                    .unwrap_or_else(|error| panic!("{schema_name} {label} typed reemit: {error}"));
                assert_eq!(
                    reemitted.get("future_evidence_extension"),
                    Some(extension),
                    "{schema_name} {label} re-emits extension"
                );
            }
            assert_eq!(
                typed.validate().is_ok(),
                *should_pass,
                "{schema_name} {label} Rust validation"
            );
            let mut instance = parse_json(directory, "valid_minimal.json");
            instance
                .as_object_mut()
                .expect("minimal fixture is an object")
                .insert(
                    evidence_field.to_owned(),
                    serde_json::json!([evidence.clone()]),
                );
            assert_eq!(
                validator.is_valid(&instance),
                *should_pass,
                "{schema_name} {label} schema validation"
            );
        }
    }
}

#[test]
fn ocla_capability_schema_is_exact_canonical_mirror() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts");
    let canonical = fs::read(root.join("capability-manifest-v1.schema.json"))
        .expect("read canonical capability schema");
    let ocla = fs::read(root.join("ocla/ocla-capability-manifest-v1.schema.json"))
        .expect("read OCLA capability schema mirror");
    assert_eq!(canonical, ocla, "OCLA capability schema drifted");
}

#[test]
fn capability_schema_covers_all_phase4_wire_kinds() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts");
    let schema: Value = serde_json::from_slice(
        &fs::read(root.join("capability-manifest-v1.schema.json"))
            .expect("read canonical capability schema"),
    )
    .expect("parse canonical capability schema");
    let kinds = schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("kind enum should be an array");
    for expected in [
        "tool",
        "model",
        "provider",
        "context_source",
        "validator",
        "agent_connector",
        "model_provider",
        "knowledge_provider",
        "outcome_evaluator",
        "cost_estimator",
        "quality_estimator",
        "policy_gate",
        "scheduler",
        "read_compression_strategy",
        "search_retrieval",
        "agent_runtime",
        "addon",
        "remote_capability",
        "shell_output_optimization",
        "other",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "missing phase4 kind {expected}"
        );
    }
}

#[test]
fn conformance_fixtures_match_rust_golden_outputs() {
    let names = [
        "manifest_valid.json",
        "manifest_invalid_classification.json",
        "invocation_success.json",
        "invocation_failure_timeout.json",
        "invocation_failure_policy.json",
    ];
    let golden: Value =
        serde_json::from_str(include_str!("../fixtures/ocla_conformance_golden.json"))
            .expect("conformance golden should be valid JSON");

    for name in names {
        let path = conformance_fixture_path(name);
        let fixture: Value = serde_json::from_str(
            &fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let manifest: CapabilityManifestV1 = serde_json::from_value(fixture["manifest"].clone())
            .unwrap_or_else(|error| panic!("{name} manifest should deserialize: {error}"));
        let actual = if fixture["kind"] == "manifest" {
            check_manifest_conformance(&manifest)
        } else {
            let invocation: CapabilityResult = serde_json::from_value(fixture["result"].clone())
                .unwrap_or_else(|error| panic!("{name} result should deserialize: {error}"));
            check_invocation_conformance(&manifest, &invocation)
        };
        let expected: ConformanceResult = serde_json::from_value(fixture["expected"].clone())
            .unwrap_or_else(|error| panic!("{name} expected result should deserialize: {error}"));
        assert_eq!(actual, expected, "fixture {name} differs from Rust output");
        assert_eq!(
            serde_json::to_value(&actual).expect("conformance result should serialize"),
            golden[name],
            "fixture {name} differs from pinned Rust golden output"
        );

        let manifest_round_trip: CapabilityManifestV1 = serde_json::from_value(
            serde_json::to_value(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest round trip should deserialize");
        assert_eq!(manifest, manifest_round_trip, "manifest {name} round trip");
        if fixture["kind"] == "invocation" {
            let invocation: CapabilityResult = serde_json::from_value(fixture["result"].clone())
                .expect("invocation should deserialize");
            let invocation_round_trip: CapabilityResult = serde_json::from_value(
                serde_json::to_value(&invocation).expect("invocation should serialize"),
            )
            .expect("invocation round trip should deserialize");
            assert_eq!(
                invocation, invocation_round_trip,
                "invocation {name} round trip"
            );
        }
    }
}
