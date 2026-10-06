//! Payload-free, delta-capable contract for multi-agent context handoffs.
//!
//! A capsule is a manifest: it names content-addressed context, evidence and
//! recovery handles but never carries prompt text, file contents or
//! chain-of-thought. Materializers resolve references after policy/freshness
//! checks.

use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};

pub const CONTEXT_CAPSULE_SCHEMA_VERSION: u16 = 1;
const MAX_CAPSULE_REFERENCES: usize = 256;
const MAX_CAPSULE_REF_LIST_ITEMS: usize = 256;
const MAX_CAPSULE_ALLOWED_AGENTS: usize = 64;
const MAX_CAPSULE_HOPS: u16 = 256;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsuleReferenceKindV1 {
    File,
    Symbol,
    Evidence,
    Recovery,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextCapsuleReferenceV1 {
    pub kind: CapsuleReferenceKindV1,
    pub content_ref: String,
    pub freshness_ref: String,
    pub recovery_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsuleSensitivityV1 {
    Public,
    Internal,
    Restricted,
}

impl From<CapsuleSensitivityV1> for lean_ctx_protocol::context_gateway::ClassificationV1 {
    fn from(level: CapsuleSensitivityV1) -> Self {
        match level {
            CapsuleSensitivityV1::Public => Self::Public,
            CapsuleSensitivityV1::Internal => Self::Internal,
            CapsuleSensitivityV1::Restricted => Self::Restricted,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextCapsuleBudgetV1 {
    pub tokens_used: u64,
    pub tokens_remaining: u64,
    pub cost_micros_used: u64,
    pub cost_micros_remaining: u64,
    pub latency_ms_used: u64,
    pub latency_ms_remaining: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextCapsuleChainV1 {
    pub chain_id: String,
    pub parent_capsule_ref: Option<String>,
    pub owner_agent_id: String,
    pub attribution_ref: String,
    pub hop: u16,
}

/// Canonical handoff manifest. Collections are canonically sorted before
/// identity is assigned, so independently produced equivalent manifests share
/// one stable capsule ID and can be transmitted as deltas.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCapsuleV1 {
    pub schema_version: u16,
    pub capsule_id: String,
    pub request_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub intent_ref: String,
    pub task_ref: String,
    pub expected_outcome_ref: String,
    pub acceptance_criteria_refs: Vec<String>,
    pub references: Vec<ContextCapsuleReferenceV1>,
    pub finding_refs: Vec<String>,
    pub decision_refs: Vec<String>,
    pub uncertainty_refs: Vec<String>,
    pub negative_result_refs: Vec<String>,
    pub source_ref: String,
    pub policy_ref: String,
    pub contract_ref: String,
    pub freshness_ref: String,
    pub sensitivity: CapsuleSensitivityV1,
    pub allowed_agent_ids: Vec<String>,
    pub budget: ContextCapsuleBudgetV1,
    pub chain: ContextCapsuleChainV1,
    pub quality_signal_refs: Vec<String>,
    pub recovery_refs: Vec<String>,
    pub delta_from: Option<String>,
}

impl ContextCapsuleV1 {
    pub fn canonicalize(&mut self) {
        self.acceptance_criteria_refs.sort();
        self.references.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.content_ref.cmp(&right.content_ref))
        });
        for refs in [
            &mut self.finding_refs,
            &mut self.decision_refs,
            &mut self.uncertainty_refs,
            &mut self.negative_result_refs,
            &mut self.quality_signal_refs,
            &mut self.recovery_refs,
            &mut self.allowed_agent_ids,
        ] {
            refs.sort();
            refs.dedup();
        }
    }

    pub fn assign_capsule_id(&mut self) -> Result<(), ContextCapsuleError> {
        self.canonicalize();
        self.capsule_id = self.computed_capsule_id()?;
        Ok(())
    }

    pub fn computed_capsule_id(&self) -> Result<String, ContextCapsuleError> {
        let mut canonical = self.clone();
        canonical.canonicalize();
        canonical.capsule_id = "capsule:pending".to_string();
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|e| ContextCapsuleError::Serialize(e.to_string()))?;
        Ok(format!("capsule:{}", blake3::hash(&bytes).to_hex()))
    }

    pub fn validate(&self) -> Result<(), ContextCapsuleError> {
        if self.schema_version != CONTEXT_CAPSULE_SCHEMA_VERSION {
            return Err(ContextCapsuleError::UnsupportedVersion(self.schema_version));
        }
        for (label, value) in [
            ("capsule_id", self.capsule_id.as_str()),
            ("request_id", self.request_id.as_str()),
            ("session_id", self.session_id.as_str()),
            ("intent_ref", self.intent_ref.as_str()),
            ("task_ref", self.task_ref.as_str()),
            ("expected_outcome_ref", self.expected_outcome_ref.as_str()),
            ("source_ref", self.source_ref.as_str()),
            ("policy_ref", self.policy_ref.as_str()),
            ("contract_ref", self.contract_ref.as_str()),
            ("freshness_ref", self.freshness_ref.as_str()),
            ("chain_id", self.chain.chain_id.as_str()),
            ("attribution_ref", self.chain.attribution_ref.as_str()),
        ] {
            opaque_ref(label, value)?;
            agent_id_value(&self.agent_id)?;
        }
        if !self.capsule_id.starts_with("capsule:") {
            return Err(ContextCapsuleError::Invalid(
                "capsule_id must use capsule: scheme".into(),
            ));
        }
        if let Some(base) = self.delta_from.as_deref() {
            opaque_ref("delta_from", base)?;
            if base == self.capsule_id {
                return Err(ContextCapsuleError::Invalid(
                    "delta_from cannot self-reference".into(),
                ));
            }
        }
        if let Some(parent) = self.chain.parent_capsule_ref.as_deref() {
            opaque_ref("parent_capsule_ref", parent)?;
            if parent == self.capsule_id {
                return Err(ContextCapsuleError::Invalid(
                    "parent_capsule_ref cannot self-reference".into(),
                ));
            }
        }
        if self.chain.hop > MAX_CAPSULE_HOPS {
            return Err(ContextCapsuleError::Invalid(format!(
                "chain hop exceeds {MAX_CAPSULE_HOPS}"
            )));
        }
        bounded("references", self.references.len(), MAX_CAPSULE_REFERENCES)?;
        for (label, count) in [
            (
                "acceptance_criteria_refs",
                self.acceptance_criteria_refs.len(),
            ),
            ("finding_refs", self.finding_refs.len()),
            ("decision_refs", self.decision_refs.len()),
            ("uncertainty_refs", self.uncertainty_refs.len()),
            ("negative_result_refs", self.negative_result_refs.len()),
            ("quality_signal_refs", self.quality_signal_refs.len()),
            ("recovery_refs", self.recovery_refs.len()),
        ] {
            bounded(label, count, MAX_CAPSULE_REF_LIST_ITEMS)?;
        }
        validate_references(&self.references)?;
        validate_ref_lists(&[
            &self.acceptance_criteria_refs,
            &self.finding_refs,
            &self.decision_refs,
            &self.uncertainty_refs,
            &self.negative_result_refs,
            &self.quality_signal_refs,
            &self.recovery_refs,
        ])?;
        if self.allowed_agent_ids.is_empty() {
            return Err(ContextCapsuleError::Invalid(
                "allowed_agent_ids cannot be empty".into(),
            ));
        }
        bounded(
            "allowed_agent_ids",
            self.allowed_agent_ids.len(),
            MAX_CAPSULE_ALLOWED_AGENTS,
        )?;
        validate_agent_ids(&self.allowed_agent_ids)?;
        agent_id_value(&self.chain.owner_agent_id)?;
        if self.capsule_id != self.computed_capsule_id()? {
            return Err(ContextCapsuleError::Invalid(
                "capsule_id does not match canonical content".into(),
            ));
        }
        Ok(())
    }

    /// Project this capsule as an agent envelope for an allowed target.
    pub fn agent_envelope(
        &self,
        to_agent_id: &str,
    ) -> Result<AgentEnvelopeV1, ContextCapsuleError> {
        self.validate()?;
        if !self.allowed_agent_ids.iter().any(|a| a == to_agent_id) {
            return Err(ContextCapsuleError::Invalid(
                "target agent is not in allowed_agent_ids".into(),
            ));
        }
        let mut envelope = AgentEnvelopeV1 {
            schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
            relay_id: "agent-relay:pending".to_string(),
            from_agent_id: self.chain.owner_agent_id.clone(),
            to_agent_id: to_agent_id.to_string(),
            capsule_ref: self.capsule_id.clone(),
            budget_tokens: self.budget.tokens_remaining,
            request_id: self.request_id.clone(),
            session_id: self.session_id.clone(),
        };
        envelope.assign_relay_id()?;
        Ok(envelope)
    }
}

// ─── Agent Envelope ──────────────────────────────────────────────────────────

pub const AGENT_ENVELOPE_SCHEMA_VERSION: u16 = 1;

/// Payload-free relay envelope for authorized agent-to-agent delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvelopeV1 {
    pub schema_version: u16,
    pub relay_id: String,
    pub from_agent_id: String,
    pub to_agent_id: String,
    pub capsule_ref: String,
    pub budget_tokens: u64,
    pub request_id: String,
    pub session_id: String,
}

impl AgentEnvelopeV1 {
    pub fn assign_relay_id(&mut self) -> Result<(), ContextCapsuleError> {
        self.relay_id = self.computed_relay_id()?;
        Ok(())
    }

    pub fn computed_relay_id(&self) -> Result<String, ContextCapsuleError> {
        let mut canonical = self.clone();
        canonical.relay_id = "agent-relay:pending".to_string();
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|e| ContextCapsuleError::Serialize(e.to_string()))?;
        Ok(format!("agent-relay:{}", blake3::hash(&bytes).to_hex()))
    }

    pub fn validate(&self) -> Result<(), ContextCapsuleError> {
        if self.schema_version != AGENT_ENVELOPE_SCHEMA_VERSION {
            return Err(ContextCapsuleError::UnsupportedVersion(self.schema_version));
        }
        agent_id_value(&self.from_agent_id)?;
        agent_id_value(&self.to_agent_id)?;
        if self.from_agent_id == self.to_agent_id {
            return Err(ContextCapsuleError::Invalid(
                "relay requires distinct source and target agents".into(),
            ));
        }
        if !self.capsule_ref.starts_with("capsule:") {
            return Err(ContextCapsuleError::Invalid("invalid capsule_ref".into()));
        }
        if self.budget_tokens == 0 {
            return Err(ContextCapsuleError::Invalid(
                "budget_tokens must be positive".into(),
            ));
        }
        Ok(())
    }
}

// ─── Signed Capsule (Ed25519) ────────────────────────────────────────────────

/// Ed25519-authenticated capsule manifest for trusted handoffs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedContextCapsuleV1 {
    pub capsule: ContextCapsuleV1,
    pub signer_public_key: String,
    pub signature: String,
}

impl SignedContextCapsuleV1 {
    pub fn sign(
        capsule: &ContextCapsuleV1,
        keypair: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ContextCapsuleError> {
        capsule.validate()?;
        let bytes = serde_json::to_vec(capsule)
            .map_err(|e| ContextCapsuleError::Serialize(e.to_string()))?;
        let signature = keypair.sign(&bytes);
        let public_key = keypair.verifying_key();
        Ok(Self {
            capsule: capsule.clone(),
            signer_public_key: encode_hex(public_key.as_bytes()),
            signature: encode_hex(&signature.to_bytes()),
        })
    }

    pub fn validate_structure(&self) -> Result<(), ContextCapsuleError> {
        self.capsule.validate()?;
        if self.signer_public_key.len() != 64 {
            return Err(ContextCapsuleError::Invalid(
                "invalid signer_public_key length".into(),
            ));
        }
        if self.signature.len() != 128 {
            return Err(ContextCapsuleError::Invalid(
                "invalid signature length".into(),
            ));
        }
        Ok(())
    }

    pub fn verify(
        &self,
        pinned_key: &ed25519_dalek::VerifyingKey,
    ) -> Result<(), ContextCapsuleError> {
        self.validate_structure()?;
        let key_bytes = decode_hex(&self.signer_public_key)
            .map_err(|_| ContextCapsuleError::Invalid("invalid signer hex".into()))?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(
            key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| ContextCapsuleError::Invalid("key must be 32 bytes".into()))?,
        )
        .map_err(|_| ContextCapsuleError::Invalid("invalid Ed25519 key".into()))?;
        if verifying_key != *pinned_key {
            return Err(ContextCapsuleError::Invalid(
                "signer key does not match pinned key".into(),
            ));
        }
        let sig_bytes = decode_hex(&self.signature)
            .map_err(|_| ContextCapsuleError::Invalid("invalid signature hex".into()))?;
        let signature = ed25519_dalek::Signature::from_bytes(
            sig_bytes
                .as_slice()
                .try_into()
                .map_err(|_| ContextCapsuleError::Invalid("signature must be 64 bytes".into()))?,
        );
        let capsule_bytes = serde_json::to_vec(&self.capsule)
            .map_err(|e| ContextCapsuleError::Serialize(e.to_string()))?;
        use ed25519_dalek::Verifier;
        verifying_key
            .verify(&capsule_bytes, &signature)
            .map_err(|_| ContextCapsuleError::Invalid("signature verification failed".into()))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn bounded(label: &str, count: usize, maximum: usize) -> Result<(), ContextCapsuleError> {
    (count <= maximum)
        .then_some(())
        .ok_or_else(|| ContextCapsuleError::Invalid(format!("{label} exceeds {maximum}")))
}

fn opaque_ref(label: &str, value: &str) -> Result<(), ContextCapsuleError> {
    let (scheme, identifier) = value.split_once(':').ok_or_else(|| {
        ContextCapsuleError::Invalid(format!("{label} must use scheme:identifier form"))
    })?;
    let scheme_valid = !scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let identifier_valid = !identifier.is_empty()
        && value.len() <= 256
        && identifier.bytes().all(|b| b.is_ascii_graphic());
    (scheme_valid && identifier_valid)
        .then_some(())
        .ok_or_else(|| ContextCapsuleError::Invalid(format!("invalid {label}")))
}

fn agent_id_value(value: &str) -> Result<(), ContextCapsuleError> {
    (!value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic()))
        .then_some(())
        .ok_or_else(|| ContextCapsuleError::Invalid("invalid agent ID".into()))
}

fn validate_references(refs: &[ContextCapsuleReferenceV1]) -> Result<(), ContextCapsuleError> {
    let mut seen = std::collections::BTreeSet::new();
    for r in refs {
        opaque_ref("reference content_ref", &r.content_ref)?;
        opaque_ref("reference freshness_ref", &r.freshness_ref)?;
        if let Some(recovery) = r.recovery_ref.as_deref() {
            opaque_ref("reference recovery_ref", recovery)?;
        }
        if !seen.insert((r.kind, r.content_ref.as_str())) {
            return Err(ContextCapsuleError::Invalid(
                "duplicate reference kind/content_ref pair".into(),
            ));
        }
    }
    Ok(())
}

fn validate_ref_lists(lists: &[&Vec<String>]) -> Result<(), ContextCapsuleError> {
    for list in lists {
        for r in *list {
            opaque_ref("list reference", r)?;
        }
    }
    Ok(())
}

fn validate_agent_ids(ids: &[String]) -> Result<(), ContextCapsuleError> {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        agent_id_value(id)?;
        if !seen.insert(id) {
            return Err(ContextCapsuleError::Invalid(
                "allowed_agent_ids must be unique".into(),
            ));
        }
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ContextCapsuleError {
    #[error("unsupported schema version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid capsule: {0}")]
    Invalid(String),
    #[error("serialization failed: {0}")]
    Serialize(String),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_capsule() -> ContextCapsuleV1 {
        let mut capsule = ContextCapsuleV1 {
            schema_version: CONTEXT_CAPSULE_SCHEMA_VERSION,
            capsule_id: "capsule:pending".to_string(),
            request_id: "request:1".to_string(),
            session_id: "session:1".to_string(),
            agent_id: "parent-agent".to_string(),
            intent_ref: "intent:refactor".to_string(),
            task_ref: "task:42".to_string(),
            expected_outcome_ref: "outcome:tests-pass".to_string(),
            acceptance_criteria_refs: vec!["criteria:green".to_string()],
            references: vec![ContextCapsuleReferenceV1 {
                kind: CapsuleReferenceKindV1::File,
                content_ref: "blake3:file".to_string(),
                freshness_ref: "freshness:1".to_string(),
                recovery_ref: Some("recovery:file".to_string()),
            }],
            finding_refs: vec!["finding:1".to_string()],
            decision_refs: vec!["decision:1".to_string()],
            uncertainty_refs: vec!["uncertainty:1".to_string()],
            negative_result_refs: vec!["negative:1".to_string()],
            source_ref: "source:workspace".to_string(),
            policy_ref: "policy:default".to_string(),
            contract_ref: "contract:ocla-v1".to_string(),
            freshness_ref: "freshness:1".to_string(),
            sensitivity: CapsuleSensitivityV1::Internal,
            allowed_agent_ids: vec!["reviewer-agent".to_string()],
            budget: ContextCapsuleBudgetV1 {
                tokens_used: 100,
                tokens_remaining: 900,
                cost_micros_used: 10,
                cost_micros_remaining: 90,
                latency_ms_used: 20,
                latency_ms_remaining: 80,
            },
            chain: ContextCapsuleChainV1 {
                chain_id: "chain:1".to_string(),
                parent_capsule_ref: None,
                owner_agent_id: "parent-agent".to_string(),
                attribution_ref: "attribution:1".to_string(),
                hop: 1,
            },
            quality_signal_refs: vec!["quality:1".to_string()],
            recovery_refs: vec!["recovery:full".to_string()],
            delta_from: None,
        };
        capsule.assign_capsule_id().expect("capsule ID assigns");
        capsule
    }

    #[test]
    fn capsule_is_payload_free_and_deterministic() {
        let c = test_capsule();
        c.validate().expect("valid capsule");
        assert_eq!(c.capsule_id, test_capsule().capsule_id);
        let json = serde_json::to_string(&c).expect("serializes");
        assert!(!json.contains("prompt"));
        assert!(!json.contains("payload"));
    }

    #[test]
    fn capsule_rejects_tampered_id() {
        let mut tampered = test_capsule();
        tampered.budget.tokens_remaining = 1;
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn capsule_rejects_duplicate_references() {
        let mut c = test_capsule();
        c.references.push(c.references[0].clone());
        c.assign_capsule_id().unwrap();
        assert!(c.validate().is_err());
    }

    #[test]
    fn capsule_envelope_requires_allowed_target() {
        let c = test_capsule();
        let env = c.agent_envelope("reviewer-agent").expect("allowed");
        assert_eq!(env.capsule_ref, c.capsule_id);
        assert_eq!(env.budget_tokens, 900);
        assert!(c.agent_envelope("other-agent").is_err());
    }

    #[test]
    fn envelope_validates_distinct_agents() {
        let c = test_capsule();
        let mut env = c.agent_envelope("reviewer-agent").unwrap();
        env.assign_relay_id().unwrap();
        env.validate().expect("valid envelope");

        let mut self_relay = env.clone();
        self_relay.to_agent_id = self_relay.from_agent_id.clone();
        assert!(self_relay.validate().is_err());
    }

    #[test]
    fn signed_capsule_roundtrip() {
        let c = test_capsule();
        let keypair = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let signed = SignedContextCapsuleV1::sign(&c, &keypair).expect("signs");
        signed.verify(&keypair.verifying_key()).expect("verifies");

        let wrong_key = ed25519_dalek::SigningKey::from_bytes(&[99u8; 32]);
        assert!(signed.verify(&wrong_key.verifying_key()).is_err());
    }
}
