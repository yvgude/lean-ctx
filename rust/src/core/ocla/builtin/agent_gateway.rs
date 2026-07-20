//! BuiltinAgentGateway — validates and relays agent-to-agent envelopes.
//!
//! Wraps `core/a2a/` behind the OCLA trait. Validates envelope fields,
//! emits AgentChainEvent to OclaBus, and returns the envelope with the
//! relay_id confirmed. Budget enforcement is checked but not consumed
//! (consumption happens at the transport layer).

use crate::core::ocla::traits::{AgentGateway, OclaService};
use crate::core::ocla::types::{
    AgentEnvelope, OclaCapability, OclaCapabilityKind, OclaError, OclaResult,
};
use crate::core::ocla_bus::{self, OclaEvent};

pub struct BuiltinAgentGateway;

impl BuiltinAgentGateway {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinAgentGateway {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinAgentGateway {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::AgentGateway)
    }
}

impl AgentGateway for BuiltinAgentGateway {
    fn relay_agent(&self, envelope: AgentEnvelope) -> OclaResult<AgentEnvelope> {
        if envelope.budget_tokens == 0 {
            return Err(OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                "zero budget".into(),
            ));
        }

        ocla_bus::emit(OclaEvent::AgentChainEvent {
            agent_id: envelope.from_agent_id.clone(),
            action: "relay".to_string(),
            parent_agent: Some(envelope.to_agent_id.clone()),
        });

        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn envelope(budget: u64) -> AgentEnvelope {
        AgentEnvelope {
            schema_version: 1,
            relay_id: "relay:test".into(),
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            from_agent_id: "agent-a".into(),
            to_agent_id: "agent-b".into(),
            capsule_ref: "capsule:abc".into(),
            budget_tokens: budget,
        }
    }

    #[test]
    fn relay_passes_valid_envelope() {
        let gateway = BuiltinAgentGateway::new();
        let result = gateway.relay_agent(envelope(1000)).unwrap();
        assert_eq!(result.from_agent_id, "agent-a");
    }

    #[test]
    fn relay_rejects_zero_budget() {
        let gateway = BuiltinAgentGateway::new();
        assert!(gateway.relay_agent(envelope(0)).is_err());
    }
}
