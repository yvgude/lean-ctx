// SPDX-License-Identifier: Apache-2.0

//! Candidate orchestration, scoring, and feedback artifacts for the context kernel.

use chrono::{DateTime, FixedOffset};
use std::collections::{BTreeMap, HashMap, HashSet};
use thiserror::Error;

use crate::core::context_compiler::{CompileCandidate, CompileMode, CompileResult, compile};
use crate::core::context_field::{
    ContextField, ContextKind, ContextState, FieldSignals, ViewCosts, ViewKind,
    normalize_token_cost,
};

use super::types::{
    CandidateProvider, ContextObjectKind, ContextObjectV1, ContextOriginV1, ContextPlanV1,
    ContextReceiptV1, ExcludedEntry, PlanBudget, PlanEntry, ProviderStat, QualitySignal,
    ReceiptOutcome, RetrievalContext,
};
use super::{enforce::BlockedEntry, policy::ContextPolicy};

#[derive(Debug, Clone)]
struct EnumeratedCandidate {
    provider_id: String,
    object: ContextObjectV1,
}

/// A compiler result cannot be bound to the kernel's candidate/accounting snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KernelPlanError {
    #[error("compiler selected an unknown candidate")]
    CompilerSelectedUnknownCandidate,
    #[error("provider statistics are missing for a selected candidate")]
    MissingProviderStats,
    #[error("plan accounting is inconsistent")]
    InvalidPlanAccounting,
    #[error("selected candidate origin is missing or ambiguous")]
    InvalidPlanOrigin,
}

/// The Context Control Kernel — orchestrates candidate gathering, Phi scoring,
/// budget-optimal selection, and plan/receipt generation.
pub struct ContextKernel {
    providers: Vec<Box<dyn CandidateProvider>>,
    field: ContextField,
    private_selection: bool,
    relevance_floor: bool,
}

impl ContextKernel {
    /// Create a kernel with an explicit field policy.
    pub(crate) fn with_field(
        providers: Vec<Box<dyn CandidateProvider>>,
        field: ContextField,
    ) -> Self {
        Self {
            providers,
            field,
            private_selection: false,
            relevance_floor: false,
        }
    }

    /// Leave out candidates unrelated to the task even when budget remains
    /// (explicit-source plans; see [`super::relevance_floor`]).
    pub(crate) fn with_relevance_floor(mut self) -> Self {
        self.relevance_floor = true;
        self
    }

    /// The canonical task planner may use an explicitly enabled installed peer;
    /// pure/reference kernels never discover or execute optional components.
    pub(crate) fn with_private_selection(mut self) -> Self {
        self.private_selection = true;
        self
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
        let mut all: Vec<_> = self
            .candidates(ctx)
            .into_iter()
            .map(|candidate| candidate.object)
            .collect();
        dedup_by_content_ref(&mut all);
        all
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<EnumeratedCandidate> {
        self.providers
            .iter()
            .flat_map(|provider| {
                let provider_id = provider.provider_id().to_owned();
                provider
                    .candidates(ctx)
                    .into_iter()
                    .map(move |object| EnumeratedCandidate {
                        provider_id: provider_id.clone(),
                        object,
                    })
            })
            .collect()
    }

    /// Score candidates, compile the best package under budget, and return its plan.
    pub fn plan(&self, ctx: &RetrievalContext) -> Result<ContextPlanV1, KernelPlanError> {
        self.plan_with_policy(ctx, &ContextPolicy::default())
            .map(|(plan, _)| plan)
    }

    /// Own policy filtering, selection, accounting and identity as one operation.
    pub(crate) fn plan_with_policy(
        &self,
        ctx: &RetrievalContext,
        policy: &ContextPolicy,
    ) -> Result<(ContextPlanV1, Vec<BlockedEntry>), KernelPlanError> {
        self.plan_with_policy_at(ctx, policy, None)
    }

    /// Own policy filtering, selection, accounting and identity at an explicit time.
    pub(crate) fn plan_with_policy_at(
        &self,
        ctx: &RetrievalContext,
        policy: &ContextPolicy,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> Result<(ContextPlanV1, Vec<BlockedEntry>), KernelPlanError> {
        self.plan_with_selection_at(ctx, policy, evaluation_time)
            .map(|(plan, blocked, _)| (plan, blocked))
    }

    pub(crate) fn plan_with_selection_at(
        &self,
        ctx: &RetrievalContext,
        policy: &ContextPolicy,
        evaluation_time: Option<&DateTime<FixedOffset>>,
    ) -> Result<(ContextPlanV1, Vec<BlockedEntry>, Option<&'static str>), KernelPlanError> {
        let mut offered = self.candidates(ctx);
        offered.sort_by(|a, b| {
            a.object
                .id
                .as_str()
                .cmp(b.object.id.as_str())
                .then_with(|| a.object.source.cmp(&b.object.source))
                .then_with(|| a.object.content_ref.cmp(&b.object.content_ref))
                .then_with(|| a.provider_id.cmp(&b.provider_id))
        });
        let mut origins = origins_from_candidates(&offered);
        let mut counts = HashMap::<String, usize>::new();
        let mut provider_stats = HashMap::<String, ProviderStat>::new();
        for candidate in &offered {
            *counts.entry(candidate.object.id.to_string()).or_default() += 1;
            provider_stats
                .entry(candidate.object.source.clone())
                .or_default()
                .candidates_offered += 1;
        }
        let mut candidates = Vec::new();
        let mut excluded = Vec::new();
        let mut rejected_ids = HashSet::new();
        for EnumeratedCandidate {
            provider_id,
            object,
        } in offered
        {
            let id = object.id.to_string();
            let reason = if provider_id.trim().is_empty() {
                Some("empty registered provider id; candidate rejected".to_owned())
            } else if counts.get(&id).copied() == Some(1) {
                evaluation_time.map_or_else(
                    || policy.candidate_violation(&object),
                    |evaluation_time| policy.candidate_violation_at(&object, Some(evaluation_time)),
                )
            } else {
                Some("ambiguous candidate id; all candidates with this id rejected".to_owned())
            };
            if let Some(reason) = reason {
                if rejected_ids.insert(id.clone()) {
                    excluded.push(ExcludedEntry {
                        object_id: id,
                        provider: object.source.clone(),
                        reason,
                    });
                }
            } else {
                let Some([origin]) = origins.get_mut(&id).map(Vec::as_mut_slice) else {
                    return Err(KernelPlanError::InvalidPlanOrigin);
                };
                origin.admitted = true;
                candidates.push(object);
            }
        }
        // A denied duplicate may never hide an eligible candidate with the same content.
        dedup_by_content_ref(&mut candidates);
        let set = CandidateSet::new(&candidates, ctx);
        let mut scored: Vec<_> = candidates
            .into_iter()
            .filter_map(|object| {
                let signals = signals_from_object(&object, ctx, &set);
                let phi = self.field.compute_phi(&signals);
                if phi.is_finite() {
                    Some((object, phi))
                } else {
                    excluded.push(ExcludedEntry {
                        object_id: object.id.to_string(),
                        provider: object.source.clone(),
                        reason: "phi is not finite".to_owned(),
                    });
                    None
                }
            })
            .collect();
        let observations = excluded
            .iter()
            .map(|entry| BlockedEntry {
                object_id: entry.object_id.clone(),
                reason: entry.reason.clone(),
            })
            .collect();
        // After the policy observations: leaving out an unrelated source is a
        // utility decision, not a block.
        if self.relevance_floor {
            let texts: Vec<_> = scored
                .iter()
                .map(|(object, _)| (object.title.as_str(), object.content.as_deref()))
                .collect();
            let kept = super::relevance_floor::kept(&texts, &ctx.query);
            let mut keep = kept.into_iter();
            scored.retain(|(object, _)| {
                let relevant = keep.next().unwrap_or(true);
                if !relevant {
                    excluded.push(ExcludedEntry {
                        object_id: object.id.to_string(),
                        provider: object.source.clone(),
                        reason:
                            "unrelated to the task: shares no term with it or a relevant source"
                                .to_owned(),
                    });
                }
                relevant
            });
        }
        let mut budget = ctx.budget;
        if let Some(cap) = policy.budget_cap_tokens {
            budget.total = budget.total.min(cap);
        }
        // Past usage cannot be undone: a lower ceiling permits zero new tokens.
        budget.total = budget.total.max(budget.used);
        let selection = if self.private_selection && scored.len() > 1 && budget.total > budget.used
        {
            use crate::core::intelligence_runtime::context_selection;
            let candidates = scored
                .iter()
                .enumerate()
                .map(|(index, (object, _))| {
                    let signals = signals_from_object(object, ctx, &set);
                    context_selection::Candidate {
                        candidate_id: index,
                        relevance_milli: (signals.relevance.clamp(0.0, 1.0) * 1000.0).round()
                            as u16,
                        confidence_milli: (signals.history_signal.clamp(0.0, 1.0) * 1000.0).round()
                            as u16,
                        tokens: to_compile_candidate(object, 0.0).selected_tokens.max(1),
                        stale: object.freshness.stale,
                    }
                })
                .collect::<Vec<_>>();
            match context_selection::scores(&candidates, budget.total - budget.used) {
                Some(Ok(scores)) => {
                    for ((_, phi), score) in scored.iter_mut().zip(scores) {
                        *phi = score;
                    }
                    Some("private_context_budget_relevance")
                }
                Some(Err(_)) => Some("private_context_unavailable"),
                None => None,
            }
        } else {
            None
        };
        let compile_candidates: Vec<_> = scored
            .iter()
            .map(|(object, phi)| to_compile_candidate(object, *phi))
            .collect();
        let result = compile(&compile_candidates, budget, CompileMode::HandleManifest);

        Ok((
            build_plan(
                ctx,
                policy,
                &scored,
                &result,
                excluded,
                provider_stats,
                origins,
                evaluation_time,
            )?,
            observations,
            selection,
        ))
    }

    /// Restrict an existing plan without replanning or inventing its request scope.
    pub(crate) fn restrict_plan(
        plan: &ContextPlanV1,
        available: impl Fn(&str) -> bool,
    ) -> Result<ContextPlanV1, KernelPlanError> {
        let preconsumed = validate_plan_accounting(plan)?;
        let mut restricted = plan.clone();
        restricted.selected.clear();
        for stat in restricted.provider_stats.values_mut() {
            stat.candidates_selected = 0;
            stat.tokens_used = 0;
        }
        let mut used_tokens = preconsumed;
        for entry in &plan.selected {
            let origin = match plan.origins.get(&entry.object_id).map(Vec::as_slice) {
                Some([origin])
                    if !origin.provider_id.trim().is_empty() && origin.source == entry.provider =>
                {
                    origin
                }
                _ => return Err(KernelPlanError::InvalidPlanOrigin),
            };
            if available(&origin.provider_id) {
                let stat = restricted
                    .provider_stats
                    .get_mut(&entry.provider)
                    .ok_or(KernelPlanError::MissingProviderStats)?;
                stat.candidates_selected += 1;
                stat.tokens_used = stat
                    .tokens_used
                    .checked_add(entry.tokens)
                    .ok_or(KernelPlanError::InvalidPlanAccounting)?;
                used_tokens = used_tokens
                    .checked_add(entry.tokens)
                    .ok_or(KernelPlanError::InvalidPlanAccounting)?;
                restricted.selected.push(entry.clone());
            } else {
                restricted.excluded.push(ExcludedEntry {
                    object_id: entry.object_id.clone(),
                    provider: entry.provider.clone(),
                    reason: "provider unavailable".to_owned(),
                });
            }
        }
        if restricted.selected.len() == plan.selected.len() {
            return Ok(plan.clone());
        }
        restricted
            .selected
            .sort_by(|a, b| a.object_id.cmp(&b.object_id));
        restricted.excluded.sort_by(|a, b| {
            (&a.object_id, &a.provider, &a.reason).cmp(&(&b.object_id, &b.provider, &b.reason))
        });
        restricted.budget.used_tokens = used_tokens;
        restricted.budget.remaining_tokens = restricted.budget.total_tokens - used_tokens;
        let mut material = serde_json::Map::from_iter([
            (
                "schema".to_owned(),
                serde_json::json!("leanctx.context-plan-derivation/v1"),
            ),
            (
                "operation".to_owned(),
                serde_json::json!("provider_degradation"),
            ),
            ("parent_plan_id".to_owned(), serde_json::json!(plan.plan_id)),
            ("intent".to_owned(), serde_json::json!(restricted.intent)),
            ("budget".to_owned(), serde_json::json!(restricted.budget)),
            (
                "deferred".to_owned(),
                serde_json::json!(restricted.deferred),
            ),
        ]);
        material.extend(plan_projection_material(&restricted));
        restricted.plan_id = canonical_plan_id(&serde_json::Value::Object(material).to_string());
        Ok(restricted)
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
        if let Some(outcome_value) = outcome_value
            && total_phi > 0.0
        {
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
            quality_signals: outcome_value
                .into_iter()
                .map(|value| QualitySignal {
                    signal_type: "outcome".to_string(),
                    value,
                })
                .collect(),
            feedback_attribution,
        }
    }
}

fn validate_plan_accounting(plan: &ContextPlanV1) -> Result<usize, KernelPlanError> {
    if plan.plan_id.trim().is_empty()
        || plan.budget.used_tokens > plan.budget.total_tokens
        || plan.budget.remaining_tokens != plan.budget.total_tokens - plan.budget.used_tokens
    {
        return Err(KernelPlanError::InvalidPlanAccounting);
    }
    let mut seen = HashSet::new();
    let mut actual = HashMap::<&str, (usize, usize)>::new();
    let mut selected_tokens = 0usize;
    for entry in &plan.selected {
        if !seen.insert(&entry.object_id) || !entry.phi.is_finite() {
            return Err(KernelPlanError::InvalidPlanAccounting);
        }
        if !plan.provider_stats.contains_key(&entry.provider) {
            return Err(KernelPlanError::MissingProviderStats);
        }
        let (count, tokens) = actual.entry(&entry.provider).or_default();
        *count += 1;
        *tokens = tokens
            .checked_add(entry.tokens)
            .ok_or(KernelPlanError::InvalidPlanAccounting)?;
        selected_tokens = selected_tokens
            .checked_add(entry.tokens)
            .ok_or(KernelPlanError::InvalidPlanAccounting)?;
    }
    for (provider, stat) in &plan.provider_stats {
        let (count, tokens) = actual.get(provider.as_str()).copied().unwrap_or_default();
        if stat.candidates_selected != count
            || stat.tokens_used != tokens
            || stat.candidates_offered < count
        {
            return Err(KernelPlanError::InvalidPlanAccounting);
        }
    }
    plan.budget
        .used_tokens
        .checked_sub(selected_tokens)
        .ok_or(KernelPlanError::InvalidPlanAccounting)
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

/// Neutral value of a signal this candidate set cannot measure.
const UNMEASURED: f64 = 0.5;

/// Signals that depend on the whole admitted candidate set rather than on
/// one object. Each is measured from what the providers supplied; where
/// they supplied nothing to measure against, the signal stays neutral.
struct CandidateSet {
    /// Files this session already delivered (the ledger's candidates).
    delivered: HashSet<String>,
    /// Files the query's search hits point at.
    anchors: HashSet<String>,
    /// Highest term overlap with a more relevant candidate, by object id.
    redundancy: HashMap<String, f64>,
}

impl CandidateSet {
    fn new(candidates: &[ContextObjectV1], ctx: &RetrievalContext) -> Self {
        let paths_of = |source: &str| -> HashSet<String> {
            candidates
                .iter()
                .filter(|object| object.source == source)
                .filter_map(object_path)
                .collect()
        };
        // Rank by relevance, then id, so redundancy does not depend on
        // provider order: each candidate is compared with those ranked above.
        let mut ranked: Vec<(f64, &ContextObjectV1, HashSet<String>)> = candidates
            .iter()
            .map(|object| {
                let mut words = terms(&object.title);
                if let Some(content) = &object.content {
                    words.extend(terms(content));
                }
                (
                    keyword_overlap(&object.title, object.content.as_deref(), &ctx.query),
                    object,
                    words,
                )
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.id.as_str().cmp(right.1.id.as_str()))
        });
        let redundancy = ranked
            .iter()
            .enumerate()
            .map(|(index, (_, object, words))| {
                let overlap = ranked[..index]
                    .iter()
                    .map(|(_, _, other)| jaccard(words, other))
                    .fold(0.0, f64::max);
                (object.id.to_string(), overlap)
            })
            .collect();
        Self {
            delivered: paths_of("context.ledger"),
            anchors: paths_of("index.bm25"),
            redundancy,
        }
    }

    /// Already delivered content carries no news; otherwise unknown.
    fn surprise(&self, object: &ContextObjectV1) -> f64 {
        let delivered = object.source == "context.ledger"
            || object_path(object).is_some_and(|path| self.delivered.contains(&path));
        if delivered { 0.0 } else { UNMEASURED }
    }

    /// Closeness to the files the query hit: a hit itself, a graph neighbour
    /// by its link weight, anything else unknown.
    fn graph_proximity(&self, object: &ContextObjectV1) -> f64 {
        if object.source == "index.graph" {
            return f64::from(object.confidence.clamp(0.0, 1.0));
        }
        match object_path(object) {
            Some(path) if self.anchors.contains(&path) => 1.0,
            _ => UNMEASURED,
        }
    }
}

/// The file an object refers to, if any.
fn object_path(object: &ContextObjectV1) -> Option<String> {
    if let Some(path) = object.metadata.get("path") {
        return Some(path.clone());
    }
    let reference = object.content_ref.strip_prefix("file:")?;
    Some(reference.split('#').next().unwrap_or(reference).to_owned())
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(right).count() as f64 / union as f64
}

fn signals_from_object(
    object: &ContextObjectV1,
    ctx: &RetrievalContext,
    set: &CandidateSet,
) -> FieldSignals {
    FieldSignals {
        relevance: keyword_overlap(&object.title, object.content.as_deref(), &ctx.query),
        surprise: set.surprise(object),
        graph_proximity: set.graph_proximity(object),
        history_signal: object.confidence.clamp(0.0, 1.0) as f64,
        token_cost_norm: normalize_token_cost(object.token_estimate, ctx.budget.total),
        redundancy: set
            .redundancy
            .get(object.id.as_str())
            .copied()
            .unwrap_or(0.0),
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
    policy: &ContextPolicy,
    scored: &[(ContextObjectV1, f64)],
    result: &CompileResult,
    mut excluded: Vec<ExcludedEntry>,
    mut provider_stats: HashMap<String, ProviderStat>,
    origins: BTreeMap<String, Vec<ContextOriginV1>>,
    evaluation_time: Option<&DateTime<FixedOffset>>,
) -> Result<ContextPlanV1, KernelPlanError> {
    let objects: HashMap<_, _> = scored
        .iter()
        .map(|(object, phi)| (object.id.to_string(), (object, *phi)))
        .collect();
    let mut selected = Vec::with_capacity(result.selected.len());
    for item in &result.selected {
        let (object, phi) = objects
            .get(&item.id)
            .copied()
            .ok_or(KernelPlanError::CompilerSelectedUnknownCandidate)?;
        let stat = provider_stats
            .get_mut(&object.source)
            .ok_or(KernelPlanError::MissingProviderStats)?;
        stat.candidates_selected += 1;
        stat.tokens_used = stat.tokens_used.saturating_add(item.tokens);
        selected.push(PlanEntry {
            object_id: item.id.clone(),
            provider: object.source.clone(),
            view: item.view.clone(),
            tokens: item.tokens,
            phi,
            reason: selected_display(object).unwrap_or_default(),
        });
    }
    excluded.extend(result.excluded_reasons.iter().map(|item| ExcludedEntry {
        object_id: item.id.clone(),
        provider: objects.get(&item.id).map_or_else(
            || "unknown".to_string(),
            |(object, _)| object.source.clone(),
        ),
        reason: item.reason.clone(),
    }));
    selected.sort_by(|a, b| a.object_id.cmp(&b.object_id));
    excluded.sort_by(|a, b| a.object_id.cmp(&b.object_id));
    let used_tokens = ctx.budget.used.saturating_add(result.budget_used);
    let mut plan = ContextPlanV1 {
        plan_id: String::new(),
        intent: ctx.task.clone().unwrap_or_else(|| ctx.query.clone()),
        budget: PlanBudget {
            total_tokens: result.budget_total,
            used_tokens,
            remaining_tokens: result.budget_total.saturating_sub(used_tokens),
        },
        selected,
        excluded,
        deferred: Vec::new(),
        provider_stats,
        origins,
    };
    let material = plan_material(ctx, policy, scored, result, &plan, evaluation_time);
    plan.plan_id = canonical_plan_id(&material);
    Ok(plan)
}

fn origins_from_candidates(
    candidates: &[EnumeratedCandidate],
) -> BTreeMap<String, Vec<ContextOriginV1>> {
    let mut origins: BTreeMap<String, Vec<ContextOriginV1>> = BTreeMap::new();
    for candidate in candidates {
        origins
            .entry(candidate.object.id.to_string())
            .or_default()
            .push(ContextOriginV1 {
                provider_id: candidate.provider_id.clone(),
                source: candidate.object.source.clone(),
                content_ref: candidate.object.content_ref.clone(),
                sensitivity: candidate.object.sensitivity,
                provenance: candidate.object.provenance.clone(),
                admitted: false,
            });
    }
    for records in origins.values_mut() {
        records.sort_by(|left, right| {
            left.provider_id
                .cmp(&right.provider_id)
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.content_ref.cmp(&right.content_ref))
                .then_with(|| {
                    sensitivity_rank(left.sensitivity).cmp(&sensitivity_rank(right.sensitivity))
                })
                .then_with(|| left.provenance.tool.cmp(&right.provenance.tool))
                .then_with(|| left.provenance.agent_id.cmp(&right.provenance.agent_id))
                .then_with(|| {
                    left.provenance
                        .client_name
                        .cmp(&right.provenance.client_name)
                })
                .then_with(|| left.provenance.timestamp.cmp(&right.provenance.timestamp))
        });
    }
    origins
}

fn sensitivity_rank(sensitivity: super::types::SensitivityLevel) -> u8 {
    match sensitivity {
        super::types::SensitivityLevel::Public => 0,
        super::types::SensitivityLevel::Internal => 1,
        super::types::SensitivityLevel::Confidential => 2,
        super::types::SensitivityLevel::Restricted => 3,
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
    policy: &ContextPolicy,
    scored: &[(ContextObjectV1, f64)],
    result: &CompileResult,
    plan: &ContextPlanV1,
    evaluation_time: Option<&DateTime<FixedOffset>>,
) -> String {
    let mut entries: Vec<_> = scored
        .iter()
        .map(|(object, phi)| {
            (
                object.id.as_str(),
                object.content_ref.as_str(),
                phi.to_bits(),
            )
        })
        .collect();
    entries.sort_unstable();
    // V2 identity material is framed and binds request scope plus the final plan.
    // Existing V1 plans remain readable; their opaque IDs are never rewritten.
    let mut material = serde_json::Map::from_iter([
        (
            "schema".to_owned(),
            serde_json::json!("leanctx.context-plan-identity/v2"),
        ),
        ("query".to_owned(), serde_json::json!(ctx.query)),
        ("task".to_owned(), serde_json::json!(ctx.task)),
        (
            "project_root".to_owned(),
            serde_json::json!(ctx.project_root),
        ),
        (
            "requested_budget".to_owned(),
            serde_json::json!([ctx.budget.total, ctx.budget.used]),
        ),
        ("policy".to_owned(), serde_json::json!(policy)),
        (
            "compiled_budget".to_owned(),
            serde_json::json!([result.budget_total, result.budget_used]),
        ),
        ("candidates".to_owned(), serde_json::json!(entries)),
    ]);
    material.extend(plan_projection_material(plan));
    if policy.retention_days.is_some() {
        material.insert(
            "evaluation_time".to_owned(),
            serde_json::json!(evaluation_time.map(DateTime::to_rfc3339)),
        );
    }
    serde_json::Value::Object(material).to_string()
}

/// Hash one canonical framed plan representation for every kernel-owned plan.
fn canonical_plan_id(material: &str) -> String {
    format!("plan_{}", blake3::hash(material.as_bytes()).to_hex())
}

fn plan_projection_material(plan: &ContextPlanV1) -> [(String, serde_json::Value); 4] {
    [
        ("selected".to_owned(), serde_json::json!(plan.selected)),
        ("excluded".to_owned(), serde_json::json!(plan.excluded)),
        (
            "provider_stats".to_owned(),
            serde_json::json!(plan.provider_stats.iter().collect::<BTreeMap<_, _>>()),
        ),
        ("origins".to_owned(), serde_json::json!(plan.origins)),
    ]
}

fn short_hash(value: &str) -> String {
    // Preserve legacy receipt lookup/replay keys; plan identity is versioned separately.
    blake3::hash(value.as_bytes()).to_hex()[..16].to_string()
}

fn outcome_value(outcome: ReceiptOutcome) -> Option<f64> {
    match outcome {
        ReceiptOutcome::Accepted => Some(1.0),
        ReceiptOutcome::Partial => Some(0.5),
        ReceiptOutcome::Rejected => Some(0.0),
        ReceiptOutcome::Unknown => None,
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
#[path = "orchestrator_tests.rs"]
pub mod tests;
