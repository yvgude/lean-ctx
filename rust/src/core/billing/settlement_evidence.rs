//! Payload-free settlement-evidence eligibility for the open data plane.
//!
//! This module verifies a bounded, content-addressed evidence manifest. It does
//! not approve customers, decide disputes, validate contracts, calculate a
//! price, issue an invoice, or mutate settlement state. Those are private
//! control-plane responsibilities. Eligibility here means only that the
//! caller-supplied evidence set is structurally complete and internally
//! consistent under the v2 contract.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};

pub const SETTLEMENT_EVIDENCE_SCHEMA_VERSION: u16 = 2;
pub const SETTLEMENT_EVIDENCE_KIND: &str = "lean-ctx.settlement-evidence";
pub const MAX_SETTLEMENT_EVIDENCE_ITEMS: usize = 1_000;
pub const MAX_SETTLEMENT_TRUST_DECISIONS: usize = 1_000;
pub const MAX_ATTRIBUTION_SOURCE_IDS: usize = 1_000;
pub const MAX_SUPERSESSION_REFS: usize = 32;
pub const MAX_SETTLEMENT_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SETTLEMENT_STRING_BYTES: usize = 256;

const PENDING_MANIFEST_ID: &str = "manifest:pending";
const PENDING_EVIDENCE_ID: &str = "artifact:pending";
const PENDING_TRUST_STORE_ID: &str = "trust-store:pending";
static ATOMIC_EXPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Closed set of evidence roles required by settlement-evidence v2.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementEvidenceRoleV2 {
    Baseline,
    Price,
    Contract,
    Quality,
    Attribution,
    PeriodCompletion,
    CustomerApproval,
}

/// A trust decision is an externally produced, content-addressed input.
///
/// The OSS verifier checks the decision's presence and integrity-shaped IDs; it
/// does not claim the referenced authority is legally or commercially valid.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceTrustV2 {
    pub status: EvidenceTrustStatusV2,
    pub trust_decision_id: String,
    pub trust_anchor_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTrustStatusV2 {
    Trusted,
    Untrusted,
}

/// One out-of-band trust decision pinned by the verifier/operator.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedEvidenceDecisionV2 {
    pub evidence_id: String,
    pub trust_decision_id: String,
    pub trust_anchor_id: String,
}

/// Caller-owned trust input. A manifest cannot make itself trusted by merely
/// setting `status = trusted`; the exact tuple must also exist here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementEvidenceTrustStoreV2 {
    pub schema_version: u16,
    pub trust_store_id: String,
    pub trusted_decisions: Vec<TrustedEvidenceDecisionV2>,
}

impl SettlementEvidenceTrustStoreV2 {
    pub fn new(
        trusted_decisions: Vec<TrustedEvidenceDecisionV2>,
    ) -> Result<Self, SettlementEvidenceError> {
        let mut store = Self {
            schema_version: SETTLEMENT_EVIDENCE_SCHEMA_VERSION,
            trust_store_id: PENDING_TRUST_STORE_ID.to_string(),
            trusted_decisions,
        };
        ensure_trust_store_bounds(&store)?;
        store.canonicalize();
        store.trust_store_id = store.computed_trust_store_id()?;
        Ok(store)
    }

    /// Empty trust is the safe default: no self-attested manifest can qualify.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("empty trust store is bounded")
    }

    pub fn load(path: &Path) -> Result<Self, SettlementEvidenceError> {
        let store: Self = read_json_bounded(path)?;
        ensure_trust_store_bounds(&store)?;
        Ok(store)
    }

    pub fn canonical_json(&self) -> Result<String, SettlementEvidenceError> {
        ensure_trust_store_bounds(self)?;
        if self.trust_store_id != self.computed_trust_store_id()? {
            return Err(SettlementEvidenceError::IntegrityMismatch);
        }
        let mut canonical = self.clone();
        canonical.canonicalize();
        serde_json::to_string(&canonical).map_err(SettlementEvidenceError::Serialize)
    }

    fn canonicalize(&mut self) {
        self.trusted_decisions.sort();
    }

    fn computed_trust_store_id(&self) -> Result<String, SettlementEvidenceError> {
        ensure_trust_store_bounds(self)?;
        let mut identity = self.clone();
        identity.trust_store_id = PENDING_TRUST_STORE_ID.to_string();
        identity.canonicalize();
        Ok(format!(
            "trust-store:blake3:{}",
            hash_bounded_json(&identity)?.to_hex()
        ))
    }

    fn contains(&self, item: &SettlementEvidenceItemV2) -> bool {
        self.trusted_decisions.iter().any(|trusted| {
            trusted.evidence_id == item.evidence_id
                && trusted.trust_decision_id == item.trust.trust_decision_id
                && trusted.trust_anchor_id == item.trust.trust_anchor_id
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStateV2 {
    Active,
    Disputed,
    Superseded,
}

/// Evidence strength is explicit; settlement consumers never infer it from a
/// signature, file name, or claim role.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementEvidenceClassV2 {
    Measured,
    Reconciled,
    Declared,
    Derived,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementEvidenceMethodV2 {
    pub method_artifact_id: String,
    pub evidence_class: SettlementEvidenceClassV2,
}

/// Typed, payload-free evidence claim. Monetary quantities never use floats.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum SettlementEvidenceClaimV2 {
    Baseline {
        baseline_version_id: String,
        baseline_tokens: u64,
    },
    Price {
        price_version_id: String,
        currency: String,
        unit_price_micros: u64,
    },
    Contract {
        contract_version_id: String,
    },
    Quality {
        quality_gate_id: String,
        passed: bool,
    },
    Attribution {
        mechanism_id: String,
        exclusive: bool,
        attributed_tokens: u64,
        attributed_minor_units: u64,
        source_evidence_ids: Vec<String>,
    },
    PeriodCompletion {
        period_start_epoch_seconds: i64,
        period_end_epoch_seconds: i64,
        complete: bool,
    },
    CustomerApproval {
        approval_artifact_id: String,
        approved: bool,
    },
}

impl SettlementEvidenceClaimV2 {
    #[must_use]
    pub const fn role(&self) -> SettlementEvidenceRoleV2 {
        match self {
            Self::Baseline { .. } => SettlementEvidenceRoleV2::Baseline,
            Self::Price { .. } => SettlementEvidenceRoleV2::Price,
            Self::Contract { .. } => SettlementEvidenceRoleV2::Contract,
            Self::Quality { .. } => SettlementEvidenceRoleV2::Quality,
            Self::Attribution { .. } => SettlementEvidenceRoleV2::Attribution,
            Self::PeriodCompletion { .. } => SettlementEvidenceRoleV2::PeriodCompletion,
            Self::CustomerApproval { .. } => SettlementEvidenceRoleV2::CustomerApproval,
        }
    }

    fn canonicalize(&mut self) {
        if let Self::Attribution {
            source_evidence_ids,
            ..
        } = self
        {
            source_evidence_ids.sort();
        }
    }
}

/// One self-contained evidence projection. `evidence_id` commits to every
/// other field using canonical JSON and BLAKE3.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementEvidenceItemV2 {
    pub evidence_id: String,
    pub subject_id: String,
    pub state: EvidenceStateV2,
    pub trust: EvidenceTrustV2,
    pub measurement: SettlementEvidenceMethodV2,
    pub claim: SettlementEvidenceClaimV2,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_reason_id: Option<String>,
}

impl SettlementEvidenceItemV2 {
    pub fn new(
        subject_id: String,
        claim: SettlementEvidenceClaimV2,
        trust: EvidenceTrustV2,
    ) -> Result<Self, SettlementEvidenceError> {
        let measurement = default_method(&claim);
        Self::new_with_method(subject_id, claim, trust, measurement)
    }

    pub fn new_with_method(
        subject_id: String,
        claim: SettlementEvidenceClaimV2,
        trust: EvidenceTrustV2,
        measurement: SettlementEvidenceMethodV2,
    ) -> Result<Self, SettlementEvidenceError> {
        let mut item = Self {
            evidence_id: PENDING_EVIDENCE_ID.to_string(),
            subject_id,
            state: EvidenceStateV2::Active,
            trust,
            measurement,
            claim,
            supersedes: Vec::new(),
            correction_reason_id: None,
        };
        ensure_item_bounds(&item)?;
        item.canonicalize();
        item.evidence_id = item.computed_evidence_id()?;
        Ok(item)
    }

    /// Create an active correction that explicitly supersedes older evidence.
    pub fn corrected(
        subject_id: String,
        claim: SettlementEvidenceClaimV2,
        trust: EvidenceTrustV2,
        supersedes: Vec<String>,
        correction_reason_id: String,
    ) -> Result<Self, SettlementEvidenceError> {
        let mut item = Self {
            evidence_id: PENDING_EVIDENCE_ID.to_string(),
            subject_id,
            state: EvidenceStateV2::Active,
            trust,
            measurement: default_method(&claim),
            claim,
            supersedes,
            correction_reason_id: Some(correction_reason_id),
        };
        ensure_item_bounds(&item)?;
        item.canonicalize();
        item.evidence_id = item.computed_evidence_id()?;
        Ok(item)
    }

    fn canonicalize(&mut self) {
        self.supersedes.sort();
        self.claim.canonicalize();
    }

    fn computed_evidence_id(&self) -> Result<String, SettlementEvidenceError> {
        ensure_item_bounds(self)?;
        let mut identity = self.clone();
        identity.evidence_id = PENDING_EVIDENCE_ID.to_string();
        identity.canonicalize();
        Ok(format!(
            "artifact:blake3:{}",
            hash_bounded_json(&identity)?.to_hex()
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementPeriodV2 {
    pub start_epoch_seconds: i64,
    pub end_epoch_seconds: i64,
}

/// Canonical payload-free export consumed across the OSS/commercial boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementEvidenceManifestV2 {
    pub schema_version: u16,
    pub kind: String,
    pub manifest_id: String,
    pub subject_id: String,
    pub period: SettlementPeriodV2,
    /// Uppercase ISO 4217 alpha code. The verifier checks shape, not legal use.
    pub currency: String,
    /// Caller-provided amount in the currency's minor unit; never a float.
    pub claimed_amount_minor_units: u64,
    pub evidence: Vec<SettlementEvidenceItemV2>,
}

impl SettlementEvidenceManifestV2 {
    pub fn new(
        subject_id: String,
        period: SettlementPeriodV2,
        currency: String,
        claimed_amount_minor_units: u64,
        evidence: Vec<SettlementEvidenceItemV2>,
    ) -> Result<Self, SettlementEvidenceError> {
        let mut manifest = Self {
            schema_version: SETTLEMENT_EVIDENCE_SCHEMA_VERSION,
            kind: SETTLEMENT_EVIDENCE_KIND.to_string(),
            manifest_id: PENDING_MANIFEST_ID.to_string(),
            subject_id,
            period,
            currency,
            claimed_amount_minor_units,
            evidence,
        };
        ensure_manifest_bounds(&manifest)?;
        manifest.canonicalize();
        manifest.manifest_id = manifest.computed_manifest_id()?;
        Ok(manifest)
    }

    /// Canonical JSON, stable across input evidence permutations.
    pub fn canonical_json(&self) -> Result<String, SettlementEvidenceError> {
        ensure_manifest_bounds(self)?;
        if self.manifest_id != self.computed_manifest_id()?
            || self
                .evidence
                .iter()
                .any(|item| address_mismatch(item.computed_evidence_id(), &item.evidence_id))
        {
            return Err(SettlementEvidenceError::IntegrityMismatch);
        }
        let mut canonical = self.clone();
        canonical.canonicalize();
        serde_json::to_string(&canonical).map_err(SettlementEvidenceError::Serialize)
    }

    /// Bounded offline load. Unknown JSON fields are rejected by serde.
    pub fn load(path: &Path) -> Result<Self, SettlementEvidenceError> {
        let manifest: Self = read_json_bounded(path)?;
        ensure_manifest_bounds(&manifest)?;
        Ok(manifest)
    }

    /// Export canonical JSON without changing any approval/dispute state.
    pub fn export(&self, path: &Path) -> Result<(), SettlementEvidenceError> {
        let body = self.canonical_json()?;
        atomic_write_no_symlink(path, body.as_bytes())
    }

    fn canonicalize(&mut self) {
        for item in &mut self.evidence {
            item.canonicalize();
        }
        self.evidence
            .sort_by(|a, b| a.evidence_id.cmp(&b.evidence_id));
    }

    fn computed_manifest_id(&self) -> Result<String, SettlementEvidenceError> {
        ensure_manifest_bounds(self)?;
        let mut identity = self.clone();
        identity.manifest_id = PENDING_MANIFEST_ID.to_string();
        identity.canonicalize();
        Ok(format!(
            "manifest:blake3:{}",
            hash_bounded_json(&identity)?.to_hex()
        ))
    }
}

/// Fail-closed reasons emitted by the offline reconciler.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementIneligibilityReasonV2 {
    UnsupportedSchemaVersion,
    InvalidKind,
    InvalidManifestId,
    InvalidSubjectId,
    InvalidPeriod,
    InvalidCurrency,
    InvalidTrustStore,
    TooManyEvidenceItems,
    TooManyTrustDecisions,
    DuplicateTrustDecision,
    TooManyAttributionSources,
    TooManySupersessionRefs,
    OversizedString,
    ManifestTooLarge,
    DuplicateEvidenceId {
        evidence_id: String,
    },
    InvalidEvidenceId {
        evidence_id: String,
    },
    SubjectMismatch {
        evidence_id: String,
    },
    MissingEvidence {
        role: SettlementEvidenceRoleV2,
    },
    AmbiguousEvidence {
        role: SettlementEvidenceRoleV2,
    },
    UntrustedEvidence {
        evidence_id: String,
    },
    DisputedEvidence {
        evidence_id: String,
    },
    SupersededEvidence {
        evidence_id: String,
    },
    InvalidEvidenceReference {
        evidence_id: String,
    },
    InvalidEvidenceMethod {
        evidence_id: String,
    },
    IneligibleEvidenceClass {
        evidence_id: String,
    },
    InvalidCorrectionLineage {
        evidence_id: String,
    },
    CorrectionTargetCollision {
        target_id: String,
        correction_ids: Vec<String>,
    },
    IncompletePeriod,
    QualityGateFailed,
    CustomerApprovalNotGranted,
    CurrencyMismatch {
        evidence_id: String,
    },
    NonExclusiveAttribution {
        evidence_id: String,
    },
    DuplicateAttribution {
        source_evidence_id: String,
    },
    InvalidAttributedAmount {
        evidence_id: String,
    },
    AttributionExceedsBaseline,
    ClaimedAmountMismatch,
    ArithmeticOverflow,
}

/// Deterministic reconciliation output. The three authority flags are always
/// false because OSS verification cannot issue invoices or validate private
/// contract/approval authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementEligibilityV2 {
    pub schema_version: u16,
    pub manifest_id: String,
    pub trust_store_id: String,
    pub eligible: bool,
    pub reasons: Vec<SettlementIneligibilityReasonV2>,
    pub evidence_count: usize,
    pub active_evidence_count: usize,
    pub attributed_tokens: Option<u64>,
    pub attributed_minor_units: Option<u64>,
    pub invoice_authority: bool,
    pub contract_validity_verified: bool,
    pub customer_approval_authority_verified: bool,
}

/// Reconcile a manifest under the v2 evidence contract.
///
/// Processing is bounded to 1,000 evidence items and 1,000 total attribution
/// source IDs. Oversized input returns immediately or stops at the global cap.
#[must_use]
pub fn reconcile_settlement_evidence_v2(
    manifest: &SettlementEvidenceManifestV2,
    trust_store: &SettlementEvidenceTrustStoreV2,
) -> SettlementEligibilityV2 {
    let mut reasons = Vec::new();
    if let Err(error) = ensure_manifest_bounds(manifest) {
        reasons.push(bound_error_reason(&error, false));
    }
    if let Err(error) = ensure_trust_store_bounds(trust_store) {
        reasons.push(bound_error_reason(&error, true));
    }
    if !reasons.is_empty() {
        return eligibility(manifest, trust_store, reasons, 0, None, None);
    }
    if trust_store.schema_version != SETTLEMENT_EVIDENCE_SCHEMA_VERSION
        || trust_store
            .computed_trust_store_id()
            .map_or(true, |computed| trust_store.trust_store_id != computed)
    {
        reasons.push(SettlementIneligibilityReasonV2::InvalidTrustStore);
    }
    let unique_trust_decisions: BTreeSet<_> = trust_store.trusted_decisions.iter().collect();
    if unique_trust_decisions.len() != trust_store.trusted_decisions.len() {
        reasons.push(SettlementIneligibilityReasonV2::DuplicateTrustDecision);
    }
    if trust_store.trusted_decisions.iter().any(|decision| {
        !valid_address(&decision.evidence_id, "artifact:blake3:")
            || !valid_address(&decision.trust_decision_id, "artifact:blake3:")
            || !valid_address(&decision.trust_anchor_id, "anchor:blake3:")
    }) {
        reasons.push(SettlementIneligibilityReasonV2::InvalidTrustStore);
    }

    if manifest.schema_version != SETTLEMENT_EVIDENCE_SCHEMA_VERSION {
        reasons.push(SettlementIneligibilityReasonV2::UnsupportedSchemaVersion);
    }
    if manifest.kind != SETTLEMENT_EVIDENCE_KIND {
        reasons.push(SettlementIneligibilityReasonV2::InvalidKind);
    }
    if manifest
        .computed_manifest_id()
        .map_or(true, |computed| manifest.manifest_id != computed)
        || !valid_address(&manifest.manifest_id, "manifest:blake3:")
    {
        reasons.push(SettlementIneligibilityReasonV2::InvalidManifestId);
    }
    if !valid_address(&manifest.subject_id, "subject:blake3:") {
        reasons.push(SettlementIneligibilityReasonV2::InvalidSubjectId);
    }
    if manifest.period.start_epoch_seconds < 0
        || manifest.period.end_epoch_seconds <= manifest.period.start_epoch_seconds
    {
        reasons.push(SettlementIneligibilityReasonV2::InvalidPeriod);
    }
    if !valid_iso_currency(&manifest.currency) {
        reasons.push(SettlementIneligibilityReasonV2::InvalidCurrency);
    }

    let mut canonical_evidence = manifest.evidence.clone();
    for item in &mut canonical_evidence {
        item.canonicalize();
    }
    canonical_evidence.sort_by(|a, b| a.evidence_id.cmp(&b.evidence_id));

    let mut seen_evidence = BTreeSet::new();
    let mut roles: BTreeMap<SettlementEvidenceRoleV2, Vec<&SettlementEvidenceItemV2>> =
        BTreeMap::new();
    let mut active_count = 0;
    let mut correction_targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for item in &canonical_evidence {
        if !seen_evidence.insert(item.evidence_id.clone()) {
            reasons.push(SettlementIneligibilityReasonV2::DuplicateEvidenceId {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if item
            .computed_evidence_id()
            .map_or(true, |computed| item.evidence_id != computed)
            || !valid_address(&item.evidence_id, "artifact:blake3:")
        {
            reasons.push(SettlementIneligibilityReasonV2::InvalidEvidenceId {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if item.subject_id != manifest.subject_id {
            reasons.push(SettlementIneligibilityReasonV2::SubjectMismatch {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if !valid_address(&item.trust.trust_decision_id, "artifact:blake3:")
            || !valid_address(&item.trust.trust_anchor_id, "anchor:blake3:")
        {
            reasons.push(SettlementIneligibilityReasonV2::InvalidEvidenceReference {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if !valid_address(&item.measurement.method_artifact_id, "artifact:blake3:") {
            reasons.push(SettlementIneligibilityReasonV2::InvalidEvidenceMethod {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if item.measurement.evidence_class != expected_evidence_class(item.claim.role()) {
            reasons.push(SettlementIneligibilityReasonV2::IneligibleEvidenceClass {
                evidence_id: item.evidence_id.clone(),
            });
        }
        if item.trust.status == EvidenceTrustStatusV2::Untrusted || !trust_store.contains(item) {
            reasons.push(SettlementIneligibilityReasonV2::UntrustedEvidence {
                evidence_id: item.evidence_id.clone(),
            });
        }
        match item.state {
            EvidenceStateV2::Active => {
                active_count += 1;
                roles.entry(item.claim.role()).or_default().push(item);
            }
            EvidenceStateV2::Disputed => {
                reasons.push(SettlementIneligibilityReasonV2::DisputedEvidence {
                    evidence_id: item.evidence_id.clone(),
                });
            }
            EvidenceStateV2::Superseded => {
                reasons.push(SettlementIneligibilityReasonV2::SupersededEvidence {
                    evidence_id: item.evidence_id.clone(),
                });
            }
        }
        validate_correction_lineage(item, &mut reasons);
        for target in &item.supersedes {
            correction_targets
                .entry(target.clone())
                .or_default()
                .insert(item.evidence_id.clone());
        }
        validate_claim_references(item, manifest, &mut reasons);
    }
    for (target_id, correction_ids) in correction_targets {
        if correction_ids.len() > 1 {
            reasons.push(SettlementIneligibilityReasonV2::CorrectionTargetCollision {
                target_id,
                correction_ids: correction_ids.into_iter().collect(),
            });
        }
    }

    for role in [
        SettlementEvidenceRoleV2::Baseline,
        SettlementEvidenceRoleV2::Price,
        SettlementEvidenceRoleV2::Contract,
        SettlementEvidenceRoleV2::Quality,
        SettlementEvidenceRoleV2::PeriodCompletion,
        SettlementEvidenceRoleV2::CustomerApproval,
    ] {
        match roles.get(&role).map(Vec::len).unwrap_or_default() {
            0 => reasons.push(SettlementIneligibilityReasonV2::MissingEvidence { role }),
            1 => {}
            _ => reasons.push(SettlementIneligibilityReasonV2::AmbiguousEvidence { role }),
        }
    }

    let attributions = roles
        .get(&SettlementEvidenceRoleV2::Attribution)
        .cloned()
        .unwrap_or_default();
    if attributions.is_empty() {
        reasons.push(SettlementIneligibilityReasonV2::MissingEvidence {
            role: SettlementEvidenceRoleV2::Attribution,
        });
    }

    let mut mechanisms = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut attributed_tokens = Some(0_u64);
    let mut attributed_minor_units = Some(0_u64);
    for item in attributions {
        if let SettlementEvidenceClaimV2::Attribution {
            mechanism_id,
            exclusive,
            attributed_tokens: tokens,
            attributed_minor_units: minor,
            source_evidence_ids,
        } = &item.claim
        {
            if !mechanisms.insert(mechanism_id.clone()) {
                reasons.push(SettlementIneligibilityReasonV2::AmbiguousEvidence {
                    role: SettlementEvidenceRoleV2::Attribution,
                });
            }
            if !exclusive {
                reasons.push(SettlementIneligibilityReasonV2::NonExclusiveAttribution {
                    evidence_id: item.evidence_id.clone(),
                });
            }
            if *tokens == 0 || *minor == 0 || source_evidence_ids.is_empty() {
                reasons.push(SettlementIneligibilityReasonV2::InvalidAttributedAmount {
                    evidence_id: item.evidence_id.clone(),
                });
            }
            for source in source_evidence_ids {
                if !sources.insert(source.clone()) {
                    reasons.push(SettlementIneligibilityReasonV2::DuplicateAttribution {
                        source_evidence_id: source.clone(),
                    });
                }
            }
            checked_add(&mut attributed_tokens, *tokens, &mut reasons);
            checked_add(&mut attributed_minor_units, *minor, &mut reasons);
        }
    }

    if let (Some(total), Some(baselines)) = (
        attributed_tokens,
        roles.get(&SettlementEvidenceRoleV2::Baseline),
    ) && baselines.len() == 1
        && let SettlementEvidenceClaimV2::Baseline {
            baseline_tokens, ..
        } = &baselines[0].claim
        && total > *baseline_tokens
    {
        reasons.push(SettlementIneligibilityReasonV2::AttributionExceedsBaseline);
    }
    if attributed_minor_units.is_some_and(|total| total != manifest.claimed_amount_minor_units) {
        reasons.push(SettlementIneligibilityReasonV2::ClaimedAmountMismatch);
    }

    eligibility(
        manifest,
        trust_store,
        reasons,
        active_count,
        attributed_tokens,
        attributed_minor_units,
    )
}

fn validate_correction_lineage(
    item: &SettlementEvidenceItemV2,
    reasons: &mut Vec<SettlementIneligibilityReasonV2>,
) {
    let mut local_targets = BTreeSet::new();
    let invalid = item.supersedes.len() > MAX_SUPERSESSION_REFS
        || (item.supersedes.is_empty() != item.correction_reason_id.is_none())
        || item.state != EvidenceStateV2::Active && !item.supersedes.is_empty()
        || item.supersedes.iter().any(|target| {
            target == &item.evidence_id
                || !valid_address(target, "artifact:blake3:")
                || !local_targets.insert(target)
        })
        || item
            .correction_reason_id
            .as_ref()
            .is_some_and(|id| !valid_address(id, "artifact:blake3:"));
    if invalid {
        reasons.push(SettlementIneligibilityReasonV2::InvalidCorrectionLineage {
            evidence_id: item.evidence_id.clone(),
        });
    }
}

fn validate_claim_references(
    item: &SettlementEvidenceItemV2,
    manifest: &SettlementEvidenceManifestV2,
    reasons: &mut Vec<SettlementIneligibilityReasonV2>,
) {
    let invalid_reference = match &item.claim {
        SettlementEvidenceClaimV2::Baseline {
            baseline_version_id,
            baseline_tokens,
        } => !valid_address(baseline_version_id, "artifact:blake3:") || *baseline_tokens == 0,
        SettlementEvidenceClaimV2::Price {
            price_version_id,
            currency,
            unit_price_micros,
        } => {
            if currency != &manifest.currency {
                reasons.push(SettlementIneligibilityReasonV2::CurrencyMismatch {
                    evidence_id: item.evidence_id.clone(),
                });
            }
            !valid_address(price_version_id, "artifact:blake3:") || *unit_price_micros == 0
        }
        SettlementEvidenceClaimV2::Contract {
            contract_version_id,
        } => !valid_address(contract_version_id, "artifact:blake3:"),
        SettlementEvidenceClaimV2::Quality {
            quality_gate_id,
            passed,
        } => {
            if !passed {
                reasons.push(SettlementIneligibilityReasonV2::QualityGateFailed);
            }
            !valid_address(quality_gate_id, "artifact:blake3:")
        }
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id,
            source_evidence_ids,
            ..
        } => {
            !valid_address(mechanism_id, "mechanism:blake3:")
                || source_evidence_ids
                    .iter()
                    .any(|id| !valid_address(id, "artifact:blake3:"))
        }
        SettlementEvidenceClaimV2::PeriodCompletion {
            period_start_epoch_seconds,
            period_end_epoch_seconds,
            complete,
        } => {
            if !complete
                || *period_start_epoch_seconds != manifest.period.start_epoch_seconds
                || *period_end_epoch_seconds != manifest.period.end_epoch_seconds
            {
                reasons.push(SettlementIneligibilityReasonV2::IncompletePeriod);
            }
            false
        }
        SettlementEvidenceClaimV2::CustomerApproval {
            approval_artifact_id,
            approved,
        } => {
            if !approved {
                reasons.push(SettlementIneligibilityReasonV2::CustomerApprovalNotGranted);
            }
            !valid_address(approval_artifact_id, "artifact:blake3:")
        }
    };
    if invalid_reference {
        reasons.push(SettlementIneligibilityReasonV2::InvalidEvidenceReference {
            evidence_id: item.evidence_id.clone(),
        });
    }
}

fn checked_add(
    total: &mut Option<u64>,
    value: u64,
    reasons: &mut Vec<SettlementIneligibilityReasonV2>,
) {
    if let Some(current) = *total {
        if let Some(next) = current.checked_add(value) {
            *total = Some(next);
        } else {
            *total = None;
            reasons.push(SettlementIneligibilityReasonV2::ArithmeticOverflow);
        }
    }
}

fn default_method(claim: &SettlementEvidenceClaimV2) -> SettlementEvidenceMethodV2 {
    let role = claim.role();
    let label = match role {
        SettlementEvidenceRoleV2::Baseline => "settlement-baseline-method-v2",
        SettlementEvidenceRoleV2::Price => "settlement-price-declaration-v2",
        SettlementEvidenceRoleV2::Contract => "settlement-contract-declaration-v2",
        SettlementEvidenceRoleV2::Quality => "settlement-quality-reconciliation-v2",
        SettlementEvidenceRoleV2::Attribution => "settlement-exclusive-attribution-v2",
        SettlementEvidenceRoleV2::PeriodCompletion => "settlement-period-observation-v2",
        SettlementEvidenceRoleV2::CustomerApproval => "settlement-approval-declaration-v2",
    };
    SettlementEvidenceMethodV2 {
        method_artifact_id: format!(
            "artifact:blake3:{}",
            blake3::hash(label.as_bytes()).to_hex()
        ),
        evidence_class: expected_evidence_class(role),
    }
}

const fn expected_evidence_class(role: SettlementEvidenceRoleV2) -> SettlementEvidenceClassV2 {
    match role {
        SettlementEvidenceRoleV2::Baseline | SettlementEvidenceRoleV2::PeriodCompletion => {
            SettlementEvidenceClassV2::Measured
        }
        SettlementEvidenceRoleV2::Quality | SettlementEvidenceRoleV2::Attribution => {
            SettlementEvidenceClassV2::Reconciled
        }
        SettlementEvidenceRoleV2::Price
        | SettlementEvidenceRoleV2::Contract
        | SettlementEvidenceRoleV2::CustomerApproval => SettlementEvidenceClassV2::Declared,
    }
}

fn eligibility(
    manifest: &SettlementEvidenceManifestV2,
    trust_store: &SettlementEvidenceTrustStoreV2,
    mut reasons: Vec<SettlementIneligibilityReasonV2>,
    active_evidence_count: usize,
    attributed_tokens: Option<u64>,
    attributed_minor_units: Option<u64>,
) -> SettlementEligibilityV2 {
    reasons.sort();
    reasons.dedup();
    SettlementEligibilityV2 {
        schema_version: SETTLEMENT_EVIDENCE_SCHEMA_VERSION,
        manifest_id: bounded_result_id(&manifest.manifest_id, "manifest:invalid"),
        trust_store_id: bounded_result_id(&trust_store.trust_store_id, "trust-store:invalid"),
        eligible: reasons.is_empty(),
        reasons,
        evidence_count: manifest.evidence.len(),
        active_evidence_count,
        attributed_tokens,
        attributed_minor_units,
        invoice_authority: false,
        contract_validity_verified: false,
        customer_approval_authority_verified: false,
    }
}

fn bounded_result_id(value: &str, invalid: &str) -> String {
    if value.len() <= MAX_SETTLEMENT_STRING_BYTES {
        value.to_string()
    } else {
        invalid.to_string()
    }
}

fn address_mismatch(computed: Result<String, SettlementEvidenceError>, expected: &str) -> bool {
    computed.map_or(true, |value| value != expected)
}

fn bound_error_reason(
    error: &SettlementEvidenceError,
    trust_store: bool,
) -> SettlementIneligibilityReasonV2 {
    match error {
        SettlementEvidenceError::TooManyEvidenceItems => {
            SettlementIneligibilityReasonV2::TooManyEvidenceItems
        }
        SettlementEvidenceError::TooManyTrustDecisions => {
            SettlementIneligibilityReasonV2::TooManyTrustDecisions
        }
        SettlementEvidenceError::TooManyAttributionSources => {
            SettlementIneligibilityReasonV2::TooManyAttributionSources
        }
        SettlementEvidenceError::TooManySupersessionRefs => {
            SettlementIneligibilityReasonV2::TooManySupersessionRefs
        }
        SettlementEvidenceError::OversizedString => {
            SettlementIneligibilityReasonV2::OversizedString
        }
        SettlementEvidenceError::ManifestTooLarge => {
            SettlementIneligibilityReasonV2::ManifestTooLarge
        }
        _ if trust_store => SettlementIneligibilityReasonV2::InvalidTrustStore,
        _ => SettlementIneligibilityReasonV2::InvalidManifestId,
    }
}

fn valid_address(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_iso_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn ensure_string_bound(value: &str) -> Result<(), SettlementEvidenceError> {
    if value.len() > MAX_SETTLEMENT_STRING_BYTES {
        return Err(SettlementEvidenceError::OversizedString);
    }
    Ok(())
}

fn ensure_item_structure(item: &SettlementEvidenceItemV2) -> Result<(), SettlementEvidenceError> {
    if item.supersedes.len() > MAX_SUPERSESSION_REFS {
        return Err(SettlementEvidenceError::TooManySupersessionRefs);
    }
    for value in [
        item.evidence_id.as_str(),
        item.subject_id.as_str(),
        item.trust.trust_decision_id.as_str(),
        item.trust.trust_anchor_id.as_str(),
        item.measurement.method_artifact_id.as_str(),
    ] {
        ensure_string_bound(value)?;
    }
    if let Some(reason) = item.correction_reason_id.as_deref() {
        ensure_string_bound(reason)?;
    }
    for target in &item.supersedes {
        ensure_string_bound(target)?;
    }
    match &item.claim {
        SettlementEvidenceClaimV2::Baseline {
            baseline_version_id,
            ..
        } => ensure_string_bound(baseline_version_id)?,
        SettlementEvidenceClaimV2::Price {
            price_version_id,
            currency,
            ..
        } => {
            ensure_string_bound(price_version_id)?;
            ensure_string_bound(currency)?;
        }
        SettlementEvidenceClaimV2::Contract {
            contract_version_id,
        } => ensure_string_bound(contract_version_id)?,
        SettlementEvidenceClaimV2::Quality {
            quality_gate_id, ..
        } => ensure_string_bound(quality_gate_id)?,
        SettlementEvidenceClaimV2::Attribution {
            mechanism_id,
            source_evidence_ids,
            ..
        } => {
            if source_evidence_ids.len() > MAX_ATTRIBUTION_SOURCE_IDS {
                return Err(SettlementEvidenceError::TooManyAttributionSources);
            }
            ensure_string_bound(mechanism_id)?;
            for source in source_evidence_ids {
                ensure_string_bound(source)?;
            }
        }
        SettlementEvidenceClaimV2::PeriodCompletion { .. } => {}
        SettlementEvidenceClaimV2::CustomerApproval {
            approval_artifact_id,
            ..
        } => ensure_string_bound(approval_artifact_id)?,
    }
    Ok(())
}

fn ensure_item_bounds(item: &SettlementEvidenceItemV2) -> Result<(), SettlementEvidenceError> {
    ensure_item_structure(item)?;
    ensure_serialized_bound(item)
}

fn ensure_manifest_bounds(
    manifest: &SettlementEvidenceManifestV2,
) -> Result<(), SettlementEvidenceError> {
    if manifest.evidence.len() > MAX_SETTLEMENT_EVIDENCE_ITEMS {
        return Err(SettlementEvidenceError::TooManyEvidenceItems);
    }
    for value in [
        manifest.kind.as_str(),
        manifest.manifest_id.as_str(),
        manifest.subject_id.as_str(),
        manifest.currency.as_str(),
    ] {
        ensure_string_bound(value)?;
    }
    let mut total_sources = 0usize;
    for item in &manifest.evidence {
        ensure_item_structure(item)?;
        if let SettlementEvidenceClaimV2::Attribution {
            source_evidence_ids,
            ..
        } = &item.claim
        {
            total_sources = total_sources
                .checked_add(source_evidence_ids.len())
                .ok_or(SettlementEvidenceError::TooManyAttributionSources)?;
            if total_sources > MAX_ATTRIBUTION_SOURCE_IDS {
                return Err(SettlementEvidenceError::TooManyAttributionSources);
            }
        }
    }
    ensure_serialized_bound(manifest)
}

fn ensure_trust_store_bounds(
    store: &SettlementEvidenceTrustStoreV2,
) -> Result<(), SettlementEvidenceError> {
    if store.trusted_decisions.len() > MAX_SETTLEMENT_TRUST_DECISIONS {
        return Err(SettlementEvidenceError::TooManyTrustDecisions);
    }
    ensure_string_bound(&store.trust_store_id)?;
    for decision in &store.trusted_decisions {
        for value in [
            decision.evidence_id.as_str(),
            decision.trust_decision_id.as_str(),
            decision.trust_anchor_id.as_str(),
        ] {
            ensure_string_bound(value)?;
        }
    }
    ensure_serialized_bound(store)
}

struct BoundedCountWriter {
    written: u64,
}

impl Write for BoundedCountWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("settlement evidence size overflow"))?;
        if next > MAX_SETTLEMENT_MANIFEST_BYTES {
            return Err(std::io::Error::other(
                "settlement evidence exceeds bounded size",
            ));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn ensure_serialized_bound<T: Serialize>(value: &T) -> Result<(), SettlementEvidenceError> {
    let mut writer = BoundedCountWriter { written: 0 };
    serde_json::to_writer(&mut writer, value).map_err(|_| SettlementEvidenceError::ManifestTooLarge)
}

struct BoundedHashWriter {
    written: u64,
    hasher: blake3::Hasher,
}

impl Write for BoundedHashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("settlement evidence size overflow"))?;
        if next > MAX_SETTLEMENT_MANIFEST_BYTES {
            return Err(std::io::Error::other(
                "settlement evidence exceeds bounded size",
            ));
        }
        self.hasher.update(bytes);
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hash_bounded_json<T: Serialize>(value: &T) -> Result<blake3::Hash, SettlementEvidenceError> {
    let mut writer = BoundedHashWriter {
        written: 0,
        hasher: blake3::Hasher::new(),
    };
    serde_json::to_writer(&mut writer, value).map_err(SettlementEvidenceError::Serialize)?;
    Ok(writer.hasher.finalize())
}

fn read_json_bounded<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<T, SettlementEvidenceError> {
    let lstat = std::fs::symlink_metadata(path).map_err(SettlementEvidenceError::Io)?;
    if lstat.file_type().is_symlink() || !lstat.is_file() {
        return Err(SettlementEvidenceError::UnsafePath);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(SettlementEvidenceError::Io)?;
    let fstat = file.metadata().map_err(SettlementEvidenceError::Io)?;
    if !fstat.is_file() {
        return Err(SettlementEvidenceError::UnsafePath);
    }
    if fstat.len() > MAX_SETTLEMENT_MANIFEST_BYTES {
        return Err(SettlementEvidenceError::ManifestTooLarge);
    }
    let mut body = String::with_capacity((fstat.len() + 1) as usize);
    file.take(MAX_SETTLEMENT_MANIFEST_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(SettlementEvidenceError::Io)?;
    if body.len() as u64 > MAX_SETTLEMENT_MANIFEST_BYTES {
        return Err(SettlementEvidenceError::ManifestTooLarge);
    }
    serde_json::from_str(&body).map_err(SettlementEvidenceError::Deserialize)
}

struct TempExportGuard(Option<PathBuf>);

impl Drop for TempExportGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn atomic_write_no_symlink(path: &Path, body: &[u8]) -> Result<(), SettlementEvidenceError> {
    if body.len() as u64 > MAX_SETTLEMENT_MANIFEST_BYTES {
        return Err(SettlementEvidenceError::ManifestTooLarge);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_meta = std::fs::symlink_metadata(parent).map_err(SettlementEvidenceError::Io)?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(SettlementEvidenceError::UnsafePath);
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 255)
        .ok_or(SettlementEvidenceError::UnsafePath)?;
    validate_export_target(path)?;

    let sequence = ATOMIC_EXPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".{file_name}.settlement-{}-{sequence}.tmp",
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options
        .open(&temp_path)
        .map_err(SettlementEvidenceError::Io)?;
    let mut guard = TempExportGuard(Some(temp_path.clone()));
    file.write_all(body).map_err(SettlementEvidenceError::Io)?;
    file.sync_all().map_err(SettlementEvidenceError::Io)?;
    drop(file);

    validate_export_target(path)?;
    std::fs::rename(&temp_path, path).map_err(SettlementEvidenceError::Io)?;
    guard.0 = None;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(SettlementEvidenceError::Io)?;
    Ok(())
}

fn validate_export_target(path: &Path) -> Result<(), SettlementEvidenceError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(SettlementEvidenceError::UnsafePath)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SettlementEvidenceError::Io(error)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettlementEvidenceError {
    #[error("settlement evidence manifest exceeds the bounded byte limit")]
    ManifestTooLarge,
    #[error("settlement evidence manifest exceeds the bounded item limit")]
    TooManyEvidenceItems,
    #[error("settlement evidence trust store exceeds the bounded decision limit")]
    TooManyTrustDecisions,
    #[error("settlement evidence attribution sources exceed the bounded limit")]
    TooManyAttributionSources,
    #[error("settlement evidence supersession references exceed the bounded limit")]
    TooManySupersessionRefs,
    #[error("settlement evidence string exceeds the bounded limit")]
    OversizedString,
    #[error("settlement evidence content address does not match canonical content")]
    IntegrityMismatch,
    #[error("settlement evidence path is a symlink or unsafe file type")]
    UnsafePath,
    #[error("settlement evidence I/O failed: {0}")]
    Io(std::io::Error),
    #[error("settlement evidence serialization failed: {0}")]
    Serialize(serde_json::Error),
    #[error("settlement evidence deserialization failed: {0}")]
    Deserialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
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

    fn item(
        subject: &str,
        label: &str,
        claim: SettlementEvidenceClaimV2,
    ) -> SettlementEvidenceItemV2 {
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
        too_many_items.evidence =
            vec![original.evidence[0].clone(); MAX_SETTLEMENT_EVIDENCE_ITEMS + 1];
        assert_eq!(
            reconcile_settlement_evidence_v2(
                &too_many_items,
                &SettlementEvidenceTrustStoreV2::empty()
            )
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
            include_str!("../../../tests/fixtures/settlement-evidence-v2/eligible.json").trim_end()
        );
        assert_eq!(
            trust_store_for(&eligible_manifest())
                .canonical_json()
                .unwrap(),
            include_str!("../../../tests/fixtures/settlement-evidence-v2/trusted-decisions.json")
                .trim_end()
        );
    }
}
