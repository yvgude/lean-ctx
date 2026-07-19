//! Built-in local AgentGateway adapter (P11).
//!
//! Validates relay envelopes, emits `AgentChainEvent` to the OclaBus, and
//! returns the admitted envelope. Does NOT resolve capsules, contact remote
//! agents, or grant commercial authority.

use crate::core::context_capsule::AgentEnvelopeV1;
use crate::core::ocla::agent_gateway::{AgentGateway, AgentGatewayError};
use crate::core::ocla_bus::OclaEvent;

pub struct BuiltinAgentGateway;

impl AgentGateway for BuiltinAgentGateway {
    fn relay_agent(&self, envelope: AgentEnvelopeV1) -> Result<AgentEnvelopeV1, AgentGatewayError> {
        envelope
            .validate()
            .map_err(|e| AgentGatewayError::InvalidEnvelope(e.to_string()))?;

        if envelope.from_agent_id == envelope.to_agent_id {
            return Err(AgentGatewayError::Rejected(
                "relay requires distinct source and target agents".into(),
            ));
        }

        crate::core::ocla_bus::emit(OclaEvent::AgentChainEvent {
            agent_id: envelope.from_agent_id.clone(),
            action: format!("relay→{}", envelope.to_agent_id),
            parent_agent: Some(envelope.from_agent_id.clone()),
        });

        Ok(envelope)
    }

    fn can_relay(&self, capsule_ref: &str, to_agent_id: &str) -> bool {
        capsule_ref.starts_with("capsule:")
            && !to_agent_id.is_empty()
            && to_agent_id.len() <= 256
            && to_agent_id.bytes().all(|b| b.is_ascii_graphic())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_capsule::{AGENT_ENVELOPE_SCHEMA_VERSION, AgentEnvelopeV1};

    fn valid_envelope() -> AgentEnvelopeV1 {
        let mut env = AgentEnvelopeV1 {
            schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
            relay_id: "agent-relay:pending".to_string(),
            from_agent_id: "owner-agent".to_string(),
            to_agent_id: "reviewer-agent".to_string(),
            capsule_ref: format!("capsule:{}", "a".repeat(64)),
            budget_tokens: 900,
            request_id: "request:1".to_string(),
            session_id: "session:1".to_string(),
        };
        env.assign_relay_id().unwrap();
        env
    }

    #[test]
    fn relays_valid_envelope() {
        let gw = BuiltinAgentGateway;
        let env = valid_envelope();
        let result = gw.relay_agent(env.clone()).unwrap();
        assert_eq!(result, env);
    }

    #[test]
    fn rejects_self_relay() {
        let gw = BuiltinAgentGateway;
        let mut env = valid_envelope();
        env.to_agent_id = env.from_agent_id.clone();
        env.assign_relay_id().unwrap();
        assert!(gw.relay_agent(env).is_err());
    }

    #[test]
    fn rejects_invalid_envelope() {
        let gw = BuiltinAgentGateway;
        let env = AgentEnvelopeV1 {
            schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
            relay_id: "agent-relay:pending".to_string(),
            from_agent_id: String::new(),
            to_agent_id: "target".to_string(),
            capsule_ref: "capsule:abc".to_string(),
            budget_tokens: 100,
            request_id: "request:1".to_string(),
            session_id: "session:1".to_string(),
        };
        assert!(gw.relay_agent(env).is_err());
    }

    #[test]
    fn can_relay_checks_format() {
        let gw = BuiltinAgentGateway;
        assert!(gw.can_relay("capsule:abc123", "agent-x"));
        assert!(!gw.can_relay("bundle:abc", "agent-x"));
        assert!(!gw.can_relay("capsule:abc", ""));
    }
}
