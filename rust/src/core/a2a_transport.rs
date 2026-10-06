//! A2A transport envelope signing for inter-agent communication.
//! Uses HMAC for transport-level message integrity.
//! Distinct from commercial Verified Attribution (Section 8)
//! which provides billing CPAO credit with chain integrity.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::a2a::relay::{RELAY_METADATA_KEY, RelayRecordV1};

const MAX_ENVELOPE_BYTES: usize = 2_000_000;

/// Domain tag prefixed to every signing transcript. Bumped from the previous
/// delimiter-joined `v2:` header, which authenticated only `sender.agent_id`.
const SIGNING_DOMAIN: &[u8] = b"leanctx.a2a.transport.sig.v3\0";
pub(crate) const LEGACY_DELIVERY_ID_METADATA: &str = "legacy_delivery_id";
pub(crate) const LEGACY_DELIVERY_SENT_AT_METADATA: &str = "legacy_delivery_sent_at";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentIdentityV1 {
    pub agent_id: String,
    pub agent_type: String,
    pub daemon_fingerprint: String,
    pub capabilities: Vec<String>,
}

impl AgentIdentityV1 {
    pub fn from_current(agent_id: &str, agent_type: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            agent_type: agent_type.to_string(),
            daemon_fingerprint: compute_daemon_fingerprint(),
            capabilities: default_capabilities(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportEnvelopeV1 {
    pub format_version: u32,
    pub sent_at: DateTime<Utc>,
    pub sender: AgentIdentityV1,
    pub recipient: Option<String>,
    pub content_type: TransportContentType,
    pub payload_json: String,
    pub signature: Option<String>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportContentType {
    HandoffBundle,
    ContextPackage,
    A2AMessage,
    A2ATask,
    EvidenceBundle,
}

impl TransportEnvelopeV1 {
    pub fn new(
        sender: AgentIdentityV1,
        recipient: Option<&str>,
        content_type: TransportContentType,
        payload_json: String,
    ) -> Self {
        Self {
            format_version: 1,
            sent_at: Utc::now(),
            sender,
            recipient: recipient.map(std::string::ToString::to_string),
            content_type,
            payload_json,
            signature: None,
            metadata: HashMap::new(),
        }
    }

    /// Sign the envelope over its canonical signing transcript.
    ///
    /// Fail-closed: a sender identity with no canonical encoding (empty or
    /// duplicate capability) clears any previous signature and returns the
    /// canonicalization error to the caller.
    pub fn sign(&mut self, secret: &[u8]) -> Result<(), String> {
        self.signature = None;
        let mac = self.compute_hmac(secret)?;
        self.signature = Some(crate::core::agent_identity::hex_encode(&mac));
        Ok(())
    }

    /// Verify the transport signature. Fails closed: a missing, malformed or
    /// non-canonicalizable envelope never verifies.
    pub fn verify_signature(&self, secret: &[u8]) -> bool {
        let Some(ref sig) = self.signature else {
            return false;
        };
        // Guard before decoding: `hex_decode` slices on byte offsets, so
        // non-ASCII input must never reach it.
        if sig.is_empty()
            || !sig.len().is_multiple_of(2)
            || !sig.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return false;
        }
        let (Ok(expected), Ok(computed)) = (
            crate::core::agent_identity::hex_decode(sig),
            self.compute_hmac(secret),
        ) else {
            return false;
        };
        constant_time_eq(&computed, &expected)
    }

    /// Stable logical delivery identity across transport retries.
    ///
    /// Retry freshness requires a new `sent_at` and therefore a new HMAC. The
    /// receiver must nevertheless deduplicate the same authenticated content,
    /// so this digest binds every envelope field except the retry timestamp and
    /// signature. Metadata remains bound and cannot be used to collide distinct
    /// deliveries.
    pub fn stable_delivery_id(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};

        if let Some(record) = self.relay_record()? {
            return Ok(record.delivery_id);
        }

        let mut stable = self.clone();
        stable.sent_at = DateTime::<Utc>::UNIX_EPOCH;
        stable.signature = None;
        stable.metadata.remove(LEGACY_DELIVERY_ID_METADATA);
        stable.metadata.remove(LEGACY_DELIVERY_SENT_AT_METADATA);
        let digest = Sha256::digest(stable.signing_transcript()?);
        Ok(crate::core::agent_identity::hex_encode(&digest))
    }

    /// Attach the strict relay contract in the reserved metadata slot. The
    /// outer TransportEnvelopeV1 wire shape remains unchanged for legacy peers.
    pub fn attach_relay_record(&mut self, record: &RelayRecordV1) -> Result<(), String> {
        let encoded = record
            .encode_metadata()
            .map_err(|error| error.to_string())?;
        if let Some(existing) = self.metadata.get(RELAY_METADATA_KEY) {
            if existing != &encoded {
                return Err("relay metadata already contains a different record".to_string());
            }
        } else {
            self.metadata
                .insert(RELAY_METADATA_KEY.to_string(), encoded);
        }
        Ok(())
    }

    pub fn relay_record(&self) -> Result<Option<RelayRecordV1>, String> {
        self.metadata
            .get(RELAY_METADATA_KEY)
            .map(|value| RelayRecordV1::decode_metadata(value).map_err(|error| error.to_string()))
            .transpose()
    }

    fn compute_hmac(&self, secret: &[u8]) -> Result<Vec<u8>, String> {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let transcript = self.signing_transcript()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
        mac.update(&transcript);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    /// Canonical signing transcript over every authenticated envelope field.
    ///
    /// Each field is length-prefixed (`u64` big-endian length, then bytes)
    /// under a domain tag, so no value can shift bytes across a field boundary
    /// the way a delimiter join allows. The full sender identity — id, type,
    /// daemon fingerprint and capability set — is bound, so none of it can be
    /// mutated while keeping a valid signature. Only the authenticated bytes
    /// change here; the serialized `format_version` 1 wire shape is untouched.
    fn signing_transcript(&self) -> Result<Vec<u8>, String> {
        let capabilities = canonical_capabilities(&self.sender.capabilities)?;

        let mut buf = Vec::with_capacity(SIGNING_DOMAIN.len() + self.payload_json.len() + 256);
        buf.extend_from_slice(SIGNING_DOMAIN);
        push_field(&mut buf, &self.format_version.to_be_bytes());
        push_field(&mut buf, self.sender.agent_id.as_bytes());
        push_field(&mut buf, self.sender.agent_type.as_bytes());
        push_field(&mut buf, self.sender.daemon_fingerprint.as_bytes());
        push_count(&mut buf, capabilities.len());
        for cap in &capabilities {
            push_field(&mut buf, cap.as_bytes());
        }
        // A missing recipient stays distinct from an empty one.
        match self.recipient.as_deref() {
            None => buf.push(0),
            Some(recipient) => {
                buf.push(1);
                push_field(&mut buf, recipient.as_bytes());
            }
        }
        push_field(&mut buf, self.content_type_str().as_bytes());
        push_field(&mut buf, self.sent_at.to_rfc3339().as_bytes());

        let mut meta: Vec<(&str, &str)> = self
            .metadata
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        meta.sort_unstable();
        push_count(&mut buf, meta.len());
        for (key, value) in &meta {
            push_field(&mut buf, key.as_bytes());
            push_field(&mut buf, value.as_bytes());
        }

        push_field(&mut buf, self.payload_json.as_bytes());
        Ok(buf)
    }

    fn content_type_str(&self) -> &str {
        match self.content_type {
            TransportContentType::HandoffBundle => "handoff_bundle",
            TransportContentType::ContextPackage => "context_package",
            TransportContentType::A2AMessage => "a2a_message",
            TransportContentType::A2ATask => "a2a_task",
            TransportContentType::EvidenceBundle => "evidence_bundle",
        }
    }
}

pub(crate) fn serialize_envelope(envelope: &TransportEnvelopeV1) -> Result<String, String> {
    envelope.relay_record()?;
    let json = serde_json::to_string_pretty(envelope).map_err(|e| e.to_string())?;
    if json.len() > MAX_ENVELOPE_BYTES {
        return Err(format!(
            "envelope too large ({} bytes, max {})",
            json.len(),
            MAX_ENVELOPE_BYTES
        ));
    }
    Ok(json)
}

pub(crate) fn parse_envelope(json: &str) -> Result<TransportEnvelopeV1, String> {
    if json.len() > MAX_ENVELOPE_BYTES {
        return Err(format!(
            "envelope too large ({} bytes, max {})",
            json.len(),
            MAX_ENVELOPE_BYTES
        ));
    }
    let env: TransportEnvelopeV1 = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if env.format_version != 1 {
        return Err(format!(
            "unsupported format_version {} (expected 1)",
            env.format_version
        ));
    }
    env.relay_record()?;
    Ok(env)
}

/// Append one length-prefixed field: `u64` big-endian length, then the bytes.
fn push_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Append the element count of a length-prefixed sequence.
fn push_count(buf: &mut Vec<u8>, count: usize) {
    buf.extend_from_slice(&(count as u64).to_be_bytes());
}

/// Canonicalize the capability set for signing.
///
/// Capabilities are an unordered set, so they are sorted to give one encoding
/// per set regardless of vector order. Empty and duplicate entries have no
/// canonical set encoding and are rejected rather than silently normalized.
fn canonical_capabilities(capabilities: &[String]) -> Result<Vec<&str>, String> {
    if capabilities.iter().any(String::is_empty) {
        return Err("capability must not be empty".to_string());
    }
    let mut sorted: Vec<&str> = capabilities.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let declared = sorted.len();
    sorted.dedup();
    if sorted.len() != declared {
        return Err("duplicate capability in sender identity".to_string());
    }
    Ok(sorted)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn compute_daemon_fingerprint() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    if let Ok(exe) = std::env::current_exe() {
        hasher.update(exe.to_string_lossy().as_bytes());
    }
    crate::core::agent_identity::hex_encode(&hasher.finalize())[..16].to_string()
}

fn default_capabilities() -> Vec<String> {
    vec![
        "context_compression".to_string(),
        "knowledge_graph".to_string(),
        "shared_sessions".to_string(),
        "a2a_messaging".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-key";

    fn test_sender() -> AgentIdentityV1 {
        AgentIdentityV1 {
            agent_id: "test-agent".to_string(),
            agent_type: "cursor".to_string(),
            daemon_fingerprint: "abcd1234".to_string(),
            capabilities: vec![
                "context_compression".to_string(),
                "knowledge_graph".to_string(),
            ],
        }
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("literal is valid rfc3339")
            .with_timezone(&Utc)
    }

    /// Envelope with a pinned `sent_at` so two builds differ only where the
    /// test varies them.
    fn envelope_with(sender: AgentIdentityV1) -> TransportEnvelopeV1 {
        let mut env = TransportEnvelopeV1::new(
            sender,
            Some("target-agent"),
            TransportContentType::A2AMessage,
            r#"{"hello":"world"}"#.to_string(),
        );
        env.sent_at = fixed_time();
        env
    }

    fn signed() -> TransportEnvelopeV1 {
        let mut env = envelope_with(test_sender());
        env.metadata.insert("trace".to_string(), "abc".to_string());
        env.metadata.insert("tenant".to_string(), "t1".to_string());
        env.sign(SECRET).expect("canonical envelope signs");
        env
    }

    fn signature_of(env: &TransportEnvelopeV1) -> String {
        let mut clone = env.clone();
        clone.sign(SECRET).expect("canonical envelope signs");
        clone.signature.expect("canonical envelope signs")
    }

    #[test]
    fn envelope_roundtrip() {
        let env = TransportEnvelopeV1::new(
            test_sender(),
            Some("target-agent"),
            TransportContentType::A2AMessage,
            r#"{"hello":"world"}"#.to_string(),
        );
        let json = serialize_envelope(&env).expect("envelope serializes");
        let parsed = parse_envelope(&json).expect("envelope parses as v1");
        assert_eq!(parsed.format_version, 1);
        assert_eq!(parsed.sender.agent_id, "test-agent");
        assert_eq!(parsed.recipient, Some("target-agent".to_string()));
        assert_eq!(parsed.content_type, TransportContentType::A2AMessage);
    }

    #[test]
    fn hmac_sign_verify() {
        let mut env = TransportEnvelopeV1::new(
            test_sender(),
            None,
            TransportContentType::HandoffBundle,
            "payload".to_string(),
        );
        assert!(!env.verify_signature(SECRET));

        env.sign(SECRET).expect("canonical envelope signs");
        assert!(env.signature.is_some());
        assert!(env.verify_signature(SECRET));
        assert!(!env.verify_signature(b"wrong-key"));
    }

    // --- sender identity binding -------------------------------------------

    #[test]
    fn mutating_agent_id_breaks_signature() {
        let mut env = signed();
        env.sender.agent_id = "impostor".to_string();
        assert!(!env.verify_signature(SECRET));
    }

    #[test]
    fn mutating_agent_type_breaks_signature() {
        let mut env = signed();
        env.sender.agent_type = "claude".to_string();
        assert!(!env.verify_signature(SECRET));
    }

    #[test]
    fn mutating_daemon_fingerprint_breaks_signature() {
        let mut env = signed();
        env.sender.daemon_fingerprint = "0000dead".to_string();
        assert!(!env.verify_signature(SECRET));
    }

    #[test]
    fn mutating_any_capability_value_breaks_signature() {
        for index in 0..test_sender().capabilities.len() {
            let mut env = signed();
            env.sender.capabilities[index] = format!("escalated_{index}");
            assert!(
                !env.verify_signature(SECRET),
                "capability {index} was not bound by the signature"
            );
        }
    }

    #[test]
    fn capability_insertion_breaks_signature() {
        let mut env = signed();
        env.sender.capabilities.push("admin_override".to_string());
        assert!(!env.verify_signature(SECRET));
    }

    #[test]
    fn capability_removal_breaks_signature() {
        let mut env = signed();
        env.sender.capabilities.pop();
        assert!(!env.verify_signature(SECRET));
    }

    #[test]
    fn capability_reordering_is_canonical() {
        // Capabilities are a set: reordering is not an identity change, so the
        // canonical transcript must produce the identical signature.
        let env = signed();
        let mut reordered = env.clone();
        reordered.sender.capabilities.reverse();
        assert_eq!(signature_of(&env), signature_of(&reordered));
        assert!(reordered.verify_signature(SECRET));
    }

    #[test]
    fn duplicate_capabilities_are_rejected() {
        let mut sender = test_sender();
        sender.capabilities.push("knowledge_graph".to_string());
        let mut env = envelope_with(sender);
        assert!(env.sign(SECRET).is_err());
        assert!(env.signature.is_none(), "duplicate capability was signed");

        // Fail closed on verify too: duplicates injected after signing.
        let mut tampered = signed();
        tampered
            .sender
            .capabilities
            .push("knowledge_graph".to_string());
        assert!(!tampered.verify_signature(SECRET));
    }

    #[test]
    fn signing_a_signed_envelope_with_bad_capabilities_clears_the_signature() {
        let mut env = signed();
        assert!(env.signature.is_some());
        env.sender.capabilities.push("knowledge_graph".to_string());
        assert!(env.sign(SECRET).is_err());
        assert!(env.signature.is_none());
    }

    #[test]
    fn empty_capability_is_rejected() {
        let mut sender = test_sender();
        sender.capabilities.push(String::new());
        let mut env = envelope_with(sender);
        assert!(env.sign(SECRET).is_err());
        assert!(env.signature.is_none(), "empty capability was signed");

        let mut tampered = signed();
        tampered.sender.capabilities.push(String::new());
        assert!(!tampered.verify_signature(SECRET));
    }

    // --- delimiter / boundary collisions ------------------------------------

    #[test]
    fn identity_field_boundaries_cannot_collide() {
        let mut left = test_sender();
        left.agent_id = "a".to_string();
        left.agent_type = "bc".to_string();
        let mut right = test_sender();
        right.agent_id = "ab".to_string();
        right.agent_type = "c".to_string();
        assert_ne!(
            signature_of(&envelope_with(left)),
            signature_of(&envelope_with(right))
        );

        let mut left = test_sender();
        left.agent_type = "x".to_string();
        left.daemon_fingerprint = "yz".to_string();
        let mut right = test_sender();
        right.agent_type = "xy".to_string();
        right.daemon_fingerprint = "z".to_string();
        assert_ne!(
            signature_of(&envelope_with(left)),
            signature_of(&envelope_with(right))
        );
    }

    #[test]
    fn capability_boundaries_cannot_collide() {
        let mut one = test_sender();
        one.capabilities = vec!["ab".to_string()];
        let mut two = test_sender();
        two.capabilities = vec!["a".to_string(), "b".to_string()];
        assert_ne!(
            signature_of(&envelope_with(one)),
            signature_of(&envelope_with(two))
        );
    }

    #[test]
    fn metadata_boundaries_cannot_collide() {
        // The previous `k=v` join with `,` separators collided on exactly this
        // pair.
        let mut left = envelope_with(test_sender());
        left.metadata.insert("a".to_string(), "b=c".to_string());
        let mut right = envelope_with(test_sender());
        right.metadata.insert("a=b".to_string(), "c".to_string());
        assert_ne!(signature_of(&left), signature_of(&right));

        let mut split = envelope_with(test_sender());
        split.metadata.insert("a".to_string(), "b".to_string());
        split.metadata.insert("c".to_string(), "d".to_string());
        let mut joined = envelope_with(test_sender());
        joined.metadata.insert("a".to_string(), "b,c=d".to_string());
        assert_ne!(signature_of(&split), signature_of(&joined));
    }

    #[test]
    fn payload_boundary_cannot_collide_with_metadata() {
        let mut left = envelope_with(test_sender());
        left.metadata.insert("k".to_string(), "v".to_string());
        left.payload_json = "payload".to_string();
        let mut right = envelope_with(test_sender());
        right.metadata.insert("k".to_string(), "vpay".to_string());
        right.payload_json = "load".to_string();
        assert_ne!(signature_of(&left), signature_of(&right));
    }

    #[test]
    fn absent_recipient_differs_from_empty_recipient() {
        let mut absent = envelope_with(test_sender());
        absent.recipient = None;
        let mut empty = envelope_with(test_sender());
        empty.recipient = Some(String::new());
        assert_ne!(signature_of(&absent), signature_of(&empty));
    }

    // --- remaining envelope fields ------------------------------------------

    #[test]
    fn header_and_payload_mutations_break_signature() {
        let mut payload = signed();
        payload.payload_json = r#"{"hello":"WORLD"}"#.to_string();
        assert!(!payload.verify_signature(SECRET));

        let mut recipient = signed();
        recipient.recipient = Some("other-agent".to_string());
        assert!(!recipient.verify_signature(SECRET));

        let mut content_type = signed();
        content_type.content_type = TransportContentType::A2ATask;
        assert!(!content_type.verify_signature(SECRET));

        let mut sent_at = signed();
        sent_at.sent_at = fixed_time() + chrono::Duration::seconds(1);
        assert!(!sent_at.verify_signature(SECRET));

        let mut version = signed();
        version.format_version = 2;
        assert!(!version.verify_signature(SECRET));

        let mut metadata = signed();
        metadata
            .metadata
            .insert("tenant".to_string(), "t2".to_string());
        assert!(!metadata.verify_signature(SECRET));
    }

    #[test]
    fn metadata_insertion_order_is_canonical() {
        let mut first = envelope_with(test_sender());
        first.metadata.insert("a".to_string(), "1".to_string());
        first.metadata.insert("b".to_string(), "2".to_string());
        first.metadata.insert("c".to_string(), "3".to_string());
        let mut second = envelope_with(test_sender());
        second.metadata.insert("c".to_string(), "3".to_string());
        second.metadata.insert("b".to_string(), "2".to_string());
        second.metadata.insert("a".to_string(), "1".to_string());
        assert_eq!(signature_of(&first), signature_of(&second));
    }

    // --- signature encoding --------------------------------------------------

    #[test]
    fn malformed_hex_signature_is_rejected() {
        let valid = signature_of(&signed());
        for bad in [
            String::new(),
            "zz".repeat(32),
            format!("{}gg", &valid[..valid.len() - 2]),
            format!("{}ä", &valid[..valid.len() - 2]),
            "€€€€".to_string(),
            format!("{valid}00"),
        ] {
            let mut env = signed();
            env.signature = Some(bad.clone());
            assert!(
                !env.verify_signature(SECRET),
                "accepted malformed signature {bad:?}"
            );
        }
    }

    #[test]
    fn odd_length_hex_signature_is_rejected() {
        let valid = signature_of(&signed());
        let mut env = signed();
        env.signature = Some(valid[..valid.len() - 1].to_string());
        assert!(!env.verify_signature(SECRET));

        let mut short = signed();
        short.signature = Some("a".to_string());
        assert!(!short.verify_signature(SECRET));
    }

    #[test]
    fn signature_is_deterministic_and_hex() {
        let env = signed();
        let first = signature_of(&env);
        assert_eq!(first, signature_of(&env));
        assert_eq!(first, signature_of(&env.clone()));
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn stable_delivery_id_survives_fresh_retry_signature_but_binds_content() {
        let first = signed();
        let first_id = first.stable_delivery_id().expect("stable id");

        let mut retry = first.clone();
        retry.metadata.insert(
            LEGACY_DELIVERY_ID_METADATA.to_string(),
            first.signature.clone().expect("original signature"),
        );
        retry.metadata.insert(
            LEGACY_DELIVERY_SENT_AT_METADATA.to_string(),
            first.sent_at.to_rfc3339(),
        );
        retry.sent_at += chrono::Duration::seconds(1);
        retry.sign(SECRET).expect("refresh retry signature");
        assert_ne!(retry.signature, first.signature);
        assert_eq!(retry.stable_delivery_id().expect("retry id"), first_id);

        retry.payload_json.push('!');
        retry.sign(SECRET).expect("sign distinct content");
        assert_ne!(retry.stable_delivery_id().expect("distinct id"), first_id);
    }

    #[test]
    fn pinned_v3_known_answer_vector() {
        let env = signed();
        assert_eq!(
            env.signature.as_deref(),
            Some("750a663b70c90997ea91f23b04035f77e7a064991f43e291315cf23733b8977f")
        );
    }

    #[test]
    fn unknown_envelope_and_sender_fields_are_rejected() {
        let json = serialize_envelope(&signed()).expect("envelope serializes");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("envelope is json");

        value
            .as_object_mut()
            .expect("envelope is an object")
            .insert("unsigned_extension".to_string(), serde_json::json!(true));
        assert!(parse_envelope(&value.to_string()).is_err());

        let mut nested: serde_json::Value = serde_json::from_str(&json).expect("envelope is json");
        nested["sender"]
            .as_object_mut()
            .expect("sender is an object")
            .insert("unsigned_role".to_string(), serde_json::json!("admin"));
        assert!(parse_envelope(&nested.to_string()).is_err());
    }

    // --- v1 wire compatibility ----------------------------------------------

    #[test]
    fn signed_envelope_survives_v1_serialization() {
        let env = signed();
        let json = serialize_envelope(&env).expect("envelope serializes");
        let parsed = parse_envelope(&json).expect("envelope parses as v1");
        assert_eq!(parsed.signature, env.signature);
        assert!(parsed.verify_signature(SECRET));

        let value: serde_json::Value = serde_json::from_str(&json).expect("envelope is json");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("envelope is a json object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "content_type",
                "format_version",
                "metadata",
                "payload_json",
                "recipient",
                "sender",
                "sent_at",
                "signature",
            ]
        );
    }

    #[test]
    fn rejects_oversized_envelope() {
        let big = "x".repeat(MAX_ENVELOPE_BYTES + 1);
        assert!(parse_envelope(&big).is_err());
    }

    #[test]
    fn rejects_wrong_version() {
        let json = r#"{"format_version":99,"sent_at":"2026-01-01T00:00:00Z","sender":{"agent_id":"a","agent_type":"b","daemon_fingerprint":"c","capabilities":[]},"recipient":null,"content_type":"a2a_message","payload_json":"{}","signature":null,"metadata":{}}"#;
        assert!(parse_envelope(json).is_err());
    }
}
