//! Static eligibility evaluation for the public proxy boundary.
//!
//! This evaluator checks only the supplied policy and observations. It does
//! not calibrate confidence, learn weights, rank candidates, or persist task
//! outcomes; production decision intelligence is Class D in
//! `lean-ctx-enterprise`.

use lean_ctx_protocol::RiskClass;
pub use lean_ctx_protocol::eligibility::{
    EligibilityContext, EligibilityInput, EligibilityPolicy, EligibilityResult,
};

/// Evaluate static eligibility conditions in stable policy-field order.
pub fn evaluate(policy: &EligibilityPolicy, context: &EligibilityContext) -> EligibilityResult {
    let mut reasons = Vec::new();

    if policy.validate().is_err() {
        reasons.push("invalid_policy".to_owned());
    }
    if context.validate().is_err() {
        reasons.push("invalid_context".to_owned());
    }

    if context.evaluated_tasks < policy.min_evaluated_tasks {
        reasons.push("insufficient_evaluated_tasks".to_owned());
    }
    if context.confidence < policy.min_confidence {
        reasons.push("confidence_below_floor".to_owned());
    }
    if risk_rank(context.risk_class) > risk_rank(policy.max_risk_class) {
        reasons.push("risk_class_exceeds_maximum".to_owned());
    }
    if !policy.allowed_regions.is_empty()
        && !policy
            .allowed_regions
            .iter()
            .any(|region| region == &context.region)
    {
        reasons.push("region_not_allowed".to_owned());
    }
    if !policy.allowed_providers.is_empty()
        && !policy
            .allowed_providers
            .iter()
            .any(|provider| provider == &context.provider)
    {
        reasons.push("provider_not_allowed".to_owned());
    }
    if policy
        .required_task_class
        .as_ref()
        .is_some_and(|required| required != &context.task_class)
    {
        reasons.push("task_class_mismatch".to_owned());
    }
    if context.quality < policy.quality_floor {
        reasons.push("quality_below_floor".to_owned());
    }

    EligibilityResult {
        eligible: reasons.is_empty(),
        reasons,
        required_sample_size: policy.min_evaluated_tasks,
    }
}

/// Explicitly named alias for callers that prefer the domain term.
pub fn evaluate_eligibility(
    policy: &EligibilityPolicy,
    context: &EligibilityContext,
) -> EligibilityResult {
    evaluate(policy, context)
}

/// Extension trait providing method syntax without adding intelligence to the
/// protocol schema type.
pub trait EligibilityPolicyEvaluator {
    fn evaluate(&self, context: &EligibilityContext) -> EligibilityResult;
}

impl EligibilityPolicyEvaluator for EligibilityPolicy {
    fn evaluate(&self, context: &EligibilityContext) -> EligibilityResult {
        evaluate(self, context)
    }
}

fn risk_rank(risk_class: RiskClass) -> u8 {
    match risk_class {
        RiskClass::Low => 0,
        RiskClass::Medium => 1,
        RiskClass::High => 2,
        RiskClass::Critical => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> EligibilityPolicy {
        EligibilityPolicy {
            min_evaluated_tasks: 200,
            min_confidence: 0.8,
            max_risk_class: RiskClass::Medium,
            allowed_regions: vec!["eu".to_owned()],
            allowed_providers: vec!["provider-a".to_owned()],
            required_task_class: Some("coding".to_owned()),
            quality_floor: 0.99,
            scheduler_gate: None,
        }
    }

    fn context() -> EligibilityContext {
        EligibilityContext {
            evaluated_tasks: 250,
            confidence: 0.9,
            risk_class: RiskClass::Low,
            region: "eu".to_owned(),
            provider: "provider-a".to_owned(),
            task_class: "coding".to_owned(),
            quality: 1.0,
        }
    }

    #[test]
    fn evaluator_accepts_static_policy_match() {
        let result = evaluate(&policy(), &context());
        assert!(result.eligible);
        assert!(result.reasons.is_empty(), "{:?}", result.reasons);
        assert_eq!(result.required_sample_size, 200);
        assert!(EligibilityPolicyEvaluator::evaluate(&policy(), &context()).eligible);
    }

    #[test]
    fn evaluator_reports_all_static_failures_in_order() {
        let mut input = context();
        input.evaluated_tasks = 1;
        input.confidence = 0.1;
        input.risk_class = RiskClass::Critical;
        input.region = "us".to_owned();
        input.provider = "provider-b".to_owned();
        input.task_class = "review".to_owned();
        input.quality = 0.5;

        let result = evaluate(&policy(), &input);
        assert!(!result.eligible);
        assert_eq!(
            result.reasons,
            vec![
                "insufficient_evaluated_tasks",
                "confidence_below_floor",
                "risk_class_exceeds_maximum",
                "region_not_allowed",
                "provider_not_allowed",
                "task_class_mismatch",
                "quality_below_floor",
            ]
        );
    }
}
