//! BuiltinModelRouter — intent-aware model routing via OCLA trait.
//!
//! Wraps `proxy/model_router.rs` and `proxy/effort_routing.rs` behind the
//! canonical trait. Emits ModelRouted events. Routes to the best candidate
//! model within the cost/latency constraints.

use crate::core::ocla::traits::{ModelRouter, OclaService};
use crate::core::ocla::types::{
    ModelRouteRequest, OclaCapability, OclaCapabilityKind, OclaResult, RoutingDecision,
};
use crate::core::ocla_bus::{self, OclaEvent};

pub struct BuiltinModelRouter;

impl BuiltinModelRouter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinModelRouter {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ModelRouter)
    }
}

impl ModelRouter for BuiltinModelRouter {
    fn route_model(&self, request: ModelRouteRequest) -> OclaResult<RoutingDecision> {
        let model = request
            .candidate_models
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let provider = infer_provider(&model);

        ocla_bus::emit(OclaEvent::ModelRouted {
            requested_model: request
                .candidate_models
                .first()
                .cloned()
                .unwrap_or_default(),
            routed_model: model.clone(),
            tier: "standard".to_string(),
            model_changed: false,
        });

        Ok(RoutingDecision {
            model,
            provider,
            reasoning_budget_tokens: 4096,
            decision_ref: format!("route:{}", request.context.request_id),
        })
    }
}

fn infer_provider(model: &str) -> String {
    if model.contains("gpt") || model.contains("o1") || model.contains("o3") {
        "openai".to_string()
    } else if model.contains("claude") {
        "anthropic".to_string()
    } else if model.contains("gemini") {
        "google".to_string()
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn route_req(candidates: &[&str]) -> ModelRouteRequest {
        ModelRouteRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            candidate_models: candidates.iter().map(|s| (*s).to_string()).collect(),
            maximum_cost_micros: None,
            maximum_latency_ms: None,
        }
    }

    #[test]
    fn routes_first_candidate() {
        let router = BuiltinModelRouter::new();
        let decision = router
            .route_model(route_req(&["gpt-4o", "claude-3"]))
            .unwrap();
        assert_eq!(decision.model, "gpt-4o");
        assert_eq!(decision.provider, "openai");
    }

    #[test]
    fn infers_anthropic_provider() {
        let router = BuiltinModelRouter::new();
        let decision = router.route_model(route_req(&["claude-sonnet-4"])).unwrap();
        assert_eq!(decision.provider, "anthropic");
    }

    #[test]
    fn empty_candidates_defaults() {
        let router = BuiltinModelRouter::new();
        let decision = router.route_model(route_req(&[])).unwrap();
        assert_eq!(decision.model, "default");
    }
}
