// SPDX-License-Identifier: Apache-2.0
//! Bounded replay state for one pre-admitted, capability-scoped local session.
//! The host supplies trusted scope and a fresh session/key, never scope learned
//! from an incoming request. This mechanism does not decide policy or trust peers.

use std::fmt;

use crate::{
    EngineInvocationV1, EngineOperationV1, EnginePolicyDecisionV1, ProtocolReference,
    ResolvedLocalEngineIdentityV1,
    runtime_exchange::RuntimeRequestV1,
    runtime_frame::{RuntimeFrameDirection, RuntimeFrameError, decode_runtime_frame},
    validate_bounded_opaque_identifier,
};

/// No Clone, Debug or serialization: neither the borrowed key nor session state
/// may be copied into another receiver to reset its replay counter.
pub struct RuntimeRequestSession<'key> {
    key: &'key [u8; 32],
    session_id: String,
    engine: ResolvedLocalEngineIdentityV1,
    operation: EngineOperationV1,
    policy_ref: ProtocolReference,
    next_sequence: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSessionError {
    InvalidScope,
    Frame(RuntimeFrameError),
    InvalidRequest,
    ScopeMismatch,
    SequenceMismatch,
    SequenceExhausted,
}

impl fmt::Display for RuntimeSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidScope => "invalid runtime session scope",
            Self::Frame(_) => "invalid authenticated runtime frame",
            Self::InvalidRequest => "invalid runtime request",
            Self::ScopeMismatch => "runtime request outside session scope",
            Self::SequenceMismatch => "runtime request sequence mismatch",
            Self::SequenceExhausted => "runtime session sequence exhausted",
        })
    }
}

impl std::error::Error for RuntimeSessionError {}

impl<'key> RuntimeRequestSession<'key> {
    /// Bind locally trusted admission, not a peer-supplied assertion of admission.
    /// The owner provisions and protects the key, serializes access to this
    /// receiver, and retires it on reconnect/revocation. Never reuse a session key.
    pub fn new(
        key: &'key [u8; 32],
        session_id: String,
        admitted: &EngineInvocationV1,
    ) -> Result<Self, RuntimeSessionError> {
        validate_bounded_opaque_identifier(&session_id, "runtime session_id")
            .and_then(|()| admitted.validate())
            .map_err(|_| RuntimeSessionError::InvalidScope)?;
        if admitted.policy_admission.decision != EnginePolicyDecisionV1::Admitted {
            return Err(RuntimeSessionError::InvalidScope);
        }
        Ok(Self {
            key,
            session_id,
            engine: admitted.engine.clone(),
            operation: admitted.operation.clone(),
            policy_ref: admitted.policy_admission.policy_ref.clone(),
            next_sequence: Some(1),
        })
    }

    /// Authenticate before JSON parsing; admit exact scope and consecutive sequence.
    /// Invalid input never advances state. A successful request is consumed even
    /// if later execution fails, so it cannot be replayed to repeat side effects.
    /// Callers still enforce a monotonic I/O deadline and live policy revocation.
    pub fn accept(
        &mut self,
        frame: &[u8],
        now_unix_ms: u64,
    ) -> Result<RuntimeRequestV1, RuntimeSessionError> {
        let sequence = self
            .next_sequence
            .ok_or(RuntimeSessionError::SequenceExhausted)?;
        let payload = decode_runtime_frame(self.key, RuntimeFrameDirection::Request, frame)
            .map_err(RuntimeSessionError::Frame)?;
        let request = RuntimeRequestV1::from_bytes(&payload, now_unix_ms)
            .map_err(|_| RuntimeSessionError::InvalidRequest)?;
        if request.session_id != self.session_id
            || request.invocation.engine != self.engine
            || request.invocation.operation != self.operation
            || request.invocation.policy_admission.policy_ref != self.policy_ref
        {
            return Err(RuntimeSessionError::ScopeMismatch);
        }
        if request.sequence != sequence {
            return Err(RuntimeSessionError::SequenceMismatch);
        }
        self.next_sequence = sequence.checked_add(1);
        Ok(request)
    }
}

#[cfg(test)]
mod tests;
