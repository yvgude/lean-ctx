// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{receipt_document::digest_bytes, runtime_frame::encode_runtime_frame};
use serde_json::json;

// Public synthetic fixture, not a production/session credential.
const KEY: [u8; 32] = [0x0b; 32];

fn request() -> RuntimeRequestV1 {
    serde_json::from_value(json!({
        "protocol_version": crate::runtime_exchange::RUNTIME_EXCHANGE_VERSION,
        "session_id": "session-a", "request_id": "request-a", "sequence": 1,
        "deadline_unix_ms": 2000, "input": "hello",
        "invocation": {
            "schema_version": 1, "invocation_id": "invocation-a",
            "engine": {"engine_id": "engine-a", "engine_version": "1.0.0"},
            "operation": {"capability_id": "source-read", "capability_version": "1.0.0"},
            "input_ref": "projection:a", "input_digest": digest_bytes(b"hello").unwrap(),
            "source_refs": ["projection:a"],
            "policy_admission": {"policy_ref": "policy:a", "decision": "admitted"}
        }
    }))
    .unwrap()
}

fn frame(request: &RuntimeRequestV1) -> Vec<u8> {
    encode_runtime_frame(
        &KEY,
        RuntimeFrameDirection::Request,
        &[0x0d; 24],
        &request.to_bytes().unwrap(),
    )
    .unwrap()
}

fn error(session: &mut RuntimeRequestSession<'_>, frame: &[u8]) -> RuntimeSessionError {
    session.accept(frame, 1000).err().expect("rejected request")
}

#[test]
fn consumes_sequence_once_without_replay_gaps_or_wraparound() {
    let mut request = request();
    let mut session =
        RuntimeRequestSession::new(&KEY, request.session_id.clone(), &request.invocation).unwrap();
    let first = frame(&request);
    assert!(session.accept(&first, 1000).is_ok());
    assert_eq!(
        error(&mut session, &first),
        RuntimeSessionError::SequenceMismatch
    );
    request.sequence = 3;
    assert_eq!(
        error(&mut session, &frame(&request)),
        RuntimeSessionError::SequenceMismatch
    );
    request.sequence = 2;
    assert!(session.accept(&frame(&request), 1000).is_ok());
    // Direct fixture state simulates the last valid counter without 2^64 requests.
    session.next_sequence = Some(u64::MAX);
    request.sequence = u64::MAX;
    assert!(session.accept(&frame(&request), 1000).is_ok());
    assert_eq!(
        error(&mut session, &first),
        RuntimeSessionError::SequenceExhausted
    );
}

#[test]
fn rejects_scope_changes_without_consuming_valid_sequence() {
    let original = request();
    let mut session =
        RuntimeRequestSession::new(&KEY, original.session_id.clone(), &original.invocation)
            .unwrap();
    for pointer in [
        "/session_id",
        "/invocation/engine/engine_id",
        "/invocation/engine/engine_version",
        "/invocation/operation/capability_id",
        "/invocation/operation/capability_version",
        "/invocation/policy_admission/policy_ref",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        *value.pointer_mut(pointer).unwrap() = json!(if pointer.ends_with("version") {
            "2.0.0"
        } else {
            "other"
        });
        let changed: RuntimeRequestV1 = serde_json::from_value(value).unwrap();
        assert_eq!(
            error(&mut session, &frame(&changed)),
            RuntimeSessionError::ScopeMismatch
        );
    }
    assert!(session.accept(&frame(&original), 1000).is_ok());
}

#[test]
fn authentication_and_deadline_failures_do_not_advance_receiver() {
    let request = request();
    let mut session =
        RuntimeRequestSession::new(&KEY, request.session_id.clone(), &request.invocation).unwrap();
    let bytes = request.to_bytes().unwrap();
    for bad in [
        encode_runtime_frame(
            &[0x0c; 32],
            RuntimeFrameDirection::Request,
            &[0x0d; 24],
            &bytes,
        )
        .unwrap(),
        encode_runtime_frame(&KEY, RuntimeFrameDirection::Response, &[0x0d; 24], &bytes).unwrap(),
    ] {
        assert!(matches!(
            error(&mut session, &bad),
            RuntimeSessionError::Frame(_)
        ));
    }
    let malformed =
        encode_runtime_frame(&KEY, RuntimeFrameDirection::Request, &[0x0d; 24], b"{}").unwrap();
    assert_eq!(
        error(&mut session, &malformed),
        RuntimeSessionError::InvalidRequest
    );
    let valid = frame(&request);
    assert_eq!(
        session.accept(&valid, 2000).err(),
        Some(RuntimeSessionError::InvalidRequest)
    );
    assert!(session.accept(&valid, 1000).is_ok());
}

#[test]
fn rejected_or_invalid_host_scope_cannot_open_receiver() {
    let mut request = request();
    assert!(RuntimeRequestSession::new(&KEY, String::new(), &request.invocation).is_err());
    request.invocation.policy_admission.decision = EnginePolicyDecisionV1::Rejected;
    assert!(RuntimeRequestSession::new(&KEY, request.session_id, &request.invocation).is_err());
}

#[test]
fn authenticated_malformed_typed_scope_is_rejected_before_state_change() {
    let request = request();
    let mut session =
        RuntimeRequestSession::new(&KEY, request.session_id.clone(), &request.invocation).unwrap();
    for (pointer, invalid) in [
        ("/invocation/operation/capability_id", ""),
        ("/invocation/operation/capability_version", "01.0.0"),
        ("/invocation/engine/engine_version", "latest"),
        ("/invocation/policy_admission/policy_ref", "\n"),
    ] {
        let mut value = serde_json::to_value(&request).unwrap();
        *value.pointer_mut(pointer).unwrap() = json!(invalid);
        let authenticated = encode_runtime_frame(
            &KEY,
            RuntimeFrameDirection::Request,
            &[0x0d; 24],
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert_eq!(
            error(&mut session, &authenticated),
            RuntimeSessionError::InvalidRequest
        );
    }
    assert!(session.accept(&frame(&request), 1000).is_ok());
}
