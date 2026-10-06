// SPDX-License-Identifier: Apache-2.0
//! Frozen, deterministic OSS reference scheduler.
//!
//! This implementation is Class B/C reference behavior only. Production
//! Class D code may implement [`SchedulerService`] with private decision
//! intelligence, but this scheduler performs no adaptation, learning, or
//! ranking from private data.

use std::cmp::Ordering;

use lean_ctx_protocol::{
    CapabilityId, CapabilityManifestV1, ContextStrategy, ExecutionPlanEstimatesV1, ExecutionPlanV1,
    PlanId, StopCondition, TaskEnvelopeV1,
};

use super::catalogue::{ProviderEntry, TechnicalCatalogue};
use super::policy_constraints::PolicyConstraints;
use super::scheduler_service::{ExecutionCandidate, SchedulerDecision, SchedulerService};
use super::types::{OclaError, OclaResult};

/// Maximum number of public candidates enumerated by the reference scheduler.
pub const MAX_CANDIDATES: usize = 100;

const SCHEDULER_REF: &str = "scheduler:reference-v1";
const FALLBACK_CAPABILITY: &str = "capability://leanctx/passthrough";
const FALLBACK_MODEL: &str = "manual";
const FALLBACK_PROVIDER: &str = "leanctx";

/// Deterministic, non-adaptive scheduler supplied for OSS conformance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReferenceScheduler;

impl ReferenceScheduler {
    /// Construct the frozen reference scheduler.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Produce a recommendation from public inputs.
    ///
    /// The synthetic manual fallback is subject to the same hard policy as
    /// generated candidates. If it is denied while a permitted candidate
    /// remains, the deterministic best permitted candidate becomes the
    /// decision fallback; otherwise scheduling fails closed.
    pub fn schedule(
        &self,
        envelope: &TaskEnvelopeV1,
        eligible: &[CapabilityManifestV1],
        catalogue: &TechnicalCatalogue,
        policy: &PolicyConstraints,
    ) -> OclaResult<SchedulerDecision> {
        let candidates = self.generate_candidates(envelope, eligible, catalogue)?;
        let fallback = self.fallback_candidate(envelope)?;
        let (filtered, excluded) = filter_with_report(candidates, policy);
        let decision_fallback = match policy.permits(&fallback) {
            Ok(()) => fallback,
            Err(error) => filtered
                .iter()
                .min_by(|left, right| compare_candidates(left, right))
                .cloned()
                .ok_or_else(|| {
                    OclaError::InvalidRequest(format!(
                        "no policy-permitted reference scheduler candidate or fallback: {error}"
                    ))
                })?,
        };
        let mut decision = self.select_plan(&filtered, &decision_fallback);
        decision.candidates_evaluated = filtered
            .len()
            .saturating_add(excluded.len())
            .try_into()
            .unwrap_or(u32::MAX);
        decision.candidates_excluded = excluded.len().try_into().unwrap_or(u32::MAX);
        decision.decision_ref = decision_ref(&filtered, &excluded, &decision_fallback);
        Ok(decision)
    }

    /// Alias suitable for callers that use recommendation terminology.
    pub fn recommend(
        &self,
        envelope: &TaskEnvelopeV1,
        eligible: &[CapabilityManifestV1],
        catalogue: &TechnicalCatalogue,
        policy: &PolicyConstraints,
    ) -> OclaResult<SchedulerDecision> {
        self.schedule(envelope, eligible, catalogue, policy)
    }

    /// Build the deterministic raw fallback plan template.
    ///
    /// This helper does not perform policy admission; callers that execute a
    /// plan must apply `PolicyConstraints` separately.
    pub fn fallback_plan(&self, envelope: &TaskEnvelopeV1) -> OclaResult<ExecutionPlanV1> {
        Ok(self.fallback_candidate(envelope)?.plan)
    }

    fn fallback_candidate(&self, envelope: &TaskEnvelopeV1) -> OclaResult<ExecutionCandidate> {
        envelope.validate().map_err(|error| {
            OclaError::InvalidRequest(format!("invalid task envelope: {error}"))
        })?;
        let capability_id =
            CapabilityId::try_from(FALLBACK_CAPABILITY.to_owned()).map_err(|error| {
                OclaError::InvalidRequest(format!("invalid fallback capability: {error}"))
            })?;
        let plan = plan_for(
            envelope,
            &capability_id,
            FALLBACK_MODEL,
            FALLBACK_PROVIDER,
            Some("fallback"),
        )?;
        Ok(ExecutionCandidate::new(
            plan,
            FALLBACK_CAPABILITY,
            FALLBACK_MODEL,
            FALLBACK_PROVIDER,
            Some(0),
            None,
            None,
        ))
    }
}

impl SchedulerService for ReferenceScheduler {
    fn generate_candidates(
        &self,
        envelope: &TaskEnvelopeV1,
        eligible: &[CapabilityManifestV1],
        catalogue: &TechnicalCatalogue,
    ) -> OclaResult<Vec<ExecutionCandidate>> {
        envelope.validate().map_err(|error| {
            OclaError::InvalidRequest(format!("invalid task envelope: {error}"))
        })?;

        let mut manifests = eligible
            .iter()
            .filter(|manifest| catalogue_allows_manifest(catalogue, manifest))
            .cloned()
            .collect::<Vec<_>>();
        manifests.sort_by_key(manifest_key);

        let mut candidates = Vec::new();
        for manifest in manifests {
            manifest.validate().map_err(|error| {
                OclaError::InvalidRequest(format!("invalid capability manifest: {error}"))
            })?;

            let provider_options = provider_options(&manifest, catalogue);
            for (provider, models) in provider_options {
                for model in models {
                    if candidates.len() == MAX_CANDIDATES {
                        return Ok(candidates);
                    }
                    let plan =
                        plan_for(envelope, &manifest.capability_id, &model, &provider, None)?;
                    candidates.push(ExecutionCandidate::new(
                        plan,
                        manifest.capability_id.as_str(),
                        model,
                        provider.clone(),
                        // Public catalogues do not contain pricing or quality/latency estimates.
                        None,
                        None,
                        None,
                    ));
                }
            }
        }
        Ok(candidates)
    }

    fn filter_candidates(
        &self,
        candidates: Vec<ExecutionCandidate>,
        policy: &PolicyConstraints,
    ) -> Vec<ExecutionCandidate> {
        filter_with_report(candidates, policy).0
    }

    fn select_plan(
        &self,
        filtered: &[ExecutionCandidate],
        fallback: &ExecutionCandidate,
    ) -> SchedulerDecision {
        let selected = filtered
            .iter()
            .min_by(|left, right| compare_candidates(left, right))
            .unwrap_or(fallback);
        let has_candidate = !filtered.is_empty();
        let rationale_code = if has_candidate {
            "reference_score_cost_latency_quality"
        } else {
            "fallback_no_permitted_candidates"
        };
        SchedulerDecision {
            selected: selected.plan.clone(),
            fallback: fallback.plan.clone(),
            decision_ref: decision_ref(filtered, &[], fallback),
            rationale_code: rationale_code.to_owned(),
            confidence_milli: if has_candidate { 1000 } else { 0 },
            candidates_evaluated: filtered.len().try_into().unwrap_or(u32::MAX),
            candidates_excluded: 0,
        }
    }
}

fn catalogue_allows_manifest(
    catalogue: &TechnicalCatalogue,
    manifest: &CapabilityManifestV1,
) -> bool {
    catalogue
        .capability(manifest.capability_id.as_str(), &manifest.version)
        .is_none_or(|entry| entry.available)
}

fn manifest_key(manifest: &CapabilityManifestV1) -> (String, String, String) {
    (
        manifest.capability_id.as_str().to_owned(),
        manifest.version.clone(),
        manifest.provider.clone(),
    )
}

fn provider_options(
    manifest: &CapabilityManifestV1,
    catalogue: &TechnicalCatalogue,
) -> Vec<(String, Vec<String>)> {
    let mut entries = catalogue
        .providers
        .iter()
        .filter(|entry| entry.provider_id == manifest.provider)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));

    if entries.is_empty() {
        return vec![(manifest.provider.clone(), model_options(catalogue, None))];
    }

    entries
        .into_iter()
        .map(|entry| {
            (
                entry.provider_id.clone(),
                model_options(catalogue, Some(entry)),
            )
        })
        .collect()
}

fn model_options(catalogue: &TechnicalCatalogue, provider: Option<&ProviderEntry>) -> Vec<String> {
    let mut models = match provider {
        Some(provider) if !provider.models_available.is_empty() => {
            provider.models_available.clone()
        }
        _ => catalogue
            .models
            .iter()
            .map(|model| model.model_id.clone())
            .collect(),
    };
    models.retain(|model| !model.is_empty());
    models.sort();
    models.dedup();
    if models.is_empty() {
        models.push(FALLBACK_MODEL.to_owned());
    }
    models
}

fn plan_for(
    envelope: &TaskEnvelopeV1,
    capability_id: &CapabilityId,
    model: &str,
    provider: &str,
    fallback_marker: Option<&str>,
) -> OclaResult<ExecutionPlanV1> {
    let seed = format!(
        "reference-v1:{}:{}:{}:{}",
        envelope.task_id.as_str(),
        capability_id.as_str(),
        model,
        provider
    );
    let plan_id = PlanId::try_from(format!("plan:{}", blake3::hash(seed.as_bytes()).to_hex()))
        .map_err(|error| {
            OclaError::InvalidRequest(format!("invalid deterministic plan id: {error}"))
        })?;
    let mut fallback_refs = Vec::new();
    if let Some(marker) = fallback_marker {
        fallback_refs.push(marker.to_owned());
    }
    let plan = ExecutionPlanV1 {
        schema_version: 1,
        plan_id,
        task_id: envelope.task_id.clone(),
        context_budget_tokens: 0,
        context_budget_policy: None,
        context_strategy: ContextStrategy::Balanced,
        knowledge_refs: Vec::new(),
        capability_ids: vec![capability_id.clone()],
        model: model.to_owned(),
        provider: provider.to_owned(),
        reasoning_allocation_milli: 0,
        max_retries: 0,
        fallback_refs,
        stop_condition: StopCondition::OnCompletion,
        expected_cost_micros: 0,
        estimates: Some(ExecutionPlanEstimatesV1 {
            cost_micros: fallback_marker.map(|_| 0_u64),
            quality_milli: None,
            latency_ms: None,
        }),
        expected_quality_milli: 0,
        expected_latency_ms: 0,
        policy_decision_ref: None,
        scheduler_decision_ref: Some(SCHEDULER_REF.to_owned()),
        executor_agent_id: Some(envelope.agent_id.clone()),
        context_plan_id: None,
        capability_bindings: Vec::new(),
        extensions: Default::default(),
    };
    plan.validate().map_err(|error| {
        OclaError::InvalidRequest(format!("invalid deterministic execution plan: {error}"))
    })?;
    Ok(plan)
}

fn filter_with_report(
    candidates: Vec<ExecutionCandidate>,
    policy: &PolicyConstraints,
) -> (Vec<ExecutionCandidate>, Vec<ExecutionCandidate>) {
    let mut permitted = Vec::with_capacity(candidates.len());
    let mut excluded = Vec::new();
    for mut candidate in candidates {
        match policy.permits(&candidate) {
            Ok(()) => {
                candidate.exclusion_reason = None;
                permitted.push(candidate);
            }
            Err(error) => {
                candidate.exclusion_reason = Some(error.to_string());
                excluded.push(candidate);
            }
        }
    }
    (permitted, excluded)
}

fn compare_candidates(left: &ExecutionCandidate, right: &ExecutionCandidate) -> Ordering {
    // Candidate options are the authoritative nullable estimate projection. The
    // zero-valued plan scalars are retained only for legacy wire compatibility;
    // treating them as ranking inputs would make unknown estimates look free.
    compare_optional_ascending(left.expected_cost_micros, right.expected_cost_micros)
        .then_with(|| {
            compare_optional_ascending(left.expected_latency_ms, right.expected_latency_ms)
        })
        .then_with(|| {
            compare_optional_descending(left.expected_quality_milli, right.expected_quality_milli)
        })
        .then_with(|| left.identity().cmp(&right.identity()))
        .then_with(|| left.plan.plan_id.as_str().cmp(right.plan.plan_id.as_str()))
}

fn compare_optional_ascending<T: Ord>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_optional_descending<T: Ord>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn decision_ref(
    filtered: &[ExecutionCandidate],
    excluded: &[ExecutionCandidate],
    fallback: &ExecutionCandidate,
) -> String {
    let mut seed = String::from("reference-v1|");
    for candidate in filtered {
        seed.push_str("permit:");
        seed.push_str(&candidate.identity());
        seed.push('|');
    }
    for candidate in excluded {
        seed.push_str("exclude:");
        seed.push_str(&candidate.identity());
        seed.push(':');
        seed.push_str(candidate.exclusion_reason.as_deref().unwrap_or("unknown"));
        seed.push('|');
    }
    seed.push_str("fallback:");
    seed.push_str(&fallback.identity());
    format!(
        "decision:reference-v1:{}",
        blake3::hash(seed.as_bytes()).to_hex()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::core::ocla::catalogue::{CatalogueEntry, ModelEntry, ProviderEntry};
    use lean_ctx_protocol::{
        CapabilityKind, DataClassification, DataMovement, Determinism, MeasurementSupportV1,
        Reversibility, SurfaceSupportV1, TaskComplexity,
    };

    fn manifest(capability_id: &str, provider: &str) -> CapabilityManifestV1 {
        CapabilityManifestV1 {
            schema_version: 1,
            capability_id: CapabilityId::try_from(capability_id).expect("capability id"),
            provider: provider.to_owned(),
            kind: CapabilityKind::Tool,
            version: "1.0.0".to_owned(),
            surfaces: vec!["context".to_owned()],
            support_matrix: BTreeMap::from([(
                "context".to_owned(),
                SurfaceSupportV1 {
                    supported: true,
                    input_schema_ref: None,
                    output_schema_ref: None,
                },
            )]),
            local: true,
            remote: false,
            reversibility: Reversibility::Reversible,
            determinism: Determinism::Deterministic,
            data_movement: DataMovement::LocalOnly,
            supported_classifications: vec![DataClassification::Public],
            measurement_support: MeasurementSupportV1 {
                latency: true,
                tokens: true,
                quality: true,
            },
            input_schema_ref: None,
            output_schema_ref: None,
            conformance_version: 1,
            extra: Default::default(),
        }
    }

    fn envelope() -> TaskEnvelopeV1 {
        TaskEnvelopeV1 {
            schema_version: 1,
            task_id: "task-reference".try_into().expect("task id"),
            trace_id: "trace-reference".try_into().expect("trace id"),
            project_id: "project-reference".try_into().expect("project id"),
            session_id: "session-reference".try_into().expect("session id"),
            agent_id: "agent-reference".try_into().expect("agent id"),
            complexity: TaskComplexity::Low,
            created_at: "2026-08-09T00:00:00Z".to_owned(),
            parent_task_id: None,
            tenant_id: None,
            intent: Some("inspect".to_owned()),
            task_class: None,
            risk_class: None,
            quality_requirement_milli: Some(700),
            cost_budget_micros: None,
            latency_budget_ms: Some(500),
            data_classification: Some(DataClassification::Public),
            region_policy_ref: None,
            model_policy_ref: None,
            context_state_ref: None,
            outcome_contract_ref: None,
            extensions: Default::default(),
        }
    }

    #[test]
    fn generate_candidates_enumerates_models_and_is_capped() {
        let capability = manifest("capability://search", "provider-a");
        let mut catalogue = TechnicalCatalogue {
            capabilities: vec![CatalogueEntry {
                capability_id: capability.capability_id.as_str().to_owned(),
                version: capability.version.clone(),
                manifest: capability.clone(),
                available: true,
            }],
            models: vec![
                ModelEntry {
                    model_id: "model-b".to_owned(),
                    context_window: 8_000,
                    supports_reasoning: false,
                    supports_streaming: true,
                },
                ModelEntry {
                    model_id: "model-a".to_owned(),
                    context_window: 8_000,
                    supports_reasoning: true,
                    supports_streaming: true,
                },
            ],
            providers: vec![ProviderEntry {
                provider_id: "provider-a".to_owned(),
                models_available: vec!["model-b".to_owned(), "model-a".to_owned()],
                regions: vec!["CH".to_owned()],
            }],
        };
        catalogue.models.reverse();

        let candidates = ReferenceScheduler::new()
            .generate_candidates(&envelope(), &[capability], &catalogue)
            .expect("candidate generation");

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].model, "model-a");
        assert_eq!(candidates[1].model, "model-b");
        assert!(candidates.len() <= MAX_CANDIDATES);
    }

    #[test]
    fn filter_candidates_applies_provider_policy() {
        let scheduler = ReferenceScheduler::new();
        let task = envelope();
        let allowed = manifest("capability://allowed", "provider-a");
        let blocked = manifest("capability://blocked", "provider-b");
        let mut catalogue = TechnicalCatalogue::from_manifests([allowed.clone()]);
        // This fixture explicitly represents an available runtime registration.
        catalogue.capabilities[0].available = true;
        let candidates = scheduler
            .generate_candidates(&task, &[allowed.clone(), blocked], &catalogue)
            .expect("candidate generation");
        let policy = PolicyConstraints {
            allowed_providers: Some(vec!["provider-a".to_owned()]),
            ..PolicyConstraints::default()
        };

        let filtered = scheduler.filter_candidates(candidates, &policy);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].provider, "provider-a");
    }

    #[test]
    fn selection_is_deterministic_by_cost_then_latency() {
        let scheduler = ReferenceScheduler::new();
        let task = envelope();
        let first_capability = CapabilityId::try_from("capability://first").expect("capability");
        let second_capability = CapabilityId::try_from("capability://second").expect("capability");
        let first_plan = plan_for(&task, &first_capability, "model-first", "provider", None)
            .expect("first plan");
        let second_plan = plan_for(&task, &second_capability, "model-second", "provider", None)
            .expect("second plan");
        let candidates = vec![
            ExecutionCandidate::new(
                first_plan,
                "capability://first",
                "model-first",
                "provider",
                Some(20),
                Some(700),
                Some(100),
            ),
            ExecutionCandidate::new(
                second_plan,
                "capability://second",
                "model-second",
                "provider",
                Some(10),
                Some(600),
                Some(900),
            ),
        ];
        let fallback = scheduler
            .fallback_candidate(&task)
            .expect("fallback candidate");

        let decision = scheduler.select_plan(&candidates, &fallback);

        assert_eq!(decision.selected.model, "model-second");
        assert_eq!(decision.fallback, fallback.plan);
        assert_eq!(decision.confidence_milli, 1000);
    }
}
