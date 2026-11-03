// SPDX-License-Identifier: Apache-2.0
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

// `http` types (what axum re-exports): the proof, audit and header helpers
// build without the `http-server` feature (#586).
use http::{HeaderMap, HeaderValue, Response};
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
    /// `true` when both prefix hashes are identical, or when the prefix first
    /// breaks at a stride step of long-session elision (`session_elide`).
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

    /// Verifies `after` against what the proxy already forwarded for this
    /// conversation (`sent`, from `prefix_replay::sent_prefix`).
    ///
    /// The provider cached the bytes the proxy sent, not the client's: the
    /// protected prefix is the run of leading messages the client sent
    /// unchanged since the last forward, and each of them must leave exactly
    /// as it left then. A message forwarded for the first time — the newest
    /// tool result — is new content and may be rewritten; a content-
    /// deterministic rewrite then leaves the same bytes on every later turn.
    /// Where the client changed or dropped a message (a rewind, another
    /// conversation with the same opening) the protected prefix ends there.
    ///
    /// Assumption: a conversation the proxy has not forwarded (`sent` is
    /// `None`: its first turn, or the first after a proxy restart) holds no
    /// provider cache made through this proxy, so nothing is protected. Once
    /// a forwarded prefix is known, a differing byte in it fails closed —
    /// except where it first differs at a message long-session elision
    /// rewrote this turn (`session_elide`, once per stride).
    ///
    /// `frozen_bytes` counts the canonical bytes of the protected messages
    /// that leave unchanged; `modification_start_byte` is where `after` first
    /// differs from `before`. The prefix hashes cover the per-message hashes
    /// of the protected prefix as forwarded then and as forwarded now.
    #[must_use]
    pub fn verify_forwarded(
        &self,
        before: &[Value],
        after: &[Value],
        sent: Option<&crate::prefix_replay::SentPrefix>,
    ) -> DeterminismProof {
        let empty = crate::prefix_replay::SentPrefix::default();
        let sent = sent.unwrap_or(&empty);
        let client = message_hashes(before);
        let outgoing = message_hashes(after);
        let protected = sent
            .client
            .iter()
            .zip(&client)
            .take_while(|(then, now)| then == now)
            .count()
            .min(sent.forwarded.len());
        let then = &sent.forwarded[..protected];
        let now = outgoing.get(..protected).unwrap_or(&outgoing);
        let unchanged = then
            .iter()
            .zip(now)
            .take_while(|(then, now)| then == now)
            .count();
        let prefix_hash_before = hash_list(then);
        let prefix_hash_after = hash_list(now);
        // The one sanctioned break: a stride step of long-session elision
        // (`session_elide`). The forwarded prefix first differs at a message
        // that now leaves elided and that the client sent whole; from there
        // on the provider writes the cache anew, once per stride.
        let elision_step = unchanged < now.len()
            && after
                .get(unchanged)
                .is_some_and(crate::session_elide::message_is_elided)
            && !before
                .get(unchanged)
                .is_some_and(crate::session_elide::message_is_elided);
        DeterminismProof {
            request_id: self.request_id.clone(),
            is_stable: prefix_hash_before == prefix_hash_after || elision_step,
            prefix_hash_before,
            prefix_hash_after,
            frozen_bytes: canonical_message_prefix_len(after, unchanged),
            modification_start_byte: common_prefix_len(
                &canonical_messages(before),
                &canonical_messages(after),
            ),
        }
    }
}

/// Content hash of each message as the provider caches it (`cache_control`
/// markers left out), in order. The determinism guard compares these against
/// what a previous turn forwarded (`prefix_replay::SentPrefix`).
///
/// A request caches up to its last marked block. In the message that carries
/// that marker, blocks after it were never cached by this request (the
/// verbosity steer's constant suffix lands there), so they are left out:
/// the next turn may send that message without them and still hit the
/// cache. Blocks after an earlier marker are cached under the last one and
/// stay in.
#[must_use]
pub fn message_hashes(messages: &[Value]) -> Vec<u64> {
    let last_marked = cache_breakpoint_len(messages).checked_sub(1);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let cached = if Some(index) == last_marked {
                cached_part(message)
            } else {
                without_cache_control(message)
            };
            let bytes = lean_ctx::core::canonical::canonical_json_bytes(&cached);
            let digest = blake3::hash(&bytes);
            let mut head = [0u8; 8];
            head.copy_from_slice(&digest.as_bytes()[..8]);
            u64::from_le_bytes(head)
        })
        .collect()
}

/// `message` up to and including its last marked content block, markers
/// left out. A marker on the message itself caches all of it.
fn cached_part(message: &Value) -> Value {
    let mut cached = without_cache_control(message);
    if message.get("cache_control").is_none()
        && let Some(blocks) = message.get("content").and_then(Value::as_array)
        && let Some(last) = blocks.iter().rposition(block_has_cache_control)
        && let Some(content) = cached.get_mut("content").and_then(Value::as_array_mut)
    {
        content.truncate(last + 1);
    }
    cached
}

fn block_has_cache_control(block: &Value) -> bool {
    block.get("cache_control").is_some()
        || block
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.get("cache_control").is_some()))
}

fn hash_list(hashes: &[u64]) -> String {
    let mut hasher = blake3::Hasher::new();
    for hash in hashes {
        hasher.update(&hash.to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Verifies that `after` leaves the provider-cached prefix of `before` intact.
///
/// Both slices are canonical JSON arrays. `frozen_bytes` is their common byte
/// prefix. The proof hashes the cached-message prefix when one exists. Without
/// explicit `cache_control` breakpoints the proof prefix is empty (stable by
/// definition): the proxy cannot determine the provider's auto-cache boundary,
/// so it allows compression of any message.
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
    let proof_prefix_bytes = if cached_messages > 0 {
        canonical_message_prefix_len(before, cached_messages)
    } else {
        // No explicit cache_control breakpoints — we cannot determine the
        // provider's auto-cache boundary. Tool-result dedup and content dedup
        // may legitimately modify any message, so there is no safe frozen region
        // to verify. The guard only adds value when the client declares a
        // cache boundary via cache_control.
        0
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
#[cfg(feature = "http-server")]
pub fn parse_request_body(
    body: &[u8],
    parts: &http::request::Parts,
    limit: usize,
) -> Option<Value> {
    use crate::codec::{
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
pub fn apply_response_headers<B>(response: &mut Response<B>, proof: &DeterminismProof) {
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
pub fn ensure_response_headers<B>(response: &mut Response<B>) {
    if !response.headers().contains_key("x-leanctx-determinism") {
        apply_response_headers(response, &verify_determinism(&[], &[]));
    }
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        http::HeaderName::from_bytes(name.as_bytes()),
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
    lean_ctx::core::data_dir::lean_ctx_data_dir()
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

/// Canonical bytes of what the provider caches. `cache_control` markers are
/// left out: they say where and for how long to cache, not what, so moving a
/// marker or changing its lifetime keeps the cached prefix (the agent-loop
/// lifetime in `cache_ttl` changes nothing else).
fn canonical_messages(messages: &[Value]) -> Vec<u8> {
    let content: Vec<Value> = messages.iter().map(without_cache_control).collect();
    lean_ctx::core::canonical::canonical_json_bytes(&Value::Array(content))
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
        bytes.extend(lean_ctx::core::canonical::canonical_json_bytes(
            &without_cache_control(message),
        ));
    }
    bytes.len()
}

fn without_cache_control(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| key.as_str() != "cache_control")
                .map(|(key, item)| (key.clone(), without_cache_control(item)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_cache_control).collect()),
        other => other.clone(),
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
        .is_some_and(|blocks| blocks.iter().any(block_has_cache_control))
}

#[cfg(test)]
mod tests {
    use http::Response;
    use serde_json::json;

    use super::{
        DeterminismGuard, apply_response_headers, canonical_messages, message_hashes,
        verify_determinism,
    };
    use crate::prefix_replay::SentPrefix;

    /// Claude Code marks the last message, so the client's breakpoint covers
    /// the newest tool result too. The guard reverted every first-sight
    /// compression of it (0 % savings from the second request on); what the
    /// provider cached is what the proxy forwarded before, so only a change
    /// to that may revert.
    #[test]
    fn guard_protects_what_the_proxy_forwarded_not_the_client_breakpoint() {
        let marker = json!({"type": "ephemeral"});
        let result = |id: &str, text: &str, marked: bool| {
            let mut block = json!({"type": "tool_result", "tool_use_id": id, "content": text});
            if marked {
                block["cache_control"] = marker.clone();
            }
            json!({"role": "user", "content": [block]})
        };
        let opening = vec![
            json!({"role": "system", "content": "rules"}),
            json!({"role": "user", "content": "fix the bug"}),
            json!({"role": "assistant", "content": "reading"}),
        ];
        let client_two = [opening.clone(), vec![result("t1", "raw one", true)]].concat();
        let sent_two = [opening.clone(), vec![result("t1", "short one", true)]].concat();
        let guard = DeterminismGuard::new("trace");

        let first_sight = guard.verify_forwarded(&client_two, &sent_two, None);
        assert!(first_sight.is_stable, "nothing was forwarded yet");

        let sent = SentPrefix {
            client: message_hashes(&client_two),
            forwarded: message_hashes(&sent_two),
        };
        let mut client_three = client_two.clone();
        client_three[3] = result("t1", "raw one", false);
        client_three.push(json!({"role": "assistant", "content": "next"}));
        client_three.push(result("t2", "raw two", true));
        let mut compressed = client_three.clone();
        compressed[3] = result("t1", "short one", false);
        compressed[5] = result("t2", "short two", true);
        assert!(!verify_determinism(&client_three, &compressed).is_stable);
        let proof = guard.verify_forwarded(&client_three, &compressed, Some(&sent));
        assert!(proof.is_stable, "the newest tool result is new content");
        assert!(proof.frozen_bytes > 0);

        let busted = guard.verify_forwarded(&client_three, &client_three, Some(&sent));
        assert!(
            !busted.is_stable,
            "t1 left compressed before: sending it raw changes the cached prefix"
        );

        // The verbosity steer appends a block after the request's last marker:
        // never cached, so the next turn may send that message without it.
        let mut steered = compressed.clone();
        steered[5]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "text", "text": "Be brief."}));
        let sent = SentPrefix {
            client: message_hashes(&client_three),
            forwarded: message_hashes(&steered),
        };
        let mut client_four = client_three.clone();
        client_four[5] = result("t2", "raw two", false);
        client_four.push(json!({"role": "assistant", "content": "done"}));
        client_four.push(result("t3", "raw three", true));
        let mut unsteered = compressed.clone();
        unsteered[5] = result("t2", "short two", false);
        unsteered.extend_from_slice(&client_four[6..]);
        assert!(
            guard
                .verify_forwarded(&client_four, &unsteered, Some(&sent))
                .is_stable,
            "a block after the last marker was never cached"
        );
        unsteered[5] = result("t2", "shorter two", false);
        assert!(
            !guard
                .verify_forwarded(&client_four, &unsteered, Some(&sent))
                .is_stable,
            "the marked block itself was cached"
        );

        // A stride step of long-session elision is the one sanctioned break:
        // t1 left compressed before and now leaves as an elision stub.
        let stub = format!(
            "{} (9000 chars); full text: cat x]",
            crate::session_elide::STUB_OPEN
        );
        let mut elided = unsteered.clone();
        elided[3] = result("t1", &stub, false);
        elided[5] = result("t2", "short two", false);
        assert!(
            guard
                .verify_forwarded(&client_four, &elided, Some(&sent))
                .is_stable,
            "an elision step breaks the prefix once, on purpose"
        );
    }

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
    fn single_message_without_cache_control_allows_compression() {
        let before = vec![json!({"role": "user", "content": "frozen"})];
        let after = vec![json!({"role": "user", "content": "changed"})];
        assert!(verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn last_message_modification_without_cache_control_is_stable() {
        let before = vec![
            json!({"role": "system", "content": "rules"}),
            json!({"role": "user", "content": "original"}),
        ];
        let after = vec![
            json!({"role": "system", "content": "rules"}),
            json!({"role": "user", "content": "compressed"}),
        ];
        assert!(verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn any_modification_without_cache_control_is_stable() {
        // Without cache_control markers the proxy cannot know the auto-cache
        // boundary, so all modifications (including earlier messages) are allowed.
        let before = vec![
            json!({"role": "system", "content": "rules"}),
            json!({"role": "user", "content": "q1"}),
            json!({"role": "assistant", "content": "a1"}),
            json!({"role": "user", "content": "q2"}),
        ];
        let after = vec![
            json!({"role": "system", "content": "MODIFIED"}),
            json!({"role": "user", "content": "q1"}),
            json!({"role": "assistant", "content": "a1"}),
            json!({"role": "user", "content": "q2"}),
        ];
        assert!(verify_determinism(&before, &after).is_stable);
    }

    #[test]
    fn explicit_cache_control_still_catches_violations() {
        // With explicit cache_control the guard MUST detect modifications
        // within the cached region.
        let before = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "rules", "cache_control": {}}]}),
            json!({"role": "user", "content": "q1"}),
        ];
        let after = vec![
            json!({"role": "system", "content": [{"type": "text", "text": "MODIFIED", "cache_control": {}}]}),
            json!({"role": "user", "content": "q1"}),
        ];
        assert!(!verify_determinism(&before, &after).is_stable);
    }

    /// A marker's lifetime is not cached content: the agent-loop lifetime
    /// (`cache_ttl`) must not be reverted as a cache bust.
    #[test]
    fn changing_a_cache_lifetime_is_not_a_violation() {
        let marked = |ttl: &str| {
            vec![
                json!({"role": "user", "content": [{"type": "text", "text": "q1"}]}),
                json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t",
                    "content": "ok", "cache_control": {"type": "ephemeral", "ttl": ttl}}]}),
            ]
        };
        assert!(verify_determinism(&marked("1h"), &marked("5m")).is_stable);
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
        let mut response = Response::new(());

        apply_response_headers(&mut response, &proof);

        let headers = response.headers();
        assert_eq!(headers["x-leanctx-determinism"], "stable");
        assert_eq!(headers["x-leanctx-frozen-bytes"], "2");
        assert_eq!(headers["x-leanctx-cache-safe"], "true");
        assert_eq!(headers["x-leanctx-proof-hash"], proof.prefix_hash_after);
    }
}
