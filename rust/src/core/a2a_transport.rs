use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAX_ENVELOPE_BYTES: usize = 2_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentityV1 {
    pub agent_id: String,
    pub agent_type: String,
    pub daemon_fingerprint: String,
    pub capabilities: Vec<String>,
}

impl AgentIdentityV1 {
    #[must_use]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TransportContentType {
    HandoffBundle,
    ContextPackage,
    A2AMessage,
    A2ATask,
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

    pub fn sign(&mut self, secret: &[u8]) {
        let mac_bytes = self.compute_hmac(secret);
        let mut sig = String::with_capacity(mac_bytes.len() * 2);
        for b in &mac_bytes {
            use std::fmt::Write;
            let _ = write!(sig, "{b:02x}");
        }
        self.signature = Some(sig);
    }

    #[must_use]
    pub fn verify_signature(&self, secret: &[u8]) -> bool {
        let Some(ref sig) = self.signature else {
            return false;
        };

        let expected: Vec<u8> = (0..sig.len())
            .step_by(2)
            .filter_map(|i| {
                sig.get(i..i + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect();
        if expected.len() != sig.len() / 2 {
            return false;
        }

        let computed = self.compute_hmac(secret);
        constant_time_eq(&computed, &expected)
    }

    fn compute_hmac(&self, secret: &[u8]) -> Vec<u8> {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        let recipient_str = self.recipient.as_deref().unwrap_or("");
        let mut sorted_meta: Vec<(&str, &str)> = self
            .metadata
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        sorted_meta.sort_by_key(|(k, _)| *k);
        let meta_str: String = sorted_meta
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");

        let header = format!(
            "v2:{}:{}:{}:{}:{}:{}:{}",
            self.format_version,
            self.sender.agent_id,
            recipient_str,
            self.content_type_str(),
            self.sent_at.to_rfc3339(),
            meta_str,
            self.payload_json.len()
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
        mac.update(header.as_bytes());
        mac.update(b"\0");
        mac.update(self.payload_json.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    fn content_type_str(&self) -> &str {
        match self.content_type {
            TransportContentType::HandoffBundle => "handoff_bundle",
            TransportContentType::ContextPackage => "context_package",
            TransportContentType::A2AMessage => "a2a_message",
            TransportContentType::A2ATask => "a2a_task",
        }
    }
}

pub fn serialize_envelope(envelope: &TransportEnvelopeV1) -> Result<String, String> {
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

pub fn parse_envelope(json: &str) -> Result<TransportEnvelopeV1, String> {
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
    Ok(env)
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

    fn test_sender() -> AgentIdentityV1 {
        AgentIdentityV1 {
            agent_id: "test-agent".to_string(),
            agent_type: "cursor".to_string(),
            daemon_fingerprint: "abcd1234".to_string(),
            capabilities: vec!["context_compression".to_string()],
        }
    }

    #[test]
    fn envelope_roundtrip() {
        let env = TransportEnvelopeV1::new(
            test_sender(),
            Some("target-agent"),
            TransportContentType::A2AMessage,
            r#"{"hello":"world"}"#.to_string(),
        );
        let json = serialize_envelope(&env).unwrap();
        let parsed = parse_envelope(&json).unwrap();
        assert_eq!(parsed.format_version, 1);
        assert_eq!(parsed.sender.agent_id, "test-agent");
        assert_eq!(parsed.recipient, Some("target-agent".to_string()));
        assert_eq!(parsed.content_type, TransportContentType::A2AMessage);
    }

    #[test]
    fn hmac_sign_verify() {
        let secret = b"test-secret-key";
        let mut env = TransportEnvelopeV1::new(
            test_sender(),
            None,
            TransportContentType::HandoffBundle,
            "payload".to_string(),
        );
        assert!(!env.verify_signature(secret));

        env.sign(secret);
        assert!(env.signature.is_some());
        assert!(env.verify_signature(secret));
        assert!(!env.verify_signature(b"wrong-key"));
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
