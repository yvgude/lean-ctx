// SPDX-License-Identifier: Apache-2.0

#[test]
fn mutating_any_security_critical_field_changes_the_digest() {
    let baseline = fixture();
    let baseline_digest = baseline.digest().expect("digest");
    let baseline_inputs = baseline.digest_inputs().expect("inputs");

    type Mutation = (&'static str, Box<dyn Fn(&mut ContextCheckpointV1)>);
    let mutations: Vec<Mutation> = vec![
        (
            "identity.checkpoint_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.identity.checkpoint_id =
                    ContextCheckpointIdV1::new("99999999-2222-4333-8444-555555555555")
                        .expect("valid id");
            }),
        ),
        (
            "lineage.tenant_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.lineage.tenant_id = TenantId::new("tenant-b").expect("valid tenant");
            }),
        ),
        (
            "identity.parent_checkpoint_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.identity.parent_checkpoint_id = Some(
                    ContextCheckpointIdV1::new("33333333-3333-4444-8555-666666666666")
                        .expect("valid id"),
                );
            }),
        ),
        (
            "identity.branch_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.identity.branch_id =
                    ContextCheckpointBranchIdV1::new("feature").expect("valid branch");
            }),
        ),
        (
            "identity.device_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.identity.device_id =
                    ContextCheckpointDeviceIdV1::new("device-b").expect("valid device");
            }),
        ),
        (
            "identity.device_sequence",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.identity.device_sequence += 1;
                checkpoint.identity.parent_device_sequence = checkpoint
                    .identity
                    .parent_device_sequence
                    .map(|value| value + 1);
            }),
        ),
        (
            "lineage.project_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.lineage.project_id = ProjectId::new("project-b").expect("valid project");
            }),
        ),
        (
            "lineage.workspace_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                let workspace = WorkspaceId::new("bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                    .expect("valid workspace");
                checkpoint.lineage.workspace_id = workspace.clone();
                if let Some(carrier) = checkpoint.carrier.as_mut() {
                    carrier.workspace_id = workspace;
                }
            }),
        ),
        (
            "lineage.task_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                let task = TaskId::new("task-b").expect("valid task");
                checkpoint.lineage.task_id = task.clone();
                checkpoint.live_state.task.task_id = task;
            }),
        ),
        (
            "lineage.plan_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                let plan = Some(PlanId::new("plan-b").expect("valid plan"));
                checkpoint.lineage.plan_id = plan.clone();
                checkpoint.live_state.task.plan_id = plan;
            }),
        ),
        (
            "lineage.receipt_ids",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .lineage
                    .receipt_ids
                    .push(ReceiptId::new("receipt-c").expect("valid receipt"));
            }),
        ),
        (
            "lineage.context_ir_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.lineage.context_ir_digest = Some(digest_of("9"));
            }),
        ),
        (
            "lineage.hosted_index_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.lineage.hosted_index_digest = None;
            }),
        ),
        (
            "lineage.evidence_refs",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .lineage
                    .evidence_refs
                    .push(reference("evidence:gamma"));
            }),
        ),
        (
            "lineage.knowledge_refs",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .lineage
                    .knowledge_refs
                    .push(reference("knowledge:beta"));
            }),
        ),
        (
            "lineage.gotcha_refs",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .lineage
                    .gotcha_refs
                    .push(reference("gotcha:beta"));
            }),
        ),
        (
            "lineage.snapshot_refs",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .lineage
                    .snapshot_refs
                    .push(reference("snapshot:beta"));
            }),
        ),
        (
            "live_state.task.status",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.task.status = ContextCheckpointTaskStatusV1::Completed;
            }),
        ),
        (
            "live_state.task.title",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.task.title = text("Continue the checkpoint domain");
            }),
        ),
        (
            "live_state.progress.completed_steps",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.progress.completed_steps += 1;
            }),
        ),
        (
            "live_state.progress.total_steps",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.progress.total_steps += 1;
            }),
        ),
        (
            "live_state.progress.confidence_milliunits",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.progress.confidence_milliunits += 1;
            }),
        ),
        (
            "live_state.progress.summary",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.progress.summary = text("Progress remains bounded");
            }),
        ),
        (
            "live_state.decisions",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.decisions[0].status =
                    ContextCheckpointDecisionStatusV1::Rejected;
            }),
        ),
        (
            "live_state.decisions.decision_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.decisions[1].decision_id =
                    DecisionId::new("decision-c").expect("valid decision");
            }),
        ),
        (
            "live_state.decisions.statement",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.decisions[0].statement =
                    text("Bind every semantic field into the digest");
            }),
        ),
        (
            "live_state.decisions.rationale",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.decisions[0].rationale =
                    text("Prevents ambiguous continuation state");
            }),
        ),
        (
            "live_state.decisions.evidence_refs",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.decisions[0]
                    .evidence_refs
                    .push(reference("evidence:beta"));
            }),
        ),
        (
            "live_state.findings",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .findings
                    .push(text("Every field is bound"));
            }),
        ),
        (
            "live_state.next_steps",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .next_steps
                    .push(text("Verify optional state"));
            }),
        ),
        (
            "live_state.handoff_summary",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.handoff_summary = text("Optional state is reference-only");
            }),
        ),
        (
            "live_state.files.content_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.files[0].content_digest = digest_of("9");
            }),
        ),
        (
            "live_state.files.role",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.files[0].role = ContextCheckpointFileRoleV1::Deleted;
            }),
        ),
        (
            "live_state.files.source_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.files[1].source_id =
                    SourceId::new("source-c").expect("valid source");
            }),
        ),
        (
            "live_state.profile_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.profile_id =
                    Some(ProfileId::new("profile-b").expect("valid profile"));
            }),
        ),
        (
            "live_state.policy_pins",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.policy_pins[0].policy_digest = digest_of("9");
            }),
        ),
        (
            "live_state.policy_pins.policy_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.policy_pins[0].policy_id =
                    PolicyId::new("policy-b").expect("valid policy");
            }),
        ),
        (
            "live_state.package_pins",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.package_pins[0].package_digest = digest_of("9");
            }),
        ),
        (
            "live_state.package_pins.package_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.package_pins[0].package_id =
                    PackageId::new("package-b").expect("valid package");
            }),
        ),
        (
            "live_state.package_pins.version",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.live_state.package_pins[0].version =
                    SemanticVersion::new("1.2.4").expect("valid version");
            }),
        ),
        (
            "carrier.state_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                if let Some(carrier) = checkpoint.carrier.as_mut() {
                    carrier.state_digest = digest_of("9");
                }
            }),
        ),
        (
            "carrier.envelope_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                if let Some(carrier) = checkpoint.carrier.as_mut() {
                    carrier.envelope_digest = digest_of("9");
                }
            }),
        ),
        (
            "carrier presence",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.carrier = None;
            }),
        ),
        (
            "engine_version",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.engine_version = SemanticVersion::new("4.0.1").expect("valid version");
            }),
        ),
        (
            "created_at",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.created_at =
                    UtcTimestamp::new("2026-09-06T11:59:59Z").expect("valid timestamp");
            }),
        ),
        (
            "updated_at",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint.updated_at =
                    UtcTimestamp::new("2026-09-06T12:00:01Z").expect("valid timestamp");
            }),
        ),
    ];

    let mut seen = std::collections::BTreeSet::new();
    for (label, mutate) in mutations {
        let mut mutated = fixture();
        mutate(&mut mutated);
        assert_ne!(mutated, baseline, "{label} must actually change the value");
        let digest = mutated.digest().expect("mutated digest");
        assert_ne!(digest, baseline_digest, "{label} must rebind the digest");
        assert!(
            seen.insert(digest.into_inner()),
            "{label} must not collide with another mutation"
        );

        let inputs = mutated.digest_inputs().expect("mutated inputs");
        assert_ne!(
            inputs.checkpoint_digest(),
            baseline_inputs.checkpoint_digest(),
            "{label} must rebind the checkpoint digest input"
        );
        assert_ne!(
            mutated.signing_payload().expect("payload").digest(),
            baseline.signing_payload().expect("payload").digest(),
            "{label} must rebind the signing payload"
        );
    }
}

#[test]
fn optional_state_mutations_change_the_digest() {
    type Mutation = (&'static str, Box<dyn Fn(&mut ContextCheckpointV1)>);
    let baseline = rich_fixture();
    let baseline_digest = baseline.digest().expect("digest");
    let mutations: Vec<Mutation> = vec![
        (
            "session_state.session_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .identity
                    .session_id = SessionId::new("session-b").expect("session");
            }),
        ),
        (
            "session_state.phase",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .state
                    .phase = ContextSessionPhaseV1::Closed;
            }),
        ),
        (
            "session_state.revision",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .state
                    .revision = 4;
            }),
        ),
        (
            "session_state.next_event_sequence",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .state
                    .next_event_sequence = 5;
            }),
        ),
        (
            "session_state.active_plan_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                let plan_id = Some(PlanId::new("plan-b").expect("plan"));
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .state
                    .active_plan_id = plan_id.clone();
                checkpoint.lineage.plan_id = plan_id.clone();
                checkpoint.live_state.task.plan_id = plan_id;
            }),
        ),
        (
            "session_state.receipt_id",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                let session = checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session");
                session.state.phase = ContextSessionPhaseV1::Closed;
                session.state.receipt_id = Some(ReceiptId::new("receipt-c").expect("receipt"));
            }),
        ),
        (
            "session_state.recovery_state",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .session_state
                    .as_mut()
                    .expect("session")
                    .state
                    .recovery_state = ContextSessionRecoveryStateV1::ResumableWithDegradation;
            }),
        ),
        (
            "learning_state.state_ref",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .learning_state
                    .as_mut()
                    .expect("learning")
                    .state_ref = reference("learning:beta");
            }),
        ),
        (
            "learning_state.state_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .learning_state
                    .as_mut()
                    .expect("learning")
                    .state_digest = digest_of("0");
            }),
        ),
        (
            "learning_state.revision",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .live_state
                    .learning_state
                    .as_mut()
                    .expect("learning")
                    .revision = 3;
            }),
        ),
        (
            "encryption_metadata.algorithm",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .encryption_metadata
                    .as_mut()
                    .expect("encryption")
                    .algorithm = ContextCheckpointEncryptionAlgorithmV1::XChaCha20Poly1305;
            }),
        ),
        (
            "encryption_metadata.key_ref",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .encryption_metadata
                    .as_mut()
                    .expect("encryption")
                    .key_ref = reference("kms:key-b");
            }),
        ),
        (
            "encryption_metadata.ciphertext_digest",
            Box::new(|checkpoint: &mut ContextCheckpointV1| {
                checkpoint
                    .encryption_metadata
                    .as_mut()
                    .expect("encryption")
                    .ciphertext_digest = digest_of("1");
            }),
        ),
    ];

    for (label, mutate) in mutations {
        let mut mutated = baseline.clone();
        mutate(&mut mutated);
        assert_ne!(mutated, baseline, "{label} must change the value");
        assert_ne!(
            mutated.digest().expect("mutated digest"),
            baseline_digest,
            "{label} must rebind the digest"
        );
    }
}

#[test]
fn part_digests_track_only_their_own_part() {
    let baseline = fixture().digest_inputs().expect("inputs");

    let mut identity_changed = fixture();
    identity_changed.identity.device_sequence += 1;
    identity_changed.identity.parent_device_sequence = identity_changed
        .identity
        .parent_device_sequence
        .map(|value| value + 1);
    let identity_changed = identity_changed.digest_inputs().expect("inputs");
    assert_ne!(
        identity_changed.identity_digest(),
        baseline.identity_digest()
    );
    assert_eq!(identity_changed.lineage_digest(), baseline.lineage_digest());
    assert_eq!(
        identity_changed.live_state_digest(),
        baseline.live_state_digest()
    );

    let mut lineage_changed = fixture();
    lineage_changed
        .lineage
        .receipt_ids
        .push(ReceiptId::new("receipt-c").expect("valid receipt"));
    let lineage_changed = lineage_changed.digest_inputs().expect("inputs");
    assert_ne!(lineage_changed.lineage_digest(), baseline.lineage_digest());
    assert_eq!(
        lineage_changed.identity_digest(),
        baseline.identity_digest()
    );

    let mut live_changed = fixture();
    live_changed.live_state.progress.completed_steps += 1;
    let live_changed = live_changed.digest_inputs().expect("inputs");
    assert_ne!(
        live_changed.live_state_digest(),
        baseline.live_state_digest()
    );
    assert_eq!(live_changed.lineage_digest(), baseline.lineage_digest());
}

#[test]
fn invalid_state_cannot_be_serialized_digested_or_signed() {
    let mut checkpoint = fixture();
    checkpoint.identity.device_sequence = 0;
    assert!(checkpoint.validate().is_err());
    assert!(checkpoint.canonical_bytes().is_err());
    assert!(checkpoint.canonical_json().is_err());
    assert!(checkpoint.digest().is_err());
    assert!(checkpoint.digest_inputs().is_err());
    assert!(checkpoint.signing_payload().is_err());
    assert!(serde_json::to_string(&checkpoint).is_err());

    let checkpoint = fixture()
        .with_updated_at(UtcTimestamp::new("2026-09-06T11:59:59Z").expect("valid timestamp"));
    assert!(checkpoint.is_err());

    let mut checkpoint = fixture();
    checkpoint.schema_version = 2;
    assert!(checkpoint.digest().is_err());
    assert!(checkpoint.signing_payload().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.schema_version = 0;
    assert!(checkpoint.digest().is_err());

    // The fallible constructor refuses the same invalid state up front.
    assert!(
        ContextCheckpointV1::try_new(
            identity(),
            lineage(),
            live_state(),
            Some(carrier()),
            SemanticVersion::new("4.0.0").expect("valid version"),
            UtcTimestamp::new("2026-09-06T12:00:00Z").expect("valid timestamp"),
        )
        .is_ok()
    );
    let mut mismatched = live_state();
    mismatched.task.task_id = TaskId::new("task-other").expect("valid task");
    assert!(
        ContextCheckpointV1::try_new(
            identity(),
            lineage(),
            mismatched,
            Some(carrier()),
            SemanticVersion::new("4.0.0").expect("valid version"),
            UtcTimestamp::new("2026-09-06T12:00:00Z").expect("valid timestamp"),
        )
        .is_err()
    );
}

#[test]
fn canonical_bytes_are_stable_across_repeated_encodings() {
    let checkpoint = fixture();
    let first = checkpoint.canonical_bytes().expect("bytes");
    let second = checkpoint.canonical_bytes().expect("bytes");
    assert_eq!(first, second);
    let round_tripped =
        ContextCheckpointV1::from_canonical_bytes(&first).expect("round trip decode");
    assert_eq!(round_tripped.canonical_bytes().expect("bytes"), first);
    assert_eq!(
        round_tripped.digest().expect("digest"),
        checkpoint.digest().expect("digest")
    );
}

#[test]
fn artifact_lineage_is_strict_and_v1_wire_stays_unchanged() {
    let mut legacy_lineage = lineage();
    legacy_lineage.plan_id = None;
    assert!(
        legacy_lineage.validate().is_ok(),
        "V1 remains compatible with deployed receipt-only lineage"
    );

    let mut receipt_refs = vec![digest_of("4"), digest_of("5")];
    receipt_refs.sort();
    let mut checkpoint = fixture();
    checkpoint.lineage.receipt_ids = vec![
        ReceiptId::new("receipt-a").expect("receipt identity"),
        ReceiptId::new("receipt-b").expect("receipt identity"),
    ];
    let v1_bytes = checkpoint.canonical_bytes().expect("V1 bytes");
    let artifact_lineage = ContextCheckpointArtifactLineageV1 {
        task_ref: digest_of("1"),
        plan_ref: Some(digest_of("2")),
        receipt_refs,
    };
    let v2 = ContextCheckpointV2::from_v1_unverified(checkpoint.clone(), artifact_lineage)
        .expect("valid V2 migration");
    let v2_bytes = v2.canonical_bytes().expect("V2 bytes");

    assert_eq!(
        ContextCheckpointV1::from_canonical_bytes(&v1_bytes).unwrap(),
        checkpoint
    );
    assert!(serde_json::from_slice::<ContextCheckpointV1>(&v2_bytes).is_err());
    assert_eq!(
        ContextCheckpointV2::from_canonical_bytes(&v2_bytes).unwrap(),
        v2
    );

    let mut unordered = v2.clone();
    unordered.lineage.artifact_lineage.receipt_refs.reverse();
    assert!(unordered.validate().is_err());

    let mut partial_plan = v2.clone();
    partial_plan.lineage.plan_id = None;
    assert!(partial_plan.validate().is_err());

    let mut mismatched_receipt = v2.clone();
    mismatched_receipt.lineage.receipt_ids.pop();
    assert!(mismatched_receipt.validate().is_err());
}

#[test]
fn artifact_lineage_rejects_unknown_duplicate_unordered_and_unbounded_wire() {
    let digest_a = digest_of("a");
    let digest_b = digest_of("b");
    let base = serde_json::json!({
        "task_ref": digest_of("1"),
        "plan_ref": digest_of("2"),
        "receipt_refs": [digest_a, digest_b]
    });
    assert!(serde_json::from_value::<ContextCheckpointArtifactLineageV1>(base.clone()).is_ok());

    let mut unknown = base.clone();
    unknown["future"] = json!(true);
    assert!(serde_json::from_value::<ContextCheckpointArtifactLineageV1>(unknown).is_err());

    let duplicate = format!(
        "{{\"task_ref\":\"{}\",\"plan_ref\":\"{}\",\"receipt_refs\":[],\"receipt_refs\":[]}}",
        digest_of("1").as_str(),
        digest_of("2").as_str()
    );
    assert!(serde_json::from_str::<ContextCheckpointArtifactLineageV1>(&duplicate).is_err());

    let unordered = serde_json::json!({
        "task_ref": digest_of("1"),
        "plan_ref": digest_of("2"),
        "receipt_refs": [digest_b, digest_a]
    });
    assert!(serde_json::from_value::<ContextCheckpointArtifactLineageV1>(unordered).is_err());

    let too_many = serde_json::json!({
        "task_ref": digest_of("1"),
        "plan_ref": digest_of("2"),
        "receipt_refs": (0..=MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS)
            .map(|index| {
                Sha256Digest::new(format!("sha256:{index:064x}"))
                    .expect("valid digest")
            })
            .collect::<Vec<_>>()
    });
    assert!(serde_json::from_value::<ContextCheckpointArtifactLineageV1>(too_many).is_err());
}

#[test]
fn v2_artifact_lineage_changes_checkpoint_digest() {
    let mut receipt_refs = vec![digest_of("4"), digest_of("5")];
    receipt_refs.sort();
    let artifact_lineage = ContextCheckpointArtifactLineageV1 {
        task_ref: digest_of("1"),
        plan_ref: Some(digest_of("2")),
        receipt_refs,
    };
    let mut source = fixture();
    source.lineage.receipt_ids = vec![
        ReceiptId::new("receipt-a").expect("receipt identity"),
        ReceiptId::new("receipt-b").expect("receipt identity"),
    ];
    let checkpoint = ContextCheckpointV2::from_v1_unverified(source, artifact_lineage)
        .expect("valid unverified V2 wire migration");
    let baseline_digest = checkpoint.digest().expect("digest");
    let mut changed = checkpoint.clone();
    changed.lineage.artifact_lineage.task_ref = digest_of("3");
    assert_ne!(changed.digest().expect("digest"), baseline_digest);
}
