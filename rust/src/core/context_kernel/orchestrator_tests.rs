// SPDX-License-Identifier: Apache-2.0

use super::super::types::{Freshness, SensitivityLevel, SideEffectPolicy};
use std::collections::{BTreeMap, HashMap};

use crate::core::context_field::{ContextItemId, FieldWeights, Provenance, TokenBudget, ViewCosts};

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

struct RegisteredProvider {
    id: String,
    items: Vec<ContextObjectV1>,
}

impl CandidateProvider for RegisteredProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    fn candidates(&self, _ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        self.items.clone()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

fn registered_kernel(provider_id: &str, item: ContextObjectV1) -> ContextKernel {
    ContextKernel::new(vec![Box::new(RegisteredProvider {
        id: provider_id.to_owned(),
        items: vec![item],
    })])
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
fn retention_rejects_before_content_dedup_and_budget_spend() {
    let mut stale = object("stale", "same-content", 1.0);
    stale.freshness.created_at = "2026-09-12T00:00:00Z".to_owned();
    let mut fresh = object("fresh", "same-content", 0.1);
    fresh.freshness.created_at = "2026-09-13T23:59:59Z".to_owned();
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![stale, fresh.clone()],
    })]);
    let mut retrieval = context();
    retrieval.budget.total = 50;
    let evaluation =
        DateTime::parse_from_rfc3339("2026-09-14T00:00:00Z").expect("valid evaluation time");
    let policy = ContextPolicy {
        retention_days: Some(2),
        ..ContextPolicy::default()
    };

    let (plan, observations) = kernel
        .plan_with_policy_at(&retrieval, &policy, Some(&evaluation))
        .expect("valid retention plan");

    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].object_id, fresh.id.to_string());
    assert_eq!(plan.budget.used_tokens, 50);
    assert!(
        observations
            .iter()
            .any(|observation| observation.object_id.contains("stale"))
    );
    assert!(
        observations
            .iter()
            .all(|observation| !observation.reason.contains("2026-09"))
    );
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
fn object_signals_are_normalized_and_measured_from_the_candidate_set() {
    let lone = object("one", "reference", 0.8);
    let signals = signals_from_object(
        &lone,
        &context(),
        &CandidateSet::new(std::slice::from_ref(&lone), &context()),
    );
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
    assert_eq!(
        (
            signals.surprise,
            signals.graph_proximity,
            signals.redundancy
        ),
        (0.5, 0.5, 0.0),
        "nothing to measure against stays neutral"
    );

    let mut delivered = object("ledger", "src/a.rs", 1.0);
    delivered.source = "context.ledger".to_owned();
    delivered
        .metadata
        .insert("path".to_owned(), "src/a.rs".to_owned());
    let mut hit = object("hit", "file:src/b.rs#L1-9", 0.9);
    hit.source = "index.bm25".to_owned();
    hit.metadata
        .insert("path".to_owned(), "src/b.rs".to_owned());
    let mut neighbour = object("near", "file:src/c.rs", 0.7);
    neighbour.source = "index.graph".to_owned();
    neighbour.title = "unrelated words".to_owned();
    neighbour.content = None;
    let again = object("again", "file:src/a.rs", 0.9);
    let set_objects = [
        delivered.clone(),
        hit.clone(),
        neighbour.clone(),
        again.clone(),
    ];
    let set = CandidateSet::new(&set_objects, &context());
    let ctx = context();
    assert_eq!(
        signals_from_object(&again, &ctx, &set).surprise,
        0.0,
        "already delivered"
    );
    assert_eq!(signals_from_object(&hit, &ctx, &set).surprise, 0.5);
    assert_eq!(
        signals_from_object(&hit, &ctx, &set).graph_proximity,
        1.0,
        "a search hit"
    );
    assert!((signals_from_object(&neighbour, &ctx, &set).graph_proximity - 0.7).abs() < 1e-6);
    // Identical text: one of the two duplicates is redundant, which one
    // depends only on relevance and id, never on provider order.
    let redundancy = |objects: &[ContextObjectV1]| {
        let set = CandidateSet::new(objects, &ctx);
        objects
            .iter()
            .map(|object| {
                (
                    object.id.to_string(),
                    signals_from_object(object, &ctx, &set).redundancy,
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let forward = redundancy(&set_objects);
    let mut reversed = set_objects.to_vec();
    reversed.reverse();
    assert_eq!(forward, redundancy(&reversed));
    assert_eq!(forward.values().filter(|value| **value == 1.0).count(), 2);
    assert_eq!(forward[&neighbour.id.to_string()], 0.0);
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

fn compiler_result(selected_id: &str) -> CompileResult {
    CompileResult {
        run_id: "test-run".to_owned(),
        mode: "handle_manifest".to_owned(),
        budget_total: 200,
        budget_used: 1,
        items_considered: 1,
        items_selected: 1,
        items_excluded: 0,
        items_pinned: 0,
        selected: vec![crate::core::context_compiler::SelectedItem {
            id: selected_id.to_owned(),
            path: "private/compiler/path".to_owned(),
            view: "full".to_owned(),
            tokens: 1,
            phi: 1.0,
            pinned: false,
        }],
        excluded_reasons: Vec::new(),
        warnings: Vec::new(),
    }
}

#[test]
fn compiler_unknown_candidate_returns_sanitized_typed_error() {
    let candidate = object("known", "private/candidate-content", 1.0);
    let selected_id = "private/compiler-selected-id";
    let error = build_plan(
        &context(),
        &ContextPolicy::default(),
        &[(candidate.clone(), 1.0)],
        &compiler_result(selected_id),
        Vec::new(),
        HashMap::new(),
        BTreeMap::new(),
        None,
    )
    .expect_err("unknown compiler selection must fail closed");

    assert_eq!(error, KernelPlanError::CompilerSelectedUnknownCandidate);
    let display = error.to_string();
    assert_eq!(display, "compiler selected an unknown candidate");
    assert!(!display.contains(selected_id));
    assert!(!display.contains(&candidate.content_ref));
    assert!(!display.contains(&candidate.source));
}

#[test]
fn missing_provider_stats_returns_sanitized_typed_error() {
    let candidate = object("known", "private/provider-content", 1.0);
    let result = compiler_result(&candidate.id.to_string());
    let error = build_plan(
        &context(),
        &ContextPolicy::default(),
        &[(candidate.clone(), 1.0)],
        &result,
        Vec::new(),
        HashMap::new(),
        BTreeMap::new(),
        None,
    )
    .expect_err("missing provider stats must fail closed");

    assert_eq!(error, KernelPlanError::MissingProviderStats);
    let display = error.to_string();
    assert_eq!(
        display,
        "provider statistics are missing for a selected candidate"
    );
    assert!(!display.contains(&candidate.id.to_string()));
    assert!(!display.contains(&candidate.content_ref));
    assert!(!display.contains(&candidate.source));
}

#[test]
fn empty_plan_preserves_budget() {
    let plan = ContextKernel::new(Vec::new())
        .plan(&context())
        .expect("valid empty plan");
    assert!(plan.selected.is_empty());
    assert_eq!(plan.budget.total_tokens, 200);
    assert_eq!(plan.budget.used_tokens, 0);
    assert_eq!(plan.budget.remaining_tokens, 200);
}

#[test]
fn unconfigured_retention_preserves_existing_identity_material() {
    let ctx = context();
    let policy = ContextPolicy::default();
    let kernel = ContextKernel::new(Vec::new());
    let time = DateTime::parse_from_rfc3339("2026-09-14T00:00:00Z").expect("fixed time");
    let (plan, _) = kernel
        .plan_with_policy_at(&ctx, &policy, Some(&time))
        .expect("valid identity plan");
    let original_material = serde_json::json!({
        "schema": "leanctx.context-plan-identity/v2",
        "query": ctx.query,
        "task": ctx.task,
        "project_root": ctx.project_root,
        "requested_budget": [200, 0],
        "policy": policy,
        "compiled_budget": [200, 0],
        "candidates": [], "selected": [], "excluded": [],
        "provider_stats": {}, "origins": {},
    })
    .to_string();
    assert_eq!(
        plan.plan_id,
        format!(
            "plan_{}",
            blake3::hash(original_material.as_bytes()).to_hex()
        )
    );
    assert_eq!(
        plan.plan_id,
        kernel.plan(&ctx).expect("valid identity plan").plan_id
    );
}

#[test]
fn plan_selects_high_phi_candidate() {
    let low = object("low", "low", 0.1);
    let mut high = object("high", "high", 1.0);
    high.title = "context kernel context kernel".to_string();
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![low, high.clone()],
    })]);

    let plan = kernel.plan(&context()).expect("valid selected plan");
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
            .plan(&context())
            .expect("valid relevance plan");
    let zero_plan =
        ContextKernel::with_field(providers(), ContextField::with_weights(weights(0.0)))
            .plan(&context())
            .expect("valid zero-weight plan");

    assert_eq!(relevance_plan.selected.len(), 1);
    assert_eq!(zero_plan.selected.len(), 1);
    assert_eq!(relevance_plan.selected[0].phi, 1.0);
    assert_eq!(zero_plan.selected[0].phi, 0.0);
}

#[test]
fn plan_identity_preserves_request_field_boundaries() {
    let kernel = ContextKernel::new(Vec::new());
    let mut first = context();
    first.query = "a".to_owned();
    first.task = Some("b|c".to_owned());
    let mut second = first.clone();
    second.query = "a|b".to_owned();
    second.task = Some("c".to_owned());
    assert_ne!(
        kernel.plan(&first).expect("valid first plan").plan_id,
        kernel.plan(&second).expect("valid second plan").plan_id
    );
    second = first.clone();
    second.project_root = "/another-project".to_owned();
    assert_ne!(
        kernel.plan(&first).expect("valid first plan").plan_id,
        kernel.plan(&second).expect("valid second plan").plan_id
    );
    second = first.clone();
    second.budget.used = 1;
    assert_ne!(
        kernel.plan(&first).expect("valid first plan").plan_id,
        kernel.plan(&second).expect("valid second plan").plan_id
    );
}

#[test]
fn plan_identity_binds_provider_offer_accounting() {
    let high = object("high", "same-content", 1.0);
    let low = object("low", "same-content", 0.1);
    let plan = |items| {
        ContextKernel::new(vec![Box::new(MockProvider { items })])
            .plan(&context())
            .expect("valid provider-accounting plan")
    };
    let single = plan(vec![high.clone()]);
    let duplicate_content = plan(vec![high, low]);
    assert_eq!(
        single.selected[0].object_id,
        duplicate_content.selected[0].object_id
    );
    assert_eq!(single.provider_stats["test.mock"].candidates_offered, 1);
    assert_eq!(
        duplicate_content.provider_stats["test.mock"].candidates_offered,
        2
    );
    assert_ne!(single.plan_id, duplicate_content.plan_id);
}

#[test]
fn registered_provider_id_binds_without_rewriting_legacy_alias_or_origin_fields() {
    let mut candidate = object("one", "alias-ref", 1.0);
    candidate.id = ContextItemId::from_provider("alias", "one");
    candidate.source = "alias".to_owned();
    candidate.sensitivity = SensitivityLevel::Public;
    candidate.provenance = Provenance {
        tool: Some("tool".to_owned()),
        agent_id: Some("agent".to_owned()),
        client_name: Some("client".to_owned()),
        timestamp: Some("stable".to_owned()),
    };
    let plan = registered_kernel("registered.authority", candidate.clone())
        .plan(&context())
        .expect("valid registered-provider plan");

    assert_eq!(plan.selected[0].provider, "alias");
    assert_eq!(plan.provider_stats["alias"].candidates_offered, 1);
    let origins = &plan.origins[&candidate.id.to_string()];
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0].provider_id, "registered.authority");
    assert_eq!(origins[0].source, "alias");
    assert_eq!(origins[0].content_ref, "alias-ref");
    assert_eq!(origins[0].sensitivity, SensitivityLevel::Public);
    assert_eq!(origins[0].provenance.tool.as_deref(), Some("tool"));

    let task_id = lean_ctx_protocol::TaskId::new("task-origin").expect("valid test id");
    let projection = super::super::projection::project_context_plan(task_id, &plan)
        .expect("origin remains internal to semantic plan");
    let wire = serde_json::to_value(&projection).expect("serialize public projection");
    assert!(wire.get("origins").is_none());
    projection.validate().expect("public projection validates");
}

#[test]
fn registered_provider_identity_changes_plan_identity_under_one_alias() {
    let mut candidate = object("one", "alias-ref", 1.0);
    candidate.id = ContextItemId::from_provider("alias", "one");
    candidate.source = "alias".to_owned();
    candidate.sensitivity = SensitivityLevel::Public;
    let first = registered_kernel("registered.first", candidate.clone())
        .plan(&context())
        .expect("valid first registered-provider plan");
    let second = registered_kernel("registered.second", candidate)
        .plan(&context())
        .expect("valid second registered-provider plan");

    assert_eq!(first.selected[0].provider, "alias");
    assert_eq!(second.selected[0].provider, "alias");
    assert_ne!(first.plan_id, second.plan_id);
    assert_eq!(
        first.origins.values().next().unwrap()[0].provider_id,
        "registered.first"
    );
    assert_eq!(
        second.origins.values().next().unwrap()[0].provider_id,
        "registered.second"
    );
}

#[test]
fn empty_registered_provider_id_is_rejected_before_selection() {
    let mut candidate = object("one", "alias-ref", 1.0);
    candidate.id = ContextItemId::from_provider("alias", "one");
    candidate.source = "alias".to_owned();
    candidate.sensitivity = SensitivityLevel::Public;
    let (plan, observations) = registered_kernel("", candidate.clone())
        .plan_with_policy(&context(), &ContextPolicy::default())
        .expect("valid rejected-provider plan");

    assert!(plan.selected.is_empty());
    assert_eq!(observations.len(), 1);
    assert!(
        observations[0]
            .reason
            .contains("empty registered provider id")
    );
    assert_eq!(plan.origins[&candidate.id.to_string()][0].provider_id, "");
}

#[test]
fn conflicting_origins_are_all_retained_while_ambiguous_id_is_rejected() {
    let mut first = object("duplicate", "first-ref", 1.0);
    first.id = ContextItemId::from_provider("alias", "duplicate");
    first.source = "alias".to_owned();
    first.sensitivity = SensitivityLevel::Public;
    let mut second = first.clone();
    second.content_ref = "second-ref".to_owned();
    second.provenance.tool = Some("second-tool".to_owned());
    let kernel = ContextKernel::new(vec![
        Box::new(RegisteredProvider {
            id: "registered.first".to_owned(),
            items: vec![first.clone()],
        }),
        Box::new(RegisteredProvider {
            id: "registered.second".to_owned(),
            items: vec![second],
        }),
    ]);

    let (plan, observations) = kernel
        .plan_with_policy(&context(), &ContextPolicy::default())
        .expect("valid ambiguous-origin plan");
    let origins = &plan.origins[&first.id.to_string()];
    assert_eq!(origins.len(), 2);
    assert!(plan.selected.is_empty());
    assert_eq!(plan.excluded.len(), 1);
    assert!(plan.excluded[0].reason.contains("ambiguous"));
    assert_eq!(observations.len(), 1);
}

#[test]
fn legacy_plan_without_origin_map_remains_readable() {
    let mut candidate = object("one", "alias-ref", 1.0);
    candidate.id = ContextItemId::from_provider("alias", "one");
    candidate.source = "alias".to_owned();
    candidate.sensitivity = SensitivityLevel::Public;
    let plan = registered_kernel("registered.authority", candidate)
        .plan(&context())
        .expect("valid legacy-readable plan");
    let mut value = serde_json::to_value(&plan).expect("serialize semantic plan");
    value
        .as_object_mut()
        .expect("plan is an object")
        .remove("origins");
    let decoded: ContextPlanV1 = serde_json::from_value(value).expect("legacy plan decodes");
    assert!(decoded.origins.is_empty());
}

#[test]
fn nonfinite_scoring_fails_closed_before_compilation() {
    let kernel = ContextKernel::with_field(
        vec![Box::new(MockProvider {
            items: vec![object("one", "ref", 1.0)],
        })],
        ContextField::with_weights(FieldWeights {
            w_relevance: f64::NAN,
            ..FieldWeights::default()
        }),
    );
    let (plan, observations) = kernel
        .plan_with_policy(&context(), &ContextPolicy::default())
        .expect("valid nonfinite-score plan");
    assert!(plan.selected.is_empty());
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].reason, "phi is not finite");
    assert_eq!(plan.provider_stats["test.mock"].candidates_selected, 0);
}

#[test]
fn empty_kernel_preserves_already_consumed_budget() {
    let mut request = context();
    request.budget.used = 75;
    let plan = ContextKernel::new(Vec::new())
        .plan(&request)
        .expect("valid consumed-budget plan");
    assert_eq!(plan.budget.used_tokens, 75);
    assert_eq!(plan.budget.remaining_tokens, 125);
}

#[test]
fn ambiguous_candidate_ids_are_not_selected_or_misattributed() {
    let first = object("duplicate", "first-reference", 1.0);
    let mut second = first.clone();
    second.content_ref = "second-reference".to_owned();
    second.content = Some("conflicting context".to_owned());
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![first.clone(), second],
    })]);
    let (plan, observations) = kernel
        .plan_with_policy(&context(), &ContextPolicy::default())
        .expect("valid ambiguous-candidate plan");
    assert!(
        plan.selected.is_empty(),
        "ambiguous identity reached compilation"
    );
    assert_eq!(observations.len(), 1, "integrity denials remain observable");
    assert_eq!(observations[0].reason, plan.excluded[0].reason);
    assert_eq!(
        plan.excluded.len(),
        1,
        "wire source references must stay unique"
    );
    assert_eq!(plan.excluded[0].object_id, first.id.to_string());
    assert!(plan.excluded[0].reason.contains("ambiguous"));
    assert_eq!(plan.provider_stats["test.mock"].candidates_selected, 0);
    let task_id = lean_ctx_protocol::TaskId::new("task-arch02").expect("valid test id");
    super::super::projection::project_context_plan(task_id, &plan)
        .expect("one unambiguous exclusion remains projectable");
}

#[test]
fn policy_precedes_content_dedup_and_caps_the_remaining_allocation() {
    for (source, sensitivity) in [
        ("blocked", SensitivityLevel::Internal),
        ("test.mock", SensitivityLevel::Restricted),
    ] {
        let mut allowed = object("allowed", "same-content", 0.5);
        allowed.view_costs = ViewCosts::new();
        allowed.view_costs.set(ViewKind::Full, 20);
        allowed.token_estimate = 20;
        let mut denied = allowed.clone();
        denied.id = ContextItemId::from_provider("test.mock", "denied");
        denied.source = source.to_owned();
        denied.sensitivity = sensitivity;
        denied.confidence = 1.0;
        let kernel = ContextKernel::new(vec![Box::new(MockProvider {
            items: vec![denied, allowed.clone()],
        })]);
        let mut request = context();
        request.budget = TokenBudget {
            total: 100,
            used: 70,
        };
        let policy = ContextPolicy {
            blocked_sources: vec!["blocked".to_owned()],
            budget_cap_tokens: Some(90),
            ..ContextPolicy::default()
        };
        let (plan, observations) = kernel
            .plan_with_policy(&request, &policy)
            .expect("valid policy plan");
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].object_id, allowed.id.to_string());
        assert_eq!(plan.budget.total_tokens, 90);
        assert_eq!(plan.budget.used_tokens, 90);
        assert_eq!(plan.budget.remaining_tokens, 0);
        assert_eq!(observations.len(), 1);
    }
}

#[test]
fn admission_marks_allowed_duplicate_but_not_policy_blocked_origin() {
    let allowed = object("allowed", "same-content", 0.5);
    let mut blocked = allowed.clone();
    blocked.id = ContextItemId::from_provider("test.mock", "blocked");
    blocked.source = "blocked".to_owned();
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![allowed.clone(), blocked.clone()],
    })]);
    let policy = ContextPolicy {
        blocked_sources: vec!["blocked".to_owned()],
        ..ContextPolicy::default()
    };

    let (plan, observations) = kernel
        .plan_with_policy(&context(), &policy)
        .expect("valid admission plan");

    assert_eq!(plan.selected[0].object_id, allowed.id.to_string());
    assert!(plan.origins[&allowed.id.to_string()][0].admitted);
    assert!(!plan.origins[&blocked.id.to_string()][0].admitted);
    assert_eq!(observations.len(), 1);
    let mut legacy = serde_json::to_value(&plan.origins[&allowed.id.to_string()][0]).unwrap();
    legacy.as_object_mut().unwrap().remove("admitted");
    let restored: ContextOriginV1 = serde_json::from_value(legacy).unwrap();
    assert!(!restored.admitted);
}

#[test]
fn exhausted_policy_cap_cannot_allocate_or_erase_prior_usage() {
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![object("one", "ref", 1.0)],
    })]);
    let mut request = context();
    request.budget = TokenBudget {
        total: 100,
        used: 70,
    };
    let policy = ContextPolicy {
        budget_cap_tokens: Some(50),
        ..ContextPolicy::default()
    };
    let (plan, _) = kernel
        .plan_with_policy(&request, &policy)
        .expect("valid exhausted-cap plan");
    assert!(plan.selected.is_empty());
    assert_eq!(plan.budget.total_tokens, 70);
    assert_eq!(plan.budget.used_tokens, 70);
    assert_eq!(plan.budget.remaining_tokens, 0);
}

#[test]
fn kernel_cap_accounts_selected_views_not_legacy_prefix_estimates() {
    let mut large = object("large", "large", 1.0);
    large.token_estimate = 100;
    large.view_costs = ViewCosts::new();
    large.view_costs.set(ViewKind::Full, 100);
    let mut cheap = object("cheap", "cheap", 0.5);
    cheap.token_estimate = 5;
    cheap.view_costs = ViewCosts::new();
    cheap.view_costs.set(ViewKind::Full, 5);
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![large, cheap.clone()],
    })]);
    let policy = ContextPolicy {
        budget_cap_tokens: Some(5),
        ..ContextPolicy::default()
    };
    let (plan, _) = kernel
        .plan_with_policy(&context(), &policy)
        .expect("valid selected-view plan");
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].object_id, cheap.id.to_string());
    assert_eq!(plan.budget.used_tokens, 5);
}

#[test]
fn legacy_receipt_identity_keeps_its_original_lookup_key() {
    let mut plan = ContextPlanV1::empty("legacy", context().budget);
    plan.plan_id = "legacy-plan".to_owned();
    let receipt = ContextKernel::new(Vec::new()).record_receipt(&plan, 7, ReceiptOutcome::Accepted);
    let original_digest = blake3::hash(b"legacy-plan|7|accepted");
    assert_eq!(
        receipt.receipt_id,
        format!("receipt_{}", &original_digest.to_hex()[..16])
    );
}

#[test]
fn receipt_references_plan() {
    let plan = ContextKernel::new(Vec::new())
        .plan(&context())
        .expect("valid receipt plan");
    let receipt = ContextKernel::new(Vec::new()).record_receipt(&plan, 0, ReceiptOutcome::Accepted);
    assert_eq!(receipt.plan_id, plan.plan_id);
}

#[test]
fn unknown_receipt_has_no_quality_signal_or_feedback_attribution() {
    let kernel = ContextKernel::new(vec![Box::new(MockProvider {
        items: vec![object("observed", "observed", 1.0)],
    })]);
    let plan = kernel.plan(&context()).unwrap();
    assert_eq!(plan.selected.len(), 1);
    let receipt = kernel.record_receipt(&plan, 10, ReceiptOutcome::Unknown);
    assert_eq!(receipt.plan_id, plan.plan_id);
    assert_eq!(receipt.delivered_tokens, 10);
    assert_eq!(receipt.outcome, ReceiptOutcome::Unknown);
    assert!(receipt.quality_signals.is_empty());
    assert!(receipt.feedback_attribution.is_empty());
}
