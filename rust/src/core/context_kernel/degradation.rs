// SPDX-License-Identifier: Apache-2.0

//! Kernel degradation levels and fallback planning.

use super::orchestrator::{ContextKernel, KernelPlanError};
use super::types::{ContextPlanV1, RetrievalContext};
use crate::core::context_field::TokenBudget;
use std::collections::BTreeMap;

/// Operational capability remaining after provider failures.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum DegradationLevel {
    /// All configured providers are available.
    #[default]
    Full,
    /// At least half of the configured providers are available.
    Reduced,
    /// At least one provider is available.
    Minimal,
    /// No providers are available, so the kernel must be bypassed.
    Bypass,
}

/// Current availability information for one provider.
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub provider_id: String,
    pub available: bool,
    pub last_error: Option<String>,
}

/// Aggregate provider health used to select a degradation level.
#[derive(Debug, Clone)]
pub struct KernelHealth {
    providers: BTreeMap<String, bool>,
}

impl KernelHealth {
    /// Creates one canonical availability value per configured provider.
    /// Conflicting duplicate reports and empty identifiers are unavailable.
    pub fn new(providers: Vec<ProviderStatus>) -> Self {
        let mut availability = BTreeMap::new();
        for provider in providers {
            let available = provider.available && !provider.provider_id.trim().is_empty();
            availability
                .entry(provider.provider_id)
                .and_modify(|current| *current &= available)
                .or_insert(available);
        }
        Self {
            providers: availability,
        }
    }

    /// Returns the capability level supported by the available providers.
    pub fn degradation_level(&self) -> DegradationLevel {
        let total = self.providers.len();
        let available = self
            .providers
            .values()
            .filter(|available| **available)
            .count();

        if total == 0 {
            return DegradationLevel::Bypass;
        }
        match available {
            n if n == total => DegradationLevel::Full,
            n if n * 2 >= total => DegradationLevel::Reduced,
            n if n >= 1 => DegradationLevel::Minimal,
            _ => DegradationLevel::Bypass,
        }
    }

    /// Lists provider identifiers currently available for planning.
    pub fn available_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|(_, available)| **available)
            .map(|(provider_id, _)| provider_id.as_str())
            .collect()
    }

    /// Lists provider identifiers currently unavailable for planning.
    pub fn unavailable_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|(_, available)| !**available)
            .map(|(provider_id, _)| provider_id.as_str())
            .collect()
    }

    /// Summarizes provider availability and the resulting capability level.
    pub fn summary(&self) -> String {
        format!(
            "{}/{} providers available ({:?})",
            self.available_providers().len(),
            self.providers.len(),
            self.degradation_level()
        )
    }

    fn provider_is_available(&self, provider_id: &str) -> bool {
        self.providers.get(provider_id).copied().unwrap_or(false)
    }
}

/// Removes selections whose providers are unavailable and updates accounting.
/// Missing origin or inconsistent accounting is an error, never an empty success.
pub fn degrade_plan(
    plan: &ContextPlanV1,
    health: &KernelHealth,
) -> Result<ContextPlanV1, KernelPlanError> {
    ContextKernel::restrict_plan(plan, |provider_id| {
        health.provider_is_available(provider_id)
    })
}

/// Creates an empty pass-through plan for bypass mode.
/// This legacy entry point starts a new unscoped request; it does not recover a failed plan.
pub fn fallback_plan(intent: &str, budget_tokens: usize) -> Result<ContextPlanV1, KernelPlanError> {
    ContextKernel::new(Vec::new()).plan(&RetrievalContext {
        query: intent.to_owned(),
        task: None,
        project_root: String::new(),
        budget: TokenBudget {
            total: budget_tokens,
            used: 0,
        },
        max_candidates: 0,
    })
}

#[cfg(test)]
pub mod tests {
    use super::super::types::{ContextOriginV1, PlanBudget, PlanEntry, SensitivityLevel};
    use super::*;
    use std::collections::HashMap;

    fn status(provider_id: &str, available: bool) -> ProviderStatus {
        ProviderStatus {
            provider_id: provider_id.to_owned(),
            available,
            last_error: (!available).then(|| "provider failed".to_owned()),
        }
    }

    fn entry(object_id: &str, provider: &str, tokens: usize) -> PlanEntry {
        PlanEntry {
            object_id: object_id.to_owned(),
            provider: provider.to_owned(),
            view: "full".to_owned(),
            tokens,
            phi: 1.0,
            reason: "selected".to_owned(),
        }
    }

    fn bind_fixture(plan: &mut ContextPlanV1) {
        for entry in &plan.selected {
            let stat = plan
                .provider_stats
                .entry(entry.provider.clone())
                .or_default();
            stat.candidates_offered += 1;
            stat.candidates_selected += 1;
            stat.tokens_used += entry.tokens;
            plan.origins.insert(
                entry.object_id.clone(),
                vec![ContextOriginV1 {
                    provider_id: entry.provider.clone(),
                    source: entry.provider.clone(),
                    content_ref: entry.object_id.clone(),
                    sensitivity: SensitivityLevel::Public,
                    provenance: Default::default(),
                    admitted: true,
                }],
            );
        }
    }

    fn consumed_plan() -> ContextPlanV1 {
        let mut plan = ContextPlanV1::empty(
            "retain context",
            TokenBudget {
                total: 100,
                used: 90,
            },
        );
        plan.selected = vec![entry("a", "files", 40), entry("b", "facts", 30)];
        bind_fixture(&mut plan);
        plan
    }

    #[test]
    fn restriction_preserves_consumption_offers_and_exact_origins() {
        let mut plan = consumed_plan();
        plan.provider_stats
            .get_mut("files")
            .unwrap()
            .candidates_offered = 3;
        plan.provider_stats
            .get_mut("facts")
            .unwrap()
            .candidates_offered = 2;
        let original = serde_json::to_value(&plan).unwrap();
        let result = degrade_plan(&plan, &KernelHealth::new(vec![status("files", true)])).unwrap();
        assert_eq!(
            (result.budget.used_tokens, result.budget.remaining_tokens),
            (60, 40)
        );
        assert_eq!(result.provider_stats["files"].candidates_offered, 3);
        assert_eq!(result.provider_stats["files"].candidates_selected, 1);
        assert_eq!(result.provider_stats["files"].tokens_used, 40);
        assert_eq!(result.provider_stats["facts"].candidates_offered, 2);
        assert_eq!(result.provider_stats["facts"].candidates_selected, 0);
        assert_eq!(result.provider_stats["facts"].tokens_used, 0);
        assert_eq!(
            serde_json::to_value(&result.origins).unwrap(),
            original["origins"]
        );
        assert_eq!(serde_json::to_value(&plan).unwrap(), original);
    }

    #[test]
    fn degradation_uses_registered_identity_not_source_alias() {
        let mut plan = consumed_plan();
        plan.origins.get_mut("a").unwrap()[0].provider_id = "registered-files".to_owned();
        let alias_only = KernelHealth::new(vec![status("files", true), status("facts", true)]);
        let result = degrade_plan(&plan, &alias_only).unwrap();
        assert_eq!(
            result
                .selected
                .iter()
                .map(|entry| entry.object_id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        let registered = KernelHealth::new(vec![
            status("registered-files", true),
            status("facts", true),
        ]);
        assert_eq!(
            degrade_plan(&plan, &registered).unwrap().plan_id,
            plan.plan_id
        );
    }

    #[test]
    fn unavailable_or_conflicting_health_never_retains_a_selection() {
        let plan = consumed_plan();
        for statuses in [vec![], vec![status("files", true), status("files", false)]] {
            let result = degrade_plan(&plan, &KernelHealth::new(statuses)).unwrap();
            assert!(result.selected.is_empty());
            assert_eq!(result.budget.used_tokens, 20);
            assert_eq!(result.budget.remaining_tokens, 80);
        }
    }

    #[test]
    fn health_views_and_selection_share_duplicate_and_invalid_id_handling() {
        let reports = vec![
            status("files", true),
            status("files", false),
            status("facts", true),
            status("facts", true),
            status("", true),
        ];
        for reports in [reports.clone(), reports.into_iter().rev().collect()] {
            let health = KernelHealth::new(reports);
            assert_eq!(health.available_providers(), ["facts"]);
            assert_eq!(health.unavailable_providers(), ["", "files"]);
            assert_eq!(health.degradation_level(), DegradationLevel::Minimal);
            assert_eq!(health.summary(), "1/3 providers available (Minimal)");
            let restricted = degrade_plan(&consumed_plan(), &health).unwrap();
            assert_eq!(restricted.selected.len(), 1);
            assert_eq!(restricted.selected[0].provider, "facts");
        }
    }

    #[test]
    fn no_op_and_repeated_restrictions_preserve_identity() {
        let plan = consumed_plan();
        let full = KernelHealth::new(vec![status("facts", true), status("files", true)]);
        assert_eq!(
            serde_json::to_value(degrade_plan(&plan, &full).unwrap()).unwrap(),
            serde_json::to_value(&plan).unwrap()
        );
        let limited = KernelHealth::new(vec![status("files", true)]);
        let restricted = degrade_plan(&plan, &limited).unwrap();
        assert_eq!(
            serde_json::to_value(degrade_plan(&restricted, &limited).unwrap()).unwrap(),
            serde_json::to_value(&restricted).unwrap()
        );
        assert_ne!(restricted.plan_id, plan.plan_id);
    }

    #[test]
    fn derived_identity_binds_parent_and_is_health_order_independent() {
        let plan = consumed_plan();
        let health = KernelHealth::new(vec![status("files", true), status("facts", false)]);
        let expected = degrade_plan(&plan, &health).unwrap().plan_id;
        let reversed = KernelHealth::new(vec![status("facts", false), status("files", true)]);
        assert_eq!(expected, degrade_plan(&plan, &reversed).unwrap().plan_id);
        let mut other_scope = plan.clone();
        other_scope.plan_id.push_str("-other-scope");
        assert_ne!(
            expected,
            degrade_plan(&other_scope, &health).unwrap().plan_id
        );
    }

    #[test]
    fn invalid_accounting_and_origins_fail_closed_even_for_no_op() {
        let mutations: &[fn(&mut ContextPlanV1)] = &[
            |plan| plan.budget.used_tokens = 10,
            |plan| plan.budget.remaining_tokens = 0,
            |plan| plan.budget.total_tokens = 10,
            |plan| plan.selected[0].tokens = usize::MAX,
            |plan| plan.selected[0].phi = f64::NAN,
            |plan| plan.selected.push(plan.selected[0].clone()),
            |plan| plan.provider_stats.clear(),
            |plan| plan.provider_stats.get_mut("files").unwrap().tokens_used = 0,
            |plan| {
                plan.provider_stats
                    .get_mut("files")
                    .unwrap()
                    .candidates_offered = 0;
            },
            |plan| plan.origins.clear(),
            |plan| {
                let origins = plan.origins.get_mut("a").unwrap();
                origins.push(origins[0].clone());
            },
            |plan| plan.origins.get_mut("a").unwrap()[0].source = "other".to_owned(),
            |plan| plan.plan_id.clear(),
        ];
        let full = KernelHealth::new(vec![status("files", true), status("facts", true)]);
        for mutate in mutations {
            let mut plan = consumed_plan();
            mutate(&mut plan);
            assert!(degrade_plan(&plan, &full).is_err());
        }
    }

    #[test]
    fn real_kernel_output_can_be_restricted_without_replanning() {
        use super::super::types::{CandidateProvider, ContextObjectV1, SideEffectPolicy};
        use crate::core::context_field::ContextItemId;
        struct Provider(&'static str);
        impl CandidateProvider for Provider {
            fn provider_id(&self) -> &str {
                self.0
            }
            fn candidates(&self, _context: &RetrievalContext) -> Vec<ContextObjectV1> {
                vec![ContextObjectV1::new_fact(
                    ContextItemId::from_knowledge("decision", self.0),
                    "work",
                    self.0,
                    1.0,
                )]
            }
            fn side_effect_policy(&self) -> SideEffectPolicy {
                SideEffectPolicy::ReadOnly
            }
        }
        let kernel = ContextKernel::new(vec![
            Box::new(Provider("first")),
            Box::new(Provider("second")),
        ]);
        let plan = kernel
            .plan(&RetrievalContext {
                query: "work".to_owned(),
                task: None,
                project_root: "fixture-project".to_owned(),
                budget: TokenBudget {
                    total: 10_000,
                    used: 100,
                },
                max_candidates: 10,
            })
            .unwrap();
        assert_eq!(plan.selected.len(), 2);
        let restricted =
            degrade_plan(&plan, &KernelHealth::new(vec![status("first", true)])).unwrap();
        assert_eq!(restricted.selected.len(), 1);
        assert_eq!(
            restricted.origins[&restricted.selected[0].object_id][0].provider_id,
            "first"
        );
        assert_eq!(
            restricted.budget.used_tokens,
            100 + restricted.selected[0].tokens
        );
    }

    #[test]
    fn full_health_means_no_degradation() {
        let health = KernelHealth::new(vec![status("files", true), status("facts", true)]);

        assert_eq!(health.degradation_level(), DegradationLevel::Full);
        assert_eq!(health.summary(), "2/2 providers available (Full)");
    }

    #[test]
    fn partial_failure_degrades_to_reduced() {
        let health = KernelHealth::new(vec![
            status("files", true),
            status("facts", true),
            status("episodes", true),
            status("search", false),
            status("session", false),
        ]);

        assert_eq!(health.degradation_level(), DegradationLevel::Reduced);
        assert_eq!(health.unavailable_providers(), vec!["search", "session"]);
    }

    #[test]
    fn degrade_plan_removes_unavailable() {
        let health = KernelHealth::new(vec![status("files", true), status("facts", false)]);
        let mut plan = ContextPlanV1 {
            plan_id: "plan:test".to_owned(),
            intent: "test degradation".to_owned(),
            budget: PlanBudget {
                total_tokens: 100,
                used_tokens: 70,
                remaining_tokens: 30,
            },
            selected: vec![entry("file:1", "files", 40), entry("fact:1", "facts", 30)],
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
            origins: Default::default(),
        };

        bind_fixture(&mut plan);
        let degraded = degrade_plan(&plan, &health).expect("valid restriction");

        assert_ne!(degraded.plan_id, plan.plan_id);
        assert_eq!(
            degraded.plan_id,
            degrade_plan(&plan, &health).unwrap().plan_id
        );
        assert_eq!(degraded.selected.len(), 1);
        assert_eq!(degraded.selected[0].object_id, "file:1");
        assert_eq!(degraded.budget.used_tokens, 40);
        assert_eq!(degraded.budget.remaining_tokens, 60);
        assert_eq!(degraded.excluded.len(), 1);
        assert_eq!(degraded.excluded[0].object_id, "fact:1");
        assert_eq!(degraded.excluded[0].reason, "provider unavailable");
    }

    #[test]
    fn fallback_plan_is_empty_with_budget() {
        let plan = fallback_plan("bypass kernel", 512).expect("canonical empty request");

        assert_eq!(plan.intent, "bypass kernel");
        assert_eq!(plan.budget.total_tokens, 512);
        assert_eq!(plan.budget.used_tokens, 0);
        assert_eq!(plan.budget.remaining_tokens, 512);
        assert!(plan.selected.is_empty());
        assert!(plan.excluded.is_empty());
        assert!(plan.deferred.is_empty());
        assert!(plan.provider_stats.is_empty());
        assert_ne!(
            plan.plan_id,
            fallback_plan("bypass kernel", 513).unwrap().plan_id
        );
        assert_eq!(
            plan.plan_id,
            fallback_plan("bypass kernel", 512).unwrap().plan_id
        );
    }
}
