// SPDX-License-Identifier: Apache-2.0

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    thread,
};

use lean_ctx_protocol::TaskId;

use crate::core::{
    data_dir::isolated_data_dir, knowledge::ProjectKnowledge, memory_policy::MemoryPolicy,
    outcome::contracts::TaskClass,
};

use super::*;

const QUERY: &str = "renderer context decisions";
const BUDGET: usize = 96;

fn task_id(value: &str) -> TaskId {
    TaskId::new(value.to_owned()).expect("valid runtime test task id")
}

fn seeded_project(value: &str) -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().expect("project root");
    let project_root = root.path().to_str().expect("UTF-8 project root").to_owned();
    let mut knowledge = ProjectKnowledge::new(&project_root);
    knowledge.remember(
        "architecture",
        "renderer",
        value,
        "runtime-test",
        1.0,
        &MemoryPolicy::default(),
    );
    knowledge.save().expect("persist runtime test knowledge");
    (root, project_root)
}

fn prepared(task: &str, project_root: &str) -> Arc<PreparedKernelContext> {
    Arc::new(
        PreparedKernelContext::plan(
            task_id(task),
            QUERY.to_owned(),
            project_root.to_owned(),
            BUDGET,
            TaskClass::Investigation,
            None,
        )
        .expect("prepared kernel context"),
    )
}

#[test]
fn admitted_envelope_created_at_binds_retention_plan_identity() {
    let directory = isolated_data_dir();
    let (_root, project_root) = seeded_project("fixed envelope retention fixture");
    ProjectKnowledge::mutate_locked(&project_root, |knowledge| {
        assert_eq!(knowledge.facts.len(), 1);
        knowledge.facts[0].created_at = DateTime::parse_from_rfc3339("2026-09-13T00:00:00Z")
            .expect("fixed knowledge creation time")
            .to_utc();
    })
    .expect("persist fixed knowledge creation time");
    let policy = crate::core::context_kernel::policy::ContextPolicy {
        retention_days: Some(2),
        ..Default::default()
    };
    std::fs::write(
        directory.path().join("kernel-policy.toml"),
        toml::to_string(&policy).expect("retention policy TOML"),
    )
    .expect("write retention policy");

    let mut first = crate::core::task_spine::TaskSpine::create_envelope(
        "fixed retention",
        "runtime-retention",
        "runtime-retention",
    );
    first.created_at = "2026-09-14T00:00:00Z".to_owned();
    let mut second = first.clone();
    second.created_at = "2026-09-15T00:00:00Z".to_owned();

    let first = PreparedKernelContext::plan_for_envelope(
        first,
        QUERY.to_owned(),
        project_root.clone(),
        BUDGET,
        TaskClass::Investigation,
        None,
    )
    .expect("fixed envelope time plans");
    let second = PreparedKernelContext::plan_for_envelope(
        second,
        QUERY.to_owned(),
        project_root,
        BUDGET,
        TaskClass::Investigation,
        None,
    )
    .expect("second fixed envelope time plans");

    assert_ne!(
        first.decision().decision().context_plan.plan_id,
        second.decision().decision().context_plan.plan_id
    );
    let fresh_plan = &first.decision().decision().context_plan;
    assert_eq!(
        fresh_plan.provider_stats["knowledge.facts"].candidates_selected,
        1
    );
    assert!(
        fresh_plan
            .selected
            .iter()
            .any(|entry| entry.reason.contains("fixed envelope retention fixture"))
    );
    let expired = second.decision().decision();
    assert_eq!(
        expired.context_plan.provider_stats["knowledge.facts"].candidates_selected,
        0
    );
    assert!(
        expired
            .policy_observations
            .iter()
            .any(|entry| entry.reason == "candidate is outside retention window")
    );
    assert!(
        first
            .decision()
            .decision()
            .policy_observations
            .iter()
            .all(|observation| !observation.reason.contains("2026-"))
    );
}

#[test]
fn malformed_admitted_envelope_time_fails_without_echoing_input() {
    let _directory = isolated_data_dir();
    let (_root, project_root) = seeded_project("invalid envelope time fixture");
    let mut envelope = crate::core::task_spine::TaskSpine::create_envelope(
        "invalid retention",
        "runtime-retention",
        "runtime-retention",
    );
    envelope.created_at = "invalid private timestamp input".to_owned();
    let error = PreparedKernelContext::plan_for_envelope(
        envelope,
        QUERY.to_owned(),
        project_root,
        BUDGET,
        TaskClass::Investigation,
        None,
    )
    .expect_err("invalid admitted time must fail closed");
    assert_eq!(error.to_string(), "invalid task evaluation timestamp");
}

fn replace_seed(project_root: &str, value: &str) {
    ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        knowledge.remember(
            "architecture",
            "renderer",
            value,
            "runtime-test-mutation",
            1.0,
            &MemoryPolicy::default(),
        );
    })
    .expect("persist mutated runtime test knowledge");
}

#[test]
fn configured_policy_gates_real_knowledge_in_both_runtime_adapters() {
    let directory = isolated_data_dir();
    let (_root, project_root) = seeded_project("Renderer uses configured policy marker.");
    let policy = crate::core::context_kernel::policy::ContextPolicy {
        blocked_sources: vec!["knowledge.facts".to_owned()],
        ..Default::default()
    };
    let path = directory.path().join("kernel-policy.toml");
    std::fs::write(&path, toml::to_string(&policy).expect("policy TOML")).expect("write policy");
    let snapshot = prepared("configured-policy", &project_root);
    let decision = snapshot.decision().decision();
    assert!(decision.context_plan.selected.is_empty());
    assert_eq!(
        decision.context_plan.provider_stats["knowledge.facts"].candidates_selected,
        0
    );
    assert_eq!(decision.policy_observations.len(), 1);
    assert!(super::super::kernel_enrich(QUERY, &project_root, BUDGET).is_none());
    std::fs::write(&path, "invalid policy").expect("replace invalid policy");
    let result = PreparedKernelContext::plan(
        task_id("invalid-policy"),
        QUERY.to_owned(),
        project_root.clone(),
        BUDGET,
        TaskClass::Investigation,
        None,
    );
    let error = result.expect_err("invalid policy prevents task-owned context planning");
    assert!(matches!(
        error.downcast_ref::<crate::core::context_kernel::policy::PolicyLoadError>(),
        Some(crate::core::context_kernel::policy::PolicyLoadError::Malformed)
    ));
    assert!(super::super::kernel_enrich(QUERY, &project_root, BUDGET).is_none());
}

#[test]
fn prepared_snapshot_stays_stable_while_legacy_replans_from_persisted_knowledge() {
    let _data_dir = isolated_data_dir();
    let (_root, project_root) = seeded_project(
        "Renderer uses runtime snapshot marker alpha for immutable context decisions.",
    );
    let prepared = prepared("runtime-snapshot", &project_root);
    let expected_snapshot = super::super::enrichment_from_decision(
        prepared.decision().decision().clone(),
        prepared.budget,
        prepared.mode,
    )
    .expect("prepared decision has persisted knowledge");

    replace_seed(
        &project_root,
        "Renderer uses runtime snapshot marker beta for mutable context decisions.",
    );

    let snapshot = KERNEL_PLANNING_HANDOFF
        .sync_scope(
            KernelPlanningHandoff::Prepared(Arc::clone(&prepared)),
            || super::super::kernel_enrich(QUERY, &project_root, BUDGET),
        )
        .expect("snapshot enrichment");
    let legacy = super::super::kernel_enrich(QUERY, &project_root, BUDGET)
        .expect("legacy planner sees persisted mutation");

    assert_eq!(snapshot.blocks, expected_snapshot.blocks);
    assert!(snapshot.blocks.contains("marker alpha"));
    assert!(legacy.blocks.contains("marker beta"));
    assert!(!legacy.blocks.contains("marker alpha"));
}

#[test]
fn exact_scope_input_mismatches_fail_closed_without_consuming_delivery() {
    let _data_dir = isolated_data_dir();
    let (_root, project_root) = seeded_project(
        "Renderer uses runtime mismatch marker alpha for immutable context decisions.",
    );
    let prepared = prepared("runtime-mismatch", &project_root);
    let wrong_root = format!("{project_root}-other-task-root");

    let delivered = KERNEL_PLANNING_HANDOFF
        .sync_scope(
            KernelPlanningHandoff::Prepared(Arc::clone(&prepared)),
            || {
                assert!(
                    super::super::kernel_enrich("wrong query", &project_root, BUDGET).is_none()
                );
                assert!(super::super::kernel_enrich(QUERY, &wrong_root, BUDGET).is_none());
                assert!(super::super::kernel_enrich(QUERY, &project_root, BUDGET + 1).is_none());
                super::super::kernel_enrich(QUERY, &project_root, BUDGET)
            },
        )
        .expect("exact inputs deliver prepared context");

    assert!(delivered.blocks.contains("mismatch marker alpha"));
    assert!(delivered.verdict.budget_used <= BUDGET);
}

#[test]
fn concurrent_consumers_deliver_at_most_one_bounded_snapshot_supplement() {
    let _data_dir = isolated_data_dir();
    let (_root, project_root) = seeded_project(
        "Renderer uses runtime concurrency marker alpha for immutable context decisions.",
    );
    let prepared = prepared("runtime-concurrency", &project_root);
    let project_root = project_root.as_str();

    let results = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let prepared = Arc::clone(&prepared);
                scope.spawn(move || {
                    KERNEL_PLANNING_HANDOFF
                        .sync_scope(KernelPlanningHandoff::Prepared(prepared), || {
                            super::super::kernel_enrich(QUERY, project_root, BUDGET)
                        })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("consumer thread"))
            .collect::<Vec<_>>()
    });

    let delivered: Vec<_> = results.into_iter().flatten().collect();
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].verdict.budget_used <= BUDGET);
    assert!(delivered[0].blocks.contains("concurrency marker alpha"));
}

#[test]
fn scope_panic_restores_parent_and_does_not_leak_between_tasks() {
    let _data_dir = isolated_data_dir();
    let (_root_a, project_root_a) =
        seeded_project("Renderer uses runtime panic marker alpha for immutable context decisions.");
    let (_root_b, project_root_b) =
        seeded_project("Renderer uses runtime panic marker beta for immutable context decisions.");
    let prepared_a = prepared("runtime-panic-a", &project_root_a);
    let prepared_b = prepared("runtime-panic-b", &project_root_b);

    let outer_result = catch_unwind(AssertUnwindSafe(|| {
        KERNEL_PLANNING_HANDOFF.sync_scope(
            KernelPlanningHandoff::Prepared(Arc::clone(&prepared_a)),
            || {
                let nested_result = catch_unwind(AssertUnwindSafe(|| {
                    KERNEL_PLANNING_HANDOFF.sync_scope(
                        KernelPlanningHandoff::Prepared(Arc::clone(&prepared_b)),
                        || {
                            match current_handoff() {
                                KernelPlanningHandoff::Prepared(current) => {
                                    assert!(Arc::ptr_eq(&current, &prepared_b));
                                }
                                _ => panic!("nested task handoff missing"),
                            }
                            panic!("runtime scope panic fixture");
                        },
                    );
                }));
                assert!(nested_result.is_err());
                match current_handoff() {
                    KernelPlanningHandoff::Prepared(current) => {
                        assert!(Arc::ptr_eq(&current, &prepared_a));
                    }
                    _ => panic!("parent task handoff was not restored"),
                }
            },
        );
    }));

    assert!(outer_result.is_ok());
    assert!(matches!(current_handoff(), KernelPlanningHandoff::Legacy));

    let task_b = KERNEL_PLANNING_HANDOFF
        .sync_scope(
            KernelPlanningHandoff::Prepared(Arc::clone(&prepared_b)),
            || super::super::kernel_enrich(QUERY, &project_root_b, BUDGET),
        )
        .expect("task B enrichment");
    assert!(task_b.blocks.contains("panic marker beta"));
    assert!(!task_b.blocks.contains("panic marker alpha"));
}

#[test]
fn compose_cache_task_uses_canonical_decision_across_task_ids() {
    let _data_dir = isolated_data_dir();
    let (_root, project_root) =
        seeded_project("Renderer uses runtime cache marker alpha for immutable context decisions.");
    let first = prepared("runtime-cache-a", &project_root);
    let second = prepared("runtime-cache-b", &project_root);
    assert_eq!(
        first.decision().decision().decision_id,
        second.decision().decision().decision_id
    );

    let first_cache = KERNEL_PLANNING_HANDOFF
        .sync_scope(KernelPlanningHandoff::Prepared(Arc::clone(&first)), || {
            compose_cache_task(QUERY)
        });
    let second_cache = KERNEL_PLANNING_HANDOFF
        .sync_scope(KernelPlanningHandoff::Prepared(Arc::clone(&second)), || {
            compose_cache_task(QUERY)
        });
    let legacy_cache = compose_cache_task(QUERY);

    assert_eq!(first_cache, second_cache);
    assert_ne!(first_cache, legacy_cache);
    assert!(first_cache.contains(&first.decision().decision().decision_id));
    replace_seed(
        &project_root,
        "Renderer uses runtime cache marker beta for changed context decisions.",
    );
    let changed = prepared("runtime-cache-c", &project_root);
    let changed_cache = KERNEL_PLANNING_HANDOFF
        .sync_scope(KernelPlanningHandoff::Prepared(changed), || {
            compose_cache_task(QUERY)
        });
    assert_ne!(
        first_cache, changed_cache,
        "cached context must not outlive its planning identity"
    );
}
