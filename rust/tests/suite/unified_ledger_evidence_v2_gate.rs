//! Adversarial gate for the savings-ledger -> Settlement Evidence V2 adapter.

use ed25519_dalek::SigningKey;
use lean_ctx::core::billing::settlement_evidence::{
    EvidenceTrustStatusV2, EvidenceTrustV2, SettlementEvidenceClaimV2,
    SettlementEvidenceManifestV2, SettlementEvidenceRoleV2, SettlementEvidenceTrustStoreV2,
    TrustedEvidenceDecisionV2, reconcile_settlement_evidence_v2,
};
use lean_ctx::core::savings_ledger::event::{
    MECHANISM_COMPRESSION, MECHANISM_ROUTING, SavingsEvent, compute_hash,
};
use lean_ctx::core::savings_ledger::evidence_projection::MAX_LEDGER_SNAPSHOT_BYTES_V2;
use lean_ctx::core::savings_ledger::signed_batch::BatchTotals;
use lean_ctx::core::savings_ledger::store::{LedgerSnapshotReadErrorV2, read_verified_snapshot_v2};
use lean_ctx::core::savings_ledger::{
    LedgerAttributionLinkV2, LedgerEvidenceProjectionV2, LedgerProjectionErrorV2,
    SignedSavingsBatchV1, VerifiedLedgerSnapshotV2, load_projection_artifact_v2,
    project_settlement_attribution_v2 as project_with_batch,
};

const SETTLEMENT_FIXTURE: &str = include_str!("../fixtures/settlement-evidence-v2/eligible.json");

fn artifact(label: &str) -> String {
    format!(
        "artifact:blake3:{}",
        blake3::hash(label.as_bytes()).to_hex()
    )
}

fn anchor(label: &str) -> String {
    format!("anchor:blake3:{}", blake3::hash(label.as_bytes()).to_hex())
}

fn trusted(label: &str) -> EvidenceTrustV2 {
    EvidenceTrustV2 {
        status: EvidenceTrustStatusV2::Trusted,
        trust_decision_id: artifact(&format!("trust-decision:{label}")),
        trust_anchor_id: anchor("operator-pinned-ledger-attestor"),
    }
}

fn event(
    previous: &str,
    mechanism: &str,
    baseline_tokens: u64,
    saved_tokens: u64,
    saved_usd: f64,
) -> SavingsEvent {
    let mut event = SavingsEvent {
        ts: "2026-07-19T10:00:00+00:00".into(),
        tool: "ctx_read".into(),
        mechanism: mechanism.into(),
        model_id: "fixture-model".into(),
        tokenizer: "o200k_base".into(),
        baseline_tokens,
        actual_tokens: baseline_tokens.saturating_sub(saved_tokens),
        saved_tokens,
        bounce_adjustment: 0,
        unit_price_per_m_usd: 2.0,
        saved_usd,
        repo_hash: "fixture-repo".into(),
        agent_id: "fixture-agent".into(),
        prev_hash: previous.into(),
        entry_hash: String::new(),
        version: "3.9.12".into(),
        intent_tag: None,
        outcome: None,
        model_original: None,
        model_routed: None,
        routing_savings: None,
        response_original_tokens: None,
        response_delivered_tokens: None,
        agent_chain_id: None,
        chain_depth: None,
        measurement_method: None,
        evidence_class: None,
        confidence: None,
        quality_signal: None,
        attribution_group: None,
        attribution_id: None,
        baseline_ref: None,
        price_version: None,
        customer_approval: None,
        settlement_status: None,
        is_first_inject: None,
        cache_read_per_m_usd: None,
        cache_write_per_m_usd: None,
    };
    event.entry_hash = compute_hash(previous, &event.canonical_content());
    event
}

fn link(
    event: &SavingsEvent,
    label: &str,
    tokens: u64,
    minor_units: u64,
) -> LedgerAttributionLinkV2 {
    LedgerAttributionLinkV2 {
        ledger_entry_hash: event.entry_hash.clone(),
        source_evidence_id: artifact(&format!("reconciled-source:{label}")),
        attribution_group_id: artifact(&format!("exclusive-group:{label}")),
        attributed_tokens: tokens,
        attributed_minor_units: minor_units,
        trust: trusted(label),
    }
}

fn signed_batch(snapshot: &VerifiedLedgerSnapshotV2) -> SignedSavingsBatchV1 {
    let mut batch = SignedSavingsBatchV1 {
        schema_version: 1,
        kind: "lean-ctx.savings-batch".into(),
        created_at: "2026-07-19T10:00:00Z".into(),
        lean_ctx_version: "3.9.12".into(),
        agent_id: "fixture-agent".into(),
        period: "all".into(),
        first_entry_hash: snapshot.first_entry_hash().into(),
        last_entry_hash: snapshot.last_entry_hash().into(),
        chain_valid: true,
        totals: BatchTotals {
            total_events: snapshot.event_count(),
            saved_tokens: 0,
            net_saved_tokens: 0,
            saved_usd: 0.0,
            bounce_tokens: 0,
            bounce_events: 0,
            tokenizers: Vec::new(),
            by_model: Vec::new(),
            by_tool: Vec::new(),
            by_mechanism: Vec::new(),
        },
        signer_public_key: None,
        signature: None,
    };
    batch
        .sign_with_key(&SigningKey::from_bytes(&[7_u8; 32]))
        .unwrap();
    batch
}

fn project(
    snapshot: &VerifiedLedgerSnapshotV2,
    subject_id: &str,
    links: &[LedgerAttributionLinkV2],
) -> Result<LedgerEvidenceProjectionV2, LedgerProjectionErrorV2> {
    project_with_batch(snapshot, &signed_batch(snapshot), subject_id, links)
}

#[cfg(unix)]
fn make_fifo(path: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a live NUL-terminated buffer and the mode is a valid permission mask.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        result,
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );
}

fn trust_store_for(manifest: &SettlementEvidenceManifestV2) -> SettlementEvidenceTrustStoreV2 {
    SettlementEvidenceTrustStoreV2::new(
        manifest
            .evidence
            .iter()
            .map(|item| TrustedEvidenceDecisionV2 {
                evidence_id: item.evidence_id.clone(),
                trust_decision_id: item.trust.trust_decision_id.clone(),
                trust_anchor_id: item.trust.trust_anchor_id.clone(),
            })
            .collect(),
    )
    .expect("bounded exact trust store")
}

fn manifest_with_projection(
    projection_items: Vec<lean_ctx::core::billing::settlement_evidence::SettlementEvidenceItemV2>,
    claimed_minor_units: u64,
) -> SettlementEvidenceManifestV2 {
    let fixture: SettlementEvidenceManifestV2 =
        serde_json::from_str(SETTLEMENT_FIXTURE).expect("settlement fixture parses");
    let mut evidence = fixture
        .evidence
        .into_iter()
        .filter(|item| item.claim.role() != SettlementEvidenceRoleV2::Attribution)
        .collect::<Vec<_>>();
    evidence.extend(projection_items);
    SettlementEvidenceManifestV2::new(
        fixture.subject_id,
        fixture.period,
        fixture.currency,
        claimed_minor_units,
        evidence,
    )
    .expect("projected manifest is bounded")
}

#[test]
fn projection_reuses_the_existing_settlement_verifier_and_external_trust_store() {
    let fixture: SettlementEvidenceManifestV2 = serde_json::from_str(SETTLEMENT_FIXTURE).unwrap();
    let ledger_event = event("genesis", MECHANISM_COMPRESSION, 10_000, 2_500, 5.0);
    let snapshot = VerifiedLedgerSnapshotV2::try_from_events(vec![ledger_event.clone()]).unwrap();
    let projection = project(
        &snapshot,
        &fixture.subject_id,
        &[link(&ledger_event, "compression-1", 2_500, 500)],
    )
    .unwrap();

    assert_eq!(projection.schema_version, 2);
    assert_eq!(projection.event_count, 1);
    assert_eq!(projection.bindings.len(), 1);
    assert_eq!(projection.settlement_attribution_items.len(), 1);
    assert!(matches!(
        projection.settlement_attribution_items[0].claim,
        SettlementEvidenceClaimV2::Attribution {
            exclusive: true,
            attributed_tokens: 2_500,
            attributed_minor_units: 500,
            ..
        }
    ));

    let manifest = manifest_with_projection(projection.settlement_attribution_items, 500);
    let no_external_trust =
        reconcile_settlement_evidence_v2(&manifest, &SettlementEvidenceTrustStoreV2::empty());
    assert!(
        !no_external_trust.eligible,
        "self-attestation must fail closed"
    );

    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(result.eligible, "{:?}", result.reasons);
    assert_eq!(result.attributed_tokens, Some(2_500));
    assert_eq!(result.attributed_minor_units, Some(500));
    assert!(!result.invoice_authority);
    assert!(!result.contract_validity_verified);
    assert!(!result.customer_approval_authority_verified);
}

#[test]
fn projection_is_link_order_deterministic_and_payload_free() {
    let first = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    let second = event(&first.entry_hash, MECHANISM_COMPRESSION, 2_000, 600, 0.06);
    let snapshot =
        VerifiedLedgerSnapshotV2::try_from_events(vec![first.clone(), second.clone()]).unwrap();
    let first_link = link(&first, "first", 400, 40);
    let mut second_link = link(&second, "second", 600, 60);
    second_link.trust = first_link.trust.clone();
    let links = vec![first_link, second_link];
    let mut reversed = links.clone();
    reversed.reverse();
    let subject = artifact("subject").replacen("artifact:", "subject:", 1);

    let forward = project(&snapshot, &subject, &links).unwrap();
    let backward = project(&snapshot, &subject, &reversed).unwrap();
    assert_eq!(forward, backward);

    let wire = serde_json::to_string(&forward).unwrap();
    for forbidden in ["ctx_read", "fixture-model", "fixture-repo", "fixture-agent"] {
        assert!(!wire.contains(forbidden), "payload leaked: {forbidden}");
    }
}

#[test]
fn missing_duplicate_and_cross_mechanism_attribution_fail_closed() {
    let compression = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    let routing = event(&compression.entry_hash, MECHANISM_ROUTING, 1_000, 0, 0.03);
    let snapshot =
        VerifiedLedgerSnapshotV2::try_from_events(vec![compression.clone(), routing.clone()])
            .unwrap();
    let subject = artifact("subject").replacen("artifact:", "subject:", 1);

    assert!(matches!(
        project(
            &snapshot,
            &subject,
            &[link(&compression, "compression", 400, 40)]
        ),
        Err(LedgerProjectionErrorV2::MissingLink { .. })
    ));

    let duplicate = link(&compression, "compression", 400, 40);
    assert!(matches!(
        project(&snapshot, &subject, &[duplicate.clone(), duplicate]),
        Err(LedgerProjectionErrorV2::DuplicateLink { .. })
    ));

    let compression_link = link(&compression, "compression", 400, 40);
    let mut routing_link = link(&routing, "routing", 1_000, 30);
    routing_link.attribution_group_id = compression_link.attribution_group_id.clone();
    assert!(matches!(
        project(&snapshot, &subject, &[compression_link, routing_link]),
        Err(LedgerProjectionErrorV2::DuplicateAttributionGroup { .. })
    ));
}

#[test]
fn duplicate_source_overclaim_unknown_mechanism_and_adjustments_fail_closed() {
    let first = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    let second = event(&first.entry_hash, MECHANISM_COMPRESSION, 1_000, 500, 0.05);
    let snapshot =
        VerifiedLedgerSnapshotV2::try_from_events(vec![first.clone(), second.clone()]).unwrap();
    let subject = artifact("subject").replacen("artifact:", "subject:", 1);
    let first_link = link(&first, "first", 400, 40);
    let mut second_link = link(&second, "second", 500, 50);
    second_link.source_evidence_id = first_link.source_evidence_id.clone();
    assert!(matches!(
        project(&snapshot, &subject, &[first_link, second_link]),
        Err(LedgerProjectionErrorV2::DuplicateSource { .. })
    ));

    let one = VerifiedLedgerSnapshotV2::try_from_events(vec![first.clone()]).unwrap();
    assert!(matches!(
        project(&one, &subject, &[link(&first, "overclaim", 401, 40)]),
        Err(LedgerProjectionErrorV2::AttributionExceedsObservation { .. })
    ));

    let mut unknown = first.clone();
    unknown.mechanism = "guessed".into();
    unknown.entry_hash = compute_hash("genesis", &unknown.canonical_content());
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![unknown]),
        Err(LedgerProjectionErrorV2::InvalidObservation { .. })
    ));

    let mut adjusted = first;
    adjusted.tool = "bounce".into();
    adjusted.baseline_tokens = 10;
    adjusted.actual_tokens = 10;
    adjusted.saved_tokens = 0;
    adjusted.bounce_adjustment = 10;
    adjusted.saved_usd = -0.001;
    adjusted.entry_hash = compute_hash("genesis", &adjusted.canonical_content());
    let adjusted_snapshot = VerifiedLedgerSnapshotV2::try_from_events(vec![adjusted]).unwrap();
    assert!(matches!(
        project(&adjusted_snapshot, &subject, &[]),
        Err(LedgerProjectionErrorV2::AdjustmentRequiresReconciliation { .. })
    ));
}

#[test]
fn tampered_chain_and_arithmetic_overflow_fail_closed() {
    let ledger_event = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    let mut tampered = ledger_event;
    tampered.saved_tokens += 1;
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![tampered]),
        Err(LedgerProjectionErrorV2::BrokenChain { index: 0 })
    ));

    let first = event("genesis", MECHANISM_COMPRESSION, u64::MAX, u64::MAX, 1.0);
    let second = event(&first.entry_hash, MECHANISM_COMPRESSION, 1, 1, 1.0);
    let snapshot =
        VerifiedLedgerSnapshotV2::try_from_events(vec![first.clone(), second.clone()]).unwrap();
    let subject = artifact("subject").replacen("artifact:", "subject:", 1);
    let mut first_link = link(&first, "first", u64::MAX, u64::MAX);
    let mut second_link = link(&second, "second", 1, 1);
    second_link.trust = first_link.trust.clone();
    first_link.trust.status = EvidenceTrustStatusV2::Trusted;
    assert!(matches!(
        project(&snapshot, &subject, &[first_link, second_link]),
        Err(LedgerProjectionErrorV2::ArithmeticOverflow)
    ));
}

#[test]
fn projection_artifact_and_signed_batch_binding_are_offline_verifiable() {
    let ledger_event = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    let snapshot = VerifiedLedgerSnapshotV2::try_from_events(vec![ledger_event.clone()]).unwrap();
    let batch = signed_batch(&snapshot);
    let subject = artifact("subject").replacen("artifact:", "subject:", 1);
    let projection = project_with_batch(
        &snapshot,
        &batch,
        &subject,
        &[link(&ledger_event, "offline", 400, 40)],
    )
    .unwrap();
    projection.verify(&batch, &snapshot).unwrap();

    let mut malformed_subject = projection.clone();
    malformed_subject.subject_id = "subject:not-a-blake3-digest".into();
    malformed_subject.settlement_attribution_items[0].subject_id =
        malformed_subject.subject_id.clone();
    assert!(matches!(
        malformed_subject.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidSubjectId)
    ));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("projection.json");
    std::fs::write(&path, projection.canonical_json(&batch, &snapshot).unwrap()).unwrap();
    assert_eq!(
        load_projection_artifact_v2(&path, &batch, &snapshot).unwrap(),
        projection
    );

    let malformed_path = dir.path().join("malformed-subject.json");
    std::fs::write(
        &malformed_path,
        serde_json::to_vec(&malformed_subject).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        load_projection_artifact_v2(&malformed_path, &batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidSubjectId)
    ));

    let mut wrong_id = projection.clone();
    wrong_id.projection_id = artifact("forged-projection");
    assert!(matches!(
        wrong_id.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidProjectionId)
    ));

    let mut wrong_binding = projection.clone();
    wrong_binding.bindings[0].projected_source_id = artifact("forged-source");
    assert!(matches!(
        wrong_binding.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidProjectionBinding)
    ));

    let mut forged_membership = projection.clone();
    forged_membership.bindings[0].ledger_entry_hash = "a".repeat(64);
    assert!(matches!(
        forged_membership.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidProjectionBinding)
    ));

    let mut forged_mechanism = projection.clone();
    forged_mechanism.bindings[0].mechanism_id = format!(
        "mechanism:blake3:{}",
        blake3::hash(b"invented-mechanism").to_hex()
    );
    assert!(matches!(
        forged_mechanism.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidProjectionBinding)
    ));

    let mut wrong_item = projection.clone();
    if let SettlementEvidenceClaimV2::Attribution {
        attributed_tokens, ..
    } = &mut wrong_item.settlement_attribution_items[0].claim
    {
        *attributed_tokens += 1;
    }
    assert!(matches!(
        wrong_item.verify(&batch, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidProjectionBinding)
    ));

    let mut wrong_subject = projection.clone();
    wrong_subject.settlement_attribution_items[0].subject_id =
        artifact("other-subject").replacen("artifact:", "subject:", 1);
    assert!(wrong_subject.verify(&batch, &snapshot).is_err());

    let mut wrong_state = projection.clone();
    wrong_state.settlement_attribution_items[0].state =
        lean_ctx::core::billing::settlement_evidence::EvidenceStateV2::Disputed;
    assert!(wrong_state.verify(&batch, &snapshot).is_err());

    let mut wrong_evidence_id = projection.clone();
    wrong_evidence_id.settlement_attribution_items[0].evidence_id = artifact("forged-item");
    assert!(wrong_evidence_id.verify(&batch, &snapshot).is_err());

    let mut duplicate_item = projection.clone();
    duplicate_item
        .settlement_attribution_items
        .push(duplicate_item.settlement_attribution_items[0].clone());
    assert!(duplicate_item.verify(&batch, &snapshot).is_err());

    let mut wrong_head = batch.clone();
    wrong_head.last_entry_hash = "0".repeat(64);
    wrong_head
        .sign_with_key(&SigningKey::from_bytes(&[7_u8; 32]))
        .unwrap();
    assert!(matches!(
        projection.verify(&wrong_head, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidSignedBatch)
    ));

    let mut wrong_count = batch.clone();
    wrong_count.totals.total_events += 1;
    wrong_count
        .sign_with_key(&SigningKey::from_bytes(&[7_u8; 32]))
        .unwrap();
    assert!(matches!(
        projection.verify(&wrong_count, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidSignedBatch)
    ));

    let mut wrong_signature = batch;
    wrong_signature.signature = Some("00".repeat(64));
    assert!(matches!(
        projection.verify(&wrong_signature, &snapshot),
        Err(LedgerProjectionErrorV2::InvalidSignedBatch)
    ));

    #[cfg(unix)]
    {
        let fifo = dir.path().join("projection.fifo");
        make_fifo(&fifo);
        assert!(matches!(
            load_projection_artifact_v2(&fifo, &signed_batch(&snapshot), &snapshot),
            Err(LedgerProjectionErrorV2::ArtifactNotRegular)
        ));
    }
}

#[test]
fn impossible_counters_prices_and_oversized_fields_fail_before_projection() {
    let mut impossible = event("genesis", MECHANISM_COMPRESSION, 1, 1, 0.01);
    impossible.saved_tokens = 10_000;
    impossible.entry_hash = compute_hash("genesis", &impossible.canonical_content());
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![impossible]),
        Err(LedgerProjectionErrorV2::InvalidObservation { .. })
    ));

    let mut negative_price = event("genesis", MECHANISM_COMPRESSION, 10, 5, 0.01);
    negative_price.unit_price_per_m_usd = -1.0;
    negative_price.entry_hash = compute_hash("genesis", &negative_price.canonical_content());
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![negative_price]),
        Err(LedgerProjectionErrorV2::InvalidObservation { .. })
    ));

    let mut invalid_routing = event("genesis", MECHANISM_ROUTING, 10, 1, 0.01);
    invalid_routing.entry_hash = compute_hash("genesis", &invalid_routing.canonical_content());
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![invalid_routing]),
        Err(LedgerProjectionErrorV2::InvalidObservation { .. })
    ));

    let mut oversized = event("genesis", MECHANISM_COMPRESSION, 10, 5, 0.01);
    oversized.tool = "x".repeat(257);
    oversized.entry_hash = compute_hash("genesis", &oversized.canonical_content());
    assert!(matches!(
        VerifiedLedgerSnapshotV2::try_from_events(vec![oversized]),
        Err(LedgerProjectionErrorV2::OversizedEventField { .. })
    ));
}

#[test]
fn file_snapshot_reader_is_single_pass_bounded_and_refuses_unsafe_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let ledger_event = event("genesis", MECHANISM_COMPRESSION, 1_000, 400, 0.04);
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&ledger_event).unwrap()),
    )
    .unwrap();
    let snapshot = read_verified_snapshot_v2(&path).unwrap();
    assert_eq!(snapshot.event_count(), 1);
    assert_eq!(snapshot.last_entry_hash(), ledger_event.entry_hash);

    let malformed = dir.path().join("malformed.jsonl");
    std::fs::write(&malformed, b"{not-json}\n").unwrap();
    assert!(matches!(
        read_verified_snapshot_v2(&malformed),
        Err(LedgerSnapshotReadErrorV2::MalformedJson { .. })
    ));

    let invalid_utf8 = dir.path().join("invalid-utf8.jsonl");
    std::fs::write(&invalid_utf8, [0xff, b'\n']).unwrap();
    assert!(matches!(
        read_verified_snapshot_v2(&invalid_utf8),
        Err(LedgerSnapshotReadErrorV2::Utf8)
    ));

    let oversized = dir.path().join("oversized.jsonl");
    std::fs::write(
        &oversized,
        vec![b'x'; MAX_LEDGER_SNAPSHOT_BYTES_V2 as usize + 1],
    )
    .unwrap();
    assert!(matches!(
        read_verified_snapshot_v2(&oversized),
        Err(LedgerSnapshotReadErrorV2::TooLarge)
    ));

    assert!(matches!(
        read_verified_snapshot_v2(dir.path()),
        Err(LedgerSnapshotReadErrorV2::NotRegular)
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link_path = dir.path().join("ledger-link.jsonl");
        symlink(&path, &link_path).unwrap();
        assert!(matches!(
            read_verified_snapshot_v2(&link_path),
            Err(LedgerSnapshotReadErrorV2::NotRegular)
        ));

        let fifo = dir.path().join("ledger.fifo");
        make_fifo(&fifo);
        assert!(matches!(
            read_verified_snapshot_v2(&fifo),
            Err(LedgerSnapshotReadErrorV2::NotRegular)
        ));
    }
}

#[test]
fn signed_savings_batch_v1_wire_remains_backward_compatible() {
    let batch: SignedSavingsBatchV1 = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "kind": "lean-ctx.savings-batch",
        "created_at": "2026-07-19T10:00:00Z",
        "lean_ctx_version": "3.9.12",
        "agent_id": "fixture-agent",
        "period": "all",
        "first_entry_hash": "genesis",
        "last_entry_hash": "genesis",
        "chain_valid": true,
        "totals": {
            "total_events": 0,
            "saved_tokens": 0,
            "net_saved_tokens": 0,
            "saved_usd": 0.0,
            "bounce_tokens": 0,
            "bounce_events": 0,
            "tokenizers": [],
            "by_model": [],
            "by_tool": [],
            "by_mechanism": []
        },
        "signer_public_key": null,
        "signature": null
    }))
    .expect("frozen v1 wire parses");
    assert_eq!(batch.schema_version, 1);
    assert_eq!(batch.kind, "lean-ctx.savings-batch");
    assert!(
        !batch.verify().signature_valid,
        "unsigned v1 remains invalid"
    );
}
