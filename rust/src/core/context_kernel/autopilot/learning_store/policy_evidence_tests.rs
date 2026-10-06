// SPDX-License-Identifier: Apache-2.0
use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextQualitySectionV1, QualityEvidenceTierV1, QualityRetentionV1,
    QualityStateV1,
};
use lean_ctx_protocol::context_policy_evidence::QualityEvidenceV1;
use lean_ctx_protocol::{AcceptanceState, Sha256Digest};
use rusqlite::Connection;

use super::*;
use crate::core::context_admission::receipt_store::{LoadError, StoredReceipt};
use crate::core::context_kernel::autopilot::tests::{controller, input, protocol_task};
use crate::core::context_kernel::types::PlanEntry;
use crate::core::execution_protocol::test_support::build_for_context;

const DAY: u64 = 20_000;

/// An observation of a task that had no runtime signal events.
fn observed(
    decision: &AutopilotDecision,
    outcome: AcceptanceState,
    day: u64,
    receipts: &TaskReceipts,
) -> PolicyObservationV2 {
    observe(
        decision,
        outcome,
        day,
        receipts,
        Some(TaskSignals::default()),
    )
}

fn decision() -> AutopilotDecision {
    controller().plan(&input(), None).expect("plan")
}

fn entry(object_id: &str) -> PlanEntry {
    PlanEntry {
        object_id: object_id.to_owned(),
        provider: "files".to_owned(),
        view: "full".to_owned(),
        tokens: 10,
        phi: 0.5,
        reason: "test".to_owned(),
    }
}

fn quality(critical: (u64, u64, u64), recovery: bool) -> ContextQualitySectionV1 {
    let counts = |(retained, recoverable, lost)| RetentionCountsV1 {
        retained,
        recoverable,
        lost,
    };
    ContextQualitySectionV1 {
        evidence_tier: QualityEvidenceTierV1::DeterministicQuality,
        retention: QualityRetentionV1 {
            critical: counts(critical),
            important: counts((0, 0, 0)),
            lost_critical_kinds: Vec::new(),
            critical_unchecked: false,
            truncated: false,
            secret_lines_withheld: 0,
        },
        recovery: recovery.then_some(QualityRecoveryV1 {
            handles_emitted: 1,
            handles_verified: 1,
            failures: 0,
            critical_failures: 0,
        }),
        task_quality: QualityStateV1::Unmeasured,
        overall: QualityStateV1::Pass,
    }
}

fn receipt(
    original: u64,
    delivered: u64,
    measured: Option<ContextQualitySectionV1>,
) -> ContextDecisionReceiptV1 {
    use lean_ctx_protocol::context_gateway::*;
    use lean_ctx_protocol::{ProtocolReference, V1_SCHEMA_VERSION};
    ContextDecisionReceiptV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: ProtocolReference::new("rcpt-1").expect("reference"),
        mode: GatewayModeV1::Developer,
        principal: PrincipalV1::unknown(),
        task: None,
        destination: DestinationV1 {
            provider: ProtocolReference::new("mcp-host").expect("provider"),
            model: None,
            locality: DestinationLocalityV1::Unknown,
            organization_managed: false,
            account_ref: None,
            region: None,
        },
        policy: None,
        sources: SourceCountsV1 {
            inspected: 0,
            permitted: 0,
            selected: 0,
            blocked: 0,
        },
        decisions: Vec::new(),
        security: SecurityCountsV1 {
            redactions: 0,
            blocked_objects: 0,
            quarantined_objects: 0,
            injection_signals: 0,
            incomplete_coverage: 0,
        },
        tokens: TokenAccountV1 {
            original,
            delivered,
        },
        final_context: None,
        outcome: DeliveryOutcomeV1::Delivered,
        duration_us: 1,
        quality: measured,
    }
}

fn stored(receipt: ContextDecisionReceiptV1) -> (String, Result<StoredReceipt, LoadError>) {
    let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).expect("digest");
    ("a".repeat(64), Ok(StoredReceipt { digest, receipt }))
}

fn task(receipts: Vec<ContextDecisionReceiptV1>) -> TaskReceipts {
    TaskReceipts {
        entries: receipts.into_iter().map(stored).collect(),
        index_complete: true,
    }
}

#[test]
fn language_is_the_majority_of_planned_files_and_never_stores_a_path() {
    let mut planned = decision();
    planned.context_plan.selected = vec![
        entry("file:src/secret_project/a.rs"),
        entry("file:src/b.rs"),
        entry("file:web/c.ts"),
        entry("knowledge:decision:x"),
    ];
    let observation = observed(&planned, AcceptanceState::Accepted, DAY, &task(Vec::new()));
    assert_eq!(observation.workload.language, WorkloadLanguageV1::Rust);
    let json = serde_json::to_string(&observation).expect("json");
    assert!(
        !json.contains("secret_project") && !json.contains(".rs"),
        "{json}"
    );

    planned.context_plan.selected = vec![entry("knowledge:decision:x")];
    let observation = observed(&planned, AcceptanceState::Accepted, DAY, &task(Vec::new()));
    assert_eq!(observation.workload.language, WorkloadLanguageV1::None);
}

#[test]
fn a_configured_mode_never_leaves_as_text() {
    let mut planned = decision();
    planned.read_policy.mode = "internal-payments".to_owned();
    let observation = observed(&planned, AcceptanceState::Accepted, DAY, &task(Vec::new()));
    assert_eq!(observation.strategy, ReadStrategyV1::Other);
    let json = serde_json::to_string(&observation).expect("json");
    assert!(!json.contains("internal-payments"), "{json}");
}

#[test]
fn measurements_count_only_when_every_delivery_is_known_and_measured() {
    let planned = decision();
    let all = observed(
        &planned,
        AcceptanceState::Accepted,
        DAY,
        &task(vec![
            receipt(900, 300, Some(quality((5, 1, 0), true))),
            receipt(100, 50, Some(quality((2, 0, 0), true))),
        ]),
    );
    let (retention, recovery) = all.quality.expect("every delivery measured");
    assert_eq!(
        (retention.retained, retention.recoverable, retention.lost),
        (7, 1, 0)
    );
    assert_eq!(recovery.handles_verified, 2);
    assert_eq!(all.tokens, Some((1000, 350)));

    // One delivery without quality.
    let partial = observed(
        &planned,
        AcceptanceState::Accepted,
        DAY,
        &task(vec![
            receipt(900, 300, Some(quality((5, 1, 0), true))),
            receipt(100, 50, None),
        ]),
    );
    assert!(partial.quality.is_none());
    assert_eq!(
        partial.tokens,
        Some((1000, 350)),
        "tokens are still complete"
    );

    // Retention without recovery is not zero recovery failures.
    let no_recovery = observed(
        &planned,
        AcceptanceState::Accepted,
        DAY,
        &task(vec![receipt(900, 300, Some(quality((5, 0, 0), false)))]),
    );
    assert!(no_recovery.quality.is_none());

    // An unverified delivery or a full index makes everything partial.
    let mut tampered = task(vec![receipt(900, 300, Some(quality((5, 0, 0), true)))]);
    tampered
        .entries
        .push(("b".repeat(64), Err(LoadError::Tampered)));
    let observation = observed(&planned, AcceptanceState::Accepted, DAY, &tampered);
    assert!(observation.quality.is_none() && observation.tokens.is_none());
    let mut truncated = task(vec![receipt(900, 300, Some(quality((5, 0, 0), true)))]);
    truncated.index_complete = false;
    let observation = observed(&planned, AcceptanceState::Accepted, DAY, &truncated);
    assert!(observation.quality.is_none() && observation.tokens.is_none());

    // No deliveries: nothing was measured, neither tokens nor quality.
    let empty = observed(&planned, AcceptanceState::Accepted, DAY, &task(Vec::new()));
    assert_eq!(empty.tokens, None);
    assert!(empty.quality.is_none());
}

#[test]
fn fold_keeps_days_apart_and_measures_security_only_on_complete_tasks() {
    let planned = decision();
    let measured = |outcome, lost, day| {
        observed(
            &planned,
            outcome,
            day,
            &task(vec![receipt(900, 300, Some(quality((5, 0, lost), true)))]),
        )
    };
    let evidence = fold(&[
        measured(AcceptanceState::Accepted, 0, DAY),
        measured(AcceptanceState::Rejected, 1, DAY),
        measured(AcceptanceState::Accepted, 0, DAY + 1),
    ])
    .expect("fold");
    assert_eq!(evidence.records.len(), 2, "one record per day");
    let first = &evidence.records[0];
    assert_eq!(first.observed_day, DAY);
    assert_eq!(
        (
            first.samples,
            first.accepted,
            first.rejected,
            first.token_samples
        ),
        (2, 1, 1, 2)
    );
    assert!(matches!(
        first.quality,
        QualityEvidenceV1::Measured { retention, .. } if retention.lost == 1
    ));
    assert_eq!(
        first.security,
        SecurityEvidenceV1::Measured { regressions: 0 }
    );

    // A delivery that reached the model without complete inspection counts.
    let mut unprotected = receipt(900, 300, Some(quality((5, 0, 0), true)));
    unprotected.security.incomplete_coverage = 1;
    let evidence = fold(&[
        measured(AcceptanceState::Accepted, 0, DAY),
        observed(
            &planned,
            AcceptanceState::Accepted,
            DAY,
            &task(vec![unprotected]),
        ),
    ])
    .expect("fold");
    assert_eq!(
        evidence.records[0].security,
        SecurityEvidenceV1::Measured { regressions: 1 }
    );

    let mut unverified = task(vec![receipt(900, 300, None)]);
    unverified.index_complete = false;
    let evidence = fold(&[
        measured(AcceptanceState::Accepted, 0, DAY),
        observed(&planned, AcceptanceState::Accepted, DAY, &unverified),
    ])
    .expect("fold");
    let record = &evidence.records[0];
    assert_eq!(record.quality, QualityEvidenceV1::Unmeasured);
    assert_eq!(record.security, SecurityEvidenceV1::Unmeasured);
    assert_eq!((record.samples, record.token_samples), (2, 1));
}

/// Signals count per task; one unattributed event makes the task's signals
/// unknown instead of clean.
#[test]
fn runtime_signals_count_tasks_and_unattributed_events_are_unknown() {
    let planned = decision();
    let complete = || task(vec![receipt(900, 300, Some(quality((5, 0, 0), true)))]);
    let signals = |bounce, unattributed| TaskSignals {
        bounce,
        unattributed,
        ..TaskSignals::default()
    };
    let evidence = fold(&[
        observe(
            &planned,
            AcceptanceState::Accepted,
            DAY,
            &complete(),
            Some(signals(2, 0)),
        ),
        observe(
            &planned,
            AcceptanceState::Accepted,
            DAY,
            &complete(),
            Some(signals(0, 0)),
        ),
        observe(
            &planned,
            AcceptanceState::Rejected,
            DAY,
            &complete(),
            Some(signals(1, 1)),
        ),
        observe(&planned, AcceptanceState::Accepted, DAY, &complete(), None),
    ])
    .expect("fold");
    let record = &evidence.records[0];
    assert_eq!(record.samples, 4);
    assert_eq!(
        (
            record.signal_samples,
            record.bounce_tasks,
            record.expand_tasks
        ),
        (2, 1, 0)
    );
}

#[test]
fn fold_is_order_independent_and_empty_is_valid() {
    let planned = decision();
    let mut full = planned.clone();
    full.read_policy.mode = "full".to_owned();
    let a = observed(&planned, AcceptanceState::Accepted, DAY, &task(Vec::new()));
    let b = observed(&full, AcceptanceState::Rejected, DAY, &task(Vec::new()));
    assert_eq!(
        serde_json::to_string(&fold(&[a.clone(), b.clone()]).expect("fold")).expect("json"),
        serde_json::to_string(&fold(&[b, a]).expect("fold")).expect("json")
    );
    assert!(fold(&[]).expect("fold").records.is_empty());
}

#[test]
fn utc_day_is_the_calendar_day_of_the_timestamp() {
    assert_eq!(utc_day("1970-01-02T00:00:00Z"), Some(1));
    assert_eq!(utc_day("2026-10-03T23:59:59+00:00"), Some(20_729));
    assert_eq!(
        utc_day("2026-10-04T00:30:00+02:00"),
        Some(20_729),
        "UTC, not local"
    );
    assert_eq!(utc_day("not a time"), None);
    assert_eq!(utc_day("1969-12-31T23:00:00Z"), None);
}

#[test]
fn unknown_outcomes_are_refused_at_insert() {
    let connection = Connection::open_in_memory().expect("db");
    initialize(&connection).expect("schema");
    let observation = observed(
        &decision(),
        AcceptanceState::Unknown,
        DAY,
        &task(Vec::new()),
    );
    assert!(insert(&connection, "scope", "receipt-1", &observation).is_err());
}

/// The live path: a validated terminal outcome trains the planner and records
/// exactly one observation; a replay records nothing; reset clears it.
#[test]
fn a_validated_outcome_records_one_observation_and_reset_clears_it() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("learning.db");
    let task_envelope = protocol_task();
    let mut store = AdaptiveLearningStore::new(
        Connection::open(&path).expect("db"),
        task_envelope.project_id.clone(),
        task_envelope.tenant_id.clone(),
    )
    .expect("store");
    assert!(
        store
            .policy_evidence()
            .expect("evidence")
            .records
            .is_empty()
    );
    let handoff = store
        .plan_for_task(&controller(), &task_envelope, &input(), None)
        .expect("plan");
    let fixture = build_for_context(
        &task_envelope,
        Some(handoff.context_projection()),
        Some(&handoff),
        AcceptanceState::Accepted,
    );
    let admitted = fixture.validated_for(&task_envelope).expect("admitted");
    assert!(
        store
            .observe_protocol_outcome(&handoff, &admitted)
            .expect("observe")
    );
    assert!(
        !store
            .observe_protocol_outcome(&handoff, &admitted)
            .expect("replay")
    );

    let evidence = store.policy_evidence().expect("evidence");
    assert_eq!(evidence.records.len(), 1);
    let record = &evidence.records[0];
    assert_eq!((record.samples, record.accepted), (1, 1));
    assert_eq!(
        record.strategy,
        ReadStrategyV1::from_mode(&handoff.decision().read_policy.mode)
    );
    assert_eq!(
        Some(record.observed_day),
        utc_day(&admitted.outcome().observed_at)
    );
    assert_eq!(
        record.quality,
        QualityEvidenceV1::Unmeasured,
        "no receipts were written"
    );

    store.reset().expect("reset");
    assert!(
        store
            .policy_evidence()
            .expect("evidence")
            .records
            .is_empty()
    );
}
