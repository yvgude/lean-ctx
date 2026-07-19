//! Stable JSON projection for the public OCLA contract.

use serde_json::{Value, json};

use super::types::{
    AGENT_ENVELOPE_SCHEMA_VERSION, AgentEnvelope, CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
    CanonicalTokenEnvelopeV1, OCLA_API_VERSION, OclaError, OclaResult,
};

pub const OCLA_WIRE_SCHEMA_ID: &str =
    "https://leanctx.com/schemas/ocla/v1/canonical-token-envelope.json";
pub const OCLA_AGENT_ENVELOPE_WIRE_SCHEMA_ID: &str =
    "https://leanctx.com/schemas/ocla/v1/agent-envelope.json";
pub const MAX_OCLA_WIRE_BYTES: usize = 64 * 1024;

fn validate_wire_size(json: &str) -> OclaResult<()> {
    if json.len() > MAX_OCLA_WIRE_BYTES {
        return Err(OclaError::InvalidRequest(format!(
            "wire document exceeds {MAX_OCLA_WIRE_BYTES} bytes"
        )));
    }
    Ok(())
}

pub fn encode_envelope(envelope: &CanonicalTokenEnvelopeV1) -> OclaResult<String> {
    envelope.validate()?;
    let json = serde_json::to_string(envelope)
        .map_err(|error| OclaError::InvalidRequest(format!("cannot encode envelope: {error}")))?;
    validate_wire_size(&json)?;
    Ok(json)
}

pub fn decode_envelope(json: &str) -> OclaResult<CanonicalTokenEnvelopeV1> {
    validate_wire_size(json)?;
    let envelope: CanonicalTokenEnvelopeV1 = serde_json::from_str(json)
        .map_err(|error| OclaError::InvalidRequest(format!("cannot decode envelope: {error}")))?;
    envelope.validate()?;
    Ok(envelope)
}

/// Encodes a validated A2A admission contract. The budget remains an
/// authorization ceiling; this projection never denotes delivered tokens.
pub fn encode_agent_envelope(envelope: &AgentEnvelope) -> OclaResult<String> {
    envelope.validate()?;
    let json = serde_json::to_string(envelope).map_err(|error| {
        OclaError::InvalidRequest(format!("cannot encode agent envelope: {error}"))
    })?;
    validate_wire_size(&json)?;
    Ok(json)
}

/// Decodes an A2A admission contract and rejects schema, lineage, relay-ID, or
/// budget drift before an adapter can observe it.
pub fn decode_agent_envelope(json: &str) -> OclaResult<AgentEnvelope> {
    validate_wire_size(json)?;
    let envelope: AgentEnvelope = serde_json::from_str(json).map_err(|error| {
        OclaError::InvalidRequest(format!("cannot decode agent envelope: {error}"))
    })?;
    envelope.validate()?;
    Ok(envelope)
}

/// Source-generated JSON Schema used by SDKs and checked against the committed
/// public projection in CI. Keep this small and strict: compatibility changes
/// require an intentional schema-version decision.
#[must_use]
pub fn canonical_envelope_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": OCLA_WIRE_SCHEMA_ID,
        "title": "LeanCTX CanonicalTokenEnvelopeV1",
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "context", "surface", "direction", "provider", "model", "token_balance", "idempotency_key"],
        "properties": {
            "schema_version": {"const": CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION},
            "context": {
                "type": "object",
                "additionalProperties": false,
                "required": ["request_id", "session_id", "agent_id", "content_ref", "tenant_id"],
                "properties": {
                    "request_id": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "session_id": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "agent_id": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "content_ref": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "tenant_id": {"type": ["string", "null"]}
                }
            },
            "surface": {"enum": ["mcp", "proxy", "shell", "agent"]},
            "direction": {"enum": ["input", "output"]},
            "provider": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
            "model": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
            "token_balance": {
                "type": "object",
                "additionalProperties": false,
                "required": ["original_tokens", "materialized_tokens", "delivered_tokens", "provider_billed_tokens"],
                "properties": {
                    "original_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
                    "materialized_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
                    "delivered_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
                    "provider_billed_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX}
                }
            },
            "route_ref": {"type": ["string", "null"]},
            "policy_ref": {"type": ["string", "null"]},
            "idempotency_key": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"}
        },
        "x-ocla-api-version": OCLA_API_VERSION
    })
}

/// Source-generated public JSON Schema for the payload-free A2A admission
/// contract. Cross-field owner and content-derived identity checks remain in
/// [`decode_agent_envelope`], where they can fail closed.
#[must_use]
pub fn agent_envelope_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": OCLA_AGENT_ENVELOPE_WIRE_SCHEMA_ID,
        "title": "LeanCTX AgentEnvelopeV1",
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "relay_id", "context", "from_agent_id", "to_agent_id", "capsule_ref", "budget_tokens"],
        "properties": {
            "schema_version": {"const": AGENT_ENVELOPE_SCHEMA_VERSION},
            "relay_id": {"type": "string", "pattern": "^agent-relay:[0-9a-f]{64}$"},
            "context": {
                "type": "object",
                "additionalProperties": false,
                "required": ["request_id", "session_id", "agent_id", "content_ref", "tenant_id"],
                "properties": {
                    "request_id": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "session_id": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "agent_id": {"type": "string", "minLength": 1, "maxLength": 256, "pattern": "^[!-~]+$"},
                    "content_ref": {"type": "string", "minLength": 1, "pattern": ".*\\S.*"},
                    "tenant_id": {"type": ["string", "null"]}
                }
            },
            "from_agent_id": {"type": "string", "minLength": 1, "maxLength": 256, "pattern": "^[!-~]+$"},
            "to_agent_id": {"type": "string", "minLength": 1, "maxLength": 256, "pattern": "^[!-~]+$"},
            "capsule_ref": {"type": "string", "pattern": "^capsule:[0-9a-f]{64}$"},
            "budget_tokens": {"type": "integer", "minimum": 1, "maximum": u64::MAX}
        },
        "x-ocla-api-version": OCLA_API_VERSION,
        "x-evidence-boundary": "admission_only"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::{
        AGENT_ENVELOPE_SCHEMA_VERSION, OclaRequestContext, TokenBalanceV1, TokenEnvelopeSurface,
        TokenFlowDirection,
    };

    fn envelope() -> CanonicalTokenEnvelopeV1 {
        CanonicalTokenEnvelopeV1 {
            schema_version: CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
            context: OclaRequestContext {
                request_id: "request-1".into(),
                session_id: "session-1".into(),
                agent_id: "agent-1".into(),
                content_ref: "blake3:content".into(),
                tenant_id: None,
            },
            surface: TokenEnvelopeSurface::Proxy,
            direction: TokenFlowDirection::Input,
            provider: "openai".into(),
            model: "gpt-5".into(),
            token_balance: TokenBalanceV1 {
                original_tokens: 100,
                materialized_tokens: 80,
                delivered_tokens: 60,
                provider_billed_tokens: 60,
            },
            route_ref: Some("route-1".into()),
            policy_ref: None,
            idempotency_key: "request-1:input".into(),
        }
    }

    fn agent_envelope() -> AgentEnvelope {
        let mut envelope = AgentEnvelope {
            schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
            relay_id: "agent-relay:pending".to_string(),
            context: OclaRequestContext {
                request_id: "agent-request-1".into(),
                session_id: "agent-session-1".into(),
                agent_id: "owner-agent".into(),
                content_ref: "blake3:agent-content".into(),
                tenant_id: None,
            },
            from_agent_id: "owner-agent".into(),
            to_agent_id: "reviewer-agent".into(),
            capsule_ref: format!("capsule:{}", "a".repeat(64)),
            budget_tokens: 900,
        };
        envelope.assign_relay_id().expect("relay ID assigns");
        envelope
    }

    #[test]
    fn canonical_envelope_roundtrips_without_payload() {
        let original = envelope();
        let wire = encode_envelope(&original).expect("valid envelope");
        assert!(!wire.contains("payload"));
        assert_eq!(decode_envelope(&wire).expect("decode"), original);
    }

    #[test]
    fn invalid_token_order_and_unknown_fields_are_rejected() {
        let mut invalid = envelope();
        invalid.token_balance.delivered_tokens = 81;
        assert!(encode_envelope(&invalid).is_err());
        assert!(decode_envelope(r#"{"schema_version":1,"extra":true}"#).is_err());
    }

    #[test]
    fn agent_envelope_roundtrips_without_payload_or_delivery_claim() {
        let original = agent_envelope();
        let wire = encode_agent_envelope(&original).expect("valid agent envelope");
        assert!(!wire.contains("payload"));
        assert!(!wire.contains("delivered_tokens"));
        assert_eq!(decode_agent_envelope(&wire).expect("decode"), original);
        assert!(decode_agent_envelope(r#"{"schema_version":1,"unexpected":true}"#).is_err());
    }

    #[test]
    fn missing_tenant_id_is_rejected_for_both_public_envelopes() {
        let token_wire = encode_envelope(&envelope()).expect("encode token envelope");
        let mut token_value: Value = serde_json::from_str(&token_wire).expect("token JSON");
        token_value["context"]
            .as_object_mut()
            .expect("token context object")
            .remove("tenant_id");
        assert!(decode_envelope(&token_value.to_string()).is_err());

        let agent_wire = encode_agent_envelope(&agent_envelope()).expect("encode agent envelope");
        let mut agent_value: Value = serde_json::from_str(&agent_wire).expect("agent JSON");
        agent_value["context"]
            .as_object_mut()
            .expect("agent context object")
            .remove("tenant_id");
        assert!(decode_agent_envelope(&agent_value.to_string()).is_err());
    }

    #[test]
    fn both_public_decoders_enforce_the_exact_wire_size_boundary() {
        let token_wire = encode_envelope(&envelope()).expect("encode token envelope");
        let token_at_limit = format!(
            "{token_wire}{}",
            " ".repeat(MAX_OCLA_WIRE_BYTES - token_wire.len())
        );
        assert_eq!(token_at_limit.len(), MAX_OCLA_WIRE_BYTES);
        assert!(decode_envelope(&token_at_limit).is_ok());
        assert!(decode_envelope(&(token_at_limit + " ")).is_err());

        let agent_wire = encode_agent_envelope(&agent_envelope()).expect("encode agent envelope");
        let agent_at_limit = format!(
            "{agent_wire}{}",
            " ".repeat(MAX_OCLA_WIRE_BYTES - agent_wire.len())
        );
        assert_eq!(agent_at_limit.len(), MAX_OCLA_WIRE_BYTES);
        assert!(decode_agent_envelope(&agent_at_limit).is_ok());
        assert!(decode_agent_envelope(&(agent_at_limit + " ")).is_err());
    }

    #[test]
    fn both_public_encoders_reject_oversize_valid_envelopes() {
        let mut oversized_token = envelope();
        oversized_token.provider = "p".repeat(MAX_OCLA_WIRE_BYTES);
        assert!(matches!(
            encode_envelope(&oversized_token),
            Err(OclaError::InvalidRequest(message))
                if message == format!("wire document exceeds {MAX_OCLA_WIRE_BYTES} bytes")
        ));

        let mut oversized_agent = agent_envelope();
        oversized_agent.context.content_ref = "c".repeat(MAX_OCLA_WIRE_BYTES);
        oversized_agent
            .assign_relay_id()
            .expect("oversize relay identity assigns");
        assert!(matches!(
            encode_agent_envelope(&oversized_agent),
            Err(OclaError::InvalidRequest(message))
                if message == format!("wire document exceeds {MAX_OCLA_WIRE_BYTES} bytes")
        ));
    }

    #[test]
    fn schema_numeric_limits_match_engine_u64_boundaries() {
        let token_schema = canonical_envelope_schema();
        for field in [
            "original_tokens",
            "materialized_tokens",
            "delivered_tokens",
            "provider_billed_tokens",
        ] {
            assert_eq!(
                token_schema["properties"]["token_balance"]["properties"][field]["maximum"],
                Value::from(u64::MAX)
            );
        }
        assert_eq!(
            agent_envelope_schema()["properties"]["budget_tokens"]["maximum"],
            Value::from(u64::MAX)
        );

        let mut max_token = envelope();
        max_token.token_balance = TokenBalanceV1 {
            original_tokens: u64::MAX,
            materialized_tokens: u64::MAX,
            delivered_tokens: u64::MAX,
            provider_billed_tokens: u64::MAX,
        };
        let wire = encode_envelope(&max_token).expect("encode u64 maximum");
        assert_eq!(
            decode_envelope(&wire).expect("decode u64 maximum"),
            max_token
        );

        let mut max_agent = agent_envelope();
        max_agent.budget_tokens = u64::MAX;
        max_agent
            .assign_relay_id()
            .expect("assign maximum-budget relay ID");
        let wire = encode_agent_envelope(&max_agent).expect("encode u64 maximum");
        assert_eq!(
            decode_agent_envelope(&wire).expect("decode u64 maximum"),
            max_agent
        );
    }

    #[test]
    fn committed_json_schema_cannot_drift_from_the_rust_projection() {
        let committed: Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/ocla-wire-v1.schema.json"
        ))
        .expect("valid committed OCLA schema");
        assert_eq!(committed, canonical_envelope_schema());
    }

    #[test]
    fn committed_agent_envelope_schema_cannot_drift_from_the_rust_projection() {
        let committed: Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/ocla-agent-envelope-v1.schema.json"
        ))
        .expect("valid committed agent-envelope schema");
        assert_eq!(committed, agent_envelope_schema());
    }
}
