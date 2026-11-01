// SPDX-License-Identifier: Apache-2.0
//! BuiltinConfigTuner — proposes configuration adjustments.
//!
//! The built-in tuner is the identity: it proposes the requested config ref
//! unchanged (the learned mode-penalty policy it once consulted is retired).
//! It generates deterministic proposal refs and signals whether the change
//! requires user approval before application; an external tuner can replace it
//! through the OCLA registry.

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

    #[allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)]
    pub fn tune(&self, request: ConfigTuningRequest) -> OclaResult<ConfigProposal> {
        let proposal_ref = format!(
            "proposal:{}->{}:{}",
            request.config_ref, request.config_ref, request.context.request_id
        );
        let rollback_ref = format!("rollback:{}", request.config_ref);

        Ok(ConfigProposal {
            proposal_ref,
            rollback_ref,
            requires_approval: self.require_approval,
        })
    }
}

impl Default for BuiltinConfigTuner {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinConfigTuner {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ConfigTuner)
    }
}

impl ConfigTuner for BuiltinConfigTuner {
    fn propose_tuning(&self, request: ConfigTuningRequest) -> OclaResult<ConfigProposal> {
        self.tune(request)
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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            config_ref: config.into(),
            objective_ref: "minimize_tokens".into(),
        }
    }

    #[test]
    fn default_requires_approval() {
        let tuner = BuiltinConfigTuner::new();
        let proposal = tuner.tune(tuning_req("aggressive")).unwrap();
        assert!(proposal.requires_approval);
        assert_eq!(proposal.proposal_ref, "proposal:aggressive->aggressive:r1");
        assert_eq!(proposal.rollback_ref, "rollback:aggressive");
    }

    #[test]
    fn auto_apply_mode() {
        let tuner = BuiltinConfigTuner::auto_apply();
        let proposal = tuner.tune(tuning_req("mode")).unwrap();
        assert!(!proposal.requires_approval);
    }

    #[test]
    fn registry_with_builtins_proposes_the_same_tuning() {
        use crate::core::ocla::registry::OclaRegistry;

        let direct = BuiltinConfigTuner::new()
            .tune(tuning_req("aggressive"))
            .unwrap();
        let via_registry = OclaRegistry::with_builtins()
            .config_tuner
            .propose_tuning(tuning_req("aggressive"))
            .unwrap();

        assert_eq!(via_registry, direct);
    }
}
