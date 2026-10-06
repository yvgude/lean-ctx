// SPDX-License-Identifier: Apache-2.0
//! Final egress control in the BYOK proxy (G6, plan cases 41–46).
//!
//! Everything the proxy forwards to a model provider passes the gateway one
//! last time, after compression, routing, translation and every cache-safety
//! revert — on exactly the request body that leaves the machine:
//!
//! - every content string of the request (system prompt, messages, tool
//!   results, tool arguments; OpenAI Chat/Responses, Anthropic Messages,
//!   Gemini) is admitted under the current policy; masked values are
//!   rewritten in place, withheld objects are replaced by a content-free
//!   marker;
//! - content the model provider has signed or encrypted (thinking signatures,
//!   `encrypted_content`) is inspected but never rewritten: altering it would
//!   break the request, so a finding there is delivered and flagged in
//!   developer mode and refuses the request in governed/sovereign mode;
//! - media (images, inline data) cannot be inspected and is reported as such;
//!   governed/sovereign mode refuses it;
//! - the destination is part of the decision: content classified `restricted`
//!   never goes to a remote model (Community rule; organisation matrices are
//!   Enterprise);
//! - a body the proxy cannot parse is forwarded only as an honestly marked
//!   opaque payload (developer mode) and refused otherwise;
//! - one Decision Receipt per request binds the policy digest, the destination
//!   and the SHA-256 of the forwarded bytes.
//!
//! Prompt-cache stability (#498/#1912): admission is a pure function of
//! (policy, text), so the same prefix is rewritten to the same bytes on every
//! turn, and a request with nothing to change is forwarded byte-identical.

use std::path::Path;
use std::sync::Arc;

use lean_ctx_protocol::ProtocolReference;
use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDecisionReceiptV1, ContextDecisionV1, ContextDispositionV1,
    DestinationLocalityV1, DestinationV1, ReasonCodeV1, TransformationKindV1,
};
use serde_json::Value;

use super::capture::{self, AdmissionCapture, CallIdentity, Delivered};
use super::{
    AdmissionCounts, AdmissionPolicy, admit, admit_uninspectable, clean, fingerprint_digest, reason,
};

/// Receipts of proxy requests are listed under this key (`lean-ctx inspect
/// --proxy`), separate from any project.
pub const PROXY_RECEIPT_KEY: &str = "<proxy>";

/// Object keys that carry identifiers or structure, never context.
const STRUCTURAL_KEYS: &[&str] = &[
    "model",
    "role",
    "type",
    "id",
    "tool_use_id",
    "tool_call_id",
    "call_id",
    "name",
    "cache_control",
    "media_type",
    "mime_type",
    "mimeType",
    "stop",
    "stop_sequences",
    "tool_choice",
    "response_format",
    "previous_response_id",
    "status",
    "detail",
    "format",
];

/// Keys whose value the provider signed or encrypted: inspected, never
/// rewritten.
const SEALED_KEYS: &[&str] = &["signature", "encrypted_content", "thought_signature"];

/// Where the request goes.
#[derive(Debug, Clone, Copy)]
pub struct EgressTarget<'a> {
    /// Provider label as the proxy meters it (`Anthropic`, `OpenAI`, …).
    pub provider: &'a str,
    pub model: Option<&'a str>,
    /// The effective upstream base URL, after routing.
    pub upstream_base: &'a str,
}

/// The destination the receipt names. Locality comes from the upstream host;
/// anything not provably on this machine is remote.
#[must_use]
pub fn destination(target: &EgressTarget<'_>) -> DestinationV1 {
    let reference = |raw: &str| {
        ProtocolReference::new(raw.to_owned()).ok().or_else(|| {
            let digest = blake3::hash(raw.as_bytes()).to_hex();
            ProtocolReference::new(digest[..32].to_owned()).ok()
        })
    };
    DestinationV1 {
        provider: reference(&target.provider.to_ascii_lowercase())
            .unwrap_or_else(|| ProtocolReference::new("provider").expect("valid reference")),
        model: target.model.and_then(reference),
        locality: locality(target.upstream_base),
        organization_managed: false,
        account_ref: None,
        region: None,
    }
}

fn locality(upstream_base: &str) -> DestinationLocalityV1 {
    let Some(host) = reqwest::Url::parse(upstream_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return DestinationLocalityV1::Unknown;
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let local = match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => bare == "localhost" || bare.ends_with(".localhost"),
    };
    if local {
        DestinationLocalityV1::Local
    } else {
        DestinationLocalityV1::Remote
    }
}

/// What happens to the request body.
#[derive(Debug)]
pub enum EgressBody {
    /// Nothing to change: forward the bytes exactly as prepared.
    Unchanged,
    /// Forward this admitted document instead.
    Rewritten(Value),
    /// Do not forward. Content-free, names the reason codes.
    Refused(String),
}

/// The egress decision for one request, with the capture its receipt is
/// built from once the forwarded bytes are known.
#[derive(Debug)]
pub struct EgressOutcome {
    pub body: EgressBody,
    /// Most sensitive classification found in the request (information-flow
    /// join over every inspected string). `None` when nothing was inspected.
    pub classification: Option<ClassificationV1>,
    capture: Option<Arc<AdmissionCapture>>,
    destination: DestinationV1,
}

impl EgressOutcome {
    fn passthrough(destination: DestinationV1) -> Self {
        Self {
            body: EgressBody::Unchanged,
            classification: None,
            capture: None,
            destination,
        }
    }
}

struct Walk<'a> {
    policy: &'a AdmissionPolicy,
    fingerprint: Vec<u8>,
    capture: &'a AdmissionCapture,
    remote: bool,
    mandatory: bool,
    refused: Option<String>,
    rewritten: bool,
    classification: Option<ClassificationV1>,
}

/// One admitted string.
enum Verdict {
    Keep,
    Replace(String),
}

impl Walk<'_> {
    fn record(&self, decision: &ContextDecisionV1, counts: AdmissionCounts) {
        self.capture.record(decision, counts);
    }

    fn observe(&mut self, classification: ClassificationV1) {
        self.classification = Some(
            self.classification
                .map_or(classification, |seen| seen.join(classification)),
        );
    }

    fn refuse(&mut self, reasons: &[ReasonCodeV1]) {
        if self.refused.is_none() {
            let codes: Vec<&str> = reasons.iter().map(ReasonCodeV1::as_str).collect();
            self.refused = Some(format!(
                "request withheld by the context gateway ({})",
                codes.join(", ")
            ));
        }
    }

    fn admit_string(&mut self, text: &str, sealed: bool) -> Verdict {
        if text.is_empty() {
            return Verdict::Keep;
        }
        // Clean memo: unchanged, unflagged text under the same policy is the
        // common case for every earlier turn of a conversation.
        let key = self
            .policy
            .semantic
            .is_none()
            .then(|| clean::key(&self.fingerprint, "egress", text));
        if let Some(key) = &key
            && let Some(decision) = clean::get(key)
        {
            // Only clean, unflagged text is memoized; it carries the mode's
            // classification for unclassified content, as a fresh admit would.
            self.observe(self.policy.gateway.mode.unclassified());
            self.record(&decision, AdmissionCounts::default());
            return Verdict::Keep;
        }
        let mut admission = admit(text, None, self.policy);
        self.observe(admission.classification);
        // Destination is part of the authorization: restricted content
        // never leaves for a remote model.
        if self.remote
            && admission.text.is_some()
            && admission.classification >= ClassificationV1::Restricted
        {
            admission.text = None;
            admission.decision.disposition = ContextDispositionV1::Deny;
            admission
                .decision
                .reason_codes
                .push(reason("destination.remote_restricted"));
        }
        if sealed
            && admission
                .text
                .as_deref()
                .is_some_and(|delivered| delivered != text)
        {
            // Signed/encrypted by the provider: masking would break the
            // request. Governed modes refuse it; developer mode delivers it
            // unchanged and says so.
            admission
                .decision
                .reason_codes
                .push(reason("egress.sealed_content"));
            admission
                .decision
                .required_transformations
                .retain(|kind| *kind != TransformationKindV1::Redaction);
            if self.mandatory {
                admission.decision.disposition = ContextDispositionV1::Deny;
                admission.text = None;
            } else {
                admission.decision.disposition = ContextDispositionV1::Allow;
                admission.text = Some(text.to_owned());
            }
        }
        self.record(&admission.decision, admission.counts);
        if let Some(key) = key
            && admission.disposition() == ContextDispositionV1::Allow
            && admission.decision.reason_codes.is_empty()
            && admission.text.as_deref() == Some(text)
        {
            clean::insert(key, admission.decision.clone());
        }
        match admission.text {
            Some(delivered) if delivered == text => Verdict::Keep,
            Some(delivered) => {
                self.rewritten = true;
                Verdict::Replace(delivered)
            }
            None if sealed => {
                self.refuse(&admission.decision.reason_codes);
                Verdict::Keep
            }
            None => {
                let codes: Vec<&str> = admission
                    .decision
                    .reason_codes
                    .iter()
                    .map(ReasonCodeV1::as_str)
                    .collect();
                self.rewritten = true;
                Verdict::Replace(format!(
                    "[lean-ctx gateway: content withheld — {}]",
                    codes.join(", ")
                ))
            }
        }
    }

    fn media(&mut self, bytes: usize) {
        let admission = admit_uninspectable(None, bytes as u64, self.policy);
        self.record(&admission.decision, admission.counts);
        if admission.text.is_none() {
            self.refuse(&admission.decision.reason_codes);
        }
    }

    fn walk(&mut self, value: &mut Value, sealed: bool) {
        match value {
            Value::String(text) => {
                if let Verdict::Replace(admitted) = self.admit_string(text, sealed) {
                    *text = admitted;
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.walk(item, sealed);
                }
            }
            Value::Object(map) => {
                // Provider-sealed blocks: thinking with a signature, reasoning
                // items carrying encrypted content.
                let sealed_block = sealed
                    || map.keys().any(|key| SEALED_KEYS.contains(&key.as_str()))
                    || matches!(
                        map.get("type").and_then(Value::as_str),
                        Some("redacted_thinking")
                    );
                for (key, child) in map.iter_mut() {
                    let key = key.as_str();
                    if STRUCTURAL_KEYS.contains(&key) {
                        continue;
                    }
                    if SEALED_KEYS.contains(&key) || (key == "data" && sealed_block) {
                        // Opaque provider material: never text to inspect.
                        continue;
                    }
                    if let Some(bytes) = media_bytes(key, child) {
                        self.media(bytes);
                        continue;
                    }
                    self.walk(child, sealed_block);
                }
            }
            _ => {}
        }
    }
}

/// Base64 media payloads: Anthropic `source.data`, Gemini `inlineData.data`,
/// OpenAI `image_url.url` / `input_image.image_url` data URIs.
fn media_bytes(key: &str, value: &Value) -> Option<usize> {
    match (key, value) {
        ("data", Value::String(data)) if data.len() > 256 && looks_base64(data) => Some(data.len()),
        ("url" | "image_url" | "file_data", Value::String(url)) if url.starts_with("data:") => {
            Some(url.len())
        }
        _ => None,
    }
}

fn looks_base64(data: &str) -> bool {
    data.bytes()
        .take(512)
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}

/// Admit a parsed request body before it is forwarded to `target`.
#[must_use]
pub fn admit_request(doc: &Value, target: &EgressTarget<'_>) -> EgressOutcome {
    let destination = destination(target);
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    if !policy.gateway.enabled_effective() {
        return EgressOutcome::passthrough(destination);
    }
    let fingerprint = policy.fingerprint();
    let capture = AdmissionCapture::new();
    capture.bind_policy(policy.gateway.mode, fingerprint_digest(&fingerprint));
    let mut admitted = doc.clone();
    let mut walk = Walk {
        policy: &policy,
        fingerprint,
        capture: &capture,
        remote: destination.locality != DestinationLocalityV1::Local,
        mandatory: policy.detectors_mandatory(),
        refused: None,
        rewritten: false,
        classification: None,
    };
    walk.walk(&mut admitted, false);
    let body = match (walk.refused.take(), walk.rewritten) {
        (Some(refusal), _) => EgressBody::Refused(refusal),
        (None, true) => EgressBody::Rewritten(admitted),
        (None, false) => EgressBody::Unchanged,
    };
    EgressOutcome {
        body,
        classification: walk.classification,
        capture: Some(capture),
        destination,
    }
}

/// A body the proxy cannot parse (opaque content encoding, not JSON). It is
/// never claimed as inspected: developer mode forwards it marked as opaque,
/// governed and sovereign mode refuse it.
#[must_use]
pub fn admit_opaque(bytes: usize, target: &EgressTarget<'_>) -> EgressOutcome {
    let destination = destination(target);
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    if !policy.gateway.enabled_effective() || bytes == 0 {
        return EgressOutcome::passthrough(destination);
    }
    let capture = AdmissionCapture::new();
    capture.bind_policy(
        policy.gateway.mode,
        fingerprint_digest(&policy.fingerprint()),
    );
    let mut admission =
        admit_uninspectable(Some(Path::new("egress.opaque")), bytes as u64, &policy);
    admission
        .decision
        .reason_codes
        .push(reason("egress.opaque_payload"));
    capture.record(&admission.decision, admission.counts);
    let body = if admission.text.is_some() {
        EgressBody::Unchanged
    } else {
        EgressBody::Refused(
            "request withheld by the context gateway (egress.opaque_payload)".to_owned(),
        )
    };
    EgressOutcome {
        body,
        // Opaque bytes were never inspected: nothing is claimed about them.
        classification: None,
        capture: Some(capture),
        destination,
    }
}

/// Build the request's receipt from the bytes that actually left (or, for a
/// refused request, from nothing), persist it and anchor its security
/// actions. Returns the receipt when one was produced.
pub fn finish(
    outcome: &EgressOutcome,
    forwarded: Option<&[u8]>,
    original_bytes: usize,
    agent_id: Option<&str>,
) -> Option<ContextDecisionReceiptV1> {
    let capture = outcome.capture.as_ref()?;
    if capture.is_empty() {
        return None;
    }
    capture.set_original_tokens((original_bytes / 4) as u64);
    let text = forwarded.map(String::from_utf8_lossy).unwrap_or_default();
    let delivered = Delivered {
        text: &text,
        // A refused request delivered nothing to the destination.
        is_error: forwarded.is_none(),
        tokens: (text.len() / 4) as u64,
    };
    let identity = CallIdentity {
        agent_id,
        destination: Some(outcome.destination.clone()),
    };
    let receipt = capture::finish(capture, &identity, &delivered)?;
    let tally = capture.security_tally();
    let audit_agent = agent_id.unwrap_or("proxy");
    crate::core::security_events::record("proxy", audit_agent, &tally);
    match super::receipt_store::persist(
        &receipt,
        PROXY_RECEIPT_KEY,
        capture.task_scope().as_deref(),
    ) {
        Ok(digest) if !tally.is_empty() => {
            crate::core::security_events::anchor_receipt("proxy", audit_agent, digest.hex());
        }
        Ok(_) => {}
        Err(error) => tracing::warn!("proxy gateway receipt not persisted: {error}"),
    }
    Some(receipt)
}

/// [`finish`] for async proxy code: receipt persistence may wait on the shared
/// index lock and on file I/O, which must never park a runtime worker.
pub async fn finish_off_runtime(
    outcome: EgressOutcome,
    forwarded: Option<Vec<u8>>,
    original_bytes: usize,
    agent_id: Option<String>,
) {
    let persisted = tokio::task::spawn_blocking(move || {
        finish(
            &outcome,
            forwarded.as_deref(),
            original_bytes,
            agent_id.as_deref(),
        );
    })
    .await;
    if let Err(error) = persisted {
        tracing::warn!("proxy gateway receipt task failed: {error}");
    }
}

/// The admitted form of a body that is sent somewhere else as well (the
/// counterfactual token probe): masked, never with withheld content. `None`
/// when nothing may be sent.
#[must_use]
pub fn admitted_copy(doc: &Value, target: &EgressTarget<'_>) -> Option<Value> {
    match admit_request(doc, target).body {
        EgressBody::Unchanged => Some(doc.clone()),
        EgressBody::Rewritten(admitted) => Some(admitted),
        EgressBody::Refused(_) => None,
    }
}
