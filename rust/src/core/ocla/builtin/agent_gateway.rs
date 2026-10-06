// SPDX-License-Identifier: Apache-2.0
//! BuiltinAgentGateway — validates and relays agent-to-agent envelopes.
//!
//! Wraps `core/a2a/` behind the OCLA trait. Validates envelope fields and
//! returns the envelope with the relay_id confirmed. Budget enforcement is checked but not consumed
//! (consumption happens at the transport layer).

use crate::core::a2a::message::PrivacyLevel;
use crate::core::a2a::remote_transport::{RemoteTransport, RemoteTransportConfig};
use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};
use crate::core::agents::AgentRegistry;
use crate::core::ocla::capsule::global_capsule_store;
use crate::core::ocla::traits::{AgentGateway, OclaService};
use crate::core::ocla::types::{
    AgentEnvelope, AgentMessageRequest, OclaCapability, OclaCapabilityKind, OclaError, OclaResult,
};

pub struct BuiltinAgentGateway {
    remote: Option<RemoteTransport>,
}

impl BuiltinAgentGateway {
    pub fn new() -> Self {
        Self {
            remote: Self::load_remote_transport(),
        }
    }

    pub(crate) fn load_remote_transport() -> Option<RemoteTransport> {
        let config_path = crate::core::paths::config_dir()
            .ok()?
            .join("a2a-transport.toml");
        let raw = std::fs::read_to_string(config_path).ok()?;
        let config: RemoteTransportConfig = toml::from_str(&raw).ok()?;
        RemoteTransport::new(config).ok()
    }

    pub fn is_remote_available(&self) -> bool {
        self.remote.is_some()
    }

    fn is_local_target(agent_id: &str) -> bool {
        AgentRegistry::load().is_some_and(|registry| {
            registry
                .list_active(None)
                .iter()
                .any(|agent| agent.agent_id == agent_id)
        })
    }

    fn try_remote_relay(&self, envelope: &AgentEnvelope) -> OclaResult<AgentEnvelope> {
        let transport = self.remote.as_ref().ok_or_else(|| {
            OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                "no remote transport configured".into(),
            )
        })?;
        let payload_json = serde_json::to_string(envelope).map_err(|error| {
            OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                format!("remote relay serialization failed: {error}"),
            )
        })?;
        let transport_envelope = TransportEnvelopeV1::new(
            AgentIdentityV1::from_current(&envelope.from_agent_id, "ocla-agent-gateway"),
            Some(&envelope.to_agent_id),
            TransportContentType::A2AMessage,
            payload_json,
        );
        let transport = transport.clone();
        let delivery = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?
                .block_on(transport.deliver(&transport_envelope))
                .map_err(|error| error.to_string())
        })
        .join()
        .map_err(|_| {
            OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                "remote relay worker panicked".into(),
            )
        })?;

        match delivery {
            Ok(receipt) => {
                let mut confirmed = envelope.clone();
                confirmed.relay_id = receipt.envelope_id;
                Ok(confirmed)
            }
            Err(error) => Err(OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                format!("remote relay failed: {error}"),
            )),
        }
    }

    pub fn can_relay(&self, capsule_ref: &str, _to_agent_id: &str) -> bool {
        capsule_ref.is_empty() || global_capsule_store().resolve(capsule_ref).is_ok()
    }
    pub fn route_message(&self, request: &AgentMessageRequest) -> OclaResult<String> {
        Self::validate_message_request(request)?;
        AgentRegistry::mutate_locked(|registry| self.route_message_in_registry(registry, request))
            .and_then(|(_, message_id)| message_id)
            .map_err(|error| {
                OclaError::Rejected(
                    OclaCapabilityKind::AgentGateway,
                    format!("agent bus routing failed: {error}"),
                )
            })
    }

    fn validate_message_request(request: &AgentMessageRequest) -> OclaResult<()> {
        for (field, value) in [
            ("project_root", request.project_root.as_str()),
            ("from_agent_id", request.from_agent_id.as_str()),
            ("category", request.category.as_str()),
            ("message", request.message.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(OclaError::Rejected(
                    OclaCapabilityKind::AgentGateway,
                    format!("agent message {field} must not be empty"),
                ));
            }
        }
        if request.privacy == PrivacyLevel::Private && request.to_agent_id.is_none() {
            return Err(OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                "private agent messages require a target".to_string(),
            ));
        }
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn route_message_in_registry(
        &self, // used for trait impl method grouping
        registry: &mut AgentRegistry,
        request: &AgentMessageRequest,
    ) -> Result<String, String> {
        registry.post_message_scoped(
            Some(&request.project_root),
            &request.from_agent_id,
            request.to_agent_id.as_deref(),
            &request.category,
            &request.message,
            request.privacy.clone(),
            request.priority.clone(),
            request.ttl_hours,
        )
    }
}

impl Default for BuiltinAgentGateway {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinAgentGateway {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        let mut manifest =
            crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())?;
        if self.is_remote_available() {
            manifest.remote = true;
            manifest.data_movement = lean_ctx_protocol::DataMovement::Remote;
        }
        Ok(manifest)
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::AgentGateway)
    }
}

impl AgentGateway for BuiltinAgentGateway {
    fn relay_agent(&self, envelope: AgentEnvelope) -> OclaResult<AgentEnvelope> {
        let mut envelope = envelope;
        if envelope.budget_tokens == 0 {
            return Err(OclaError::Rejected(
                OclaCapabilityKind::AgentGateway,
                "zero budget".into(),
            ));
        }
        if !envelope.capsule_ref.is_empty() {
            let parent_ref = envelope.capsule_ref.clone();
            envelope.capsule_ref = global_capsule_store()
                .fork(&parent_ref, envelope.budget_tokens)
                .map_err(|error| {
                    tracing::debug!(error = %error, "capsule fork failed for relay");
                    OclaError::Rejected(
                        OclaCapabilityKind::AgentGateway,
                        format!("capsule fork failed: {error}"),
                    )
                })?;
            tracing::debug!("capsule forked for relay");
        }

        if !Self::is_local_target(&envelope.to_agent_id) && self.is_remote_available() {
            // Failures land in the dead-letter queue inside try_remote_relay.
            return self.try_remote_relay(&envelope);
        }

        Ok(envelope)
    }

    fn route_message(&self, request: &AgentMessageRequest) -> OclaResult<String> {
        BuiltinAgentGateway::route_message(self, request)
    }
}

#[cfg(test)]
mod tests {
    use super::BuiltinAgentGateway;
    use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
    use crate::core::a2a::remote_transport::{RemoteTransport, RemoteTransportConfig};
    use crate::core::agents::AgentRegistry;
    use crate::core::ocla::capsule::global_capsule_store;
    use crate::core::ocla::traits::AgentGateway;
    use crate::core::ocla::types::{AgentEnvelope, AgentMessageRequest, OclaRequestContext};

    fn message_request(
        to_agent_id: Option<&str>,
        category: &str,
        message: &str,
    ) -> AgentMessageRequest {
        AgentMessageRequest {
            project_root: "/project".to_string(),
            from_agent_id: "agent-a".to_string(),
            to_agent_id: to_agent_id.map(str::to_string),
            category: category.to_string(),
            message: message.to_string(),
            privacy: PrivacyLevel::Private,
            priority: MessagePriority::High,
            ttl_hours: Some(2),
        }
    }

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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            from_agent_id: "agent-a".into(),
            to_agent_id: "agent-b".into(),
            capsule_ref: String::new(),
            budget_tokens: budget,
        }
    }

    #[test]
    fn relay_without_capsule_passes_through() {
        let gateway = BuiltinAgentGateway::new();
        let result = gateway.relay_agent(envelope(1000)).unwrap();
        assert_eq!(result.from_agent_id, "agent-a");
        assert!(result.capsule_ref.is_empty());
    }

    #[test]
    fn new_without_transport_config_keeps_local_relay_available() {
        let _lock = crate::core::data_dir::test_env_lock();
        let config_dir = tempfile::tempdir().expect("config directory");
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", config_dir.path());

        let gateway = BuiltinAgentGateway::new();
        let result = gateway.relay_agent(envelope(1000));

        crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
        assert!(!gateway.is_remote_available());
        assert!(result.is_ok());
    }

    #[test]
    fn remote_availability_is_false_without_transport_config() {
        let _lock = crate::core::data_dir::test_env_lock();
        let config_dir = tempfile::tempdir().expect("config directory");
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", config_dir.path());

        let gateway = BuiltinAgentGateway::new();

        crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
        assert!(!gateway.is_remote_available());
    }

    #[test]
    fn unregistered_target_attempts_configured_remote_relay() {
        let remote = RemoteTransport::new(RemoteTransportConfig {
            endpoint_url: "http://127.0.0.1:9".to_string(),
            auth_token: Some("bearer-secret".to_string()),
            signing_key: Some("signing-secret".to_string()),
            recipient_id: Some("remote-agent".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            project_id: Some("project-a".to_string()),
            retry_count: 0,
            ..RemoteTransportConfig::default()
        })
        .expect("valid remote transport");
        let gateway = BuiltinAgentGateway {
            remote: Some(remote),
        };
        let mut input = envelope(1000);
        input.to_agent_id = "remote-agent".to_string();

        let error = gateway
            .relay_agent(input)
            .expect_err("unreachable remote must fail delivery");

        assert!(error.to_string().contains("remote relay failed"));
    }

    #[test]
    fn relay_with_capsule_forks() {
        let gateway = BuiltinAgentGateway::new();
        let parent_ref = global_capsule_store().register(b"relay capsule");
        let mut input = envelope(1000);
        input.capsule_ref = parent_ref.clone();

        let result = gateway.relay_agent(input).expect("relay succeeds");

        assert_ne!(result.capsule_ref, parent_ref);
        assert_eq!(
            global_capsule_store()
                .resolve(&result.capsule_ref)
                .expect("child resolves"),
            b"relay capsule"
        );
    }

    #[test]
    fn can_relay_false_for_unknown_ref() {
        let gateway = BuiltinAgentGateway::new();

        assert!(gateway.can_relay("", "agent-b"));
        assert!(!gateway.can_relay("capsule:unknown-ref", "agent-b"));
    }

    #[test]
    fn relay_deducts_budget_tokens() {
        let gateway = BuiltinAgentGateway::new();
        let parent_ref = global_capsule_store().register(b"budget capsule");
        let mut input = envelope(321);
        input.capsule_ref = parent_ref;

        let result = gateway.relay_agent(input).expect("relay succeeds");

        assert_eq!(
            global_capsule_store()
                .budget_tokens(&result.capsule_ref)
                .expect("child budget exists"),
            321
        );
    }

    #[test]
    fn relay_rejects_zero_budget() {
        let gateway = BuiltinAgentGateway::new();
        assert!(gateway.relay_agent(envelope(0)).is_err());
    }

    #[test]
    fn route_message_writes_to_agent_bus() {
        let gateway = BuiltinAgentGateway::new();
        let mut registry = AgentRegistry::new();
        let message_id = gateway
            .route_message_in_registry(
                &mut registry,
                &message_request(Some("agent-b"), "request", "Please review"),
            )
            .expect("message route");

        let messages = registry
            .read_unread_scoped("agent-b", "/project")
            .expect("message read");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message_id);
        assert_eq!(messages[0].project_root.as_deref(), Some("/project"));
        assert_eq!(messages[0].message, "Please review");
        assert_eq!(messages[0].privacy, PrivacyLevel::Private);
        assert_eq!(messages[0].priority, MessagePriority::High);
    }

    #[test]
    fn route_message_supports_broadcast() {
        let gateway = BuiltinAgentGateway::new();
        let mut registry = AgentRegistry::new();
        let mut request = message_request(None, "status", "Ready");
        request.privacy = PrivacyLevel::Team;
        request.priority = MessagePriority::Normal;
        request.ttl_hours = None;
        gateway
            .route_message_in_registry(&mut registry, &request)
            .expect("message route");

        assert_eq!(
            registry.read_unread("agent-b").expect("message read").len(),
            1
        );
    }

    #[test]
    fn route_message_rejects_unscoped_or_private_broadcasts() {
        let gateway = BuiltinAgentGateway::new();

        let mut unscoped = message_request(Some("agent-b"), "request", "Review this");
        unscoped.project_root.clear();
        assert!(gateway.route_message(&unscoped).is_err());

        let private_broadcast = message_request(None, "request", "Review this");
        assert!(gateway.route_message(&private_broadcast).is_err());
    }

    #[test]
    fn registry_routes_message_through_agent_gateway() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let registry = crate::core::ocla::registry::OclaRegistry::with_builtins();
        let message_id = registry
            .agent_gateway
            .route_message(&message_request(
                Some("agent-b"),
                "request",
                "Please review",
            ))
            .unwrap();

        assert!(!message_id.is_empty());
    }
}
