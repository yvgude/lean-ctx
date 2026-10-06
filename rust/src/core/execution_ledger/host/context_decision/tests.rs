// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::{
    context_kernel::bridge::runtime::PreparedKernelContext, execution_ledger::ExecutionLedgerStore,
    outcome::contracts::TaskClass,
};
use ed25519_dalek::SigningKey;
use lean_ctx_ocla::{ReceiptSignerAdmissionV1, verify_decision_signature};
use lean_ctx_protocol::UtcTimestamp;

fn authority(path: &std::path::Path, enabled: bool) -> HostReceiptAuthority {
    let key = SigningKey::from_bytes(&[67; 32]);
    HostReceiptAuthority {
        signer_admission: ReceiptSignerAdmissionV1 {
            key_id: "context-decision-test-key".into(),
            public_key_digest: digest(key.verifying_key().as_bytes()).unwrap(),
            admitted_at: UtcTimestamp::new("2020-01-01T00:00:00Z").unwrap(),
            expires_at: UtcTimestamp::new("9999-01-01T00:00:00Z").unwrap(),
            revoked_at: None,
        },
        signing_key: key,
        ledger: ExecutionLedgerStore::new(path.join("ledger.jsonl")),
        allow_context_decision_signing: enabled,
        allow_outcome_signing: false,
        allow_checkpoint_signing: false,
        checkpoint_resume: None,
        allow_checkpoint_transfer_export: false,
        checkpoint_import: None,
        checkpoint_continue: None,
        checkpoint_source_files: None,
        checkpoint_source_providers: None,
        task_scope: None,
    }
}

fn planning(path: &std::path::Path) -> (TaskEnvelopeV1, ExecutionPlanV1, PreparedKernelContext) {
    let task: TaskEnvelopeV1 = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/ocla_contract_suite/v1/task-envelope/valid_minimal.json"
    )))
    .unwrap();
    let context = PreparedKernelContext::plan(
        task.task_id.clone(),
        "inspect context authentication".into(),
        path.to_str().unwrap().into(),
        150,
        TaskClass::Investigation,
        Some("aggressive".into()),
    )
    .unwrap();
    let mut plan: ExecutionPlanV1 = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/ocla_contract_suite/v1/execution-plan/valid_minimal.json"
    )))
    .unwrap();
    plan.task_id = task.task_id.clone();
    plan.executor_agent_id = Some(task.agent_id.clone());
    plan.context_plan_id = Some(
        context
            .decision()
            .context_projection()
            .context_plan_id
            .clone(),
    );
    plan.context_budget_tokens = context.decision().context_projection().budget_tokens;
    plan.context_budget_policy = None;
    let plan = context.decision().bind_execution_plan(plan).unwrap();
    (task, plan, context)
}

#[test]
fn operator_outcome_appends_authenticated_receipt_and_records_no_learning() {
    use crate::core::{
        context_kernel::autopilot::learning_store::AdaptiveLearningStore,
        engine_interface::{NativeContextEngine, verified_output_view},
        execution_ledger::host::HostOutcomeRequest,
        outcome::signals::LocalSignalAdapters,
    };
    use lean_ctx_protocol::{
        CapabilityBindingV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ProtocolReference,
    };
    for accepted in [true, false] {
        let data = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path().canonicalize().unwrap());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap();
        let (task, mut plan, context) = planning(&path);
        let manifest = crate::core::ocla::adapters::native_context::manifest();
        plan.provider = "local-native".into();
        plan.model = "local-native".into();
        plan.capability_ids = vec![manifest.capability_id.clone()];
        plan.capability_bindings = vec![CapabilityBindingV1 {
            capability_id: manifest.capability_id.clone(),
            version: manifest.version.clone(),
            manifest_digest: None,
        }];
        plan.scheduler_decision_ref = None;
        plan.knowledge_refs.clear();
        plan.policy_decision_ref = Some("policy:host-planning-test".into());
        let mut host = authority(&path, true);
        let attempt = host
            .begin_with_context(&task, &plan, Some(context.decision()))
            .unwrap();
        let (invocation, observation) = NativeContextEngine::with_root(&path)
            .unwrap()
            .execute_ctx_read_rooted_snapshot_with_plan(
                path.join("planning.rs").to_str().unwrap(),
                "fn observed_context() { let value = 42; }\n",
                EnginePolicyAdmissionV1 {
                    policy_ref: ProtocolReference::new("policy:host-planning-test").unwrap(),
                    decision: EnginePolicyDecisionV1::Admitted,
                },
                &task,
                &plan,
            )
            .unwrap();
        let view = verified_output_view(&invocation, &observation).unwrap();
        let published = host
            .publish(&attempt, &invocation, &observation, &view.text)
            .unwrap();
        let decision_hash =
            digest(&canonical_serialize(attempt.context_decision().unwrap())).unwrap();
        drop(attempt); // publication owns this same task lock until delivery finishes
        let original_bytes = std::fs::read(&published.path).unwrap();
        let mut request = HostOutcomeRequest {
            schema_version: 1,
            receipt_digest: lean_ctx_protocol::Sha256Digest::new(published.receipt_digest).unwrap(),
            context_decision_digest: decision_hash,
            signals: vec![LocalSignalAdapters::human_acceptance(accepted)],
            learn: false,
        };
        let original_events = host
            .ledger
            .by_task_verified(task.task_id.as_str())
            .unwrap()
            .len();
        assert!(host.observe_outcome(&request).is_err()); // receipt authority is not outcome authority
        host.allow_outcome_signing = true;
        request.signals = vec![LocalSignalAdapters::build_success(true)];
        assert!(host.observe_outcome(&request).is_err()); // missing investigation acceptance
        assert_eq!(
            host.ledger
                .by_task_verified(task.task_id.as_str())
                .unwrap()
                .len(),
            original_events
        );
        request.signals = vec![LocalSignalAdapters::human_acceptance(accepted)];
        let first = host.observe_outcome(&request).unwrap();
        assert_eq!(
            first["acceptance"],
            if accepted { "accepted" } else { "rejected" }
        );
        assert_eq!(first["learning_recorded"], false);
        assert_eq!(first["already_recorded"], false);
        assert_eq!(std::fs::read(&published.path).unwrap(), original_bytes);
        // `learn` stays accepted on the wire, but model/provider outcome
        // learning was removed with automatic routing: nothing is recorded.
        request.learn = true;
        let resumed = host.observe_outcome(&request).unwrap();
        assert_eq!(resumed["receipt_id"], first["receipt_id"]);
        assert_eq!(resumed["learning_recorded"], false);
        assert_eq!(resumed["already_recorded"], true);
        request.signals = vec![LocalSignalAdapters::human_acceptance(!accepted)];
        assert!(host.observe_outcome(&request).is_err()); // no silent replacement
        let store = AdaptiveLearningStore::open_default(task.project_id.clone(), None).unwrap();
        assert!(store.routing_history(10).unwrap().is_empty());
        assert!(host.ledger.verify_chain().unwrap());
    }
}

#[test]
fn legacy_host_configuration_does_not_grant_decision_purpose() {
    let root = tempfile::tempdir().unwrap();
    let host = authority(root.path(), false);
    let mut config = serde_json::json!({
        "schema_version": 1,
        "signing_key_hex": crate::core::agent_identity::hex_encode(&host.signing_key.to_bytes()),
        "signer": {
            "key_id": host.signer_admission.key_id,
            "public_key_digest": host.signer_admission.public_key_digest,
            "admitted_at": host.signer_admission.admitted_at,
            "expires_at": host.signer_admission.expires_at,
            "revoked_at": null
        },
        "ledger_path": root.path().join("configured.jsonl")
    });
    for enabled in [None, Some(false), Some(true)] {
        if let Some(value) = enabled {
            config["allow_context_decision_signing"] = value.into();
        }
        let bytes = serde_json::to_vec(&config).unwrap();
        let loaded = HostReceiptAuthority::from_reader(&mut bytes.as_slice()).unwrap();
        assert_eq!(loaded.allow_context_decision_signing, enabled == Some(true));
    }
    config["allow_context_decision_signing"] = "true".into();
    let bytes = serde_json::to_vec(&config).unwrap();
    assert!(HostReceiptAuthority::from_reader(&mut bytes.as_slice()).is_err());
}

#[test]
fn native_planner_handoff_reaches_host_signing_without_manual_plan_repair() {
    use crate::core::context_kernel::bridge::runtime::{
        KERNEL_PLANNING_HANDOFF, KernelPlanningHandoff,
    };
    use lean_ctx_protocol::{EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ProtocolReference};

    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let (task, _, context) = planning(root.path());
    let context = std::sync::Arc::new(context);
    let host = authority(root.path(), true);
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new("policy:native-context-signing-test").unwrap(),
        decision: EnginePolicyDecisionV1::Admitted,
    };
    let plan = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(KERNEL_PLANNING_HANDOFF.scope(
            KernelPlanningHandoff::Prepared(context.clone()),
            async {
                let plan = crate::core::engine_interface::planning::native_plan(&task, &admission)
                    .expect("real native plan");
                let attempt = host
                    .begin_with_context(&task, &plan, Some(context.decision()))
                    .expect("native planner must bind the actual handoff before host signing");
                let decision = attempt.context_decision().unwrap();
                assert_eq!(decision.plan_id.as_ref(), Some(&plan.plan_id));
                assert_eq!(
                    decision.decision_id.as_str(),
                    context.decision().decision().decision_id
                );
                let engine_root = root.path().canonicalize().unwrap();
                let engine =
                    crate::core::engine_interface::NativeContextEngine::with_root(&engine_root)
                        .unwrap();
                let (invocation, observation) = engine
                    .execute_ctx_read_rooted_snapshot_with_plan(
                        engine_root.join("native-handoff.rs").to_str().unwrap(),
                        "fn native_handoff() { let actual = 42; }\n",
                        admission.clone(),
                        &task,
                        &plan,
                    )
                    .expect("execute the actual native plan");
                let view =
                    crate::core::engine_interface::verified_output_view(&invocation, &observation)
                        .unwrap();
                let published = host
                    .publish(&attempt, &invocation, &observation, &view.text)
                    .expect("publish actual native receipt");
                let protocol = host
                    .published_context_protocol(&attempt, &invocation, &observation, &published)
                    .expect("validate actual planning and execution lineage")
                    .unwrap();
                assert_eq!(
                    protocol.accepted_outcome.accepted,
                    lean_ctx_protocol::AcceptanceState::Unknown
                );
                assert_eq!(protocol.accepted_outcome.quality_score_milli, None);
                plan
            },
        ));
    assert_eq!(plan.context_token_limit(), None);
    assert_eq!(plan.context_budget_tokens, 0);
    assert_eq!(context.decision().context_projection().budget_tokens, 150);
    // Replay is outside the task-local handoff: persisted evidence is sufficient.
    let verify = |candidate: &ExecutionPlanV1| {
        crate::core::engine_interface::planning::validate_native_plan(&task, candidate, &admission)
    };
    verify(&plan).expect("offline native plan verification");
    let mut budget = plan.clone();
    budget.context_budget_tokens = 150;
    budget.context_budget_policy =
        Some(lean_ctx_protocol::ContextBudgetPolicyV1::TokenLimit { tokens: 150 });
    assert!(verify(&budget).is_err());
    for (field, value) in [
        ("budget_scope", serde_json::json!("primary")),
        (
            "handoff_digest",
            serde_json::json!(format!("sha256:{}", "0".repeat(64))),
        ),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut changed = plan.clone();
        let mut binding = changed
            .extensions
            .get("native_context_handoff_v1")
            .unwrap()
            .clone();
        binding[field] = value;
        changed
            .extensions
            .insert("native_context_handoff_v1", binding)
            .unwrap();
        assert!(verify(&changed).is_err(), "tampered {field}");
    }
    let mut other_task = task.clone();
    other_task.project_id = lean_ctx_protocol::ProjectId::new("other-project").unwrap();
    assert!(
        crate::core::engine_interface::planning::validate_native_plan(
            &other_task,
            &plan,
            &admission,
        )
        .is_err()
    );
}

#[test]
fn host_capture_signs_and_persists_complete_executed_planning_handoff() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let (task, plan, context) = planning(root.path());
    let host = authority(root.path(), true);
    let before_capture = now().unwrap();
    let attempt = host
        .begin_with_context(&task, &plan, Some(context.decision()))
        .unwrap();
    let after_capture = now().unwrap();
    let record = attempt.context_decision().unwrap();
    assert!(record.observed_at.as_str() >= before_capture.as_str());
    assert!(record.observed_at.as_str() <= after_capture.as_str());
    let actual = context.decision().canonical_bytes().unwrap();
    assert_eq!(
        record.selected_result,
        serde_json::from_slice::<serde_json::Value>(&actual).unwrap()
    );
    assert_eq!(
        record.decision_id.as_str(),
        context.decision().decision().decision_id
    );
    assert_eq!(record.plan_id.as_ref(), Some(&plan.plan_id));
    let projection = context.decision().context_projection();
    assert_eq!(
        projection.projection_digest.as_ref().unwrap(),
        &projection.compute_projection_digest().unwrap()
    );
    for expected in [
        projection.projection_digest.as_ref().unwrap(),
        &digest(&canonical_serialize(&plan)).unwrap(),
    ] {
        assert!(
            record
                .input_refs
                .iter()
                .any(|reference| reference == expected.as_str())
        );
    }
    assert!(
        record.input_refs.contains(
            &digest(&task.canonical_bytes().unwrap())
                .unwrap()
                .as_str()
                .to_owned()
        )
    );
    let grant = DecisionSignerAdmissionV1 {
        key_admission: host.signer_admission.clone(),
        task: task.clone(),
        stage: DecisionStageV1::Planning,
        kind: DecisionKind::ContextSelection,
    };
    verify_decision_signature(
        record,
        &task,
        &grant,
        &host.signing_key.verifying_key(),
        &now().unwrap(),
    )
    .unwrap();
    for bytes in [&actual, &canonical_serialize(record)] {
        let digest = digest(bytes).unwrap();
        let persisted =
            crate::core::engine_artifact::read_content("execution/evidence", digest.hex(), "json")
                .unwrap();
        assert_eq!(&persisted, bytes);
    }
    assert_eq!(
        record.evidence_refs[0].signature_status,
        SignatureStatus::NotSigned
    );
    let mut tampered = record.clone();
    tampered.selected_result = serde_json::json!({"read_policy": "full"});
    assert!(
        verify_decision_signature(
            &tampered,
            &task,
            &grant,
            &host.signing_key.verifying_key(),
            &now().unwrap()
        )
        .is_err()
    );
}

#[test]
fn host_capture_rejects_missing_or_unbound_context_before_recording_attempt() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let (task, plan, context) = planning(root.path());
    let host = authority(root.path(), true);
    assert_eq!(
        host.begin(&task, &plan).err(),
        Some("host_context_decision_missing")
    );
    for kind in 0..3 {
        let mut changed = plan.clone();
        match kind {
            0 => changed.context_budget_tokens += 1,
            1 => changed.context_plan_id = None,
            _ => changed.extensions = Default::default(),
        }
        assert!(
            host.begin_with_context(&task, &changed, Some(context.decision()))
                .is_err()
        );
    }
    assert!(
        host.ledger
            .by_task_verified(task.task_id.as_str())
            .unwrap()
            .is_empty()
    );
    let legacy = authority(root.path(), false);
    assert!(capture(&legacy, &task, &plan, context.decision()).is_err());
    let attempt = legacy
        .begin_with_context(&task, &plan, Some(context.decision()))
        .unwrap();
    assert!(attempt.context_decision().is_none());
}

#[test]
fn actual_host_planning_signature_admits_unknown_without_future_decision_inputs() {
    use crate::core::engine_interface::{NativeContextEngine, verified_output_view};
    use lean_ctx_protocol::{
        CapabilityBindingV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1, ProtocolReference,
    };

    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(root.path()).unwrap();
    let (task, mut plan, context) = planning(&path);
    let manifest = crate::core::ocla::adapters::native_context::manifest();
    // An explicit declared native plan, not a fabricated scheduler recommendation.
    plan.provider = "local-native".into();
    plan.model = "local-native".into();
    plan.capability_ids = vec![manifest.capability_id.clone()];
    plan.capability_bindings = vec![CapabilityBindingV1 {
        capability_id: manifest.capability_id.clone(),
        version: manifest.version.clone(),
        manifest_digest: None,
    }];
    plan.scheduler_decision_ref = None;
    plan.knowledge_refs.clear();
    plan.policy_decision_ref = Some("policy:host-planning-test".into());
    let host = authority(&path, true);
    let attempt = host
        .begin_with_context(&task, &plan, Some(context.decision()))
        .unwrap();
    let engine = NativeContextEngine::with_root(&path).unwrap();
    let (invocation, observation) = engine
        .execute_ctx_read_rooted_snapshot_with_plan(
            path.join("planning.rs").to_str().unwrap(),
            "fn actual_planning_authentication() { let value = 42; }\n",
            EnginePolicyAdmissionV1 {
                policy_ref: ProtocolReference::new("policy:host-planning-test").unwrap(),
                decision: EnginePolicyDecisionV1::Admitted,
            },
            &task,
            &plan,
        )
        .unwrap();
    let view = verified_output_view(&invocation, &observation).unwrap();
    let published = host
        .publish(&attempt, &invocation, &observation, &view.text)
        .unwrap();
    let protocol = host
        .published_context_protocol(&attempt, &invocation, &observation, &published)
        .unwrap()
        .unwrap();
    assert_eq!(
        protocol.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert_eq!(protocol.accepted_outcome.quality_score_milli, None);
    assert!(protocol.receipt.outcome.outcome_id.is_none());
    assert!(protocol.receipt.outcome.outcome_ref.is_none());
    for future in [
        protocol.receipt.receipt_id.as_str(),
        protocol.receipt.lineage.invocation_ref.as_str(),
    ] {
        assert!(
            !protocol
                .decision
                .input_refs
                .iter()
                .any(|reference| reference == future)
        );
    }
    let grant = DecisionSignerAdmissionV1 {
        key_admission: host.signer_admission.clone(),
        task: task.clone(),
        stage: DecisionStageV1::Planning,
        kind: DecisionKind::ContextSelection,
    };
    let key = host.signing_key.verifying_key();
    let validate = |value: &crate::core::execution_protocol::RecordedExecutionProtocolV1,
                    actual: Option<&TaskAutopilotDecision>| {
        value
            .validated_for(
                &task,
                (&host.signer_admission, &key),
                (&grant, &key),
                actual,
                &now().unwrap(),
            )
            .map(|_| ())
    };
    validate(&protocol, Some(context.decision())).unwrap();
    assert!(validate(&protocol, None).is_err());

    let admitted = protocol
        .validated_for(
            &task,
            (&host.signer_admission, &key),
            (&grant, &key),
            Some(context.decision()),
            &now().unwrap(),
        )
        .unwrap();
    admitted
        .validate_learning_handoff(context.decision())
        .unwrap();
    let other = PreparedKernelContext::plan(
        task.task_id.clone(),
        "inspect context authentication".into(),
        path.to_str().unwrap().into(),
        150,
        TaskClass::Investigation,
        Some("map".into()),
    )
    .unwrap();
    assert!(
        admitted
            .validate_learning_handoff(other.decision())
            .is_err()
    );
    let mut learning = crate::core::context_kernel::autopilot::AdaptiveLearningState::default();
    let before = learning.export_json().unwrap();
    assert!(
        !context
            .decision()
            .observe_protocol_outcome(&admitted, &mut learning)
            .unwrap()
    );
    assert_eq!(learning.export_json().unwrap(), before);

    // Even a valid trusted signature cannot replace the captured selected payload.
    let mut forged = protocol.clone();
    forged.decision.selected_result = serde_json::json!({"read_policy": "full"});
    forged.decision.signature = sign_decision_record(
        &forged.decision,
        &task,
        &grant,
        &host.signing_key,
        &now().unwrap(),
    )
    .unwrap();
    let error = validate(&forged, Some(context.decision()))
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("actual selected policies"), "{error}");

    let mut relocated = protocol.clone();
    relocated.published_receipt.path = path.join("not-the-canonical-publication.json");
    assert!(validate(&relocated, Some(context.decision())).is_err());
    let handoff = context.decision().canonical_bytes().unwrap();
    let handoff_digest = digest(&handoff).unwrap();
    let artifact = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("execution/evidence")
        .join(format!("{}.json", handoff_digest.hex()));
    std::fs::write(&artifact, b"{}").unwrap();
    assert!(validate(&protocol, Some(context.decision())).is_err());
    std::fs::write(&artifact, handoff).unwrap();
    validate(&protocol, Some(context.decision())).unwrap();
}
