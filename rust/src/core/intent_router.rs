use serde::Serialize;

use crate::core::context_ledger::PressureAction;
use crate::core::intent_engine::{IntentDimension, TaskType, classify, route_intent};

/// Context strategy only (the read view `ctx_plan` uses). Model and provider
/// selection belong to the caller.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IntentRouteV2 {
    pub inputs: IntentRouteInputsV2,
    pub decision: IntentRouteDecisionV2,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IntentRouteInputsV2 {
    pub query_md5: String,
    pub query_redacted: String,
    pub role: String,
    pub profile: String,
    pub task_type: TaskType,
    pub confidence: f64,
    pub dimension: IntentDimension,
    pub pressure: PressureSummaryV1,
    pub policy_md5: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PressureSummaryV1 {
    pub utilization_pct: u8,
    pub remaining_tokens: usize,
    pub action: PressureActionV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PressureActionV1 {
    NoAction,
    SuggestCompression,
    ForceCompression,
    EvictLeastRelevant,
}

impl PressureActionV1 {
    fn from_action(a: PressureAction) -> Self {
        match a {
            PressureAction::NoAction => Self::NoAction,
            PressureAction::SuggestCompression => Self::SuggestCompression,
            PressureAction::ForceCompression => Self::ForceCompression,
            PressureAction::EvictLeastRelevant => Self::EvictLeastRelevant,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IntentRouteDecisionV2 {
    pub recommended_read_mode: String,
    pub effective_read_mode: String,
    pub degraded_by_pressure: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RouteInputs {
    pub pressure_action: PressureAction,
    pub pressure_utilization: f64,
    pub pressure_remaining_tokens: usize,
}

pub(crate) fn route_v2(query: &str) -> IntentRouteV2 {
    let ledger = crate::core::context_ledger::ContextLedger::load();
    let pressure = ledger.pressure();
    let profile_name = crate::core::profiles::active_profile_name();
    let profile = crate::core::profiles::active_profile();
    let role_name = crate::core::roles::active_role_name();

    let inputs = RouteInputs {
        pressure_action: pressure.recommendation,
        pressure_utilization: pressure.utilization,
        pressure_remaining_tokens: pressure.remaining_tokens,
    };

    route_v2_with(query, &role_name, &profile_name, &profile.routing, &inputs)
}

fn route_v2_with(
    query: &str,
    role_name: &str,
    profile_name: &str,
    routing: &crate::core::profiles::RoutingConfig,
    inputs: &RouteInputs,
) -> IntentRouteV2 {
    let classification = classify(query);
    let base = route_intent(query, &classification);
    let query_redacted = truncate(&crate::core::redaction::redact_text(query), 180);
    let query_md5 = crate::core::hasher::hash_str(query);
    let policy_md5 = crate::core::hasher::hash_str(&format!(
        "context_strategy_v2;degrade_under_pressure={}",
        routing.degrade_under_pressure_effective()
    ));

    let recommended_read_mode = read_mode_for_task(classification.task_type);
    let (effective_read_mode, degraded_by_pressure) = if routing.degrade_under_pressure_effective()
    {
        apply_pressure_degrade(&recommended_read_mode, inputs.pressure_action)
    } else {
        (recommended_read_mode.clone(), false)
    };
    let reason = format!(
        "task={} dim={} read={} pressure={:?} degraded_by_pressure={}",
        classification.task_type.as_str(),
        base.dimension.as_str(),
        effective_read_mode,
        inputs.pressure_action,
        degraded_by_pressure
    );

    IntentRouteV2 {
        inputs: IntentRouteInputsV2 {
            query_md5,
            query_redacted,
            role: role_name.to_string(),
            profile: profile_name.to_string(),
            task_type: classification.task_type,
            confidence: classification.confidence,
            dimension: base.dimension,
            pressure: PressureSummaryV1 {
                utilization_pct: (inputs.pressure_utilization * 100.0).min(254.0) as u8,
                remaining_tokens: inputs.pressure_remaining_tokens,
                action: PressureActionV1::from_action(inputs.pressure_action),
            },
            policy_md5,
        },
        decision: IntentRouteDecisionV2 {
            recommended_read_mode,
            effective_read_mode,
            degraded_by_pressure,
            reason,
        },
    }
}

pub(crate) fn read_mode_for_task(task_type: TaskType) -> String {
    // Editing and debugging require complete source, independently of the
    // caller's model. Exploration/review can use a structural or semantic view.
    match task_type {
        TaskType::Explore | TaskType::Review => "map".to_string(),
        _ => "full".to_string(),
    }
}

fn apply_pressure_degrade(mode: &str, pressure: PressureAction) -> (String, bool) {
    if let Some(downgraded) = crate::core::auto_mode_resolver::pressure_downgrade(mode, &pressure) {
        (downgraded, true)
    } else {
        (mode.to_string(), false)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_pressure() -> RouteInputs {
        RouteInputs {
            pressure_action: PressureAction::NoAction,
            pressure_utilization: 0.1,
            pressure_remaining_tokens: 1000,
        }
    }

    #[test]
    fn routing_is_deterministic_for_same_inputs() {
        let routing = crate::core::profiles::RoutingConfig::default();
        let inputs = no_pressure();
        let a = route_v2_with(
            "fix bug in src/lib.rs",
            "coder",
            "bugfix",
            &routing,
            &inputs,
        );
        let b = route_v2_with(
            "fix bug in src/lib.rs",
            "coder",
            "bugfix",
            &routing,
            &inputs,
        );
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn route_carries_no_model_or_budget_fields() {
        let inputs = no_pressure();
        let routing = crate::core::profiles::RoutingConfig::default();
        for query in [
            "explore the codebase",
            "fix bug in src/lib.rs",
            "implement feature x",
        ] {
            let route = route_v2_with(query, "coder", "same-profile", &routing, &inputs);
            let payload = serde_json::to_value(&route).unwrap();
            assert!(payload.get("created_at").is_none());
            assert!(payload["inputs"].get("budgets").is_none());
            for removed in [
                "recommended_model_tier",
                "effective_model_tier",
                "degraded_by_budget",
            ] {
                assert!(payload["decision"].get(removed).is_none());
            }
        }
        for task in [
            TaskType::Generate,
            TaskType::FixBug,
            TaskType::Refactor,
            TaskType::Debug,
        ] {
            assert_eq!(read_mode_for_task(task), "full");
        }
    }

    #[test]
    fn pressure_forces_degraded_mode() {
        let routing = crate::core::profiles::RoutingConfig::default();
        let inputs = RouteInputs {
            pressure_action: PressureAction::EvictLeastRelevant,
            pressure_utilization: 0.95,
            pressure_remaining_tokens: 100,
        };
        let route = route_v2_with(
            "review the auth module",
            "coder",
            "review",
            &routing,
            &inputs,
        );
        assert!(
            matches!(
                route.decision.effective_read_mode.as_str(),
                "signatures" | "cognitive" | "reference"
            ),
            "expected degraded mode, got: {}",
            route.decision.effective_read_mode
        );
        assert!(route.decision.degraded_by_pressure);
    }
}
