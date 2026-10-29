// SPDX-License-Identifier: Apache-2.0
//! Signing of C0 2.0 events (SPEC-ENGINE §3.3.1, §3.3.5;
//! `vectors/signing.vectors.json`, `vectors/body_commit.vectors.json`).
//!
//! The signature covers `"leanctx.hive.event.v2\n" ‖ canonical_json(event)`
//! where the event is taken without its body (bound through
//! `body_commit`), `ck_wrapped`, `signature` and every server-set field.

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde_json::{Map, Value};

use lean_ctx_protocol::hive::{canonical_json, signing_input};

/// Signature domain of an event.
pub const EVENT_DOMAIN: &str = "leanctx.hive.event.v2\n";

/// Fields that are not part of the signed envelope.
const UNSIGNED: [&str; 14] = [
    "body",
    "ck_wrapped",
    "signature",
    "seq",
    "received_at",
    "provenance",
    "review_state",
    "reviewed_by",
    "review_id",
    "batch_id",
    "purged",
    "key_lost",
    "revoked_signer",
    "legacy",
];

/// `hex(blake3(public_key))[..16]`.
pub fn key_id(public_key: &[u8; 32]) -> String {
    blake3::hash(public_key).to_hex()[..16].to_string()
}

/// The signed envelope: the event without body, `ck_wrapped`, signature
/// and server fields.
pub fn envelope(event: &Value) -> Result<Map<String, Value>, String> {
    let Value::Object(fields) = event else {
        return Err("an event is a JSON object".into());
    };
    let mut fields = fields.clone();
    for name in UNSIGNED {
        fields.remove(name);
    }
    Ok(fields)
}

/// The exact bytes an event signature is made over.
pub fn event_signing_input(event: &Value) -> Result<Vec<u8>, String> {
    signing_input(EVENT_DOMAIN, &Value::Object(envelope(event)?)).map_err(|e| e.to_string())
}

/// Sign `event` in place with `key` (sets `key_id` and `signature`).
pub fn sign(event: &mut Value, key: &SigningKey) -> Result<(), String> {
    let kid = key_id(key.verifying_key().as_bytes());
    let Value::Object(fields) = event else {
        return Err("an event is a JSON object".into());
    };
    fields.insert("key_id".into(), Value::String(kid));
    let signature = key.sign(&event_signing_input(event)?);
    if let Value::Object(fields) = event {
        fields.insert(
            "signature".into(),
            Value::String(hex::encode(signature.to_bytes())),
        );
    }
    Ok(())
}

/// Whether `event` carries a valid signature of `public_key` whose key id
/// it names.
pub fn verify(event: &Value, public_key: &[u8; 32]) -> bool {
    let Some(sig_hex) = event.get("signature").and_then(Value::as_str) else {
        return false;
    };
    if event.get("key_id").and_then(Value::as_str) != Some(key_id(public_key).as_str()) {
        return false;
    }
    let (Ok(sig), Ok(key)) = (
        hex::decode(sig_hex)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
            .ok_or(()),
        VerifyingKey::from_bytes(public_key),
    ) else {
        return false;
    };
    event_signing_input(event)
        .is_ok_and(|input| key.verify(&input, &Signature::from_bytes(&sig)).is_ok())
}

/// `hex(blake3(canonical_json(envelope + signature)))`: the leaf digest of
/// a signed envelope.
pub fn envelope_digest(event: &Value) -> Result<String, String> {
    let mut fields = envelope(event)?;
    if let Some(sig) = event.get("signature") {
        fields.insert("signature".into(), sig.clone());
    }
    Ok(
        blake3::hash(&canonical_json(&Value::Object(fields)).map_err(|e| e.to_string())?)
            .to_hex()
            .to_string(),
    )
}

/// `hex(blake3_keyed(key, canonical_json(body)))`; `key` is the place's
/// `id_salt` (or the sealed `pk_id`).
pub fn body_commit(key: &[u8; 32], body: &Value) -> Result<String, String> {
    Ok(
        blake3::keyed_hash(key, &canonical_json(body).map_err(|e| e.to_string())?)
            .to_hex()
            .to_string(),
    )
}

/// A fresh `body_nonce`: 16 random bytes, hex.
pub fn new_body_nonce() -> Result<String, crate::core::hive::HiveError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| crate::core::hive::HiveError::Local(format!("random: {e}")))?;
    Ok(hex::encode(bytes))
}
