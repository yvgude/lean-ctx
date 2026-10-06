// SPDX-License-Identifier: Apache-2.0
use super::*;

fn digest(fill: char) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn reference(value: &str) -> ProtocolReference {
    ProtocolReference::new(value).expect("reference")
}

fn reason(value: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(value).expect("reason")
}

fn person() -> PrincipalV1 {
    PrincipalV1::new(PrincipalKindV1::Person, reference("user-1")).expect("principal")
}

fn complete(bytes: u64) -> DetectorCoverageV1 {
    DetectorCoverageV1 {
        kind: CoverageKindV1::Complete,
        bytes_total: bytes,
        bytes_inspected: bytes,
        chunks_total: 1,
        chunks_inspected: 1,
        reason: None,
    }
}

fn signal(category: DetectorCategoryV1) -> DetectorSignalV1 {
    DetectorSignalV1 {
        detector: DetectorRefV1 {
            id: reference("secrets.builtin"),
            version: SemanticVersion::new("1.0.0").expect("version"),
        },
        category,
        severity: SeverityV1::High,
        confidence_milli: Some(900),
        calibrated: false,
        evidence_count: 1,
        coverage: complete(10),
        status: DetectorStatusV1::Completed,
        latency_us: 12,
    }
}

fn passport(
    fill: char,
    classification: ClassificationV1,
    trust: TrustLevelV1,
    constraints: Vec<DestinationConstraintV1>,
) -> ContextObjectPassportV1 {
    ContextObjectPassportV1 {
        schema_version: V1_SCHEMA_VERSION,
        object: digest(fill),
        source: SourceRefV1 {
            kind: SourceKindV1::File,
            id: SourceId::new(format!("src-{fill}")).expect("source"),
        },
        principal: person(),
        task: None,
        classification,
        trust,
        policy_refs: Vec::new(),
        risk_signals: Vec::new(),
        destination_constraints: constraints,
        tokens: 100,
        bytes: 400,
        lineage: Vec::new(),
    }
}

fn step(input: char, output: char) -> TransformationRecordV1 {
    TransformationRecordV1 {
        kind: TransformationKindV1::Summarization,
        input: digest(input),
        output: digest(output),
        tokens_before: 200,
        tokens_after: 50,
        reason_codes: vec![reason("budget.fit")],
    }
}

fn destination(locality: DestinationLocalityV1, organization_managed: bool) -> DestinationV1 {
    DestinationV1 {
        provider: reference("anthropic"),
        model: None,
        locality,
        organization_managed,
        account_ref: None,
        region: None,
    }
}

fn decision(disposition: ContextDispositionV1, fill: char) -> ContextDecisionV1 {
    ContextDecisionV1 {
        object: digest(fill),
        disposition,
        reason_codes: if disposition == ContextDispositionV1::Allow {
            Vec::new()
        } else {
            vec![reason("secret.detected")]
        },
        signals: Vec::new(),
        required_transformations: Vec::new(),
    }
}

fn receipt() -> ContextDecisionReceiptV1 {
    ContextDecisionReceiptV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: reference("rcpt-1"),
        mode: GatewayModeV1::Governed,
        principal: person(),
        task: None,
        destination: destination(DestinationLocalityV1::Remote, true),
        policy: None,
        sources: SourceCountsV1 {
            inspected: 3,
            permitted: 2,
            selected: 2,
            blocked: 1,
        },
        decisions: vec![
            decision(ContextDispositionV1::Allow, 'a'),
            decision(ContextDispositionV1::AllowRedacted, 'b'),
            decision(ContextDispositionV1::Deny, 'c'),
        ],
        security: SecurityCountsV1 {
            redactions: 1,
            blocked_objects: 1,
            quarantined_objects: 0,
            injection_signals: 0,
            incomplete_coverage: 0,
        },
        tokens: TokenAccountV1 {
            original: 900,
            delivered: 300,
        },
        final_context: Some(digest('f')),
        outcome: DeliveryOutcomeV1::Delivered,
        duration_us: 1_500,
        quality: None,
    }
}

// ─── Classification ─────────────────────────────────────────────────────────

#[test]
fn classification_join_is_the_maximum_and_commutative() {
    use ClassificationV1::*;
    let all = [Public, Internal, Confidential, Restricted];
    for a in all {
        for b in all {
            assert_eq!(a.join(b), b.join(a));
            assert!(a.join(b) >= a && a.join(b) >= b);
        }
        assert_eq!(a.join(a), a);
    }
    assert_eq!(Internal.join(Restricted), Restricted);
}

#[test]
fn existing_four_level_enums_convert_losslessly() {
    use crate::experiment::DataClassification as D;
    use crate::knowledge::ClassificationLevel as K;
    use crate::team_context::TeamClassification as T;
    assert_eq!(
        ClassificationV1::from(T::Restricted),
        ClassificationV1::Restricted
    );
    assert_eq!(
        ClassificationV1::from(K::Internal),
        ClassificationV1::Internal
    );
    assert_eq!(
        ClassificationV1::from(D::Confidential),
        ClassificationV1::Confidential
    );
    assert_eq!(ClassificationV1::from(D::Public), ClassificationV1::Public);
}

#[test]
fn via_placement_becomes_a_constraint_not_a_sensitivity_guess() {
    use crate::edge_via::ViaClassificationV1 as V;
    assert_eq!(
        project_via_classification(V::LocalOnly),
        (
            ClassificationV1::Confidential,
            Some(DestinationConstraintV1::LocalOnly)
        )
    );
    assert_eq!(
        project_via_classification(V::EnterprisePrivate),
        (
            ClassificationV1::Confidential,
            Some(DestinationConstraintV1::OrganizationPrivate)
        )
    );
    assert_eq!(
        project_via_classification(V::Secret),
        (ClassificationV1::Restricted, None)
    );
}

#[test]
fn governed_modes_never_treat_unclassified_content_as_public() {
    assert_eq!(
        GatewayModeV1::Developer.unclassified(),
        ClassificationV1::Public
    );
    assert_eq!(
        GatewayModeV1::Governed.unclassified(),
        ClassificationV1::Internal
    );
    assert_eq!(
        GatewayModeV1::Sovereign.unclassified(),
        ClassificationV1::Internal
    );
}

// ─── Principal and destination ──────────────────────────────────────────────

#[test]
fn unknown_principal_is_explicit_and_carries_no_identity() {
    assert!(PrincipalV1::new(PrincipalKindV1::Unknown, reference("x")).is_err());
    let unknown = PrincipalV1::unknown();
    assert!(!unknown.is_known());
    let json = serde_json::to_string(&unknown).expect("serialize");
    assert_eq!(json, r#"{"kind":"unknown"}"#);
    assert_eq!(
        serde_json::from_str::<PrincipalV1>(&json).expect("parse"),
        unknown
    );
    assert!(serde_json::from_str::<PrincipalV1>(r#"{"kind":"unknown","id":"x"}"#).is_err());
    assert!(serde_json::from_str::<PrincipalV1>(r#"{"kind":"person"}"#).is_err());
}

#[test]
fn destination_constraints_fail_closed_on_unknown_locality() {
    let local = destination(DestinationLocalityV1::Local, false);
    let org = destination(DestinationLocalityV1::Remote, true);
    let public = destination(DestinationLocalityV1::Remote, false);
    let unknown = destination(DestinationLocalityV1::Unknown, true);
    use DestinationConstraintV1::*;
    assert!(local.satisfies(LocalOnly) && local.satisfies(OrganizationPrivate));
    assert!(!org.satisfies(LocalOnly) && org.satisfies(OrganizationPrivate));
    assert!(!public.satisfies(LocalOnly) && !public.satisfies(OrganizationPrivate));
    assert!(!unknown.satisfies(LocalOnly) && !unknown.satisfies(OrganizationPrivate));
    assert!(org.satisfies_all(&[]));
    assert!(!org.satisfies_all(&[OrganizationPrivate, LocalOnly]));
}

// ─── Reason codes, coverage, signals ────────────────────────────────────────

#[test]
fn reason_codes_are_stable_machine_identifiers() {
    assert!(ReasonCodeV1::new("secret.detected").is_ok());
    assert!(ReasonCodeV1::new("pii_email.v2").is_ok());
    for bad in [
        "",
        "ab",
        "Secret",
        "1abc",
        "has space",
        "a-b-c",
        &"a".repeat(65),
    ] {
        assert!(ReasonCodeV1::new(bad).is_err(), "{bad:?} accepted");
    }
    assert!(serde_json::from_str::<ReasonCodeV1>(r#""Bad Code""#).is_err());
}

#[test]
fn coverage_never_claims_more_than_it_inspected() {
    let mut coverage = complete(10);
    assert!(coverage.validate().is_ok());
    coverage.bytes_inspected = 9;
    assert!(coverage.validate().is_err(), "complete with gaps");
    coverage.kind = CoverageKindV1::Partial;
    assert!(coverage.validate().is_ok());
    coverage.bytes_inspected = 10;
    assert!(coverage.validate().is_err(), "partial without gaps");
    coverage.bytes_inspected = 11;
    assert!(coverage.validate().is_err(), "more than the object");
    let wire = r#"{"kind":"complete","bytes_total":10,"bytes_inspected":4,"chunks_total":1,"chunks_inspected":1}"#;
    assert!(serde_json::from_str::<DetectorCoverageV1>(wire).is_err());
}

#[test]
fn only_a_completed_full_scan_satisfies_a_required_detector() {
    let ok = signal(DetectorCategoryV1::Secret);
    assert!(ok.validate().is_ok() && ok.satisfies_requirement());

    let mut partial = ok.clone();
    partial.coverage.kind = CoverageKindV1::Partial;
    partial.coverage.bytes_inspected = 5;
    assert!(partial.validate().is_ok() && !partial.satisfies_requirement());

    let mut failed = ok.clone();
    failed.status = DetectorStatusV1::TimedOut;
    assert!(
        failed.validate().is_err(),
        "timeout cannot claim full coverage"
    );

    let mut overconfident = ok;
    overconfident.confidence_milli = Some(1_001);
    assert!(overconfident.validate().is_err());
}

// ─── Decisions ──────────────────────────────────────────────────────────────

#[test]
fn dispositions_are_ordered_by_restrictiveness() {
    use ContextDispositionV1::*;
    assert_eq!(Allow.most_restrictive(Deny), Deny);
    assert_eq!(
        AllowRedacted.most_restrictive(AllowMinimized),
        AllowRedacted
    );
    assert!(AllowLocalModelOnly.delivers_content());
    for held in [AllowWithApproval, Quarantine, Deny] {
        assert!(!held.delivers_content());
    }
}

#[test]
fn every_restriction_must_state_a_reason() {
    assert!(
        decision(ContextDispositionV1::Allow, 'a')
            .validate()
            .is_ok()
    );
    let mut silent = decision(ContextDispositionV1::Deny, 'a');
    silent.reason_codes.clear();
    assert!(silent.validate().is_err());
}

// ─── Passport derivation ────────────────────────────────────────────────────

#[test]
fn derivation_is_monotone_in_every_security_dimension() {
    let mut secret = passport(
        'a',
        ClassificationV1::Restricted,
        TrustLevelV1::Untrusted,
        vec![DestinationConstraintV1::LocalOnly],
    );
    secret.risk_signals.push(signal(DetectorCategoryV1::Secret));
    let plain = passport(
        'b',
        ClassificationV1::Internal,
        TrustLevelV1::Trusted,
        vec![DestinationConstraintV1::OrganizationPrivate],
    );
    let derived = ContextObjectPassportV1::derive(
        &[&plain, &secret],
        step('b', 'c'),
        plain.source.clone(),
        50,
        200,
    )
    .expect("derive");

    assert_eq!(derived.classification, ClassificationV1::Restricted);
    assert_eq!(derived.trust, TrustLevelV1::Untrusted);
    assert_eq!(
        derived.destination_constraints,
        vec![
            DestinationConstraintV1::LocalOnly,
            DestinationConstraintV1::OrganizationPrivate
        ]
    );
    assert_eq!(derived.risk_signals.len(), 1);
    assert_eq!(derived.object, digest('c'));
    assert_eq!(derived.lineage.len(), 1);
    assert!(derived.validate().is_ok());
}

#[test]
fn derivation_refuses_to_mix_principals_or_start_from_nothing() {
    let a = passport('a', ClassificationV1::Public, TrustLevelV1::Trusted, vec![]);
    let mut b = a.clone();
    b.principal = PrincipalV1::unknown();
    assert!(
        ContextObjectPassportV1::derive(&[&a, &b], step('a', 'c'), a.source.clone(), 1, 1).is_err()
    );
    assert!(ContextObjectPassportV1::derive(&[], step('a', 'c'), a.source.clone(), 1, 1).is_err());
}

#[test]
fn passport_rejects_unsorted_constraints_and_wrong_schema() {
    let mut p = passport(
        'a',
        ClassificationV1::Public,
        TrustLevelV1::Trusted,
        vec![
            DestinationConstraintV1::OrganizationPrivate,
            DestinationConstraintV1::LocalOnly,
        ],
    );
    assert!(p.validate().is_err());
    p.destination_constraints.sort();
    assert!(p.validate().is_ok());
    p.schema_version = 2;
    assert!(p.validate().is_err());
}

#[test]
fn passport_round_trips_and_rejects_unknown_fields() {
    let p = passport(
        'a',
        ClassificationV1::Confidential,
        TrustLevelV1::Internal,
        vec![],
    );
    let json = serde_json::to_value(&p).expect("serialize");
    assert_eq!(
        serde_json::from_value::<ContextObjectPassportV1>(json.clone()).expect("parse"),
        p
    );
    let mut extra = json;
    extra["content"] = serde_json::json!("leak");
    assert!(serde_json::from_value::<ContextObjectPassportV1>(extra).is_err());
}

// ─── Receipt ────────────────────────────────────────────────────────────────

#[test]
fn receipt_counts_must_match_its_decisions() {
    assert!(receipt().validate().is_ok());

    let mut hidden_denial = receipt();
    hidden_denial.security.blocked_objects = 0;
    assert!(hidden_denial.validate().is_err());

    let mut inflated = receipt();
    inflated.sources.selected = 3;
    assert!(inflated.validate().is_err());

    let mut withheld = receipt();
    withheld.outcome = DeliveryOutcomeV1::Withheld;
    assert!(
        withheld.validate().is_err(),
        "withheld may not name a context"
    );
    withheld.final_context = None;
    assert!(withheld.validate().is_ok());

    let mut delivered_nothing = receipt();
    delivered_nothing.final_context = None;
    assert!(delivered_nothing.validate().is_err());
}

#[test]
fn receipt_digest_is_deterministic_and_content_sensitive() {
    let first = receipt().canonical_digest().expect("digest");
    assert_eq!(first, receipt().canonical_digest().expect("digest"));

    let json = serde_json::to_string(&receipt()).expect("serialize");
    let parsed: ContextDecisionReceiptV1 = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed.canonical_digest().expect("digest"), first);

    let mut changed = receipt();
    changed.tokens.delivered = 301;
    assert_ne!(changed.canonical_digest().expect("digest"), first);
}

#[test]
fn receipt_never_serializes_content_fields() {
    let json = serde_json::to_string(&receipt()).expect("serialize");
    for forbidden in ["\"content\"", "\"text\"", "\"match\"", "\"value\""] {
        assert!(!json.contains(forbidden), "{forbidden} in receipt");
    }
}

// ─── Context quality section ────────────────────────────────────────────────

fn quality(critical_lost: u64) -> ContextQualitySectionV1 {
    let mut section = ContextQualitySectionV1 {
        evidence_tier: QualityEvidenceTierV1::DeterministicQuality,
        retention: QualityRetentionV1 {
            critical: RetentionCountsV1 {
                retained: 4,
                recoverable: 1,
                lost: critical_lost,
            },
            important: RetentionCountsV1 {
                retained: 2,
                recoverable: 0,
                lost: 0,
            },
            lost_critical_kinds: if critical_lost > 0 {
                vec![QualityProbeKindV1::ErrorCode]
            } else {
                Vec::new()
            },
            critical_unchecked: false,
            truncated: false,
            secret_lines_withheld: 1,
        },
        recovery: Some(QualityRecoveryV1 {
            handles_emitted: 1,
            handles_verified: 1,
            failures: 0,
            critical_failures: 0,
        }),
        task_quality: QualityStateV1::Unmeasured,
        overall: QualityStateV1::Pass,
    };
    section.overall = section.derived_overall();
    section
}

#[test]
fn an_absent_quality_section_keeps_the_receipt_digest() {
    let without = receipt();
    let json = serde_json::to_value(&without).expect("serialize");
    assert!(
        json.get("quality").is_none(),
        "absent section is not written"
    );
    let mut with = receipt();
    with.quality = Some(quality(0));
    assert!(with.validate().is_ok());
    assert_ne!(
        with.canonical_digest().expect("digest"),
        without.canonical_digest().expect("digest")
    );
}

#[test]
fn quality_overall_follows_the_measured_dimensions() {
    assert_eq!(quality(0).overall, QualityStateV1::Pass);
    assert_eq!(quality(2).overall, QualityStateV1::Fail);

    let mut claimed_pass = quality(2);
    claimed_pass.overall = QualityStateV1::Pass;
    assert!(
        claimed_pass.validate().is_err(),
        "a lost critical fact cannot pass"
    );

    let mut unchecked = quality(0);
    unchecked.retention.critical_unchecked = true;
    assert_eq!(unchecked.derived_overall(), QualityStateV1::Fail);

    let mut nothing = quality(0);
    nothing.retention.critical = RetentionCountsV1 {
        retained: 0,
        recoverable: 0,
        lost: 0,
    };
    nothing.retention.important = nothing.retention.critical;
    nothing.recovery = None;
    assert_eq!(nothing.derived_overall(), QualityStateV1::Unmeasured);
}

#[test]
fn quality_rejects_impossible_claims() {
    let mut task_measured = quality(0);
    task_measured.task_quality = QualityStateV1::Pass;
    assert!(
        task_measured.validate().is_err(),
        "task quality is never per round"
    );

    let mut unsorted = quality(0);
    unsorted.retention.lost_critical_kinds =
        vec![QualityProbeKindV1::Url, QualityProbeKindV1::ErrorCode];
    unsorted.overall = unsorted.derived_overall();
    assert!(unsorted.validate().is_err());

    let mut overcounted = quality(0);
    overcounted.recovery = Some(QualityRecoveryV1 {
        handles_emitted: 1,
        handles_verified: 1,
        failures: 1,
        critical_failures: 0,
    });
    overcounted.overall = overcounted.derived_overall();
    assert!(overcounted.validate().is_err());

    let tampered = serde_json::json!({
        "evidence_tier": "deterministic_quality",
        "retention": serde_json::to_value(quality(0).retention).expect("retention"),
        "task_quality": "unmeasured",
        "overall": "pass",
        "note": "values leak here"
    });
    assert!(serde_json::from_value::<ContextQualitySectionV1>(tampered).is_err());
}
