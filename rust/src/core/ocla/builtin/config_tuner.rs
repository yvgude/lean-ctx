//! BuiltinConfigTuner — proposes configuration adjustments.
//!
//! Wraps `core/config/mod.rs` tuning logic behind the OCLA trait. Generates
//! deterministic proposal refs and signals whether the change requires user
//! approval before application.

use crate::core::ocla::traits::{ConfigTuner, OclaService};
use crate::core::ocla::types::{
    ConfigProposal, ConfigTuningRequest, OclaCapability, OclaCapabilityKind, OclaResult,
};

pub struct BuiltinConfigTuner {
    require_approval: bool,
}

impl BuiltinConfigTuner {
    pub fn new() -> Self {
        Self {
            require_approval: true,
        }
    }

    pub fn auto_apply() -> Self {
        Self {
            require_approval: false,
        }
    }
}

impl Default for BuiltinConfigTuner {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinConfigTuner {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ConfigTuner)
    }
}

impl ConfigTuner for BuiltinConfigTuner {
    fn propose_tuning(&self, request: ConfigTuningRequest) -> OclaResult<ConfigProposal> {
        let proposal_ref = format!(
            "proposal:{}:{}",
            request.config_ref, request.context.request_id
        );
        let rollback_ref = format!("rollback:{}", request.config_ref);

        Ok(ConfigProposal {
            proposal_ref,
            rollback_ref,
            requires_approval: self.require_approval,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn tuning_req(config: &str) -> ConfigTuningRequest {
        ConfigTuningRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            config_ref: config.into(),
            objective_ref: "minimize_tokens".into(),
        }
    }

    #[test]
    fn default_requires_approval() {
        let tuner = BuiltinConfigTuner::new();
        let proposal = tuner
            .propose_tuning(tuning_req("compression.level"))
            .unwrap();
        assert!(proposal.requires_approval);
        assert!(proposal.proposal_ref.contains("compression.level"));
    }

    #[test]
    fn auto_apply_mode() {
        let tuner = BuiltinConfigTuner::auto_apply();
        let proposal = tuner.propose_tuning(tuning_req("mode")).unwrap();
        assert!(!proposal.requires_approval);
    }
}
