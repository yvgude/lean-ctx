//! Formal, verifiable prompt-cache safety for proxy request rewrites.
//!
//! A provider can reuse a prompt-cache entry only while its cached prefix stays
//! byte-identical. This module makes that invariant observable: it canonicalizes
//! cache-relevant messages, hashes their frozen prefix, and writes compact audit
//! records for offline verification.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use axum::{
    http::{HeaderMap, HeaderValue, request::Parts},
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A cryptographic proof that a provider-cached message prefix was preserved.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeterminismProof {
    /// Stable request identifier supplied by the proxy trace layer.
    pub request_id: String,
    /// BLAKE3 of the cache-relevant prefix before lean-ctx processing.
    pub prefix_hash_before: String,
    /// BLAKE3 of the cache-relevant prefix after lean-ctx processing.
    pub prefix_hash_after: String,
    /// `true` only when both prefix hashes are identical.
    pub is_stable: bool,
    /// Exact number of canonical JSON bytes equal before the first modification.
    pub frozen_bytes: usize,
    /// Byte offset at which lean-ctx processing first differs from the input.
    pub modification_start_byte: usize,
}

/// Verifies transformations under one proxy trace id.
#[derive(Clone, Debug)]
pub struct DeterminismGuard {
    request_id: String,
}

impl DeterminismGuard {
    #[must_use]
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
        }
    }

    /// Produces a proof for the supplied request id.
    #[must_use]
    pub fn verify(&self, before: &[Value], after: &[Value]) -> DeterminismProof {
        verify_with_request_id(&self.request_id, before, after)
    }
}

/// Verifies that `after` leaves the provider-cached prefix of `before` intact.
///
/// Both slices are canonical JSON arrays. `frozen_bytes` is their common byte
/// prefix. The proof hashes the full cached-message prefix when one exists. If
/// the client did not disclose a cache breakpoint, the whole message list is
/// frozen: this conservative default avoids guessing an automatic cache window.
#[must_use]
pub fn verify_determinism(before: &[Value], after: &[Value]) -> DeterminismProof {
    let request_id = blake3::hash(&canonical_messages(before))
        .to_hex()
        .to_string();
    verify_with_request_id(&request_id, before, after)
}

fn verify_with_request_id(request_id: &str, before: &[Value], after: &[Value]) -> DeterminismProof {
    let before_bytes = canonical_messages(before);
    let after_bytes = canonical_messages(after);
    let frozen_bytes = common_prefix_len(&before_bytes, &after_bytes);
    let cached_messages = cache_breakpoint_len(before);
    let proof_prefix_bytes = if cached_messages == 0 {
        before_bytes.len()
    } else {
        canonical_message_prefix_len(before, cached_messages)
    };
    let before_prefix = &before_bytes[..proof_prefix_bytes];
    let after_prefix = after_bytes
        .get(..proof_prefix_bytes)
        .unwrap_or(&after_bytes);
    let prefix_hash_before = blake3::hash(before_prefix).to_hex().to_string();
    let prefix_hash_after = blake3::hash(after_prefix).to_hex().to_string();

    DeterminismProof {
        request_id: request_id.to_owned(),
        is_stable: prefix_hash_before == prefix_hash_after,
        prefix_hash_before,
        prefix_hash_after,
        frozen_bytes,
        modification_start_byte: frozen_bytes,
    }
}

/// Extracts the fields whose order and content determine prompt-cache reuse.
///
/// Anthropic carries `system` outside `messages`; modeling it as the first
/// message lets the same proof detect a cached system-prompt mutation.
#[must_use]
pub fn cache_relevant_messages(request: &Value) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = request.get("system") {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    if let Some(items) = request
        .get("messages")
        .or_else(|| request.get("input"))
        .and_then(Value::as_array)
    {
        messages.extend(items.iter().cloned());
    }
    messages
}

/// Parses an incoming proxy body without mutating it, including supported
/// content encodings. Invalid bodies intentionally yield `None` and pass through.
#[must_use]
pub fn parse_request_body(body: &[u8], parts: &Parts, limit: usize) -> Option<Value> {
    use crate::proxy::codec::{
        RequestBodyEncoding, decode_gzip_bounded, decode_zstd_bounded, request_body_encoding,
    };

    let decoded = match request_body_encoding(parts) {
        RequestBodyEncoding::Identity => body.to_vec(),
        RequestBodyEncoding::Gzip => decode_gzip_bounded(body, limit).ok()?,
        RequestBodyEncoding::Zstd => decode_zstd_bounded(body, limit).ok()?,
        RequestBodyEncoding::Passthrough => return None,
    };
    serde_json::from_slice(&decoded).ok()
}

/// Applies the public verification headers to a proxied response.
pub fn apply_response_headers(response: &mut Response, proof: &DeterminismProof) {
    let headers = response.headers_mut();
    headers.insert(
        "x-leanctx-determinism",
        HeaderValue::from_static(if proof.is_stable {
            "stable"
        } else {
            "unstable"
        }),
    );
    insert_header(
        headers,
        "x-leanctx-frozen-bytes",
        &proof.frozen_bytes.to_string(),
    );
    headers.insert(
        "x-leanctx-cache-safe",
        HeaderValue::from_static(if proof.is_stable { "true" } else { "false" }),
    );
    insert_header(headers, "x-leanctx-proof-hash", &proof.prefix_hash_after);
}

/// Adds deterministic defaults for responses that never enter the shared
/// forwarder (health, auth refusals, local APIs, and WebSocket handshakes).
pub fn ensure_response_headers(response: &mut Response) {
    if !response.headers().contains_key("x-leanctx-determinism") {
        apply_response_headers(response, &verify_determinism(&[], &[]));
    }
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        axum::http::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AuditRecord {
    proof: DeterminismProof,
    violation: bool,
}

/// A machine-readable audit summary for `lean-ctx audit determinism`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeterminismAuditReport {
    pub total_requests: usize,
    pub stable_count: usize,
    pub unstable_count: usize,
    pub stability_rate: f64,
    pub violations: Vec<DeterminismProof>,
}

const AUDIT_FILE: &str = "proxy-determinism-audit.jsonl";

fn audit_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join(AUDIT_FILE))
}

/// Persists a compact, newline-delimited proof record for offline audit.
pub fn record_audit(proof: &DeterminismProof, violation: bool) {
    let Some(path) = audit_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(error) = fs::create_dir_all(parent) {
        tracing::debug!(%error, "determinism audit directory unavailable");
        return;
    }
    let record = AuditRecord {
        proof: proof.clone(),
        violation,
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(error) = writeln!(file, "{line}") {
                tracing::debug!(%error, "determinism audit write failed");
            }
        }
        Err(error) => tracing::debug!(%error, "determinism audit file unavailable"),
    }
}

/// Reads the recent determinism audit records and summarizes cache safety.
#[must_use]
pub fn audit_report() -> DeterminismAuditReport {
    let records = audit_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| serde_json::from_str::<AuditRecord>(line).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let total_requests = records.len();
    let violations = records
        .iter()
        .filter(|record| record.violation || !record.proof.is_stable)
        .map(|record| record.proof.clone())
        .collect::<Vec<_>>();
    let unstable_count = violations.len();
    let stable_count = total_requests.saturating_sub(unstable_count);
    let stability_rate = if total_requests == 0 {
        1.0
    } else {
        stable_count as f64 / total_requests as f64
    };
    DeterminismAuditReport {
        total_requests,
        stable_count,
        unstable_count,
        stability_rate,
        violations,
    }
}

fn canonical_messages(messages: &[Value]) -> Vec<u8> {
    canonical_json(&Value::Array(messages.to_vec()))
}

fn canonical_message_prefix_len(messages: &[Value], count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let mut bytes = Vec::from(b"[".as_slice());
    for (index, message) in messages.iter().take(count).enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        write_canonical_json(message, &mut bytes);
    }
    bytes.len()
}

fn canonical_json(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_canonical_json(value, &mut bytes);
    bytes
}

fn write_canonical_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let sorted = map.iter().collect::<BTreeMap<_, _>>();
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.extend(serde_json::to_vec(key).expect("JSON object keys serialize"));
                out.push(b':');
                write_canonical_json(item, out);
            }
            out.push(b'}');
        }
        _ => out.extend(serde_json::to_vec(value).expect("JSON scalar serializes")),
    }
}

fn common_prefix_len(before: &[u8], after: &[u8]) -> usize {
    before
        .iter()
        .zip(after)
        .take_while(|(left, right)| left == right)
        .count()
}

fn cache_breakpoint_len(messages: &[Value]) -> usize {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| message_has_cache_control(message).then_some(index + 1))
        .last()
        .unwrap_or_default()
}

fn message_has_cache_control(message: &Value) -> bool {
    if message.get("cache_control").is_some() {
        return true;
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("cache_control").is_some()
                    || block
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|items| {
                            items.iter().any(|item| item.get("cache_control").is_some())
                        })
            })
        })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, response::Response};
    use serde_json::json;

    use super::{apply_response_headers, canonical_messages, verify_determinism};

    #[test]
    fn stable_cached_prefix_produces_a_stable_proof() {
        let before = vec![
            json!({"role": "user", "content": "frozen", "cache_control": {"type": "ephemeral"}}),
            json!({"role": "user", "content": "live"}),
        ];
        let after = vec![
            before[0].clone(),
            json!({"role": "user", "content": "rewritten live"}),
        ];

        let proof = verify_determinism(&before, &after);

        assert!(proof.is_stable);
        assert_eq!(proof.prefix_hash_before, proof.prefix_hash_after);
    }

    #[test]
    fn modified_cached_prefix_is_unstable() {
        let before = vec![json!({"role": "user", "content": "frozen", "cache_control": {}})];
        let after = vec![json!({"role": "user", "content": "changed", "cache_control": {}})];

        assert!(!verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn modified_unanchored_prefix_is_conservatively_unstable() {
        let before = vec![json!({"role": "user", "content": "frozen"})];
        let after = vec![json!({"role": "user", "content": "changed"})];

        assert!(!verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn cached_system_message_modification_is_detected() {
        let before = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "rules", "cache_control": {}}]}),
        ];
        let after = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "changed", "cache_control": {}}]}),
        ];

        assert!(!verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn frozen_byte_count_matches_the_common_canonical_prefix() {
        let before = vec![json!({"role": "user", "content": "same", "cache_control": {}})];
        let after = vec![json!({"role": "user", "content": "different", "cache_control": {}})];
        let before_bytes = canonical_messages(&before);
        let after_bytes = canonical_messages(&after);
        let expected = before_bytes
            .iter()
            .zip(&after_bytes)
            .take_while(|(left, right)| left == right)
            .count();

        assert_eq!(verify_determinism(&before, &after).frozen_bytes, expected);
    }

    #[test]
    fn proof_headers_are_set() {
        let proof = verify_determinism(&[], &[]);
        let mut response = Response::new(Body::empty());

        apply_response_headers(&mut response, &proof);

        let headers = response.headers();
        assert_eq!(headers["x-leanctx-determinism"], "stable");
        assert_eq!(headers["x-leanctx-frozen-bytes"], "2");
        assert_eq!(headers["x-leanctx-cache-safe"], "true");
        assert_eq!(headers["x-leanctx-proof-hash"], proof.prefix_hash_after);
    }
}
