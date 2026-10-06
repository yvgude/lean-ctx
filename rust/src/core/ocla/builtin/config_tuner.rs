// SPDX-License-Identifier: Apache-2.0
//! BuiltinConfigTuner — proposes configuration adjustments.
//!
//! Wraps `core/config/mod.rs` tuning logic behind the OCLA trait. Generates
//! deterministic proposal refs and signals whether the change requires user
//! approval before application.

use crate::core::adaptive_mode_policy::AdaptiveModePolicyStore;
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

    pub fn tune(&self, request: ConfigTuningRequest) -> OclaResult<ConfigProposal> {
        let policy = AdaptiveModePolicyStore::load();
        self.tune_with_policy(request, &policy)
    }

    #[allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)]
    fn tune_with_policy(
        &self,
        request: ConfigTuningRequest,
        policy: &AdaptiveModePolicyStore,
    ) -> OclaResult<ConfigProposal> {
        let intent =
            (!request.objective_ref.trim().is_empty()).then_some(request.objective_ref.as_str());
        let tuned_mode = policy.choose_auto_mode(intent, &request.config_ref);
        let proposal_ref = format!(
            "proposal:{}->{}:{}",
            request.config_ref, tuned_mode, request.context.request_id
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
        let proposal = tuner
            .tune_with_policy(
                tuning_req("aggressive"),
                &AdaptiveModePolicyStore::default(),
            )
            .unwrap();
        assert!(proposal.requires_approval);
        assert!(proposal.proposal_ref.contains("aggressive->aggressive"));
    }

    #[test]
    fn auto_apply_mode() {
        let tuner = BuiltinConfigTuner::auto_apply();
        let proposal = tuner
            .tune_with_policy(tuning_req("mode"), &AdaptiveModePolicyStore::default())
            .unwrap();
        assert!(!proposal.requires_approval);
    }

    #[test]
    fn tune_uses_penalty_for_predicted_mode() {
        let tuner = BuiltinConfigTuner::new();
        let mut policy = AdaptiveModePolicyStore::default();
        policy.global.modes.insert(
            "aggressive".to_string(),
            crate::core::adaptive_mode_policy::ModePenalty {
                ema_badness: 1.0,
                samples: 1,
                last_ts: Some("t".to_string()),
            },
        );

        let proposal = tuner
            .tune_with_policy(tuning_req("aggressive"), &policy)
            .unwrap();

        assert!(!proposal.proposal_ref.contains("aggressive->aggressive"));
        assert!(proposal.proposal_ref.starts_with("proposal:aggressive->"));
    }

    #[test]
    fn tune_honors_intent_specific_policy() {
        let tuner = BuiltinConfigTuner::new();
        let mut policy = AdaptiveModePolicyStore::default();
        let intent = "minimize_tokens".to_string();
        policy
            .by_intent
            .entry(intent.clone())
            .or_default()
            .modes
            .insert(
                "aggressive".to_string(),
                crate::core::adaptive_mode_policy::ModePenalty {
                    ema_badness: 1.0,
                    samples: 1,
                    last_ts: Some("t".to_string()),
                },
            );

        let proposal = tuner
            .tune_with_policy(tuning_req("aggressive"), &policy)
            .unwrap();

        assert!(!proposal.proposal_ref.contains("aggressive->aggressive"));
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
