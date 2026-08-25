//! Candidate orchestration, scoring, and feedback artifacts for the context kernel.

use std::collections::{HashMap, HashSet};

use crate::core::context_compiler::{CompileCandidate, CompileMode, CompileResult, compile};
use crate::core::context_field::{
    ContextField, ContextKind, ContextState, FieldSignals, ViewCosts, ViewKind,
    normalize_token_cost,
};

use super::types::{
    CandidateProvider, ContextObjectKind, ContextObjectV1, ContextPlanV1, ContextReceiptV1,
    ExcludedEntry, PlanBudget, PlanEntry, ProviderStat, QualitySignal, ReceiptOutcome,
    RetrievalContext,
};

/// The Context Control Kernel — orchestrates candidate gathering, Phi scoring,
/// budget-optimal selection, and plan/receipt generation.
pub struct ContextKernel {
    providers: Vec<Box<dyn CandidateProvider>>,
    field: ContextField,
}

impl ContextKernel {
    /// Create a kernel with an explicit field policy.
    pub(crate) fn with_field(
        providers: Vec<Box<dyn CandidateProvider>>,
        field: ContextField,
    ) -> Self {
        Self { providers, field }
    }

    /// Create a new kernel with the given providers.
    pub fn new(providers: Vec<Box<dyn CandidateProvider>>) -> Self {
        Self::with_field(providers, ContextField::active())
    }

    /// Create a kernel with default providers for a project.
    pub fn for_project(project_root: &str) -> Self {
        Self::new(super::providers::default_providers(project_root))
    }

    /// Register an additional provider.
    pub fn register(&mut self, provider: Box<dyn CandidateProvider>) {
        self.providers.push(provider);
    }

    /// Gather and content-deduplicate candidates from all registered providers.
    pub fn gather(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        let mut all = Vec::new();
        for provider in &self.providers {
            all.extend(provider.candidates(ctx));
        }
        dedup_by_content_ref(&mut all);
        all
    }

    /// Score candidates, compile the best package under budget, and return its plan.
    pub fn plan(&self, ctx: &RetrievalContext) -> ContextPlanV1 {
        let candidates = self.gather(ctx);
        let scored: Vec<_> = candidates
            .into_iter()
            .map(|object| {
                let signals = signals_from_object(&object, ctx);
                let phi = self.field.compute_phi(&signals);
                (object, phi)
            })
            .collect();
        let compile_candidates: Vec<_> = scored
            .iter()
            .map(|(object, phi)| to_compile_candidate(object, *phi))
            .collect();
        let result = compile(&compile_candidates, ctx.budget, CompileMode::HandleManifest);

        build_plan(ctx, &scored, &result)
    }

    /// Record delivery outcome and provider-level feedback for a completed plan.
    pub fn record_receipt(
        &self,
        plan: &ContextPlanV1,
        delivered_tokens: usize,
        outcome: ReceiptOutcome,
    ) -> ContextReceiptV1 {
        let outcome_value = outcome_value(outcome);
        let total_phi: f64 = plan.selected.iter().map(|entry| entry.phi.max(0.0)).sum();
        let mut feedback_attribution = HashMap::new();
        if total_phi > 0.0 {
            for entry in &plan.selected {
                let contribution = entry.phi.max(0.0) / total_phi * outcome_value;
                *feedback_attribution
                    .entry(entry.provider.clone())
                    .or_insert(0.0) += contribution;
            }
        }

        let receipt_material = format!(
            "{}|{}|{}",
            plan.plan_id,
            delivered_tokens,
            receipt_outcome_name(outcome)
        );
        ContextReceiptV1 {
            receipt_id: format!("receipt_{}", short_hash(&receipt_material)),
            plan_id: plan.plan_id.clone(),
            task_id: None,
            delivered_tokens,
            cache_hits: 0,
            cache_misses: 0,
            outcome,
            quality_signals: vec![QualitySignal {
                signal_type: "outcome".to_string(),
                value: outcome_value,
            }],
            feedback_attribution,
        }
    }
}

fn dedup_by_content_ref(objects: &mut Vec<ContextObjectV1>) {
    let mut retained = HashMap::<String, usize>::new();
    let mut deduplicated: Vec<ContextObjectV1> = Vec::with_capacity(objects.len());
    for object in objects.drain(..) {
        match retained.get(&object.content_ref).copied() {
            Some(index) if object.confidence > deduplicated[index].confidence => {
                deduplicated[index] = object;
            }
            Some(_) => {}
            None => {
                retained.insert(object.content_ref.clone(), deduplicated.len());
                deduplicated.push(object);
            }
        }
    }
    *objects = deduplicated;
}

fn signals_from_object(object: &ContextObjectV1, ctx: &RetrievalContext) -> FieldSignals {
    FieldSignals {
        relevance: keyword_overlap(&object.title, object.content.as_deref(), &ctx.query),
        surprise: 0.5,
        graph_proximity: 0.5,
        history_signal: object.confidence.clamp(0.0, 1.0) as f64,
        token_cost_norm: normalize_token_cost(object.token_estimate, ctx.budget.total),
        redundancy: 0.0,
    }
}

fn keyword_overlap(title: &str, content: Option<&str>, query: &str) -> f64 {
    let query_terms = terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let mut object_terms = terms(title);
    if let Some(content) = content {
        object_terms.extend(terms(content));
    }
    let overlap = query_terms.intersection(&object_terms).count();
    overlap as f64 / query_terms.len() as f64
}

fn terms(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn to_compile_candidate(object: &ContextObjectV1, phi: f64) -> CompileCandidate {
    let view_costs = if object.view_costs.estimates.is_empty() {
        ViewCosts::from_full_tokens(object.token_estimate.max(1))
    } else {
        object.view_costs.clone()
    };
    let (selected_view, selected_tokens) = view_costs
        .cheapest_content_view()
        .unwrap_or((ViewKind::Full, object.token_estimate.max(1)));

    CompileCandidate {
        id: object.id.clone(),
        kind: context_kind(object.kind),
        path: object.content_ref.clone(),
        state: if object.freshness.stale {
            ContextState::Stale
        } else {
            ContextState::Candidate
        },
        phi,
        view_costs,
        selected_view,
        selected_tokens,
        pinned: false,
        content_sketch: object
            .semantic_fingerprint
            .clone()
            .or_else(|| Some(object.content_ref.clone())),
    }
}

fn context_kind(kind: ContextObjectKind) -> ContextKind {
    match kind {
        ContextObjectKind::File => ContextKind::File,
        ContextObjectKind::Fact => ContextKind::Knowledge,
        ContextObjectKind::Episode
        | ContextObjectKind::Procedure
        | ContextObjectKind::SessionItem => ContextKind::Memory,
        ContextObjectKind::SearchChunk => ContextKind::Provider,
    }
}

fn build_plan(
    ctx: &RetrievalContext,
    scored: &[(ContextObjectV1, f64)],
    result: &CompileResult,
) -> ContextPlanV1 {
    let objects: HashMap<_, _> = scored
        .iter()
        .map(|(object, phi)| (object.id.to_string(), (object, *phi)))
        .collect();
    let mut provider_stats = HashMap::new();
    for (object, _) in scored {
        provider_stats
            .entry(object.source.clone())
            .or_insert(ProviderStat {
                candidates_offered: 0,
                candidates_selected: 0,
                tokens_used: 0,
            })
            .candidates_offered += 1;
    }

    let selected = result
        .selected
        .iter()
        .map(|item| {
            let (object, phi) = objects
                .get(&item.id)
                .copied()
                .unwrap_or_else(|| panic!("compiler selected unknown candidate: {}", item.id));
            let stat = provider_stats
                .get_mut(&object.source)
                .unwrap_or_else(|| panic!("missing provider statistics: {}", object.source));
            stat.candidates_selected += 1;
            stat.tokens_used = stat.tokens_used.saturating_add(item.tokens);
            PlanEntry {
                object_id: item.id.clone(),
                provider: object.source.clone(),
                view: item.view.clone(),
                tokens: item.tokens,
                phi,
                reason: selected_display(object).unwrap_or_default(),
            }
        })
        .collect();
    let excluded = result
        .excluded_reasons
        .iter()
        .map(|item| ExcludedEntry {
            object_id: item.id.clone(),
            provider: objects.get(&item.id).map_or_else(
                || "unknown".to_string(),
                |(object, _)| object.source.clone(),
            ),
            reason: item.reason.clone(),
        })
        .collect();
    let plan_material = plan_material(ctx, scored, result);

    ContextPlanV1 {
        plan_id: format!("plan_{}", short_hash(&plan_material)),
        intent: ctx.task.clone().unwrap_or_else(|| ctx.query.clone()),
        budget: PlanBudget {
            total_tokens: ctx.budget.total,
            used_tokens: result.budget_used,
            remaining_tokens: ctx.budget.total.saturating_sub(result.budget_used),
        },
        selected,
        excluded,
        deferred: Vec::new(),
        provider_stats,
    }
}

fn selected_display(object: &ContextObjectV1) -> Option<String> {
    let path = object
        .metadata
        .get("path")
        .or_else(|| object.metadata.get("file"))
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| match object.kind {
            ContextObjectKind::File => {
                (!object.title.trim().is_empty()).then_some(object.title.as_str())
            }
            _ => None,
        });
    let snippet = object
        .content
        .as_deref()
        .filter(|value| !value.trim().is_empty());

    match (path, snippet) {
        (Some(path), Some(snippet)) => Some(format!("{path}: {snippet}")),
        (Some(path), None) => Some(path.to_owned()),
        (None, Some(snippet)) => Some(snippet.to_owned()),
        (None, None) => None,
    }
}

fn plan_material(
    ctx: &RetrievalContext,
    scored: &[(ContextObjectV1, f64)],
    result: &CompileResult,
) -> String {
    let mut entries: Vec<_> = scored
        .iter()
        .map(|(object, phi)| format!("{}:{}:{phi:.12}", object.id, object.content_ref))
        .collect();
    entries.sort_unstable();
    format!(
        "{}|{}|{}|{}|{}",
        ctx.query,
        ctx.task.as_deref().unwrap_or_default(),
        ctx.budget.total,
        result.budget_used,
        entries.join("|")
    )
}

fn short_hash(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..16].to_string()
}

fn outcome_value(outcome: ReceiptOutcome) -> f64 {
    match outcome {
        ReceiptOutcome::Accepted => 1.0,
        ReceiptOutcome::Partial => 0.5,
        ReceiptOutcome::Rejected | ReceiptOutcome::Unknown => 0.0,
    }
}

fn receipt_outcome_name(outcome: ReceiptOutcome) -> &'static str {
    match outcome {
        ReceiptOutcome::Accepted => "accepted",
        ReceiptOutcome::Rejected => "rejected",
        ReceiptOutcome::Partial => "partial",
        ReceiptOutcome::Unknown => "unknown",
    }
}

#[cfg(test)]
pub mod tests {
    use super::super::types::{Freshness, SensitivityLevel, SideEffectPolicy};
    use std::collections::HashMap;

    use crate::core::context_field::{
        ContextItemId, FieldWeights, Provenance, TokenBudget, ViewCosts,
    };

    use super::*;

    struct MockProvider {
        items: Vec<ContextObjectV1>,
    }

    impl CandidateProvider for MockProvider {
        #[allow(clippy::unnecessary_literal_bound)]
        fn provider_id(&self) -> &str {
            "test.mock"
        }

        fn candidates(&self, _ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
            self.items.clone()
        }

        fn side_effect_policy(&self) -> SideEffectPolicy {
            SideEffectPolicy::ReadOnly
        }
    }

    fn context() -> RetrievalContext {
        RetrievalContext {
            query: "context kernel".to_string(),
            task: Some("build kernel".to_string()),
            project_root: "/project".to_string(),
            budget: TokenBudget {
                total: 200,
                used: 0,
            },
            max_candidates: 10,
        }
    }

    fn object(id: &str, content_ref: &str, confidence: f32) -> ContextObjectV1 {
        ContextObjectV1 {
            id: ContextItemId::from_provider("test.mock", id),
            kind: ContextObjectKind::Fact,
            source: "test.mock".to_string(),
            content_ref: content_ref.to_string(),
            title: "context kernel".to_string(),
            content: Some("context kernel orchestration".to_string()),
            freshness: Freshness {
                created_at: "2026-01-01T00:00:00Z".to_string(),
                ttl_secs: None,
                stale: false,
            },
            confidence,
            sensitivity: SensitivityLevel::Internal,
            token_estimate: 50,
            view_costs: ViewCosts::from_full_tokens(50),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn empty_kernel_gathers_nothing() {
        assert!(ContextKernel::new(Vec::new()).gather(&context()).is_empty());
    }

    #[test]
    fn gather_keeps_highest_confidence_duplicate() {
        let kernel = ContextKernel::new(vec![Box::new(MockProvider {
            items: vec![object("one", "same", 0.2), object("two", "same", 0.9)],
        })]);

        let gathered = kernel.gather(&context());
        assert_eq!(gathered.len(), 1);
        assert_eq!(gathered[0].confidence, 0.9);
    }

    #[test]
    fn object_signals_are_normalized() {
        let signals = signals_from_object(&object("one", "reference", 0.8), &context());
        for signal in [
            signals.relevance,
            signals.surprise,
            signals.graph_proximity,
            signals.history_signal,
            signals.token_cost_norm,
            signals.redundancy,
        ] {
            assert!((0.0..=1.0).contains(&signal));
        }
    }

    #[test]
    fn compiler_candidate_maps_object_fields() {
        let source = object("one", "reference", 0.8);
        let candidate = to_compile_candidate(&source, 0.75);
        assert_eq!(candidate.id, source.id);
        assert_eq!(candidate.kind, ContextKind::Knowledge);
        assert_eq!(candidate.path, source.content_ref);
        assert_eq!(candidate.phi, 0.75);
    }

    #[test]
    fn empty_plan_preserves_budget() {
        let plan = ContextKernel::new(Vec::new()).plan(&context());
        assert!(plan.selected.is_empty());
        assert_eq!(plan.budget.total_tokens, 200);
        assert_eq!(plan.budget.used_tokens, 0);
        assert_eq!(plan.budget.remaining_tokens, 200);
    }

    #[test]
    fn plan_selects_high_phi_candidate() {
        let low = object("low", "low", 0.1);
        let mut high = object("high", "high", 1.0);
        high.title = "context kernel context kernel".to_string();
        let kernel = ContextKernel::new(vec![Box::new(MockProvider {
            items: vec![low, high.clone()],
        })]);

        let plan = kernel.plan(&context());
        assert!(
            plan.selected
                .iter()
                .any(|entry| entry.object_id == high.id.to_string())
        );
    }

    #[test]
    fn explicit_field_controls_scoring_without_global_strategy() {
        let providers = || -> Vec<Box<dyn CandidateProvider>> {
            vec![Box::new(MockProvider {
                items: vec![object("one", "reference", 0.8)],
            })]
        };
        let weights = |w_relevance| FieldWeights {
            w_relevance,
            w_surprise: 0.0,
            w_graph: 0.0,
            w_history: 0.0,
            w_cost: 0.0,
            w_redundancy: 0.0,
        };

        let relevance_plan =
            ContextKernel::with_field(providers(), ContextField::with_weights(weights(1.0)))
                .plan(&context());
        let zero_plan =
            ContextKernel::with_field(providers(), ContextField::with_weights(weights(0.0)))
                .plan(&context());

        assert_eq!(relevance_plan.selected.len(), 1);
        assert_eq!(zero_plan.selected.len(), 1);
        assert_eq!(relevance_plan.selected[0].phi, 1.0);
        assert_eq!(zero_plan.selected[0].phi, 0.0);
    }

    #[test]
    fn receipt_references_plan() {
        let plan = ContextKernel::new(Vec::new()).plan(&context());
        let receipt =
            ContextKernel::new(Vec::new()).record_receipt(&plan, 0, ReceiptOutcome::Accepted);
        assert_eq!(receipt.plan_id, plan.plan_id);
    }
}
