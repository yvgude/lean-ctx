// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeSet, HashMap};

use crate::core::{
    context_field::{ContextItemId, Provenance, TokenBudget, ViewCosts},
    context_kernel::types::{
        CandidateProvider, ContextObjectKind, ContextObjectV1, Freshness, SensitivityLevel,
        SideEffectPolicy,
    },
};

use super::*;

struct Provider {
    registered_id: &'static str,
}

impl CandidateProvider for Provider {
    fn provider_id(&self) -> &'static str {
        self.registered_id
    }

    fn candidates(&self, _ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        vec![ContextObjectV1 {
            id: ContextItemId::from_file("src/lib.rs"),
            kind: ContextObjectKind::File,
            source: "files".to_owned(),
            content_ref: "src/lib.rs".to_owned(),
            title: "fix parser bug".to_owned(),
            content: Some("parser validation".to_owned()),
            freshness: Freshness::default(),
            confidence: 1.0,
            sensitivity: SensitivityLevel::Internal,
            token_estimate: 40,
            view_costs: ViewCosts::from_full_tokens(40),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }]
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

fn kernel() -> ContextKernel {
    kernel_with_registered_id("files")
}

fn kernel_with_registered_id(registered_id: &'static str) -> ContextKernel {
    ContextKernel::with_field(
        vec![Box::new(Provider { registered_id })],
        ContextField::with_weights(FieldWeights::default()),
    )
}

pub(super) fn controller() -> AutopilotController {
    AutopilotController::with_kernels(kernel(), kernel())
}

fn controller_with_registered_id(registered_id: &'static str) -> AutopilotController {
    AutopilotController::with_kernels(
        kernel_with_registered_id(registered_id),
        kernel_with_registered_id(registered_id),
    )
}

pub(super) fn input() -> AutopilotInput {
    AutopilotInput {
        retrieval: RetrievalContext {
            query: "fix parser bug".to_owned(),
            task: Some("fix github bug".to_owned()),
            project_root: "/project".to_owned(),
            budget: TokenBudget {
                total: 200,
                used: 0,
            },
            max_candidates: 10,
        },
        evaluation_time: None,
        task_class: TaskClass::BugFix,
        entitled_to_adaptive: true,
        confidence_milli: 900,
        configured_mode: None,
        default_mode: "map".to_owned(),
        security_forced_mode: None,
        overrides: UserOverrides::default(),
        policy: ContextPolicy::default(),
        kernel_mode: KernelMode::Enforce,
        economics: AutopilotEconomics {
            baseline_cost_micros: 1_000,
            candidate_cost_micros: 500,
            expected_quality_value_micros: 100,
            ..AutopilotEconomics::default()
        },
        learning: AdaptiveLearningState {
            learned_mode: Some("signatures".to_owned()),
            ..AdaptiveLearningState::default()
        },
        available_providers: vec!["github".to_owned()],
        local_providers: BTreeSet::new(),
        cached_preloads: BTreeSet::new(),
        preload_budget: PreloadBudget {
            max_items: 2,
            max_tokens: 1_000,
            tokens_per_item: 500,
            cost_per_item_micros: 10,
            value_per_hit_micros: 1_000,
            allow_remote_queries: true,
            remote_query_consent: true,
        },
        context_policy: None,
    }
}

#[test]
fn community_plan_is_byte_deterministic_and_ignores_learning() {
    let mut request = input();
    request.entitled_to_adaptive = false;
    let first = controller()
        .plan(&request, None)
        .expect("valid first community plan");
    request.learning.learned_mode = Some("full".to_owned());
    let second = controller()
        .plan(&request, None)
        .expect("valid second community plan");

    assert_eq!(
        first.canonical_bytes().unwrap(),
        second.canonical_bytes().unwrap()
    );
    assert_eq!(first.tier, PlannerTier::Community);
}

#[test]
fn canonical_autopilot_binds_origin_without_extending_public_projection() {
    let mut request = input();
    request.entitled_to_adaptive = false;
    let first = controller_with_registered_id("registered.first")
        .plan(&request, None)
        .expect("valid first registered-provider plan");
    let second = controller_with_registered_id("registered.second")
        .plan(&request, None)
        .expect("valid second registered-provider plan");

    assert_ne!(first.context_plan.plan_id, second.context_plan.plan_id);
    assert_ne!(
        first
            .canonical_bytes()
            .expect("first canonical decision serializes"),
        second
            .canonical_bytes()
            .expect("second canonical decision serializes")
    );
    let canonical: serde_json::Value = serde_json::from_slice(
        &first
            .canonical_bytes()
            .expect("canonical decision serializes"),
    )
    .expect("canonical decision is JSON");
    assert!(canonical["context_plan"]["origins"].is_object());

    let task_id = TaskId::new("task-origin-projection").expect("valid task id");
    let handoff = first.bind_task(task_id).expect("bind origin decision");
    let projection =
        serde_json::to_value(handoff.context_projection()).expect("public projection serializes");
    assert!(projection.get("origins").is_none());
    handoff
        .context_projection()
        .validate()
        .expect("public projection validates");
}

#[test]
fn task_handoff_projects_the_authoritative_plan_for_both_tiers() {
    for adaptive in [false, true] {
        let mut request = input();
        request.entitled_to_adaptive = adaptive;
        let task_id = TaskId::new("task-projection").expect("valid autopilot test fixture");
        let handoff = controller()
            .plan_for_task(task_id.clone(), &request, None)
            .expect("valid autopilot test fixture");
        let expected = project_context_plan(task_id.clone(), &handoff.decision().context_plan)
            .expect("valid autopilot test fixture");
        assert_eq!(handoff.context_projection(), &expected);
        assert_eq!(handoff.context_projection().task_id, task_id);
        handoff
            .context_projection()
            .validate()
            .expect("valid autopilot test fixture");

        let repeated = controller()
            .plan_for_task(task_id, &request, None)
            .expect("valid autopilot test fixture");
        assert_eq!(
            handoff
                .canonical_bytes()
                .expect("valid autopilot test fixture"),
            repeated
                .canonical_bytes()
                .expect("valid autopilot test fixture")
        );
        let other_task = controller()
            .plan_for_task(
                TaskId::new("task-other").expect("valid autopilot test fixture"),
                &request,
                None,
            )
            .expect("valid autopilot test fixture");
        assert_ne!(
            handoff
                .canonical_bytes()
                .expect("valid autopilot test fixture"),
            other_task
                .canonical_bytes()
                .expect("valid autopilot test fixture")
        );
        assert_ne!(
            handoff.context_projection().projection_digest,
            other_task.context_projection().projection_digest
        );
    }
}

#[test]
fn task_handoff_does_not_project_the_counterfactual_baseline() {
    let planner = AutopilotController::with_kernels(
        ContextKernel::with_field(
            Vec::new(),
            ContextField::with_weights(FieldWeights::default()),
        ),
        kernel(),
    );
    let handoff = planner
        .plan_for_task(
            TaskId::new("task-adaptive").expect("valid autopilot test fixture"),
            &input(),
            None,
        )
        .expect("valid autopilot test fixture");
    let shadow = handoff
        .decision()
        .shadow
        .as_ref()
        .expect("valid autopilot test fixture");
    assert_ne!(shadow.counterfactual_plan_id, shadow.authoritative_plan_id);
    assert_eq!(
        handoff.context_projection().context_plan_id.as_str(),
        shadow.authoritative_plan_id
    );
    assert!(!handoff.context_projection().selections.is_empty());
}

#[test]
fn shadow_plan_roles_preserve_wire_names_and_accept_explicit_aliases() {
    let shadow = controller()
        .plan(&input(), None)
        .expect("valid shadow plan")
        .shadow
        .expect("valid autopilot test fixture");
    let wire = serde_json::to_value(&shadow).expect("valid autopilot test fixture");
    assert_eq!(wire["authoritative"], false);
    assert_eq!(wire["baseline_plan_id"], shadow.counterfactual_plan_id);
    assert_eq!(wire["candidate_plan_id"], shadow.authoritative_plan_id);
    assert!(wire.get("authoritative_plan_id").is_none());
    assert_eq!(
        serde_json::from_value::<ShadowComparison>(wire.clone())
            .expect("valid autopilot test fixture"),
        shadow
    );

    let mut aliases = wire;
    let object = aliases
        .as_object_mut()
        .expect("valid autopilot test fixture");
    let baseline = object
        .remove("baseline_plan_id")
        .expect("valid autopilot test fixture");
    let candidate = object
        .remove("candidate_plan_id")
        .expect("valid autopilot test fixture");
    object.insert("counterfactual_plan_id".to_owned(), baseline);
    object.insert("authoritative_plan_id".to_owned(), candidate);
    object.remove("authoritative");
    assert_eq!(
        serde_json::from_value::<ShadowComparison>(aliases).expect("valid autopilot test fixture"),
        shadow
    );
}

#[test]
fn task_handoff_rejects_changed_decisions_and_wrong_execution_lineage() {
    let task_id = TaskId::new("task-bound").expect("valid autopilot test fixture");
    let mut changed = controller()
        .plan(&input(), None)
        .expect("valid changed plan");
    changed.read_policy.mode = "raw".to_owned();
    assert!(changed.bind_task(task_id.clone()).is_err());

    let mut wrong_role = controller()
        .plan(&input(), None)
        .expect("valid wrong-role plan");
    wrong_role
        .shadow
        .as_mut()
        .expect("valid autopilot test fixture")
        .authoritative_plan_id = "plan-other".to_owned();
    wrong_role.decision_id = decision_id(&wrong_role);
    assert!(wrong_role.bind_task(task_id.clone()).is_err());

    let handoff = controller()
        .plan_for_task(task_id.clone(), &input(), None)
        .expect("valid autopilot test fixture");
    let mut plan: ExecutionPlanV1 = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/ocla_contract_suite/v1/execution-plan/valid_minimal.json"
    )))
    .expect("valid autopilot test fixture");
    plan.task_id = task_id;
    plan.context_plan_id = Some(handoff.context_projection().context_plan_id.clone());
    plan.context_budget_tokens = handoff.context_projection().budget_tokens;
    handoff
        .validate_execution_plan(&plan)
        .expect("valid autopilot test fixture");

    let mut other_task = plan.clone();
    other_task.task_id = TaskId::new("task-wrong").expect("valid autopilot test fixture");
    assert!(handoff.validate_execution_plan(&other_task).is_err());
    let mut other_context = plan.clone();
    other_context.context_plan_id = Some(
        lean_ctx_protocol::ContextPlanId::new("plan-wrong").expect("valid autopilot test fixture"),
    );
    assert!(handoff.validate_execution_plan(&other_context).is_err());
    plan.context_budget_tokens += 1;
    assert!(handoff.validate_execution_plan(&plan).is_err());
}

#[test]
fn adaptive_planner_uses_learned_mode_and_kernel_plan() {
    let request = input();
    let decision = controller()
        .plan(&request, None)
        .expect("valid adaptive plan");

    assert_eq!(decision.tier, PlannerTier::AdaptivePro);
    assert_eq!(decision.read_policy.mode, "signatures");
    assert_eq!(decision.context_plan.selected.len(), 1);
    assert!(decision.shadow.is_some());
}

#[test]
fn explicit_override_wins_over_learning() {
    let mut request = input();
    request.overrides.read_mode = Some("lines:10-20".to_owned());
    let decision = controller()
        .plan(&request, None)
        .expect("valid explicit-override plan");

    assert_eq!(decision.read_policy.mode, "lines:10-20");
    assert!(decision.read_policy.explicit_override);
}

#[test]
fn promoted_policy_is_shadow_until_applied_and_never_outranks_explicit_modes() {
    use lean_ctx_protocol::context_policy_evidence::{
        CONTEXT_POLICY_VERSION, ContextPolicyEntryV1, ContextPolicyV1, ReadStrategyV1,
    };
    let reasons = |decision: &AutopilotDecision| {
        decision
            .reasons
            .iter()
            .map(|reason| reason.code.clone())
            .collect::<Vec<_>>()
    };
    let mut request = input();
    let unpoliced = controller().plan(&request, None).expect("plan");
    let workload = learning_store::workload_of(request.task_class, &unpoliced.context_plan);
    let policy = ContextPolicyV1 {
        schema_version: CONTEXT_POLICY_VERSION,
        version: 1,
        parent_version: None,
        entries: vec![ContextPolicyEntryV1 {
            workload,
            strategy: ReadStrategyV1::Map,
        }],
        learner_digest: "d".repeat(64),
    };

    request.context_policy = Some(ScopeContextPolicy {
        policy: policy.clone(),
        apply: false,
    });
    let shadow = controller().plan(&request, None).expect("plan");
    assert_eq!(
        shadow.read_policy.mode, "signatures",
        "shadow changes nothing"
    );
    assert!(reasons(&shadow).contains(&"context_policy_shadow".to_owned()));
    assert_ne!(shadow.decision_id, unpoliced.decision_id);

    request.context_policy = Some(ScopeContextPolicy {
        policy: policy.clone(),
        apply: true,
    });
    let applied = controller().plan(&request, None).expect("plan");
    assert_eq!(applied.read_policy.mode, "map");
    assert!(!applied.read_policy.explicit_override);
    assert!(reasons(&applied).contains(&"context_policy_applied".to_owned()));
    request.entitled_to_adaptive = false;
    let community = controller().plan(&request, None).expect("plan");
    assert_eq!(
        community.read_policy.mode, "map",
        "the user's switch, any tier"
    );

    request.overrides.read_mode = Some("full".to_owned());
    let explicit = controller().plan(&request, None).expect("plan");
    assert_eq!(explicit.read_policy.mode, "full");
    assert!(reasons(&explicit).contains(&"context_policy_shadow".to_owned()));
    request.overrides.read_mode = None;
    request.security_forced_mode = Some("signatures".to_owned());
    let forced = controller().plan(&request, None).expect("plan");
    assert_eq!(forced.read_policy.mode, "signatures");
}

#[test]
fn security_policy_is_the_only_precedence_above_user_override() {
    let mut request = input();
    request.overrides.no_compression = true;
    request.security_forced_mode = Some("signatures".to_owned());
    let decision = controller()
        .plan(&request, None)
        .expect("valid security-policy plan");

    assert_eq!(decision.read_policy.mode, "signatures");
}

#[test]
fn low_confidence_falls_back_to_community() {
    let mut request = input();
    request.confidence_milli = 649;
    let decision = controller()
        .plan(&request, None)
        .expect("valid low-confidence plan");

    assert_eq!(decision.tier, PlannerTier::Community);
    assert_eq!(decision.strategy, PlanningStrategy::SafeFallback);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.code == "low_confidence")
    );
}

#[test]
fn break_even_requires_strictly_positive_value() {
    let mut request = input();
    request.economics = AutopilotEconomics {
        baseline_cost_micros: 500,
        candidate_cost_micros: 500,
        ..AutopilotEconomics::default()
    };
    let decision = controller()
        .plan(&request, None)
        .expect("valid break-even plan");

    assert_eq!(decision.tier, PlannerTier::Community);
    assert_eq!(decision.economics.net_value_micros, 0);
    assert!(!decision.economics.positive);
}

#[test]
fn predictive_preload_is_bounded_positive_and_cache_aware() {
    let request = input();
    let mut bandit = ProviderBandit::new();
    let decision = controller()
        .plan(&request, Some(&mut bandit))
        .expect("valid preload plan");

    assert_eq!(decision.preloads.len(), 1);
    assert!(decision.preloads[0].expected_net_value_micros > 0);
    assert!(decision.preloads[0].low_priority);
    assert!(decision.preloads[0].cancellable);

    let mut cached = request;
    cached.cached_preloads.insert("github:issues".to_owned());
    assert!(
        controller()
            .plan(&cached, Some(&mut bandit))
            .expect("valid cached preload plan")
            .preloads
            .is_empty()
    );
}

#[test]
fn predictive_remote_preload_requires_consent() {
    let mut request = input();
    request.preload_budget.remote_query_consent = false;
    let mut bandit = ProviderBandit::new();

    assert!(
        controller()
            .plan(&request, Some(&mut bandit))
            .expect("valid remote preload plan")
            .preloads
            .is_empty()
    );

    request.local_providers.insert("github".to_owned());
    request.preload_budget.allow_remote_queries = false;
    assert_eq!(
        controller()
            .plan(&request, Some(&mut bandit))
            .expect("valid local preload plan")
            .preloads
            .len(),
        1
    );
}

#[test]
fn repeated_preload_waste_disables_prediction() {
    let mut request = input();
    request.learning.preload_misses = 4;
    let mut bandit = ProviderBandit::new();

    assert!(
        controller()
            .plan(&request, Some(&mut bandit))
            .expect("valid repeated preload plan")
            .preloads
            .is_empty()
    );
}

#[test]
fn unknown_outcome_does_not_train() {
    let mut state = AdaptiveLearningState::default();
    state.observe_mode_outcome("signatures", ReceiptOutcome::Unknown);

    assert_eq!(state, AdaptiveLearningState::default());
}

pub(super) fn protocol_task() -> lean_ctx_protocol::TaskEnvelopeV1 {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/ocla_contract_suite/v1/task-envelope/valid_minimal.json"
    )))
    .expect("valid task fixture")
}

#[test]
fn validated_outcome_trains_once_and_replay_survives_state_restore() {
    let task = protocol_task();
    let handoff = controller()
        .plan_for_task(task.task_id.clone(), &input(), None)
        .expect("task handoff");
    let fixture = crate::core::execution_protocol::test_support::build_for_context(
        &task,
        Some(handoff.context_projection()),
        Some(&handoff),
        AcceptanceState::Accepted,
    );
    let admitted = fixture.validated_for(&task).expect("valid evidence chain");
    let mut state = AdaptiveLearningState::default();
    assert!(
        handoff
            .observe_protocol_outcome(&admitted, &mut state)
            .expect("train")
    );
    assert_eq!(state.accepted, 1);
    assert_eq!(
        state.modes[&handoff.decision().read_policy.mode].accepted,
        1
    );
    assert_eq!(state.preload_hits, 0);
    assert!(
        !handoff
            .observe_protocol_outcome(&admitted, &mut state)
            .expect("replay")
    );
    let mut restored =
        AdaptiveLearningState::import_json(&state.export_json().expect("export")).expect("restore");
    assert!(
        !handoff
            .observe_protocol_outcome(&admitted, &mut restored)
            .expect("restored replay")
    );
    assert_eq!(restored, state);
}

#[test]
fn validated_rejection_trains_only_its_mode_unknown_and_community_do_not_train() {
    for (adaptive, acceptance) in [
        (true, AcceptanceState::Rejected),
        (true, AcceptanceState::Unknown),
        (false, AcceptanceState::Accepted),
    ] {
        let task = protocol_task();
        let mut request = input();
        request.entitled_to_adaptive = adaptive;
        let handoff = controller()
            .plan_for_task(task.task_id.clone(), &request, None)
            .expect("handoff");
        let fixture = crate::core::execution_protocol::test_support::build_for_context(
            &task,
            Some(handoff.context_projection()),
            Some(&handoff),
            acceptance,
        );
        let mut state = AdaptiveLearningState::default();
        let admitted = fixture
            .validated_for(&task)
            .expect("authenticated operational protocol");
        let trained = handoff
            .observe_protocol_outcome(&admitted, &mut state)
            .expect("observe");
        if adaptive && acceptance == AcceptanceState::Rejected {
            assert!(trained);
            assert_eq!(state.rejected, 1);
            assert_eq!(state.modes.len(), 1);
            assert_eq!(
                state.modes[&handoff.decision().read_policy.mode].rejected,
                1
            );
            assert_eq!(state.accepted, 0);
        } else {
            assert!(!trained);
            assert_eq!(state, AdaptiveLearningState::default());
            assert!(
                !handoff
                    .observe_protocol_outcome(&admitted, &mut state)
                    .expect("non-learning protocol replay")
            );
            assert_eq!(state, AdaptiveLearningState::default());
        }
    }
}

#[test]
fn learning_rejects_valid_receipts_for_another_mode_or_missing_decision_binding() {
    let task = protocol_task();
    let request = input();
    let handoff = controller()
        .plan_for_task(task.task_id.clone(), &request, None)
        .expect("handoff");
    let mut other_request = request;
    other_request.overrides.read_mode = Some("full".to_owned());
    let other_mode = controller()
        .plan_for_task(task.task_id.clone(), &other_request, None)
        .expect("other mode");
    assert_eq!(
        handoff.context_projection(),
        other_mode.context_projection()
    );
    assert_ne!(
        handoff.decision().decision_id,
        other_mode.decision().decision_id
    );
    for binding in [Some(&handoff), None] {
        let fixture = crate::core::execution_protocol::test_support::build_for_context(
            &task,
            Some(handoff.context_projection()),
            binding,
            AcceptanceState::Accepted,
        );
        let admitted = fixture.validated_for(&task).expect("valid receipt");
        let mut state = AdaptiveLearningState::default();
        assert!(
            other_mode
                .observe_protocol_outcome(&admitted, &mut state)
                .is_err()
        );
        if binding.is_none() {
            assert!(
                handoff
                    .observe_protocol_outcome(&admitted, &mut state)
                    .is_err()
            );
        }
        assert_eq!(state, AdaptiveLearningState::default());
    }
}

#[test]
fn invalid_chain_cannot_issue_learning_token() {
    let task = protocol_task();
    let handoff = controller()
        .plan_for_task(task.task_id.clone(), &input(), None)
        .expect("handoff");
    let mut fixture = crate::core::execution_protocol::test_support::build_for_context(
        &task,
        Some(handoff.context_projection()),
        Some(&handoff),
        AcceptanceState::Accepted,
    );
    fixture.validated_for(&task).expect("valid chain");
    let mut other_task = task.clone();
    other_task.task_id = TaskId::new("other-task").expect("task id");
    assert!(fixture.validated_for(&other_task).is_err());
    fixture.protocol.accepted_outcome.evidence_refs.clear();
    assert!(fixture.validated_for(&task).is_err());
}

#[test]
fn receipt_replay_memory_fails_closed_at_capacity() {
    let task = protocol_task();
    let handoff = controller()
        .plan_for_task(task.task_id.clone(), &input(), None)
        .expect("handoff");
    let fixture = crate::core::execution_protocol::test_support::build_for_context(
        &task,
        Some(handoff.context_projection()),
        Some(&handoff),
        AcceptanceState::Accepted,
    );
    let admitted = fixture.validated_for(&task).expect("valid chain");
    let mut state = AdaptiveLearningState {
        processed_receipts: (0..MAX_LEARNING_OBSERVATIONS)
            .map(|index| format!("prior-receipt-{index}"))
            .collect(),
        ..AdaptiveLearningState::default()
    };
    let before = state.clone();
    assert!(
        !handoff
            .observe_protocol_outcome(&admitted, &mut state)
            .expect("bounded learning")
    );
    assert_eq!(state, before);
}

#[test]
fn learning_is_mode_attributed_inspectable_and_resettable() {
    let mut state = AdaptiveLearningState::default();
    state.observe_mode_outcome("signatures", ReceiptOutcome::Accepted);
    state.observe_mode_outcome("full", ReceiptOutcome::Rejected);
    assert_eq!(state.learned_mode.as_deref(), Some("signatures"));

    let exported = state.export_json().unwrap();
    assert_eq!(
        AdaptiveLearningState::import_json(&exported).unwrap(),
        state
    );
    state.reset();
    assert_eq!(state, AdaptiveLearningState::default());
}

#[test]
fn replay_state_restore_preserves_capacity_and_rejects_oversized_history() {
    let mut state = AdaptiveLearningState {
        processed_receipts: (0..MAX_LEARNING_OBSERVATIONS)
            .map(|index| format!("prior-receipt-{index}"))
            .collect(),
        ..AdaptiveLearningState::default()
    };
    let exported = state.export_json().expect("export at capacity");
    assert_eq!(
        AdaptiveLearningState::import_json(&exported).expect("restore at capacity"),
        state
    );
    state
        .processed_receipts
        .insert("overflow-receipt".to_owned());
    let oversized = state.export_json().expect("export oversized fixture");
    assert!(AdaptiveLearningState::import_json(&oversized).is_err());
}

#[test]
fn learning_mode_cardinality_is_bounded_live_and_on_import() {
    let mut state = AdaptiveLearningState::default();
    for index in 0..(MAX_LEARNING_MODES + 5) {
        state.observe_mode_outcome(&format!("mode_{index}"), ReceiptOutcome::Accepted);
    }
    assert_eq!(state.modes.len(), MAX_LEARNING_MODES);

    let mut imported = AdaptiveLearningState::default();
    for index in 0..(MAX_LEARNING_MODES + 5) {
        imported.modes.insert(
            format!("imported_{index}"),
            ModeLearningState {
                accepted: u32::MAX,
                ..ModeLearningState::default()
            },
        );
    }
    let imported = AdaptiveLearningState::import_json(&imported.export_json().unwrap()).unwrap();
    assert_eq!(imported.modes.len(), MAX_LEARNING_MODES);
    assert!(
        imported
            .modes
            .values()
            .all(|mode| mode.accepted == MAX_LEARNING_OBSERVATIONS)
    );
}

#[test]
fn reported_shadow_metrics_remain_diagnostic_only() {
    let mut decision = controller()
        .plan(&input(), None)
        .expect("valid shadow-observation plan");
    let originating_decision_id = decision.decision_id.clone();
    let evidence_id = decision
        .record_shadow_outcome(ShadowOutcomeMetrics {
            baseline_cost_micros: 100,
            candidate_cost_micros: 80,
            baseline_quality_milli: 700,
            candidate_quality_milli: 800,
            preload_hits: 2,
            preload_misses: 1,
        })
        .unwrap();

    assert_ne!(decision.decision_id, originating_decision_id);
    let observed = decision.shadow.unwrap().observed.unwrap();
    assert_eq!(observed.evidence_id, evidence_id);
    assert_eq!(observed.decision_id, originating_decision_id);
    assert_eq!(observed.candidate_plan_id, decision.context_plan.plan_id);
    assert_eq!(observed.candidate_mode, "signatures");
}

#[test]
fn shadow_kernel_mode_never_bypasses_executable_policy() {
    let mut request = input();
    request.kernel_mode = KernelMode::Shadow;
    request.policy.blocked_sources = vec!["files".to_owned()];
    let decision = controller()
        .plan(&request, None)
        .expect("valid shadow-policy plan");

    assert!(decision.context_plan.selected.is_empty());
    assert_eq!(decision.context_plan.excluded[0].provider, "files");
    assert_eq!(decision.policy_observations.len(), 1);
    assert_eq!(
        decision.shadow.as_ref().unwrap().authoritative_plan_id,
        decision.context_plan.plan_id
    );
}

#[test]
fn blocked_provider_statistics_describe_the_final_kernel_plan() {
    let mut request = input();
    request.policy.blocked_sources = vec!["files".to_owned()];
    let decision = controller()
        .plan(&request, None)
        .expect("valid blocked-provider plan");
    assert!(decision.context_plan.selected.is_empty());
    let stats = &decision.context_plan.provider_stats["files"];
    assert_eq!(stats.candidates_offered, 1);
    assert_eq!(stats.candidates_selected, 0);
    assert_eq!(stats.tokens_used, 0);
    assert_eq!(decision.policy_observations.len(), 1);
}

#[test]
fn preload_execution_honors_cancellation() {
    #[derive(Default)]
    struct CountingExecutor(usize);

    impl PreloadExecutor for CountingExecutor {
        type Error = ();

        fn preload_low_priority(
            &mut self,
            _action: &PreloadAction,
            _cancellation: &PreloadCancellation,
        ) -> Result<(), Self::Error> {
            self.0 += 1;
            Ok(())
        }
    }

    let request = input();
    let mut bandit = ProviderBandit::new();
    let actions = controller()
        .plan(&request, Some(&mut bandit))
        .expect("valid cancellable preload plan")
        .preloads;
    let cancellation = PreloadCancellation::default();
    cancellation.cancel();
    let mut executor = CountingExecutor::default();

    assert!(execute_preloads(&actions, &cancellation, &mut executor).is_empty());
    assert_eq!(executor.0, 0);
}

#[test]
fn preload_planning_applies_hard_item_and_token_ceilings() {
    let mut request = input();
    request.preload_budget.max_items = usize::MAX;
    request.preload_budget.max_tokens = usize::MAX;
    request.preload_budget.tokens_per_item = MAX_PRELOAD_TOKENS_PER_ITEM + 1;
    let mut bandit = ProviderBandit::new();
    assert!(
        controller()
            .plan(&request, Some(&mut bandit))
            .expect("valid bounded preload plan")
            .preloads
            .is_empty()
    );

    request.preload_budget.tokens_per_item = 1;
    let preloads = controller()
        .plan(&request, Some(&mut bandit))
        .expect("valid one-token preload plan")
        .preloads;
    assert!(preloads.len() <= MAX_PRELOAD_ITEMS);
    assert!(
        preloads
            .iter()
            .map(|preload| preload.estimated_tokens)
            .sum::<usize>()
            <= MAX_PRELOAD_TOKENS
    );
}

#[test]
fn decision_id_covers_material_controls() {
    let request = input();
    let first = controller()
        .plan(&request, None)
        .expect("valid first decision");
    let mut changed = request;
    changed.overrides.no_telemetry = true;
    let second = controller()
        .plan(&changed, None)
        .expect("valid changed decision");

    assert_ne!(first.decision_id, second.decision_id);
}

#[test]
fn explanations_and_shadow_are_stable() {
    let request = input();
    let first = controller()
        .plan(&request, None)
        .expect("valid first stable decision");
    let second = controller()
        .plan(&request, None)
        .expect("valid second stable decision");

    assert_eq!(first.decision_id, second.decision_id);
    assert_eq!(first.reasons, second.reasons);
    assert_eq!(first.shadow, second.shadow);
}

#[test]
fn routing_telemetry_and_tool_surface_honor_controls() {
    let mut request = input();
    request.overrides.no_routing = true;
    request.overrides.no_telemetry = true;
    request.economics = AutopilotEconomics::default();
    let decision = controller()
        .plan(&request, None)
        .expect("valid routing-policy decision");

    assert!(!decision.routing_policy.allow_provider_routing);
    assert!(!decision.routing_policy.proxy_only);
    assert!(!decision.tool_surface.telemetry_enabled);
    assert!(!decision.tool_surface.expose_mcp_tools);
}
