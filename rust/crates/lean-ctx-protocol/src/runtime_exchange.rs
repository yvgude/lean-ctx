// SPDX-License-Identifier: Apache-2.0
//! Strict payloads for the optional local Intelligence peer, not a second Engine.
//! Transport authenticates exact bytes before decoding and owns replay state.
//! Only explicitly policy-approved projections belong here; never log payloads.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    EngineInvocationV1, EngineObservationV1, EnginePolicyDecisionV1, Sha256Digest, ValidationError,
    receipt_document::digest_bytes, validate_bounded_opaque_identifier,
};

/// Independent successor to the legacy, one-way runtime notification.
pub const RUNTIME_EXCHANGE_VERSION: &str = "leanctx.runtime-exchange/v1";
/// Encoded JSON limit; transport must check the length prefix before allocation.
pub const MAX_RUNTIME_EXCHANGE_BYTES: usize = 1_048_576;
/// A peer cannot extend an invocation beyond this host-admitted time budget.
pub const MAX_RUNTIME_DEADLINE_MS: u64 = 60_000;

/// One admitted invocation plus its exact bounded input projection.
/// No Debug implementation: the projection may contain private user content.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequestV1 {
    pub protocol_version: String,
    pub session_id: String,
    pub request_id: String,
    /// Strictly increasing within the authenticated session; zero is reserved.
    pub sequence: u64,
    pub deadline_unix_ms: u64,
    pub invocation: EngineInvocationV1,
    pub input: String,
}

impl RuntimeRequestV1 {
    /// Validate structure and input binding; this does not grant authorization.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_version(&self.protocol_version)?;
        validate_bounded_opaque_identifier(&self.session_id, "runtime session_id")?;
        validate_bounded_opaque_identifier(&self.request_id, "runtime request_id")?;
        self.invocation.validate()?;
        if self.sequence == 0 || self.deadline_unix_ms == 0 {
            return Err(ValidationError::new(
                "runtime sequence/deadline must be nonzero",
            ));
        }
        if self.invocation.policy_admission.decision != EnginePolicyDecisionV1::Admitted {
            return Err(ValidationError::new(
                "runtime input requires host policy admission",
            ));
        }
        validate_content(self.input.as_bytes(), &self.invocation.input_digest)
    }

    /// Deterministically check time admission using the receiver's clock.
    /// The transport also enforces a monotonic I/O deadline for the entire call.
    pub fn validate_at(&self, now_unix_ms: u64) -> Result<(), ValidationError> {
        self.validate()?;
        let remaining = self.deadline_unix_ms.saturating_sub(now_unix_ms);
        if remaining == 0 || remaining > MAX_RUNTIME_DEADLINE_MS {
            return Err(ValidationError::new(
                "runtime deadline expired or exceeds budget",
            ));
        }
        Ok(())
    }

    /// Encode the exact stable representation used by response correlation.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        encode(self)
    }

    /// Decode bounded JSON, rejecting unknown fields, duplicates and bad bindings.
    /// Equivalent JSON whitespace/key order is accepted after authentication of
    /// the exact received bytes; decoding is not transport authentication.
    pub fn from_bytes(bytes: &[u8], now_unix_ms: u64) -> Result<Self, ValidationError> {
        let request: Self = decode(bytes)?;
        request.validate_at(now_unix_ms)?;
        Ok(request)
    }

    /// Bind all request fields, not merely a caller-provided invocation ID.
    /// This is a semantic correlation digest of stable re-encoding, not a MAC
    /// of the received representation. Transport authenticates bytes separately.
    pub fn digest(&self) -> Result<Sha256Digest, ValidationError> {
        digest_bytes(&self.to_bytes()?)
    }
}

/// Peer result bound to the whole request; receipts remain host-owned.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeResponseV1 {
    pub protocol_version: String,
    pub request_digest: Sha256Digest,
    pub observation: EngineObservationV1,
    pub output: Option<String>,
}

impl RuntimeResponseV1 {
    /// Reuse canonical Engine lineage/status validation and check exact content.
    pub fn validate_for(&self, request: &RuntimeRequestV1) -> Result<(), ValidationError> {
        validate_version(&self.protocol_version)?;
        if self.request_digest != request.digest()? {
            return Err(ValidationError::new(
                "runtime response belongs to another request",
            ));
        }
        self.observation.validate_for(&request.invocation)?;
        if self.observation.receipt_link.is_some() {
            return Err(ValidationError::new(
                "runtime peer cannot issue a host receipt",
            ));
        }
        match (&self.output, &self.observation.output_digest) {
            (Some(output), Some(digest)) => validate_content(output.as_bytes(), digest),
            (None, None) => Ok(()),
            _ => Err(ValidationError::new(
                "runtime output and digest must be present together",
            )),
        }
    }

    /// Encode only a result that is bound to the admitted request.
    pub fn to_bytes(&self, request: &RuntimeRequestV1) -> Result<Vec<u8>, ValidationError> {
        self.validate_for(request)?;
        encode(self)
    }

    /// Reject late, mismatched, malformed or oversized replies before use.
    pub fn from_bytes(
        bytes: &[u8],
        request: &RuntimeRequestV1,
        now_unix_ms: u64,
    ) -> Result<Self, ValidationError> {
        request.validate_at(now_unix_ms)?;
        let response: Self = decode(bytes)?;
        response.validate_for(request)?;
        Ok(response)
    }
}

fn validate_version(version: &str) -> Result<(), ValidationError> {
    if version != RUNTIME_EXCHANGE_VERSION {
        return Err(ValidationError::new("unsupported runtime exchange version"));
    }
    Ok(())
}

fn validate_content(bytes: &[u8], digest: &Sha256Digest) -> Result<(), ValidationError> {
    if bytes.len() > MAX_RUNTIME_EXCHANGE_BYTES || digest_bytes(bytes)? != *digest {
        return Err(ValidationError::new(
            "runtime content exceeds limit or differs from digest",
        ));
    }
    Ok(())
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, ValidationError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ValidationError::new("runtime JSON encoding failed"))?;
    if bytes.len() > MAX_RUNTIME_EXCHANGE_BYTES {
        return Err(ValidationError::new(
            "runtime message exceeds encoded limit",
        ));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ValidationError> {
    if bytes.is_empty() || bytes.len() > MAX_RUNTIME_EXCHANGE_BYTES {
        return Err(ValidationError::new("runtime message length is invalid"));
    }
    serde_json::from_slice(bytes).map_err(|_| ValidationError::new("invalid runtime JSON"))
}

#[cfg(test)]
mod tests;
