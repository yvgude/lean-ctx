// SPDX-License-Identifier: Apache-2.0

//! Adapter contract and family behavior tests.

use super::*;
use lean_ctx_protocol::{
    ContextCheckpointBranchIdV1, ContextCheckpointDecisionStatusV1, ContextCheckpointDecisionV1,
    ContextCheckpointDeviceIdV1, ContextCheckpointFileRoleV1, ContextCheckpointFileV1,
    ContextCheckpointIdV1, ContextCheckpointIdentityV1, ContextCheckpointLineageV1,
    ContextCheckpointLiveStateV1, ContextCheckpointProgressV1, ContextCheckpointSessionStateV1,
    ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1, ContextCheckpointTextV1,
    ContextSessionPhaseV1, ContextSessionRecoveryStateV1, ContextSessionStateV1, DecisionId,
    PlanId, ProtocolReference, SemanticVersion, SessionIdentityV1, Sha256Digest, SourceId, TaskId,
    UtcTimestamp,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("adapter fixture identity is valid")
}

fn reference(value: &str) -> ProtocolReference {
    ProtocolReference::new(value).expect("adapter fixture reference is valid")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{value:0<64}")).expect("adapter fixture digest is valid")
}

fn fixture_checkpoint() -> ContextCheckpointV1 {
    let task_id: TaskId = id("task-1");
    let plan_id: PlanId = id("plan-1");
    let lineage = ContextCheckpointLineageV1 {
        project_id: id("project-1"),
        workspace_id: id("550e8400-e29b-41d4-a716-446655440000"),
        tenant_id: id("tenant-1"),
        task_id: task_id.clone(),
        plan_id: Some(plan_id.clone()),
        receipt_ids: vec![id("receipt-1")],
        context_ir_digest: Some(digest("a")),
        hosted_index_digest: Some(digest("b")),
        evidence_refs: vec![reference("evidence:one")],
        knowledge_refs: vec![reference("knowledge:one")],
        gotcha_refs: vec![reference("gotcha:one")],
        snapshot_refs: vec![reference("snapshot:one")],
    };
    let live_state = ContextCheckpointLiveStateV1 {
        schema_version: 1,
        task: ContextCheckpointTaskV1 {
            task_id,
            title: ContextCheckpointTextV1::new("adapter fixture").unwrap(),
            status: ContextCheckpointTaskStatusV1::InProgress,
            plan_id: Some(plan_id),
        },
        progress: ContextCheckpointProgressV1 {
            completed_steps: 1,
            total_steps: 2,
            confidence_milliunits: 750,
            summary: ContextCheckpointTextV1::new("half complete").unwrap(),
        },
        decisions: vec![ContextCheckpointDecisionV1 {
            decision_id: id::<DecisionId>("decision-1"),
            statement: ContextCheckpointTextV1::new("retain typed source").unwrap(),
            rationale: ContextCheckpointTextV1::new("one semantic owner").unwrap(),
            status: ContextCheckpointDecisionStatusV1::Accepted,
            evidence_refs: vec![reference("evidence:one")],
        }],
        findings: vec![ContextCheckpointTextV1::new("finding one").unwrap()],
        next_steps: vec![ContextCheckpointTextV1::new("verify gates").unwrap()],
        handoff_summary: ContextCheckpointTextV1::new("continue adapter").unwrap(),
        files: vec![ContextCheckpointFileV1 {
            source_id: id::<SourceId>("source-1"),
            role: ContextCheckpointFileRoleV1::Modified,
            content_digest: digest("c"),
        }],
        profile_id: Some(id("profile-1")),
        policy_pins: vec![],
        package_pins: vec![],
        session_state: Some(
            ContextCheckpointSessionStateV1::try_new(
                SessionIdentityV1 {
                    session_id: id("session-1"),
                    task_id: id("task-1"),
                    run_id: id("run-1"),
                    trace_id: id("trace-1"),
                    parent_task_id: None,
                    agent_id: id("agent-1"),
                    project_id: id("project-1"),
                    workspace_id: Some(id("550e8400-e29b-41d4-a716-446655440000")),
                    tenant_id: Some(id("tenant-1")),
                    project_root_ref: reference("project:root"),
                    project_revision_ref: None,
                    created_at: UtcTimestamp::new("2026-01-01T00:00:00Z").unwrap(),
                },
                ContextSessionStateV1 {
                    phase: ContextSessionPhaseV1::Executing,
                    revision: 1,
                    next_event_sequence: 2,
                    active_plan_id: Some(id("plan-1")),
                    receipt_id: None,
                    last_checkpoint_digest: None,
                    recovery_state: ContextSessionRecoveryStateV1::Resumable,
                    abort_reason: None,
                },
            )
            .unwrap(),
        ),
        learning_state: None,
    };
    let identity = ContextCheckpointIdentityV1::try_new(
        id::<ContextCheckpointIdV1>("550e8400-e29b-41d4-a716-446655440001"),
        None,
        None,
        None,
        None,
        id::<ContextCheckpointBranchIdV1>("main"),
        id::<ContextCheckpointDeviceIdV1>("device-1"),
        1,
    )
    .unwrap();
    ContextCheckpointV1::try_new(
        identity,
        lineage,
        live_state,
        None,
        SemanticVersion::new("1.0.0").unwrap(),
        UtcTimestamp::new("2026-01-01T00:00:00Z").unwrap(),
    )
    .unwrap()
}

fn minimal_checkpoint() -> ContextCheckpointV1 {
    let mut source = fixture_checkpoint();
    source.lineage.receipt_ids.clear();
    source.lineage.context_ir_digest = None;
    source.lineage.hosted_index_digest = None;
    source.lineage.evidence_refs.clear();
    source.lineage.knowledge_refs.clear();
    source.lineage.gotcha_refs.clear();
    source.lineage.snapshot_refs.clear();
    source
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::new(value).unwrap()
}

fn hash() -> ContextCheckpointLegacyHashV1 {
    ContextCheckpointLegacyHashV1::new("a".repeat(64)).unwrap()
}

fn label(value: &str) -> ContextCheckpointLegacyLabelV1 {
    ContextCheckpointLegacyLabelV1::new(value).unwrap()
}

fn project() -> ContextCheckpointLegacyProjectIdentityV1 {
    ContextCheckpointLegacyProjectIdentityV1 {
        project_root_hash: None,
        project_identity_hash: None,
    }
}

fn session_request() -> ContextCheckpointLegacyRequestV1 {
    ContextCheckpointLegacyRequestV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        target: ContextCheckpointLegacyTargetV1::SessionBundle,
        target_schema_version: CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
        inputs: ContextCheckpointLegacyInputsV1::SessionBundle(
            ContextCheckpointSessionBundleInputsV1 {
                exported_at: timestamp("2026-01-01T00:00:03Z"),
                project: project(),
                role: ContextCheckpointLegacyPolicyIdentityV1 {
                    name: label("role-1"),
                    policy_digest: hash(),
                },
                profile: ContextCheckpointLegacyPolicyIdentityV1 {
                    name: label("profile-1"),
                    policy_digest: hash(),
                },
                decision_observed_at: vec![timestamp("2026-01-01T00:00:04Z")],
                finding_observed_at: vec![timestamp("2026-01-01T00:00:05Z")],
                stats: ContextCheckpointLegacySessionCountersV1 {
                    total_tool_calls: 1,
                    total_tokens_saved: 2,
                    total_tokens_input: 3,
                    cache_hits: 0,
                    files_read: 1,
                    commands_run: 1,
                    intents_inferred: 0,
                    intents_explicit: 1,
                    unsaved_changes: 0,
                },
                compression_level: Some(label("normal")),
                terse_mode: false,
            },
        ),
    }
}

fn handoff_request() -> ContextCheckpointLegacyRequestV1 {
    ContextCheckpointLegacyRequestV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        target: ContextCheckpointLegacyTargetV1::HandoffBundle,
        target_schema_version: HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
        inputs: ContextCheckpointLegacyInputsV1::HandoffBundle(
            ContextCheckpointHandoffBundleInputsV1 {
                exported_at: timestamp("2026-01-01T00:00:03Z"),
                ledger_created_at: timestamp("2026-01-01T00:00:04Z"),
                manifest_digest: hash(),
                project: project(),
                agent_id: Some(label("agent-1")),
                client_name: Some(label("client-1")),
            },
        ),
    }
}

fn ctxpkg_request() -> ContextCheckpointLegacyRequestV1 {
    ContextCheckpointLegacyRequestV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        target: ContextCheckpointLegacyTargetV1::Ctxpkg,
        target_schema_version: CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        inputs: ContextCheckpointLegacyInputsV1::Ctxpkg(ContextCheckpointCtxpkgInputsV1 {
            carrier_envelope_json: None,
        }),
    }
}

fn snapshot_request() -> ContextCheckpointLegacyRequestV1 {
    ContextCheckpointLegacyRequestV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        target: ContextCheckpointLegacyTargetV1::ContextSnapshot,
        target_schema_version: CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
        inputs: ContextCheckpointLegacyInputsV1::ContextSnapshot(
            ContextCheckpointContextSnapshotInputsV1 {
                created_at: timestamp("2026-01-01T00:00:03Z"),
                lean_ctx_version: SemanticVersion::new("1.2.3").unwrap(),
                project: project(),
                parent_snapshot_id: None,
                git: ContextCheckpointLegacyGitAnchorV1 {
                    commit: Some(ContextCheckpointLegacyCommitV1::new("a".repeat(40)).unwrap()),
                    branch: Some(label("main")),
                    dirty: false,
                },
                roi: ContextCheckpointLegacyRoiV1 {
                    input_tokens: 10,
                    output_tokens: 4,
                    tokens_saved: 6,
                },
                ledger_totals: ContextCheckpointLegacyLedgerTotalsV1 {
                    window_size: 8,
                    total_tokens_sent: 10,
                    total_tokens_saved: 6,
                    lineage_items_recorded: 0,
                },
            },
        ),
    }
}

#[test]
fn session_bundle_materializes_canonically_without_mutating_source() {
    let source = fixture_checkpoint();
    let before = source.clone();
    let request = session_request();
    let first = materialize_checkpoint_legacy(&source, &request).unwrap();
    let second = materialize_checkpoint_legacy(&source, &request).unwrap();
    let ContextCheckpointLegacyMaterializationV1::Materialized(first) = first else {
        panic!("session bundle unexpectedly refused");
    };
    let ContextCheckpointLegacyMaterializationV1::Materialized(second) = second else {
        panic!("session bundle unexpectedly refused");
    };
    assert_eq!(first, second);
    assert!(!first.losses.is_empty());
    assert!(first.canonical_json.contains("\"schema_version\":1"));
    assert_eq!(source, before);
    first.validate().unwrap();
}

#[test]
fn handoff_bundle_materializes_through_deployed_owner() {
    let source = fixture_checkpoint();
    let request = handoff_request();
    let result = materialize_checkpoint_legacy(&source, &request).unwrap();
    let ContextCheckpointLegacyMaterializationV1::Materialized(payload) = result else {
        panic!("handoff bundle unexpectedly refused");
    };
    assert!(payload.canonical_json.contains("\"ledger\""));
    assert_eq!(
        payload.payload_content_hash.as_str(),
        crate::core::hasher::hash_hex(payload.canonical_json.as_bytes())
    );
    payload.validate().unwrap();
}

#[test]
fn ctxpkg_and_lineage_rich_snapshot_refuse_named_dependencies() {
    let source = fixture_checkpoint();
    let package = materialize_checkpoint_legacy(&source, &ctxpkg_request()).unwrap();
    assert!(matches!(
        package,
        ContextCheckpointLegacyMaterializationV1::Refused(ContextCheckpointLegacyRefusalV1 {
            reason: ContextCheckpointLegacyRefusalReasonV1::MissingCarrierBinding,
            ..
        })
    ));
    let snapshot = materialize_checkpoint_legacy(&source, &snapshot_request()).unwrap();
    assert!(matches!(
        snapshot,
        ContextCheckpointLegacyMaterializationV1::Refused(ContextCheckpointLegacyRefusalV1 {
            reason: ContextCheckpointLegacyRefusalReasonV1::TargetOwnerDependency,
            ..
        })
    ));
}

#[test]
fn minimal_snapshot_materializes_with_owner_computed_id() {
    let source = minimal_checkpoint();
    let result = materialize_checkpoint_legacy(&source, &snapshot_request()).unwrap();
    let ContextCheckpointLegacyMaterializationV1::Materialized(payload) = result else {
        panic!("minimal snapshot unexpectedly refused");
    };
    assert!(payload.canonical_json.contains("\"snapshot_id\""));
    payload.validate().unwrap();
}

#[test]
fn target_pairing_versions_and_unknown_source_fail_closed() {
    let mut mismatched = session_request();
    mismatched.inputs =
        ContextCheckpointLegacyInputsV1::HandoffBundle(match handoff_request().inputs {
            ContextCheckpointLegacyInputsV1::HandoffBundle(value) => value,
            _ => unreachable!(),
        });
    assert!(matches!(
        mismatched.validate(),
        Err(ContextCheckpointLegacyErrorV1::TargetInputsMismatch { .. })
    ));

    let mut newer = session_request();
    newer.target_schema_version = 99;
    assert!(matches!(
        newer.validate(),
        Err(ContextCheckpointLegacyErrorV1::UnsupportedTargetVersion { .. })
    ));

    let mut source = fixture_checkpoint();
    source.schema_version = 2;
    assert!(matches!(
        materialize_checkpoint_legacy(&source, &session_request()),
        Err(ContextCheckpointLegacyErrorV1::UnsupportedSourceVersion { .. })
    ));
}

#[test]
fn direct_deserialization_and_duplicate_inputs_are_rejected() {
    let request = session_request();
    let mut encoded = serde_json::to_value(&request).unwrap();
    encoded
        .as_object_mut()
        .unwrap()
        .insert("unknown".to_owned(), Value::from(1));
    assert!(serde_json::from_value::<ContextCheckpointLegacyRequestV1>(encoded).is_err());

    let error_with_unknown_field = serde_json::json!({
        "unsupported_target_version": {
            "target": "session_bundle",
            "requested": 2,
            "supported": 1,
            "unknown": true
        }
    });
    assert!(
        serde_json::from_value::<ContextCheckpointLegacyErrorV1>(error_with_unknown_field).is_err()
    );

    let ContextCheckpointLegacyInputsV1::SessionBundle(mut inputs) = request.inputs else {
        unreachable!()
    };
    inputs.decision_observed_at = vec![
        timestamp("2026-01-01T00:00:04Z"),
        timestamp("2026-01-01T00:00:04Z"),
    ];
    assert!(matches!(
        inputs.validate(),
        Err(ContextCheckpointLegacyErrorV1::DuplicateLegacyInput { .. })
    ));
}

#[test]
fn lossy_reverse_conversion_refuses_and_malformed_payload_fails() {
    let source = fixture_checkpoint();
    let request = session_request();
    let result = materialize_checkpoint_legacy(&source, &request).unwrap();
    let ContextCheckpointLegacyMaterializationV1::Materialized(payload) = result else {
        panic!("session bundle unexpectedly refused");
    };
    let mut cross_workspace = payload.clone();
    cross_workspace.source_identity.workspace_id = id("workspace-other");
    assert!(matches!(
        adopt_legacy_payload(&source, &request, &cross_workspace),
        Err(ContextCheckpointLegacyErrorV1::IdentityMismatch {
            field: ContextCheckpointLegacyFieldV1::SourceIdentity,
        })
    ));
    let before = source.clone();
    assert!(matches!(
        adopt_legacy_payload(&source, &request, &payload).unwrap(),
        ContextCheckpointLegacyAdoptionV1::Refused(ContextCheckpointLegacyRefusalV1 {
            reason: ContextCheckpointLegacyRefusalReasonV1::ReverseNotRepresentable,
            ..
        })
    ));
    assert_eq!(source, before);

    let malformed = ContextCheckpointLegacyPayloadV1 {
        adapter_schema_version: CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION,
        source_identity: source_identity(&source),
        target: ContextCheckpointLegacyTargetV1::SessionBundle,
        target_schema_version: CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
        canonical_json: "{}".to_owned(),
        payload_content_hash: hash(),
        losses: Vec::new(),
    };
    assert!(matches!(
        malformed.validate(),
        Err(ContextCheckpointLegacyErrorV1::InvalidLegacyPayload { .. })
    ));
}

#[test]
fn ctxpkg_accepts_only_deployed_versions() {
    assert!(
        ContextCheckpointLegacyTargetV1::Ctxpkg
            .accepts_schema_version(CONTEXT_PACKAGE_V1_SCHEMA_VERSION)
    );
    assert!(
        ContextCheckpointLegacyTargetV1::Ctxpkg
            .accepts_schema_version(CONTEXT_PACKAGE_V2_SCHEMA_VERSION)
    );
    assert!(!ContextCheckpointLegacyTargetV1::Ctxpkg.accepts_schema_version(3));
}

#[path = "tests/review_regressions.rs"]
mod review_regressions;
