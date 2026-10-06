// SPDX-License-Identifier: Apache-2.0
//! Invariants for the public shadow scheduler boundary.

use std::collections::BTreeMap;

use lean_ctx::core::ocla::scheduler_service::{ExecutionCandidate, SchedulerService};
use lean_ctx::core::ocla::{
    CatalogueEntry, PolicyConstraints, ReferenceScheduler, TechnicalCatalogue,
};
use lean_ctx_protocol::{
    CapabilityId, CapabilityKind, CapabilityManifestV1, DataClassification, DataMovement,
    Determinism, ExecutionPlanEstimatesV1, MeasurementSupportV1, Reversibility, SurfaceSupportV1,
    TaskComplexity, TaskEnvelopeV1,
};

#[test]
fn hard_discovery_excludes_remote_capability_under_local_policy() {
    let local = manifest("capability://local-control", "provider-public");
    let mut remote = manifest("capability://remote-denied", "provider-public");
    remote.local = false;
    remote.remote = true;
    remote.data_movement = DataMovement::Remote;
    let policy = PolicyConstraints {
        allowed_providers: Some(vec!["provider-public".to_owned()]),
        require_local_execution: true,
        ..PolicyConstraints::default()
    };
    let admitted =
        lean_ctx::core::ocla::registry::discover_compatible(&[remote, local.clone()], &policy);
    assert_eq!(
        admitted,
        vec![local],
        "local-only policy must use manifest facts"
    );
}

#[test]
fn hard_discovery_requires_unconditional_reversibility() {
    let reversible = manifest("capability://reversible-control", "provider-public");
    let mut irreversible = manifest("capability://irreversible-denied", "provider-public");
    irreversible.reversibility = Reversibility::Irreversible;
    let mut conditional = manifest("capability://conditional-denied", "provider-public");
    conditional.reversibility = Reversibility::Conditional;
    let policy = PolicyConstraints {
        require_reversible: true,
        ..PolicyConstraints::default()
    };
    let admitted = lean_ctx::core::ocla::registry::discover_compatible(
        &[irreversible, conditional, reversible.clone()],
        &policy,
    );
    assert_eq!(
        admitted,
        vec![reversible],
        "conditional reversibility is insufficient without its condition being satisfied"
    );
}

#[test]
fn hard_discovery_excludes_manifests_without_supported_surfaces() {
    let supported = manifest("capability://supported-control", "provider-public");
    let mut unsupported = manifest("capability://unsupported-denied", "provider-public");
    unsupported
        .support_matrix
        .get_mut("context")
        .expect("fixture context surface")
        .supported = false;
    let admitted = lean_ctx::core::ocla::registry::discover_compatible(
        &[unsupported, supported.clone()],
        &PolicyConstraints::default(),
    );
    assert_eq!(
        admitted,
        vec![supported],
        "a valid unavailable manifest must remain discoverable only outside execution eligibility"
    );
}

fn manifest(capability_id: &str, provider: &str) -> CapabilityManifestV1 {
    CapabilityManifestV1 {
        schema_version: 1,
        capability_id: CapabilityId::try_from(capability_id.to_owned()).expect("capability id"),
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

#[test]
fn hard_discovery_excludes_ambiguous_keys_before_policy_filtering() {
    let valid = manifest("capability://collision", "allowed");
    let control = manifest("capability://control", "allowed");
    let policy = PolicyConstraints {
        allowed_providers: Some(vec!["allowed".into()]),
        ..PolicyConstraints::default()
    };
    let mut conflicting = valid.clone();
    conflicting.provider = "denied".into();
    let mut invalid = valid.clone();
    invalid.schema_version = 0;
    for duplicate in [conflicting, invalid] {
        for input in [
            vec![valid.clone(), duplicate.clone(), control.clone()],
            vec![control.clone(), duplicate, valid.clone()],
        ] {
            assert_eq!(
                lean_ctx::core::ocla::registry::discover_compatible(&input, &policy),
                vec![control.clone()]
            );
        }
    }
    assert_eq!(
        lean_ctx::core::ocla::registry::discover_compatible(
            &[valid.clone(), valid.clone()],
            &policy
        ),
        vec![valid]
    );
}

fn envelope() -> TaskEnvelopeV1 {
    TaskEnvelopeV1 {
        schema_version: 1,
        task_id: "task-shadow".try_into().expect("task id"),
        trace_id: "trace-shadow".try_into().expect("trace id"),
        project_id: "project-shadow".try_into().expect("project id"),
        session_id: "session-shadow".try_into().expect("session id"),
        agent_id: "agent-shadow".try_into().expect("agent id"),
        complexity: TaskComplexity::Low,
        created_at: "2026-08-09T00:00:00Z".to_owned(),
        parent_task_id: None,
        tenant_id: None,
        intent: Some("inspect".to_owned()),
        task_class: None,
        risk_class: None,
        quality_requirement_milli: Some(700),
        cost_budget_micros: Some(100),
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
fn recommendation_does_not_execute_a_capability() {
    let capability = manifest("capability://shadow", "local");
    let catalogue = TechnicalCatalogue {
        capabilities: vec![CatalogueEntry {
            capability_id: capability.capability_id.as_str().to_owned(),
            version: capability.version.clone(),
            manifest: capability.clone(),
            available: true,
        }],
        ..TechnicalCatalogue::default()
    };
    let decision = ReferenceScheduler::new()
        .schedule(
            &envelope(),
            std::slice::from_ref(&capability),
            &catalogue,
            &PolicyConstraints::default(),
        )
        .expect("reference recommendation");

    assert_eq!(decision.selected.task_id, envelope().task_id);
    assert_ne!(decision.selected, decision.fallback);
    assert!(!decision.decision_ref.is_empty());
    assert!(!decision.rationale_code.is_empty());
    // A recommendation is a value only; no adapter invocation is reachable.
}

#[test]
fn fallback_and_candidate_accounting_are_always_present() {
    let decision = ReferenceScheduler::new()
        .schedule(
            &envelope(),
            &[],
            &TechnicalCatalogue::default(),
            &PolicyConstraints::default(),
        )
        .expect("fallback recommendation");
    assert_eq!(decision.selected, decision.fallback);
    assert_eq!(decision.candidates_evaluated, 0);
    assert_eq!(decision.candidates_excluded, 0);
    assert_eq!(decision.fallback.capability_ids.len(), 1);
}

#[test]
fn denied_synthetic_fallback_fails_closed_without_candidates() {
    let policy = PolicyConstraints {
        allowed_providers: Some(vec!["provider-a".to_owned()]),
        ..PolicyConstraints::default()
    };
    let error = ReferenceScheduler::new()
        .schedule(&envelope(), &[], &TechnicalCatalogue::default(), &policy)
        .expect_err("a denied synthetic fallback must not bypass provider policy");

    assert!(matches!(
        &error,
        lean_ctx::core::ocla::OclaError::InvalidRequest(_)
    ));
    assert!(error.to_string().contains("provider is not allowed"));
}

#[test]
fn local_region_and_reversibility_policies_deny_raw_fallback() {
    let policies = [
        PolicyConstraints {
            require_local_execution: true,
            ..PolicyConstraints::default()
        },
        PolicyConstraints {
            allowed_regions: Some(vec!["CH".to_owned()]),
            ..PolicyConstraints::default()
        },
        PolicyConstraints {
            require_reversible: true,
            ..PolicyConstraints::default()
        },
    ];

    for policy in policies {
        let error = ReferenceScheduler::new()
            .schedule(&envelope(), &[], &TechnicalCatalogue::default(), &policy)
            .expect_err("an unmet hard policy must deny the raw fallback");

        assert!(matches!(
            &error,
            lean_ctx::core::ocla::OclaError::InvalidRequest(_)
        ));
        assert!(
            error
                .to_string()
                .contains("no policy-permitted reference scheduler")
        );
    }
}

#[test]
fn permitted_candidate_replaces_denied_synthetic_fallback() {
    let capability = manifest("capability://permitted", "provider-a");
    let mut catalogue = TechnicalCatalogue::from_manifests([capability.clone()]);
    catalogue.capabilities[0].available = true;
    let policy = PolicyConstraints {
        allowed_providers: Some(vec!["provider-a".to_owned()]),
        ..PolicyConstraints::default()
    };
    let decision = ReferenceScheduler::new()
        .schedule(
            &envelope(),
            std::slice::from_ref(&capability),
            &catalogue,
            &policy,
        )
        .expect("the permitted candidate remains available");

    assert_eq!(decision.selected.provider, "provider-a");
    assert_eq!(decision.fallback.provider, "provider-a");
    assert_eq!(decision.selected, decision.fallback);
}

#[test]
fn public_catalogue_contains_no_private_economic_fields() {
    let catalogue = TechnicalCatalogue {
        models: vec![lean_ctx::core::ocla::ModelEntry {
            model_id: "model-public".to_owned(),
            context_window: 128_000,
            supports_reasoning: true,
            supports_streaming: true,
        }],
        providers: vec![lean_ctx::core::ocla::ProviderEntry {
            provider_id: "provider-public".to_owned(),
            models_available: vec!["model-public".to_owned()],
            regions: vec!["CH".to_owned()],
        }],
        ..TechnicalCatalogue::default()
    };
    let output = serde_json::to_string(&catalogue).expect("catalogue serialization");
    for forbidden in [
        "price",
        "rate",
        "performance",
        "score",
        "weight",
        "capacity",
    ] {
        assert!(
            !output.contains(forbidden),
            "private field leaked: {forbidden}"
        );
    }
}

#[test]
fn requested_quality_and_latency_are_not_capability_estimates() {
    let capability = manifest("capability://unknown-estimates", "provider-public");
    let mut catalogue = TechnicalCatalogue::from_manifests([capability.clone()]);
    catalogue.capabilities[0].available = true;
    let candidates = ReferenceScheduler::new()
        .generate_candidates(&envelope(), &[capability], &catalogue)
        .expect("reference candidates");

    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate.expected_cost_micros, None);
    assert_eq!(candidate.expected_quality_milli, None);
    assert_eq!(candidate.expected_latency_ms, None);
    assert_eq!(candidate.plan.expected_cost_micros, 0);
    assert_eq!(candidate.plan.expected_quality_milli, 0);
    assert_eq!(candidate.plan.expected_latency_ms, 0);
    let estimates = candidate
        .plan
        .estimates
        .as_ref()
        .expect("explicit unknowns");
    assert_eq!(estimates.cost_micros, None);
    assert_eq!(estimates.quality_milli, None);
    assert_eq!(estimates.latency_ms, None);
    candidate.plan.validate().expect("valid dual projection");
}

#[test]
fn requested_targets_cannot_satisfy_hard_prediction_requirements() {
    let capability = manifest("capability://no-prediction", "provider-public");
    let mut catalogue = TechnicalCatalogue::from_manifests([capability.clone()]);
    catalogue.capabilities[0].available = true;
    let task = envelope();
    let policies = [
        PolicyConstraints {
            min_quality_milli: task.quality_requirement_milli.map(u32::from),
            ..PolicyConstraints::default()
        },
        PolicyConstraints {
            max_latency_ms: task.latency_budget_ms,
            ..PolicyConstraints::default()
        },
    ];
    for policy in policies {
        for sources in [vec![], vec![capability.clone()]] {
            let error = ReferenceScheduler::new()
                .schedule(&task, &sources, &catalogue, &policy)
                .expect_err("unknown predictions deny both candidate and manual fallback");
            assert!(
                error
                    .to_string()
                    .contains("no policy-permitted reference scheduler")
            );
            assert!(error.to_string().contains("no public"));
        }
    }
}

#[test]
fn manual_fallback_keeps_zero_cost_but_no_invented_quality_or_latency() {
    let plan = ReferenceScheduler::new()
        .fallback_plan(&envelope())
        .expect("manual fallback template");
    let wire = serde_json::to_value(&plan).expect("fallback JSON");
    assert_eq!(wire["expected_cost_micros"], 0);
    assert_eq!(wire["expected_quality_milli"], 0);
    assert_eq!(wire["expected_latency_ms"], 0);
    assert_eq!(
        wire["estimates"],
        serde_json::json!({
            "cost_micros": 0, "quality_milli": null, "latency_ms": null,
        })
    );
    plan.validate().expect("valid fallback dual projection");
}

#[test]
fn nullable_estimates_rank_unknown_after_known_and_break_ties_stably() {
    let scheduler = ReferenceScheduler::new();
    let task = envelope();
    let fallback = ExecutionCandidate::new(
        scheduler
            .fallback_plan(&task)
            .expect("manual fallback template"),
        "capability://leanctx/passthrough",
        "manual",
        "leanctx",
        Some(0),
        None,
        None,
    );
    let candidate = |capability: &str,
                     model: &str,
                     cost: Option<u64>,
                     quality: Option<u16>,
                     latency: Option<u64>| {
        let mut plan = scheduler
            .fallback_plan(&task)
            .expect("candidate plan template");
        plan.capability_ids =
            vec![CapabilityId::try_from(capability.to_owned()).expect("capability")];
        plan.model = model.to_owned();
        plan.provider = "provider-public".to_owned();
        plan.expected_cost_micros = cost.unwrap_or_default();
        plan.expected_quality_milli = quality.unwrap_or_default();
        plan.expected_latency_ms = latency.unwrap_or_default();
        plan.estimates = Some(ExecutionPlanEstimatesV1 {
            cost_micros: cost,
            quality_milli: quality,
            latency_ms: latency,
        });
        plan.validate().expect("candidate plan");
        ExecutionCandidate::new(
            plan,
            capability,
            model,
            "provider-public",
            cost,
            quality.map(u32::from),
            latency,
        )
    };

    let known = candidate(
        "capability://known",
        "known-model",
        Some(25),
        Some(700),
        Some(100),
    );
    let unknown = candidate("capability://unknown", "unknown-model", None, None, None);
    let mixed = scheduler.select_plan(&[unknown, known.clone()], &fallback);
    assert_eq!(
        mixed.selected.model, "known-model",
        "unknown estimates must not inherit legacy zero scalars"
    );

    let mut legacy = candidate("capability://legacy", "legacy-model", Some(1), None, None);
    legacy.plan.estimates = None;
    legacy.expected_cost_micros = None;
    legacy
        .plan
        .validate()
        .expect("legacy scalar plan remains valid");
    assert_eq!(
        scheduler
            .select_plan(&[legacy, known], &fallback)
            .selected
            .model,
        "known-model",
        "missing candidate estimate must not inherit a legacy plan's scalar"
    );

    let known_latency = candidate(
        "capability://known-latency",
        "known-latency-model",
        Some(10),
        Some(500),
        Some(10),
    );
    let unknown_latency = candidate(
        "capability://unknown-latency",
        "unknown-latency-model",
        Some(10),
        Some(500),
        None,
    );
    let latency = scheduler.select_plan(&[unknown_latency, known_latency], &fallback);
    assert_eq!(latency.selected.model, "known-latency-model");

    let known_quality = candidate(
        "capability://z-known-quality",
        "known-quality-model",
        Some(10),
        Some(0),
        Some(10),
    );
    let unknown_quality = candidate(
        "capability://a-unknown-quality",
        "unknown-quality-model",
        Some(10),
        None,
        Some(10),
    );
    let quality = scheduler.select_plan(&[unknown_quality, known_quality], &fallback);
    assert_eq!(quality.selected.model, "known-quality-model");

    let tie_a = candidate(
        "capability://tie-a",
        "tie-a-model",
        Some(10),
        Some(500),
        Some(10),
    );
    let tie_b = candidate(
        "capability://tie-b",
        "tie-b-model",
        Some(10),
        Some(500),
        Some(10),
    );
    let forward = scheduler.select_plan(&[tie_b.clone(), tie_a.clone()], &fallback);
    let reverse = scheduler.select_plan(&[tie_a, tie_b], &fallback);
    assert_eq!(forward.selected.model, "tie-a-model");
    assert_eq!(
        forward.selected, reverse.selected,
        "equal estimates must select by stable candidate identity"
    );
}
