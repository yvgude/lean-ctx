use super::*;

fn digest(prefix: &str, label: &str) -> String {
    format!("{prefix}{}", blake3::hash(label.as_bytes()).to_hex())
}

fn trust(label: &str) -> EvidenceTrustV2 {
    EvidenceTrustV2 {
        status: EvidenceTrustStatusV2::Trusted,
        trust_decision_id: digest("artifact:blake3:", &format!("trust-{label}")),
        trust_anchor_id: digest("anchor:blake3:", "authority"),
    }
}

fn item(subject: &str, label: &str, claim: SettlementEvidenceClaimV2) -> SettlementEvidenceItemV2 {
    SettlementEvidenceItemV2::new(subject.to_string(), claim, trust(label))
        .expect("bounded fixture item")
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

fn eligible_manifest() -> SettlementEvidenceManifestV2 {
    let subject = digest("subject:blake3:", "tenant-a");
    let evidence = vec![
        item(
            &subject,
            "baseline",
            SettlementEvidenceClaimV2::Baseline {
                baseline_version_id: digest("artifact:blake3:", "baseline-v1"),
                baseline_tokens: 10_000,
            },
        ),
        item(
            &subject,
            "price",
            SettlementEvidenceClaimV2::Price {
                price_version_id: digest("artifact:blake3:", "price-v1"),
                currency: "CHF".to_string(),
                unit_price_micros: 125,
            },
        ),
        item(
            &subject,
            "contract",
            SettlementEvidenceClaimV2::Contract {
                contract_version_id: digest("artifact:blake3:", "contract-v1"),
            },
        ),
        item(
            &subject,
            "quality",
            SettlementEvidenceClaimV2::Quality {
                quality_gate_id: digest("artifact:blake3:", "quality-v1"),
                passed: true,
            },
        ),
        item(
            &subject,
            "attribution",
            SettlementEvidenceClaimV2::Attribution {
                mechanism_id: digest("mechanism:blake3:", "compression"),
                exclusive: true,
                attributed_tokens: 2_500,
                attributed_minor_units: 500,
                source_evidence_ids: vec![digest("artifact:blake3:", "usage-1")],
            },
        ),
        item(
            &subject,
            "period",
            SettlementEvidenceClaimV2::PeriodCompletion {
                period_start_epoch_seconds: 1_767_225_600,
                period_end_epoch_seconds: 1_769_904_000,
                complete: true,
            },
        ),
        item(
            &subject,
            "approval",
            SettlementEvidenceClaimV2::CustomerApproval {
                approval_artifact_id: digest("artifact:blake3:", "approval-v1"),
                approved: true,
            },
        ),
    ];
    SettlementEvidenceManifestV2::new(
        subject,
        SettlementPeriodV2 {
            start_epoch_seconds: 1_767_225_600,
            end_epoch_seconds: 1_769_904_000,
        },
        "CHF".to_string(),
        500,
        evidence,
    )
    .expect("bounded fixture manifest")
}

#[test]
fn complete_trusted_manifest_is_structurally_eligible_only() {
    let manifest = eligible_manifest();
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(result.eligible, "{:?}", result.reasons);
    assert_eq!(result.attributed_tokens, Some(2_500));
    assert_eq!(result.attributed_minor_units, Some(500));
    assert!(!result.invoice_authority);
    assert!(!result.contract_validity_verified);
    assert!(!result.customer_approval_authority_verified);
}

#[test]
fn canonicalization_is_permutation_stable_and_tamper_evident() {
    let manifest = eligible_manifest();
    let canonical = manifest.canonical_json().unwrap();
    let mut permuted = manifest.clone();
    permuted.evidence.reverse();
    assert_eq!(permuted.canonical_json().unwrap(), canonical);
    permuted.claimed_amount_minor_units += 1;
    assert!(matches!(
        permuted.canonical_json(),
        Err(SettlementEvidenceError::IntegrityMismatch)
    ));
}

#[test]
fn missing_ambiguous_untrusted_and_incomplete_fail_closed() {
    let original = eligible_manifest();
    let mut evidence = original.evidence.clone();
    evidence.retain(|item| item.claim.role() != SettlementEvidenceRoleV2::Quality);
    let duplicate = evidence
        .iter()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Baseline)
        .unwrap()
        .clone();
    evidence.push(duplicate);
    let approval = evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::CustomerApproval)
        .unwrap();
    approval.trust.status = EvidenceTrustStatusV2::Untrusted;
    approval.evidence_id = approval.computed_evidence_id().unwrap();
    let period = evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::PeriodCompletion)
        .unwrap();
    if let SettlementEvidenceClaimV2::PeriodCompletion { complete, .. } = &mut period.claim {
        *complete = false;
    }
    period.evidence_id = period.computed_evidence_id().unwrap();
    let manifest = SettlementEvidenceManifestV2::new(
        original.subject_id,
        original.period,
        original.currency,
        original.claimed_amount_minor_units,
        evidence,
    )
    .unwrap();
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(!result.eligible);
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::MissingEvidence {
                role: SettlementEvidenceRoleV2::Quality,
            })
    );
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::AmbiguousEvidence {
                role: SettlementEvidenceRoleV2::Baseline,
            })
    );
    assert!(result.reasons.iter().any(|reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::UntrustedEvidence { .. }
    )));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::IncompletePeriod)
    );
}

#[test]
fn cross_mechanism_duplicate_attribution_is_rejected() {
    let original = eligible_manifest();
    let mut evidence = original.evidence.clone();
    let source = digest("artifact:blake3:", "usage-1");
    evidence.push(item(
        &original.subject_id,
        "attribution-2",
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id: digest("mechanism:blake3:", "cache"),
            exclusive: true,
            attributed_tokens: 100,
            attributed_minor_units: 20,
            source_evidence_ids: vec![source.clone()],
        },
    ));
    let manifest = SettlementEvidenceManifestV2::new(
        original.subject_id,
        original.period,
        original.currency,
        520,
        evidence,
    )
    .unwrap();
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::DuplicateAttribution {
                source_evidence_id: source,
            })
    );
}

#[test]
fn checked_arithmetic_overflow_is_ineligible() {
    let original = eligible_manifest();
    let mut evidence = original.evidence.clone();
    let attribution = evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Attribution)
        .unwrap();
    if let SettlementEvidenceClaimV2::Attribution {
        attributed_minor_units,
        ..
    } = &mut attribution.claim
    {
        *attributed_minor_units = u64::MAX;
    }
    attribution.evidence_id = attribution.computed_evidence_id().unwrap();
    evidence.push(item(
        &original.subject_id,
        "attribution-overflow",
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id: digest("mechanism:blake3:", "cache"),
            exclusive: true,
            attributed_tokens: 1,
            attributed_minor_units: 1,
            source_evidence_ids: vec![digest("artifact:blake3:", "usage-2")],
        },
    ));
    let manifest = SettlementEvidenceManifestV2::new(
        original.subject_id,
        original.period,
        original.currency,
        u64::MAX,
        evidence,
    )
    .unwrap();
    let result = reconcile_settlement_evidence_v2(&manifest, &trust_store_for(&manifest));
    assert!(
        result
            .reasons
            .contains(&SettlementIneligibilityReasonV2::ArithmeticOverflow)
    );
    assert_eq!(result.attributed_minor_units, None);
}

#[test]
fn correction_and_supersession_lineage_is_explicit_and_tamper_evident() {
    let original = eligible_manifest();
    let old_baseline = original
        .evidence
        .iter()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Baseline)
        .unwrap();
    let old_id = old_baseline.evidence_id.clone();
    let baseline_claim = old_baseline.claim.clone();
    let mut evidence = original.evidence.clone();
    evidence.retain(|item| item.claim.role() != SettlementEvidenceRoleV2::Baseline);
    evidence.push(
        SettlementEvidenceItemV2::corrected(
            original.subject_id.clone(),
            baseline_claim,
            trust("baseline-correction"),
            vec![old_id],
            digest("artifact:blake3:", "correction-reason"),
        )
        .unwrap(),
    );
    let corrected = SettlementEvidenceManifestV2::new(
        original.subject_id,
        original.period,
        original.currency,
        original.claimed_amount_minor_units,
        evidence,
    )
    .unwrap();
    let result = reconcile_settlement_evidence_v2(&corrected, &trust_store_for(&corrected));
    assert!(result.eligible, "{:?}", result.reasons);

    let mut invalid = corrected.clone();
    let item = invalid
        .evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Baseline)
        .unwrap();
    item.correction_reason_id = None;
    let result = reconcile_settlement_evidence_v2(&invalid, &trust_store_for(&invalid));
    assert!(result.reasons.iter().any(|reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::InvalidCorrectionLineage { .. }
    )));
}

#[test]
fn constructors_and_direct_reconcile_enforce_every_structural_bound() {
    let original = eligible_manifest();
    let subject = original.subject_id.clone();
    let source = digest("artifact:blake3:", "bounded-source");
    let oversized_item = SettlementEvidenceItemV2::new(
        subject.clone(),
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id: digest("mechanism:blake3:", "oversized"),
            exclusive: true,
            attributed_tokens: 1,
            attributed_minor_units: 1,
            source_evidence_ids: vec![source; MAX_ATTRIBUTION_SOURCE_IDS + 1],
        },
        trust("oversized"),
    );
    assert!(matches!(
        oversized_item,
        Err(SettlementEvidenceError::TooManyAttributionSources)
    ));

    let corrected = SettlementEvidenceItemV2::corrected(
        subject,
        SettlementEvidenceClaimV2::Baseline {
            baseline_version_id: digest("artifact:blake3:", "baseline-v2"),
            baseline_tokens: 1,
        },
        trust("too-many-corrections"),
        vec![digest("artifact:blake3:", "old"); MAX_SUPERSESSION_REFS + 1],
        digest("artifact:blake3:", "reason"),
    );
    assert!(matches!(
        corrected,
        Err(SettlementEvidenceError::TooManySupersessionRefs)
    ));

    let mut oversized_string = original.clone();
    oversized_string.kind = "x".repeat(MAX_SETTLEMENT_STRING_BYTES + 1);
    let result = reconcile_settlement_evidence_v2(
        &oversized_string,
        &SettlementEvidenceTrustStoreV2::empty(),
    );
    assert_eq!(
        result.reasons,
        vec![SettlementIneligibilityReasonV2::OversizedString]
    );
    assert!(result.manifest_id.len() <= MAX_SETTLEMENT_STRING_BYTES);

    let mut too_many_items = original.clone();
    too_many_items.evidence = vec![original.evidence[0].clone(); MAX_SETTLEMENT_EVIDENCE_ITEMS + 1];
    assert_eq!(
        reconcile_settlement_evidence_v2(&too_many_items, &SettlementEvidenceTrustStoreV2::empty())
            .reasons,
        vec![SettlementIneligibilityReasonV2::TooManyEvidenceItems]
    );

    let decision = TrustedEvidenceDecisionV2 {
        evidence_id: digest("artifact:blake3:", "evidence"),
        trust_decision_id: digest("artifact:blake3:", "decision"),
        trust_anchor_id: digest("anchor:blake3:", "anchor"),
    };
    let oversized_trust =
        SettlementEvidenceTrustStoreV2::new(vec![decision; MAX_SETTLEMENT_TRUST_DECISIONS + 1]);
    assert!(matches!(
        oversized_trust,
        Err(SettlementEvidenceError::TooManyTrustDecisions)
    ));
}

fn next_evidence_permutation(items: &mut [SettlementEvidenceItemV2]) -> bool {
    let Some(index) = (1..items.len())
        .rev()
        .find(|&index| items[index - 1].evidence_id < items[index].evidence_id)
    else {
        return false;
    };
    let successor = (index..items.len())
        .rev()
        .find(|&candidate| items[index - 1].evidence_id < items[candidate].evidence_id)
        .expect("permutation successor");
    items.swap(index - 1, successor);
    items[index..].reverse();
    true
}

#[test]
fn every_evidence_permutation_has_identical_reconciliation() {
    let original = eligible_manifest();
    let expected = reconcile_settlement_evidence_v2(&original, &trust_store_for(&original));
    let mut evidence = original.evidence.clone();
    evidence.sort_by(|a, b| a.evidence_id.cmp(&b.evidence_id));
    let mut permutations = 0usize;
    loop {
        let mut candidate = original.clone();
        candidate.evidence.clone_from(&evidence);
        assert_eq!(
            reconcile_settlement_evidence_v2(&candidate, &trust_store_for(&candidate)),
            expected
        );
        permutations += 1;
        if !next_evidence_permutation(&mut evidence) {
            break;
        }
    }
    assert_eq!(permutations, 5_040);
}

#[test]
fn correction_collisions_and_overflow_reasons_are_permutation_stable() {
    let original = eligible_manifest();
    let collision_target = digest("artifact:blake3:", "shared-old-evidence");
    let mut evidence = original.evidence.clone();
    for role in [
        SettlementEvidenceRoleV2::Baseline,
        SettlementEvidenceRoleV2::Price,
    ] {
        let index = evidence
            .iter()
            .position(|item| item.claim.role() == role)
            .unwrap();
        let old = evidence.remove(index);
        evidence.push(
            SettlementEvidenceItemV2::corrected(
                original.subject_id.clone(),
                old.claim,
                old.trust,
                vec![collision_target.clone()],
                digest("artifact:blake3:", &format!("reason-{role:?}")),
            )
            .unwrap(),
        );
    }
    let collision = SettlementEvidenceManifestV2::new(
        original.subject_id.clone(),
        original.period.clone(),
        original.currency.clone(),
        original.claimed_amount_minor_units,
        evidence,
    )
    .unwrap();
    let expected = reconcile_settlement_evidence_v2(&collision, &trust_store_for(&collision));
    assert!(expected.reasons.iter().any(|reason| matches!(
        reason,
        SettlementIneligibilityReasonV2::CorrectionTargetCollision {
            target_id,
            correction_ids,
        } if target_id == &collision_target && correction_ids.len() == 2
    )));
    let mut reversed = collision.clone();
    reversed.evidence.reverse();
    assert_eq!(
        reconcile_settlement_evidence_v2(&reversed, &trust_store_for(&reversed)),
        expected
    );

    let mut overflow = original.clone();
    let attribution = overflow
        .evidence
        .iter_mut()
        .find(|item| item.claim.role() == SettlementEvidenceRoleV2::Attribution)
        .unwrap();
    if let SettlementEvidenceClaimV2::Attribution {
        attributed_minor_units,
        ..
    } = &mut attribution.claim
    {
        *attributed_minor_units = u64::MAX;
    }
    attribution.evidence_id = attribution.computed_evidence_id().unwrap();
    overflow.evidence.push(item(
        &overflow.subject_id,
        "overflow-second",
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id: digest("mechanism:blake3:", "overflow-second"),
            exclusive: true,
            attributed_tokens: 1,
            attributed_minor_units: 1,
            source_evidence_ids: vec![digest("artifact:blake3:", "overflow-source")],
        },
    ));
    overflow = SettlementEvidenceManifestV2::new(
        overflow.subject_id,
        overflow.period,
        overflow.currency,
        u64::MAX,
        overflow.evidence,
    )
    .unwrap();
    let expected = reconcile_settlement_evidence_v2(&overflow, &trust_store_for(&overflow));
    assert!(
        expected
            .reasons
            .contains(&SettlementIneligibilityReasonV2::ArithmeticOverflow)
    );
    let mut reversed = overflow.clone();
    reversed.evidence.reverse();
    assert_eq!(
        reconcile_settlement_evidence_v2(&reversed, &trust_store_for(&reversed)),
        expected
    );
}

#[cfg(unix)]
#[test]
fn file_io_rejects_symlinks_oversize_and_unsafe_targets_without_temp_leaks() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "lean-ctx-settlement-file-gate-{}-{}",
        std::process::id(),
        ATOMIC_EXPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let manifest = eligible_manifest();
    let manifest_path = root.join("manifest.json");
    manifest.export(&manifest_path).unwrap();
    manifest.export(&manifest_path).unwrap();
    assert_eq!(
        SettlementEvidenceManifestV2::load(&manifest_path).unwrap(),
        manifest
    );

    let manifest_link = root.join("manifest-link.json");
    symlink(&manifest_path, &manifest_link).unwrap();
    assert!(matches!(
        SettlementEvidenceManifestV2::load(&manifest_link),
        Err(SettlementEvidenceError::UnsafePath)
    ));

    let trust_path = root.join("trust.json");
    let trust_store = trust_store_for(&manifest);
    std::fs::write(&trust_path, trust_store.canonical_json().unwrap()).unwrap();
    let trust_link = root.join("trust-link.json");
    symlink(&trust_path, &trust_link).unwrap();
    assert!(matches!(
        SettlementEvidenceTrustStoreV2::load(&trust_link),
        Err(SettlementEvidenceError::UnsafePath)
    ));

    let victim = root.join("victim.json");
    std::fs::write(&victim, b"sentinel").unwrap();
    let export_link = root.join("export-link.json");
    symlink(&victim, &export_link).unwrap();
    assert!(matches!(
        manifest.export(&export_link),
        Err(SettlementEvidenceError::UnsafePath)
    ));
    assert_eq!(std::fs::read(&victim).unwrap(), b"sentinel");

    let oversized = root.join("oversized.json");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_SETTLEMENT_MANIFEST_BYTES + 1)
        .unwrap();
    assert!(matches!(
        SettlementEvidenceManifestV2::load(&oversized),
        Err(SettlementEvidenceError::ManifestTooLarge)
    ));

    let real_parent = root.join("real-parent");
    std::fs::create_dir(&real_parent).unwrap();
    let linked_parent = root.join("linked-parent");
    symlink(&real_parent, &linked_parent).unwrap();
    assert!(matches!(
        manifest.export(&linked_parent.join("unsafe.json")),
        Err(SettlementEvidenceError::UnsafePath)
    ));

    let leaked_temp = std::fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".settlement-")
    });
    assert!(!leaked_temp);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_fields_are_rejected() {
    let manifest = eligible_manifest();
    let mut value = serde_json::to_value(manifest).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("invoice_number".to_string(), serde_json::json!("no"));
    assert!(serde_json::from_value::<SettlementEvidenceManifestV2>(value).is_err());
}

#[test]
fn committed_fixture_is_the_canonical_manifest() {
    assert_eq!(
        eligible_manifest().canonical_json().unwrap(),
        include_str!("../../../../tests/fixtures/settlement-evidence-v2/eligible.json").trim_end()
    );
    assert_eq!(
        trust_store_for(&eligible_manifest())
            .canonical_json()
            .unwrap(),
        include_str!("../../../../tests/fixtures/settlement-evidence-v2/trusted-decisions.json")
            .trim_end()
    );
}
