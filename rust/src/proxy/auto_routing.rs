//! Public `model=auto` proxy surface.
//!
//! The wire contracts live in `lean_ctx_protocol` so the private
//! `lean-ctx-enterprise` control plane can interoperate without depending on
//! proxy internals. The OSS proxy intentionally contains no model/provider
//! selection intelligence; production decision intelligence is Class D and
//! remains in `lean-ctx-enterprise`.

pub use lean_ctx_protocol::auto_routing::{
    AutoRoutingConfig, AutoRoutingEvidence, AutoRoutingRequest, RoutingDecision, RoutingMode,
    RoutingReceipt, SchedulerGate,
};
use lean_ctx_protocol::eligibility::EligibilityResult;

/// Check whether shadow-scheduler evidence is strong enough for `model=auto`.
#[must_use]
pub fn check_auto_eligibility(
    gate: &SchedulerGate,
    evidence: &AutoRoutingEvidence,
) -> EligibilityResult {
    let mut reasons = Vec::new();

    if evidence.evaluated_tasks < gate.min_evaluated_tasks {
        reasons.push("insufficient_evaluated_tasks".to_owned());
    }
    if !evidence.outcome_degradation_pct.is_finite()
        || evidence.outcome_degradation_pct > gate.max_outcome_degradation_pct
    {
        reasons.push("outcome_degradation_exceeds_maximum".to_owned());
    }
    if !evidence.cost_improvement_pct.is_finite()
        || evidence.cost_improvement_pct < gate.min_cost_improvement_pct
    {
        reasons.push("cost_improvement_below_minimum".to_owned());
    }
    if gate.require_holdout_evidence && !evidence.has_holdout_evidence {
        reasons.push("holdout_evidence_required".to_owned());
    }

    EligibilityResult {
        eligible: reasons.is_empty(),
        reasons,
        required_sample_size: gate.min_evaluated_tasks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_reexports_protocol_wire_types() {
        let request = AutoRoutingRequest {
            task_id: "task-1".to_owned(),
            requested_model: Some("model".to_owned()),
            routing_mode: RoutingMode::Shadow,
        };
        let json = serde_json::to_string(&request).expect("request serializes");
        let decoded: AutoRoutingRequest = serde_json::from_str(&json).expect("request parses");
        assert_eq!(decoded, request);
    }

    fn gate() -> SchedulerGate {
        SchedulerGate {
            min_evaluated_tasks: 100,
            max_outcome_degradation_pct: 2.0,
            min_cost_improvement_pct: 10.0,
            require_holdout_evidence: true,
        }
    }

    fn evidence() -> AutoRoutingEvidence {
        AutoRoutingEvidence {
            evaluated_tasks: 120,
            outcome_degradation_pct: 1.0,
            cost_improvement_pct: 15.0,
            has_holdout_evidence: true,
        }
    }

    #[test]
    fn gate_accepts_enough_evidence_within_tolerance() {
        let result = check_auto_eligibility(&gate(), &evidence());

        assert!(result.eligible);
        assert!(result.reasons.is_empty(), "{:?}", result.reasons);
        assert_eq!(result.required_sample_size, 100);
    }

    #[test]
    fn gate_rejects_outcome_degradation_above_tolerance() {
        let mut observations = evidence();
        observations.outcome_degradation_pct = 2.1;

        let result = check_auto_eligibility(&gate(), &observations);

        assert!(!result.eligible);
        assert_eq!(result.reasons, vec!["outcome_degradation_exceeds_maximum"]);
    }
}
