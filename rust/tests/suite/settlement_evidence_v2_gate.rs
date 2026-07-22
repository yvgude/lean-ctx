//! Adversarial contract gate for CO-09, EV-03/05/06/07/09/10, and BC-06.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lean_ctx::core::billing::Usage;
use lean_ctx::core::billing::settlement_evidence::{
    EvidenceStateV2, EvidenceTrustStatusV2, MAX_ATTRIBUTION_SOURCE_IDS,
    MAX_SETTLEMENT_EVIDENCE_ITEMS, MAX_SETTLEMENT_STRING_BYTES, SettlementEvidenceClaimV2,
    SettlementEvidenceClassV2, SettlementEvidenceManifestV2, SettlementEvidenceRoleV2,
    SettlementEvidenceTrustStoreV2, SettlementIneligibilityReasonV2, TrustedEvidenceDecisionV2,
    reconcile_settlement_evidence_v2,
};

const FIXTURE: &str = include_str!("../fixtures/settlement-evidence-v2/eligible.json");
const TRUST_FIXTURE: &str =
    include_str!("../fixtures/settlement-evidence-v2/trusted-decisions.json");
const LEGACY_USAGE_V1: &str =
    include_str!("../fixtures/settlement-evidence-v2/legacy-usage-v1.json");

fn manifest() -> SettlementEvidenceManifestV2 {
    serde_json::from_str(FIXTURE).expect("canonical v2 fixture parses")
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
    .expect("bounded fixture trust store")
}

fn pinned_trust_store() -> SettlementEvidenceTrustStoreV2 {
    serde_json::from_str(TRUST_FIXTURE).expect("canonical trust fixture parses")
}

fn has_reason(
    result: &lean_ctx::core::billing::SettlementEligibilityV2,
    expected: impl Fn(&SettlementIneligibilityReasonV2) -> bool,
) -> bool {
    result.reasons.iter().any(expected)
}

#[test]
fn canonical_fixture_is_eligible_bounded_and_authority_free() {
    let manifest = manifest();
    assert_eq!(manifest.evidence.len(), 7);
    assert!(manifest.evidence.len() <= MAX_SETTLEMENT_EVIDENCE_ITEMS);
    assert_eq!(manifest.canonical_json().unwrap(), FIXTURE.trim_end());
    let result = reconcile_settlement_evidence_v2(&manifest, &pinned_trust_store());
    assert!(result.eligible, "{:?}", result.reasons);
    assert_eq!(result.attributed_tokens, Some(2_500));
    assert_eq!(result.attributed_minor_units, Some(500));
    assert!(!result.invoice_authority);
    assert!(!result.contract_validity_verified);
    assert!(!result.customer_approval_authority_verified);
}

#[test]
fn legacy_usage_v1_wire_and_api_remain_exactly_compatible() {
    let usage: Usage = serde_json::from_str(LEGACY_USAGE_V1).expect("legacy usage parses");
    let expected: serde_json::Value = serde_json::from_str(LEGACY_USAGE_V1).unwrap();
    assert_eq!(serde_json::to_value(&usage).unwrap(), expected);
    let keys: BTreeSet<_> = expected
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "agent_id",
            "chain_valid",
            "created_at",
            "last_entry_hash",
            "metered_events",
            "net_saved_tokens",
            "period",
            "saved_usd",
            "schema_version",
            "signed",
        ])
    );
    assert!(usage.is_billable(), "frozen v1 semantics changed");
    assert_eq!(usage.is_billable(), usage.source_integrity_verified());
}

#[test]
fn signed_and_chain_valid_usage_alone_is_never_v2_eligible() {
    let usage: Usage = serde_json::from_str(LEGACY_USAGE_V1).unwrap();
    assert!(usage.is_billable());
    let mut no_evidence = manifest();
    no_evidence.evidence.clear();
    let result =
        reconcile_settlement_evidence_v2(&no_evidence, &SettlementEvidenceTrustStoreV2::empty());
    assert!(!result.eligible);
    for role in [
        SettlementEvidenceRoleV2::Baseline,
        SettlementEvidenceRoleV2::Price,
        SettlementEvidenceRoleV2::Contract,
        SettlementEvidenceRoleV2::Quality,
        SettlementEvidenceRoleV2::Attribution,
        SettlementEvidenceRoleV2::PeriodCompletion,
        SettlementEvidenceRoleV2::CustomerApproval,
    ] {
        assert!(
            result
                .reasons
                .contains(&SettlementIneligibilityReasonV2::MissingEvidence { role })
        );
    }
}

#[test]
fn manifest_self_attestation_cannot_replace_out_of_band_trust() {
    let manifest = manifest();
    let result =
        reconcile_settlement_evidence_v2(&manifest, &SettlementEvidenceTrustStoreV2::empty());
    assert!(!result.eligible);
    assert_eq!(
        result
            .reasons
            .iter()
            .filter(|reason| matches!(
                reason,
                SettlementIneligibilityReasonV2::UntrustedEvidence { .. }
            ))
            .count(),
        7
    );
}

#[test]
fn missing_ambiguous_duplicate_untrusted_disputed_and_superseded_are_typed() {
    let mut missing = manifest();
    missing
        .evidence
        .retain(|item| item.claim.role() != SettlementEvidenceRoleV2::Quality);
    assert!(
        reconcile_settlement_evidence_v2(&missing, &trust_store_for(&missing))
            .reasons
            .contains(&SettlementIneligibilityReasonV2::MissingEvidence {
                role: SettlementEvidenceRoleV2::Quality,
            })
    );

    let mut duplicate = manifest();
    let baseline = duplicate
        .evidence
        .iter()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Baseline)
        .unwrap()
        .clone();
    let duplicated_id = baseline.evidence_id.clone();
    duplicate.evidence.push(baseline);
    let result = reconcile_settlement_evidence_v2(&duplicate, &trust_store_for(&duplicate));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::DuplicateEvidenceId {
                evidence_id: duplicated_id,
            })
    );
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::AmbiguousEvidence {
                role: SettlementEvidenceRoleV2::Baseline,
            })
    );

    let mut untrusted = manifest();
    untrusted.evidence[0].trust.status = EvidenceTrustStatusV2::Untrusted;
    assert!(has_reason(
        &reconcile_settlement_evidence_v2(&untrusted, &trust_store_for(&untrusted)),
        |reason| matches!(
            reason,
            SettlementIneligibilityReasonV2::UntrustedEvidence { .. }
        )
    ));

    let mut disputed = manifest();
    disputed.evidence[0].state = EvidenceStateV2::Disputed;
    assert!(has_reason(
        &reconcile_settlement_evidence_v2(&disputed, &trust_store_for(&disputed)),
        |reason| matches!(
            reason,
            SettlementIneligibilityReasonV2::DisputedEvidence { .. }
        )
    ));

    let mut superseded = manifest();
    superseded.evidence[0].state = EvidenceStateV2::Superseded;
    assert!(has_reason(
        &reconcile_settlement_evidence_v2(&superseded, &trust_store_for(&superseded)),
        |reason| matches!(
            reason,
            SettlementIneligibilityReasonV2::SupersededEvidence { .. }
        )
    ));
}

#[test]
fn incomplete_quality_approval_currency_and_nonexclusive_claims_fail_closed() {
    let mut manifest = manifest();
    for item in &mut manifest.evidence {
        match &mut item.claim {
            SettlementEvidenceClaimV2::PeriodCompletion { complete, .. } => *complete = false,
            SettlementEvidenceClaimV2::Quality { passed, .. } => *passed = false,
            SettlementEvidenceClaimV2::CustomerApproval { approved, .. } => *approved = false,
            SettlementEvidenceClaimV2::Price { currency, .. } => *currency = "USD".to_string(),
            SettlementEvidenceClaimV2::Attribution { exclusive, .. } => *exclusive = false,
            _ => {}
        }
    }
    manifest
        .evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Quality)
        .unwrap()
        .measurement
        .evidence_class = SettlementEvidenceClassV2::Derived;
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::IncompletePeriod)
    );
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::QualityGateFailed)
    );
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::CustomerApprovalNotGranted)
    );
    assert!(has_reason(&result, |reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::CurrencyMismatch { .. }
    )));
    assert!(has_reason(&result, |reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::NonExclusiveAttribution { .. }
    )));
    assert!(has_reason(&result, |reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::IneligibleEvidenceClass { .. }
    )));
}

#[test]
fn cross_mechanism_duplicate_attribution_and_overflow_are_blocked() {
    let mut manifest = manifest();
    let mut second = manifest
        .evidence
        .iter()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Attribution)
        .unwrap()
        .clone();
    if let SettlementEvidenceClaimV2::Attribution {
        mechanism_id,
        attributed_tokens,
        attributed_minor_units,
        ..
    } = &mut second.claim
    {
        *mechanism_id = format!(
            "mechanism:blake3:{}",
            blake3::hash(b"second-mechanism").to_hex()
        );
        *attributed_tokens = 1;
        *attributed_minor_units = u64::MAX;
    }
    manifest.evidence.push(second);
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(has_reason(&result, |reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::DuplicateAttribution { .. }
    )));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::ArithmeticOverflow)
    );
    assert_eq!(result.attributed_minor_units, None);
}

#[test]
fn evidence_limit_unknown_fields_and_permutations_are_enforced() {
    let original = manifest();
    let mut permuted = original.clone();
    permuted.evidence.reverse();
    assert_eq!(permuted.canonical_json().unwrap(), FIXTURE.trim_end());
    assert_eq!(
        reconcile_settlement_evidence_v2(&permuted, &trust_store_for(&permuted)),
        reconcile_settlement_evidence_v2(&original, &trust_store_for(&original))
    );

    let mut oversized = original.clone();
    oversized.evidence = vec![original.evidence[0].clone(); MAX_SETTLEMENT_EVIDENCE_ITEMS + 1];
    assert_eq!(
        reconcile_settlement_evidence_v2(&oversized, &SettlementEvidenceTrustStoreV2::empty())
            .reasons,
        vec![SettlementIneligibilityReasonV2::TooManyEvidenceItems]
    );

    let mut oversized_sources = original.clone();
    let attribution = oversized_sources
        .evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Attribution)
        .unwrap();
    if let SettlementEvidenceClaimV2::Attribution {
        source_evidence_ids,
        ..
    } = &mut attribution.claim
    {
        *source_evidence_ids = vec!["artifact:bounded".to_string(); MAX_ATTRIBUTION_SOURCE_IDS + 1];
    }
    assert_eq!(
        reconcile_settlement_evidence_v2(
            &oversized_sources,
            &SettlementEvidenceTrustStoreV2::empty()
        )
        .reasons,
        vec![SettlementIneligibilityReasonV2::TooManyAttributionSources]
    );

    let mut oversized_string = original.clone();
    oversized_string.subject_id = "x".repeat(MAX_SETTLEMENT_STRING_BYTES + 1);
    let result = reconcile_settlement_evidence_v2(
        &oversized_string,
        &SettlementEvidenceTrustStoreV2::empty(),
    );
    assert_eq!(
        result.reasons,
        vec![SettlementIneligibilityReasonV2::OversizedString]
    );
    assert_eq!(result.manifest_id, original.manifest_id);

    let mut root: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    root.as_object_mut()
        .unwrap()
        .insert("invoice".to_string(), serde_json::json!(true));
    assert!(serde_json::from_value::<SettlementEvidenceManifestV2>(root).is_err());

    let mut nested: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    nested["evidence"][0]["trust"]["signature_valid"] = serde_json::json!(true);
    assert!(serde_json::from_value::<SettlementEvidenceManifestV2>(nested).is_err());
}

#[test]
fn fixture_is_payload_pii_path_and_secret_free() {
    fn scan(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    let key = key.to_ascii_lowercase();
                    for forbidden in [
                        "path",
                        "prompt",
                        "content",
                        "payload",
                        "cwd",
                        "file",
                        "email",
                        "username",
                        "person_name",
                        "secret",
                        "password",
                        "api_key",
                    ] {
                        assert_ne!(key, forbidden, "forbidden fixture key: {key}");
                    }
                    scan(value);
                }
            }
            serde_json::Value::Array(values) => values.iter().for_each(scan),
            serde_json::Value::String(value) => {
                let lower = value.to_ascii_lowercase();
                for forbidden in ["/", "\\", "@", "sk-", "secret", "password", "prompt"] {
                    assert!(!lower.contains(forbidden), "forbidden fixture value shape");
                }
            }
            _ => {}
        }
    }
    scan(&serde_json::from_str(FIXTURE).unwrap());
    scan(&serde_json::from_str(TRUST_FIXTURE).unwrap());
}

#[test]
fn offline_load_verify_and_export_are_deterministic() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/settlement-evidence-v2/eligible.json");
    let manifest = SettlementEvidenceManifestV2::load(&fixture_path).unwrap();
    let trust_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/settlement-evidence-v2/trusted-decisions.json");
    let trust_store = SettlementEvidenceTrustStoreV2::load(&trust_path).unwrap();
    assert!(reconcile_settlement_evidence_v2(&manifest, &trust_store).eligible);

    let output = std::env::temp_dir().join(format!(
        "lean-ctx-settlement-v2-export-{}.json",
        std::process::id()
    ));
    manifest.export(&output).unwrap();
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        FIXTURE.trim_end()
    );
    let _ = std::fs::remove_file(output);
}
