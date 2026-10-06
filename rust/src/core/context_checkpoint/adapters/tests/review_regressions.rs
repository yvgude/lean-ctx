// SPDX-License-Identifier: Apache-2.0

use super::*;

fn reencode(payload: &mut ContextCheckpointLegacyPayloadV1, value: &Value) {
    payload.canonical_json =
        String::from_utf8(wire::canonicalize_json_value(&value).unwrap()).unwrap();
    payload.payload_content_hash = ContextCheckpointLegacyHashV1::new(
        crate::core::hasher::hash_hex(payload.canonical_json.as_bytes()),
    )
    .unwrap();
}

fn materialized(
    source: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
) -> ContextCheckpointLegacyPayloadV1 {
    let ContextCheckpointLegacyMaterializationV1::Materialized(payload) =
        materialize_checkpoint_legacy(source, request).unwrap()
    else {
        panic!("fixture target unexpectedly refused")
    };
    payload
}

#[test]
fn labels_reject_machine_local_and_secret_material() {
    for denied in [
        "/tmp/secret",
        "path=/tmp/secret",
        "api_key=secret",
        "C:\\Users\\alice\\secret",
        "https://host/private",
    ] {
        assert!(
            ContextCheckpointLegacyLabelV1::new(denied).is_err(),
            "{denied}"
        );
    }
}

#[test]
fn snapshot_preserves_recorded_lineage_count() {
    let mut request = snapshot_request();
    let ContextCheckpointLegacyInputsV1::ContextSnapshot(inputs) = &mut request.inputs else {
        unreachable!()
    };
    inputs.ledger_totals.lineage_items_recorded = 7;
    let payload = materialized(&minimal_checkpoint(), &request);
    let value: Value = serde_json::from_str(&payload.canonical_json).unwrap();
    assert_eq!(
        value.pointer("/lineage/items_recorded"),
        Some(&Value::from(7))
    );
}

#[test]
fn owner_round_trip_rejects_nested_unknown_fields() {
    let source = fixture_checkpoint();
    for (request, pointer) in [
        (session_request(), "/session"),
        (handoff_request(), "/ledger"),
    ] {
        let mut payload = materialized(&source, &request);
        let mut value: Value = serde_json::from_str(&payload.canonical_json).unwrap();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), Value::Bool(true));
        reencode(&mut payload, &value);
        assert!(payload.validate().is_err());
    }
}

#[test]
fn handoff_rejects_tampered_content_hash_and_partial_signature() {
    let source = fixture_checkpoint();
    let request = handoff_request();

    let mut tampered = materialized(&source, &request);
    let mut value: Value = serde_json::from_str(&tampered.canonical_json).unwrap();
    *value.pointer_mut("/ledger/content_md5").unwrap() = Value::String("0".repeat(32));
    reencode(&mut tampered, &value);
    assert!(tampered.validate().is_err());

    let mut partial = materialized(&source, &request);
    let mut value: Value = serde_json::from_str(&partial.canonical_json).unwrap();
    value.as_object_mut().unwrap().insert(
        "signer_agent_id".to_owned(),
        Value::String("agent-1".to_owned()),
    );
    reencode(&mut partial, &value);
    assert!(matches!(
        partial.validate(),
        Err(ContextCheckpointLegacyErrorV1::MalformedSignature)
    ));
}

#[test]
fn ctxpkg_shape_without_owner_parser_is_rejected() {
    let source = fixture_checkpoint();
    let mut payload = ContextCheckpointLegacyPayloadV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        source_identity: source_identity(&source),
        target: ContextCheckpointLegacyTargetV1::Ctxpkg,
        target_schema_version: CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        canonical_json: String::new(),
        payload_content_hash: hash(),
        losses: Vec::new(),
    };
    reencode(&mut payload, &serde_json::json!({"schema_version": 2}));
    assert!(payload.validate().is_err());
}

#[test]
fn handoff_represents_evidence_and_large_roi_does_not_saturate() {
    let payload = materialized(&fixture_checkpoint(), &handoff_request());
    assert!(
        !payload
            .losses
            .iter()
            .any(|loss| loss.field_path.as_str() == "lineage.evidence_refs")
    );
    let value: Value = serde_json::from_str(&payload.canonical_json).unwrap();
    assert!(value.pointer("/ledger/evidence_keys").is_some());

    let roi = ContextCheckpointLegacyRoiV1 {
        input_tokens: u64::MAX,
        output_tokens: 0,
        tokens_saved: u64::MAX,
    };
    let rate = wire::compression_rate(&roi);
    assert!(rate.is_finite());
    assert!((rate - 0.5).abs() < f64::EPSILON);
}
