//! Canonical JSON serialization and Ed25519 signatures for signed artifacts.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use lean_ctx_protocol::ExecutionReceiptV1;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use super::agent_identity::{sign_bytes_with, verify_signature};

/// Error returned when a signed artifact cannot be encoded or decoded.
pub type Error = String;

/// The Ed25519 identity used by the existing agent identity store.
pub type AgentIdentity = SigningKey;

/// Serialize `value` as compact, deterministic JSON with recursively sorted
/// object keys.
///
/// `serde_json::to_vec` emits UTF-8 JSON without trailing whitespace or an
/// optional newline. Sorting after serialization through `Value` also removes
/// any dependence on Rust struct declaration order or map implementation.
#[must_use]
pub fn canonical_serialize<T: Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("value must be JSON serializable");
    serde_json::to_vec(&sort_json(value)).expect("JSON value must be serializable")
}

/// `value` in canonical form: every object's keys sorted by byte order,
/// recursively; arrays keep their order. The one canonical JSON of the engine:
/// digests, signatures and exact-bytes gates all go through it.
#[must_use]
pub fn canonical_value(value: &Value) -> Value {
    sort_json(value.clone())
}

/// Compact canonical JSON bytes of `value` ([`canonical_value`], no
/// whitespace, scalars and escapes as `serde_json` writes them).
#[must_use]
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&sort_json(value.clone())).expect("JSON values always serialize")
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let sorted: BTreeMap<String, Value> = object
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        scalar => scalar,
    }
}

fn receipt_without_signature(receipt: &ExecutionReceiptV1) -> Result<Value, Error> {
    let mut value = serde_json::to_value(receipt)
        .map_err(|error| format!("serialize execution receipt: {error}"))?;
    value
        .as_object_mut()
        .ok_or_else(|| "execution receipt must serialize as a JSON object".to_string())?
        .remove("signature");
    Ok(value)
}

fn signature_bytes(signature: &str, artifact: &str) -> Result<Vec<u8>, Error> {
    STANDARD
        .decode(signature)
        .map_err(|error| format!("invalid {artifact} base64 signature: {error}"))
}

/// Sign an execution receipt without its `signature` field.
pub fn sign_receipt(
    receipt: &ExecutionReceiptV1,
    identity: &AgentIdentity,
) -> Result<String, Error> {
    let unsigned = receipt_without_signature(receipt)?;
    let signature = sign_bytes_with(identity, &canonical_serialize(&unsigned));
    Ok(STANDARD.encode(signature))
}

/// Verify an execution receipt whose `signature` field is populated.
pub fn verify_receipt_signature(
    receipt: &ExecutionReceiptV1,
    public_key: &[u8],
) -> Result<bool, Error> {
    let signature = signature_bytes(&receipt.signature, "receipt")?;
    let unsigned = receipt_without_signature(receipt)?;
    Ok(verify_signature(
        public_key,
        &canonical_serialize(&unsigned),
        &signature,
    ))
}

fn manifest_without_signature(manifest: &Value) -> Result<(Value, Option<String>), Error> {
    let mut unsigned = manifest.clone();
    let mut signature = None;

    if let Some(object) = unsigned.as_object_mut()
        && let Some(value) = object.remove("signature")
    {
        signature = Some(
            value
                .as_str()
                .ok_or_else(|| "manifest signature must be a string".to_string())?
                .to_owned(),
        );
    }

    if let Some(signing) = unsigned.get_mut("signing")
        && let Some(object) = signing.as_object_mut()
        && let Some(value) = object.remove("signature")
    {
        if signature.is_some() {
            return Err("manifest has multiple signature fields".to_string());
        }
        signature = Some(
            value
                .as_str()
                .ok_or_else(|| "manifest signature must be a string".to_string())?
                .to_owned(),
        );
    }

    Ok((unsigned, signature))
}

/// Sign an evidence bundle manifest without its signature field.
pub fn sign_manifest(manifest: &Value, identity: &AgentIdentity) -> Result<String, Error> {
    let (unsigned, _) = manifest_without_signature(manifest)?;
    let signature = sign_bytes_with(identity, &canonical_serialize(&unsigned));
    Ok(STANDARD.encode(signature))
}

/// Verify an evidence bundle manifest whose signature is top-level or nested
/// under its `signing` object.
pub fn verify_manifest_signature(manifest: &Value, public_key: &[u8]) -> Result<bool, Error> {
    let (unsigned, signature) = manifest_without_signature(manifest)?;
    let signature = signature.ok_or_else(|| "manifest signature is missing".to_string())?;
    let signature = signature_bytes(&signature, "manifest")?;
    Ok(verify_signature(
        public_key,
        &canonical_serialize(&unsigned),
        &signature,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The one canonical form, pinned: nested objects in insertion order,
    /// escaped and non-ASCII keys, numbers, arrays of objects. Eight former
    /// private copies (handoff ledger digests, checkpoint wire gates, bundle
    /// manifests, customer proofs) now produce exactly these bytes.
    #[test]
    fn canonical_form_is_pinned() {
        let value = json!({
            "z": [{"b": 1, "a": [3, 2]}, null, true],
            "ä": "ü\n\"",
            "a": {"y": 1.5, "x": -7, "\u{1}": "ctl"},
        });
        let expected = r#"{"a":{"\u0001":"ctl","x":-7,"y":1.5},"z":[{"a":[3,2],"b":1},null,true],"ä":"ü\n\""}"#;
        assert_eq!(
            String::from_utf8(canonical_json_bytes(&value)).unwrap(),
            expected
        );
        assert_eq!(canonical_serialize(&value), expected.as_bytes());
        assert_eq!(canonical_value(&value).to_string(), expected);
    }

    fn key(seed: u8) -> AgentIdentity {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn receipt() -> ExecutionReceiptV1 {
        serde_json::from_value(json!({
            "schema_version": 1,
            "receipt_id": "receipt-1",
            "task_id": "task-1",
            "plan_id": "plan-1",
            "context_balance": {
                "original_tokens": 1_000,
                "materialized_tokens": 800,
                "delivered_tokens": 700,
                "provider_billed_tokens": 650
            },
            "fresh_input_tokens": 600,
            "cached_input_tokens": 100,
            "output_tokens": 200,
            "reasoning_tokens": 50,
            "requested_model": "model-requested",
            "selected_model": "model-selected",
            "provider": "provider-1",
            "model_calls": 2,
            "retries": 1,
            "latency_ms": 900,
            "actual_cost_micros": 2_000,
            "baseline_cost_micros": 3_000,
            "avoided_cost_micros": 1_000,
            "etpao_milli": 1_250,
            "outcome_ref": "outcome:1",
            "knowledge_refs": ["knowledge:1"],
            "decision_refs": ["decision:1"],
            "evidence_refs": [],
            "signature": ""
        }))
        .expect("receipt should deserialize")
    }

    #[test]
    fn canonical_serialization_is_sorted_compact_and_deterministic() {
        let value = json!({
            "z": { "d": 4, "a": 1 },
            "a": 2,
            "array": [{ "b": 1, "a": 0 }]
        });
        let expected = br#"{"a":2,"array":[{"a":0,"b":1}],"z":{"a":1,"d":4}}"#;
        let first = canonical_serialize(&value);

        assert_eq!(first, expected);
        assert_eq!(first, canonical_serialize(&value));
        assert!(!first.ends_with(b"\n"));
        assert!(std::str::from_utf8(&first).is_ok());
    }

    #[test]
    fn canonical_form_survives_serialize_deserialize_cycle() {
        let value = json!({"b": {"z": true, "a": null}, "a": [3, 2, 1]});
        let bytes = canonical_serialize(&value);
        let decoded: Value = serde_json::from_slice(&bytes).expect("canonical JSON is valid");

        assert_eq!(bytes, canonical_serialize(&decoded));
    }

    #[test]
    fn receipt_sign_and_verify_round_trip() {
        let signing_key = key(7);
        let mut signed = receipt();
        signed.signature = sign_receipt(&signed, &signing_key).expect("receipt should sign");

        assert!(
            verify_receipt_signature(&signed, &signing_key.verifying_key().to_bytes())
                .expect("receipt should verify")
        );
    }

    #[test]
    fn tampered_receipt_bytes_fail_verification() {
        let signing_key = key(7);
        let mut signed = receipt();
        signed.signature = sign_receipt(&signed, &signing_key).expect("receipt should sign");
        signed.provider = "tampered-provider".to_owned();

        assert!(
            !verify_receipt_signature(&signed, &signing_key.verifying_key().to_bytes())
                .expect("tampered receipt should be checked")
        );
    }

    #[test]
    fn different_key_fails_verification() {
        let signing_key = key(7);
        let other_key = key(8);
        let mut signed = receipt();
        signed.signature = sign_receipt(&signed, &signing_key).expect("receipt should sign");

        assert!(
            !verify_receipt_signature(&signed, &other_key.verifying_key().to_bytes())
                .expect("wrong key should be checked")
        );
    }

    #[test]
    fn manifest_sign_and_verify_round_trip() {
        let signing_key = key(9);
        let mut manifest = json!({
            "files": [{"path": "a.txt", "sha256": "abc"}],
            "signing": {"algorithm": "ed25519", "signature": ""}
        });
        let signature = sign_manifest(&manifest, &signing_key).expect("manifest should sign");
        manifest["signing"]["signature"] = Value::String(signature);

        assert!(
            verify_manifest_signature(&manifest, &signing_key.verifying_key().to_bytes())
                .expect("manifest should verify")
        );
    }
}
