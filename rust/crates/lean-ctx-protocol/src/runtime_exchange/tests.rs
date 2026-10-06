// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde_json::{Value, json};

fn request() -> RuntimeRequestV1 {
    serde_json::from_value(json!({
        "protocol_version": RUNTIME_EXCHANGE_VERSION,
        "session_id": "test-session", "request_id": "test-request",
        "sequence": 1, "deadline_unix_ms": 2000, "input": "hello",
        "invocation": {
            "schema_version": 1, "invocation_id": "test-invocation",
            "engine": {"engine_id": "test-peer", "engine_version": "1.0.0"},
            "operation": {"capability_id": "source-read", "capability_version": "1.0.0"},
            "input_ref": "projection:test", "input_digest": digest_bytes(b"hello").unwrap(),
            "source_refs": ["projection:test"],
            "policy_admission": {"policy_ref": "policy:test", "decision": "admitted"}
        }
    }))
    .expect("valid request fixture")
}

fn response(request: &RuntimeRequestV1) -> RuntimeResponseV1 {
    serde_json::from_value(json!({
        "protocol_version": RUNTIME_EXCHANGE_VERSION,
        "request_digest": request.digest().unwrap(), "output": "world",
        "observation": {
            "schema_version": 1, "invocation_id": "test-invocation", "status": "succeeded",
            "output_ref": "projection:result", "output_digest": digest_bytes(b"world").unwrap(),
            "source_lineage": ["projection:test"], "measurements": []
        }
    }))
    .expect("valid response fixture")
}

#[test]
fn exchange_round_trip_preserves_engine_records_and_exact_bytes() {
    let request = request();
    let bytes = request.to_bytes().unwrap();
    assert!(RuntimeRequestV1::from_bytes(&bytes, 1000).unwrap() == request);
    assert_eq!(
        request.invocation.input_digest.as_str(),
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    let response = response(&request);
    let bytes = response.to_bytes(&request).unwrap();
    assert!(RuntimeResponseV1::from_bytes(&bytes, &request, 1000).unwrap() == response);
    assert_eq!(request.to_bytes().unwrap(), concat!(
        r#"{"protocol_version":"leanctx.runtime-exchange/v1","session_id":"test-session","request_id":"test-request","sequence":1,"deadline_unix_ms":2000,"invocation":{"schema_version":1,"invocation_id":"test-invocation","engine":{"engine_id":"test-peer","engine_version":"1.0.0"},"operation":{"capability_id":"source-read","capability_version":"1.0.0"},"input_ref":"projection:test","input_digest":"sha256:"#,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        r#"","source_refs":["projection:test"],"policy_admission":{"policy_ref":"policy:test","decision":"admitted"}},"input":"hello"}"#,
    ).as_bytes());
}

#[test]
fn request_rejects_version_fields_admission_digest_and_time_failures() {
    let baseline = serde_json::to_value(request()).unwrap();
    for (pointer, value) in [
        ("/protocol_version", json!("leanctx.protocol/v4")),
        ("/sequence", json!(0)),
        ("/session_id", json!("")),
        ("/request_id", json!("")),
        ("/deadline_unix_ms", json!(0)),
        ("/input", json!("tampered")),
        ("/invocation/policy_admission/decision", json!("rejected")),
    ] {
        let mut changed = baseline.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            RuntimeRequestV1::from_bytes(&serde_json::to_vec(&changed).unwrap(), 1000).is_err()
        );
    }
    let mut unknown = baseline;
    unknown["invocation"]["unexpected"] = Value::Null;
    assert!(RuntimeRequestV1::from_bytes(&serde_json::to_vec(&unknown).unwrap(), 1000).is_err());
    let request = request();
    assert!(request.validate_at(2000).is_err());
    assert!(request.validate_at(u64::MAX).is_err());
    let mut distant = request;
    distant.deadline_unix_ms = 1001 + MAX_RUNTIME_DEADLINE_MS;
    assert!(distant.validate_at(1000).is_err());
}

#[test]
fn response_binds_entire_request_and_output_not_only_invocation_id() {
    let request = request();
    let response = response(&request);
    let mut changed = request.clone();
    changed.sequence += 1;
    assert!(response.validate_for(&changed).is_err());
    changed = request.clone();
    changed.invocation.operation.capability_id = crate::CapabilityId::new("another").unwrap();
    assert!(response.validate_for(&changed).is_err());
    let mut changed = response.clone();
    changed.output = Some("tampered".into());
    assert!(changed.validate_for(&request).is_err());
    changed.output = None;
    assert!(changed.validate_for(&request).is_err());
    changed = response.clone();
    changed.observation.source_lineage =
        vec![crate::ProtocolReference::new("other:source").unwrap()];
    assert!(changed.validate_for(&request).is_err());
    assert!(
        RuntimeResponseV1::from_bytes(&response.to_bytes(&request).unwrap(), &request, 2000)
            .is_err()
    );
}

#[test]
fn decoding_rejects_duplicates_trailing_data_and_encoded_oversize() {
    let bytes = request().to_bytes().unwrap();
    let duplicate = format!(
        "{{\"sequence\":1,{}",
        std::str::from_utf8(&bytes[1..]).unwrap()
    );
    assert!(RuntimeRequestV1::from_bytes(duplicate.as_bytes(), 1000).is_err());
    for bytes in [
        vec![],
        vec![0xff],
        vec![b' '; MAX_RUNTIME_EXCHANGE_BYTES + 1],
    ] {
        assert!(RuntimeRequestV1::from_bytes(&bytes, 1000).is_err());
    }
    let mut trailing = bytes;
    trailing.extend_from_slice(b"{}");
    assert!(RuntimeRequestV1::from_bytes(&trailing, 1000).is_err());
    let mut expanded = request();
    expanded.input = "\0".repeat(MAX_RUNTIME_EXCHANGE_BYTES / 2);
    expanded.invocation.input_digest = digest_bytes(expanded.input.as_bytes()).unwrap();
    assert!(expanded.to_bytes().is_err());
}

#[test]
fn semantic_correlation_does_not_replace_exact_byte_authentication() {
    use crate::runtime_frame::{
        RUNTIME_FRAME_TAG_BYTES, RuntimeFrameDirection, decode_runtime_frame, encode_runtime_frame,
    };
    let original = request();
    let compact = original.to_bytes().unwrap();
    let pretty = serde_json::to_vec_pretty(&serde_json::to_value(&original).unwrap()).unwrap();
    assert_ne!(compact, pretty);
    let decoded = RuntimeRequestV1::from_bytes(&pretty, 1000).unwrap();
    assert_eq!(original.digest().unwrap(), decoded.digest().unwrap());
    assert!(response(&original).validate_for(&decoded).is_ok());

    // Public synthetic fixtures; each representation needs its own valid AEAD tag.
    let key = [0x0b; 32];
    let direction = RuntimeFrameDirection::Request;
    let compact_frame = encode_runtime_frame(&key, direction, &[0x0d; 24], &compact).unwrap();
    let mut pretty_frame = encode_runtime_frame(&key, direction, &[0x0e; 24], &pretty).unwrap();
    assert_eq!(
        decode_runtime_frame(&key, direction, &pretty_frame).unwrap(),
        pretty
    );
    let pretty_tag = pretty_frame.len() - RUNTIME_FRAME_TAG_BYTES;
    let compact_tag = compact_frame.len() - RUNTIME_FRAME_TAG_BYTES;
    pretty_frame[pretty_tag..].copy_from_slice(&compact_frame[compact_tag..]);
    assert!(decode_runtime_frame(&key, direction, &pretty_frame).is_err());
}

#[test]
fn peer_cannot_attach_an_otherwise_valid_host_receipt_link() {
    let request = request();
    let mut response = response(&request);
    response.observation.receipt_link = Some(crate::EngineReceiptLinkV1 {
        schema_version: crate::V1_SCHEMA_VERSION,
        receipt_id: crate::ReceiptId::new("peer-receipt").unwrap(),
        receipt_ref: crate::ProtocolReference::new("receipt:peer").unwrap(),
        receipt_digest: digest_bytes(b"receipt").unwrap(),
        invocation_id: request.invocation.invocation_id.clone(),
    });
    assert!(
        response
            .observation
            .validate_for(&request.invocation)
            .is_ok()
    );
    assert!(response.validate_for(&request).is_err());
}
