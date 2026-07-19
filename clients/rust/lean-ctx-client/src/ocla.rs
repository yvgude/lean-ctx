//! Standalone OCLA v1 wire decoding and offline verification.
//!
//! Types mirror public JSON Schemas without linking engine internals.

use serde::{Deserialize, Serialize};

/// OCLA API version implemented by this verifier.
pub const OCLA_API_VERSION: &str = "ocla/v1";
/// Supported canonical-token-envelope schema version.
pub const CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION: u16 = 1;
/// Supported agent-envelope schema version.
pub const AGENT_ENVELOPE_SCHEMA_VERSION: u16 = 1;
/// Public canonical-token-envelope JSON Schema identifier.
pub const CANONICAL_TOKEN_ENVELOPE_SCHEMA_ID: &str =
    "https://leanctx.com/schemas/ocla/v1/canonical-token-envelope.json";
/// Public agent-envelope JSON Schema identifier.
pub const AGENT_ENVELOPE_SCHEMA_ID: &str =
    "https://leanctx.com/schemas/ocla/v1/agent-envelope.json";
/// Maximum accepted wire document size.
pub const MAX_OCLA_WIRE_BYTES: usize = 64 * 1024;

/// A rejected OCLA wire document.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum OclaWireError {
    /// Input exceeded the maximum wire size.
    #[error("OCLA wire document is {actual} bytes; maximum is {maximum}")]
    Oversize {
        /// Observed byte length.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// JSON could not be decoded into the strict public type.
    #[error("malformed OCLA wire document: {0}")]
    Malformed(String),
    /// Schema version is unsupported.
    #[error("unsupported OCLA schema version {actual}; expected {expected}")]
    UnsupportedVersion {
        /// Supported version.
        expected: u16,
        /// Observed version.
        actual: u16,
    },
    /// A cross-field public contract invariant failed.
    #[error("invalid OCLA wire invariant: {0}")]
    InvalidInvariant(&'static str),
    /// JSON bytes were valid but not the canonical compact projection.
    #[error("OCLA wire document is not canonical JSON")]
    NonCanonical,
}

/// A typed OCLA envelope that is wire-valid but rejected by the SDK's explicit
/// gateway policy check.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum OclaGatewayAdmissibilityError {
    /// A relay must cross an agent boundary.
    #[error("agent gateway rejects self-relay")]
    SelfRelay,
}

/// Stable identifiers joining decisions across interception surfaces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OclaRequestContext {
    /// Request lineage identifier.
    pub request_id: String,
    /// Session lineage identifier.
    pub session_id: String,
    /// Agent lineage identifier.
    pub agent_id: String,
    /// Payload-free content reference.
    pub content_ref: String,
    /// Optional tenant lineage identifier; present as null when unknown.
    pub tenant_id: Option<String>,
}

/// Interception surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEnvelopeSurface {
    /// Model Context Protocol.
    Mcp,
    /// Provider proxy.
    Proxy,
    /// Shell.
    Shell,
    /// Agent-to-agent.
    Agent,
}

/// Token-flow direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenFlowDirection {
    /// Tokens entering a boundary.
    Input,
    /// Tokens leaving a boundary.
    Output,
}

/// Provider-neutral accounting at token lifecycle stages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenBalanceV1 {
    /// Tokens before materialization.
    pub original_tokens: u64,
    /// Tokens materialized by runtime.
    pub materialized_tokens: u64,
    /// Tokens delivered across measured boundary.
    pub delivered_tokens: u64,
    /// Tokens reported as billed by provider.
    pub provider_billed_tokens: u64,
}

/// Payload-free token decision at an OCLA boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalTokenEnvelopeV1 {
    /// Wire schema version.
    pub schema_version: u16,
    /// Complete request lineage.
    pub context: OclaRequestContext,
    /// Interception surface.
    pub surface: TokenEnvelopeSurface,
    /// Token-flow direction.
    pub direction: TokenFlowDirection,
    /// Provider identifier.
    pub provider: String,
    /// Model identifier.
    pub model: String,
    /// Lifecycle token accounting.
    pub token_balance: TokenBalanceV1,
    /// Optional route decision reference.
    pub route_ref: Option<String>,
    /// Optional policy decision reference.
    pub policy_ref: Option<String>,
    /// Caller-provided idempotency identity.
    pub idempotency_key: String,
}

/// Payload-free A2A admission contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvelopeV1 {
    /// Wire schema version.
    pub schema_version: u16,
    /// Content-derived relay identity.
    pub relay_id: String,
    /// Complete request lineage.
    pub context: OclaRequestContext,
    /// Sending agent; must own context.agent_id.
    pub from_agent_id: String,
    /// Receiving agent.
    pub to_agent_id: String,
    /// Payload-free capsule BLAKE3 reference.
    pub capsule_ref: String,
    /// Authorized token ceiling, not delivery evidence.
    pub budget_tokens: u64,
}

impl AgentEnvelopeV1 {
    /// Derive the content-bound relay identity used by the public v1 wire
    /// contract. Payload bytes and the existing identity are excluded.
    pub fn canonical_relay_id(&self) -> Result<String, OclaWireError> {
        let mut canonical = self.clone();
        canonical.relay_id = "agent-relay:pending".to_string();
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|error| OclaWireError::Malformed(error.to_string()))?;
        Ok(format!("agent-relay:{}", blake3::hash(&bytes).to_hex()))
    }
}

/// Decode and verify one canonical-token-envelope v1 wire document.
pub fn decode_canonical_token_envelope(
    bytes: &[u8],
) -> Result<CanonicalTokenEnvelopeV1, OclaWireError> {
    let envelope: CanonicalTokenEnvelopeV1 = decode_bounded(bytes)?;
    require_version(
        envelope.schema_version,
        CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
    )?;
    validate_context(&envelope.context)?;
    if envelope.token_balance.materialized_tokens > envelope.token_balance.original_tokens {
        return Err(OclaWireError::InvalidInvariant(
            "materialized_tokens exceeds original_tokens",
        ));
    }
    if envelope.token_balance.delivered_tokens > envelope.token_balance.materialized_tokens {
        return Err(OclaWireError::InvalidInvariant(
            "delivered_tokens exceeds materialized_tokens",
        ));
    }
    for (value, message) in [
        (&envelope.provider, "provider is required"),
        (&envelope.model, "model is required"),
        (&envelope.idempotency_key, "idempotency_key is required"),
    ] {
        require_text(value, message)?;
    }
    require_canonical(bytes, &envelope)?;
    Ok(envelope)
}

/// Decode and verify one agent-envelope v1 wire document.
///
/// This proves bounded type, canonical JSON, lineage and content-derived
/// identity integrity only. Gateway admissibility is a separate policy layer.
pub fn decode_agent_envelope(bytes: &[u8]) -> Result<AgentEnvelopeV1, OclaWireError> {
    let envelope: AgentEnvelopeV1 = decode_bounded(bytes)?;
    require_version(envelope.schema_version, AGENT_ENVELOPE_SCHEMA_VERSION)?;
    validate_context(&envelope.context)?;
    if !valid_agent_id(&envelope.from_agent_id) {
        return Err(OclaWireError::InvalidInvariant("invalid from_agent_id"));
    }
    if !valid_agent_id(&envelope.to_agent_id) {
        return Err(OclaWireError::InvalidInvariant("invalid to_agent_id"));
    }
    if envelope.context.agent_id != envelope.from_agent_id {
        return Err(OclaWireError::InvalidInvariant(
            "context agent_id must match from_agent_id",
        ));
    }
    if !valid_digest_ref("capsule:", &envelope.capsule_ref) {
        return Err(OclaWireError::InvalidInvariant("invalid capsule_ref"));
    }
    if !valid_digest_ref("agent-relay:", &envelope.relay_id) {
        return Err(OclaWireError::InvalidInvariant("invalid relay_id"));
    }
    if envelope.budget_tokens == 0 {
        return Err(OclaWireError::InvalidInvariant(
            "budget_tokens must be greater than zero",
        ));
    }
    if envelope.relay_id != envelope.canonical_relay_id()? {
        return Err(OclaWireError::InvalidInvariant(
            "relay_id does not match canonical relay content",
        ));
    }
    require_canonical(bytes, &envelope)?;
    Ok(envelope)
}

/// Apply the SDK's explicit gateway policy to an already wire-verified agent
/// envelope.
///
/// This local check rejects self-relays only. It does not claim remote gateway
/// admission, transport delivery, authorization, billing, or interoperability.
pub fn verify_agent_gateway_admissibility(
    envelope: &AgentEnvelopeV1,
) -> Result<(), OclaGatewayAdmissibilityError> {
    if envelope.from_agent_id == envelope.to_agent_id {
        return Err(OclaGatewayAdmissibilityError::SelfRelay);
    }
    Ok(())
}

fn decode_bounded<T>(bytes: &[u8]) -> Result<T, OclaWireError>
where
    T: for<'de> Deserialize<'de>,
{
    if bytes.len() > MAX_OCLA_WIRE_BYTES {
        return Err(OclaWireError::Oversize {
            actual: bytes.len(),
            maximum: MAX_OCLA_WIRE_BYTES,
        });
    }
    serde_json::from_slice(bytes).map_err(|error| OclaWireError::Malformed(error.to_string()))
}

fn require_version(actual: u16, expected: u16) -> Result<(), OclaWireError> {
    if actual != expected {
        return Err(OclaWireError::UnsupportedVersion { expected, actual });
    }
    Ok(())
}

fn require_text(value: &str, message: &'static str) -> Result<(), OclaWireError> {
    if value.trim().is_empty() {
        return Err(OclaWireError::InvalidInvariant(message));
    }
    Ok(())
}

fn validate_context(context: &OclaRequestContext) -> Result<(), OclaWireError> {
    for (value, message) in [
        (&context.request_id, "request_id is required"),
        (&context.session_id, "session_id is required"),
        (&context.agent_id, "agent_id is required"),
        (&context.content_ref, "content_ref is required"),
    ] {
        require_text(value, message)?;
    }
    Ok(())
}

fn valid_agent_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_digest_ref(prefix: &str, value: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn require_canonical<T: Serialize>(bytes: &[u8], value: &T) -> Result<(), OclaWireError> {
    let canonical =
        serde_json::to_vec(value).map_err(|error| OclaWireError::Malformed(error.to_string()))?;
    if canonical != bytes {
        return Err(OclaWireError::NonCanonical);
    }
    Ok(())
}
