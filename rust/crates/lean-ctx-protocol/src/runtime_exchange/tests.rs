// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde_json::{Value, json};

#[test]
fn a_request_without_lease_is_byte_identical_and_a_lease_is_bounded() {
    let plain = request();
    let bytes = plain.to_bytes().unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("lease_b64"));

    let mut leased = request();
    leased.lease_b64 = Some("eyJzY2hlbWEiOiJ4In0=".into());
    assert!(leased.validate().is_ok());
    assert!(String::from_utf8_lossy(&leased.to_bytes().unwrap()).contains("\"lease_b64\""));
    assert_ne!(
        leased.digest().unwrap(),
        plain.digest().unwrap(),
        "the lease is bound"
    );

    for bad in [
        "",
        "not base64!",
        "abc",
        &"A".repeat(MAX_RUNTIME_LEASE_B64_BYTES + 4),
    ] {
        leased.lease_b64 = Some(bad.into());
        assert!(leased.validate().is_err(), "{bad:.20}");
    }
}

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

fn with_input(bytes: usize) -> RuntimeRequestV1 {
    let mut request = request();
    request.input = "a".repeat(bytes);
    request.invocation.input_digest = digest_bytes(request.input.as_bytes()).unwrap();
    request
}

fn with_output(request: &RuntimeRequestV1, output: String) -> RuntimeResponseV1 {
    let mut response = response(request);
    response.observation.output_digest = Some(digest_bytes(output.as_bytes()).unwrap());
    response.output = Some(output);
    response
}

#[test]
fn profiles_bound_content_and_encoding_separately_and_ordinary_is_unchanged() {
    let ordinary = RuntimeExchangeProfile::ORDINARY;
    let verifier = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    let ceilings = |profile: RuntimeExchangeProfile| {
        [
            profile.max_input_bytes(),
            profile.max_output_bytes(),
            profile.max_request_bytes(),
            profile.max_response_bytes(),
        ]
    };
    assert_eq!(ceilings(ordinary), [MAX_RUNTIME_EXCHANGE_BYTES; 4]);
    assert_eq!(
        ceilings(verifier),
        [9_824_944, 6_307_840, 9_857_712, 7_389_184]
    );

    // Input content: the ceiling passes, one byte more does not, and the
    // functions without a profile keep the ordinary megabyte exactly.
    let widest = with_input(verifier.max_input_bytes());
    assert!(widest.validate_in(verifier).is_ok());
    assert!(widest.validate().is_err());
    assert!(
        with_input(verifier.max_input_bytes() + 1)
            .validate_in(verifier)
            .is_err()
    );
    assert!(with_input(MAX_RUNTIME_EXCHANGE_BYTES).validate().is_ok());
    assert!(
        with_input(MAX_RUNTIME_EXCHANGE_BYTES + 1)
            .validate()
            .is_err()
    );

    // Encoding is refused while it is written, at the exact byte; a profile
    // changes no byte and no digest.
    let encoded = widest.to_bytes_in(verifier).unwrap();
    assert!(encoded.len() <= verifier.max_request_bytes());
    assert!(encode(&widest, encoded.len()).unwrap() == encoded);
    assert!(encode(&widest, encoded.len() - 1).is_err());
    assert!(request().digest_in(verifier).unwrap() == request().digest().unwrap());
    assert!(request().to_bytes_in(verifier).unwrap() == request().to_bytes().unwrap());
    assert!(RuntimeRequestV1::from_bytes_in(&encoded, 1000, verifier).unwrap() == widest);
    assert!(RuntimeRequestV1::from_bytes(&encoded, 1000).is_err());
}

#[test]
fn the_widest_escaped_answer_fits_its_profile_and_anything_wider_is_refused() {
    let verifier = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    let request = request();
    let pack_bytes = crate::org_policy_verifier::MAX_ORG_POLICY_VERIFIED_PACK_BYTES;
    // The worst a pack can be for two JSON layers: every byte a control
    // character, six bytes in the answer and seven on the wire.
    let pack = "\u{1}".repeat(pack_bytes);
    let answer = serde_json::to_string(&json!({ "pack_toml": pack })).unwrap();
    assert!(answer.len() > 6 * pack_bytes && answer.len() <= verifier.max_output_bytes());
    let response = with_output(&request, answer);
    let encoded = response.to_bytes_in(&request, verifier).unwrap();
    assert!(encoded.len() > 7 * pack_bytes && encoded.len() <= verifier.max_response_bytes());
    assert!(
        RuntimeResponseV1::from_bytes_in(&encoded, &request, 1000, verifier).unwrap() == response
    );
    assert!(response.to_bytes(&request).is_err());
    assert!(RuntimeResponseV1::from_bytes(&encoded, &request, 1000).is_err());

    // Output content has its own ceiling, and content inside it that the
    // encoding would stretch past the response ceiling stops the encoder.
    let over = with_output(&request, "a".repeat(verifier.max_output_bytes() + 1));
    assert!(over.validate_for_in(&request, verifier).is_err());
    let stretched = with_output(&request, "\"".repeat(verifier.max_output_bytes()));
    assert!(stretched.validate_for_in(&request, verifier).is_ok());
    assert!(stretched.to_bytes_in(&request, verifier).is_err());
}

#[test]
fn the_widest_verifier_input_byte_shape_and_its_request_fit_the_verifier_profile() {
    use crate::org_policy_verifier::{
        MAX_ORG_REQUIRED_MARKER_BYTES, ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID,
        OrgPolicyVerifyRequestV1,
    };
    let verifier = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    // The typed request at its widest: the longest organization, all three
    // snapshot digests present and timestamps carrying nanoseconds.
    let digest = |fill: char| format!("sha256:{}", fill.to_string().repeat(64));
    let typed: OrgPolicyVerifyRequestV1 = serde_json::from_value(json!({
        "schema": ORG_POLICY_VERIFY_REQUEST_SCHEMA_ID,
        "organization_id": format!("a{}a", "-".repeat(126)),
        "artifact_digest": digest('a'), "trust_digest": digest('b'),
        "marker_digest": digest('c'), "nonce": digest('d'),
        "requested_at": "2026-10-10T09:00:00.123456789Z",
        "deadline": "2026-10-10T09:00:05.123456789Z"
    }))
    .unwrap();
    typed.validate().unwrap();
    // Artifact, trust store and marker each at the ceiling the host captures
    // and the adapter decodes (the artifact's is the profile's output width):
    // canonical padded standard base64 of that many zero bytes, which JSON never
    // escapes. A neutral maximum-width byte shape, not a signed legal pack and
    // no evidence of origin.
    let base64 = |bytes: usize| {
        let tail = match bytes % 3 {
            1 => "AA==",
            2 => "AAA=",
            _ => "",
        };
        format!("{}{tail}", "AAAA".repeat(bytes / 3))
    };
    let input = json!({
        "request": typed,
        "artifact": base64(verifier.max_output_bytes()),
        "trust": base64(MAX_ORG_REQUIRED_MARKER_BYTES),
        "marker": base64(MAX_ORG_REQUIRED_MARKER_BYTES)
    })
    .to_string();
    assert!(input.len() <= verifier.max_input_bytes());

    // The runtime request around it, named the way the host names it; its
    // input is escaped once more on the wire and still fits the request.
    let binding = digest_bytes(input.as_bytes()).unwrap();
    let hex = binding.as_str().strip_prefix("sha256:").unwrap();
    let name = format!("orgverify-{hex}");
    let input_ref = format!("projection:sha256:{hex}");
    let request: RuntimeRequestV1 = serde_json::from_value(json!({
        "protocol_version": RUNTIME_EXCHANGE_VERSION,
        "session_id": name, "request_id": name,
        "sequence": 1, "deadline_unix_ms": 2000, "input": input,
        "invocation": {
            "schema_version": 1, "invocation_id": name,
            "engine": {"engine_id": "leanctx-intelligence", "engine_version": "1.1.0"},
            "operation": {
                "capability_id": "enterprise.runtime.org_policy_verifier",
                "capability_version": "1.0.0"
            },
            "input_ref": input_ref, "input_digest": binding, "source_refs": [input_ref],
            "policy_admission": {"policy_ref": format!("policy:{name}"), "decision": "admitted"}
        }
    }))
    .unwrap();
    let encoded = request.to_bytes_in(verifier).unwrap();
    assert!(RuntimeRequestV1::from_bytes_in(&encoded, 1000, verifier).unwrap() == request);
}

#[test]
fn decoding_takes_each_ceiling_exactly_and_refuses_one_byte_more() {
    // Valid JSON padded with whitespace: only the length can refuse it, so
    // the ceiling and one byte past it separate the size check from parsing.
    let padded = |bytes: &[u8], length: usize| {
        let mut padded = bytes.to_vec();
        padded.resize(length, b' ');
        padded
    };
    let request = request();
    let response = response(&request);
    let asked = request.to_bytes().unwrap();
    let answered = response.to_bytes(&request).unwrap();

    // Without a profile: the ordinary megabyte in both directions.
    let limit = MAX_RUNTIME_EXCHANGE_BYTES;
    let (exact, over) = (padded(&asked, limit), padded(&asked, limit + 1));
    assert!(RuntimeRequestV1::from_bytes(&exact, 1000).unwrap() == request);
    assert!(RuntimeRequestV1::from_bytes(&over, 1000).is_err());
    let (exact, over) = (padded(&answered, limit), padded(&answered, limit + 1));
    assert!(RuntimeResponseV1::from_bytes(&exact, &request, 1000).unwrap() == response);
    assert!(RuntimeResponseV1::from_bytes(&over, &request, 1000).is_err());

    // Under the verifier profile each direction keeps its own ceiling: a
    // response is never decoded against the wider request ceiling.
    let verifier = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    let limit = verifier.max_request_bytes();
    let (exact, over) = (padded(&asked, limit), padded(&asked, limit + 1));
    assert!(RuntimeRequestV1::from_bytes_in(&exact, 1000, verifier).unwrap() == request);
    assert!(RuntimeRequestV1::from_bytes_in(&over, 1000, verifier).is_err());
    let limit = verifier.max_response_bytes();
    let (exact, over) = (padded(&answered, limit), padded(&answered, limit + 1));
    assert!(
        RuntimeResponseV1::from_bytes_in(&exact, &request, 1000, verifier).unwrap() == response
    );
    assert!(RuntimeResponseV1::from_bytes_in(&over, &request, 1000, verifier).is_err());
}
