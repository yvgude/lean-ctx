// SPDX-License-Identifier: Apache-2.0
//! Context Gateway v1 — the canonical, public decision contract.
//!
//! Every governed piece of context carries a [`ContextObjectPassportV1`]; the
//! gateway records one [`ContextDecisionV1`] per object and summarises a
//! delivery in one [`ContextDecisionReceiptV1`], which is the single source for
//! audit, HUD and SDK surfaces. The contract never carries content, secret
//! values or detector matches — only digests, counts and reason codes.
//!
//! Field-level invariants (principal, coverage, reason codes) are enforced at
//! construction *and* deserialization; cross-field invariants by `validate()`,
//! which `derive` and `canonical_digest` call. All of them fail closed:
//! - an unknown principal is explicit and never carries an identity;
//! - classification only ever rises through derivation ([`ClassificationV1::join`]);
//! - destination constraints only accumulate;
//! - a detector may not report more coverage than it inspected;
//! - a receipt's security counts must equal its per-object decisions.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use sha2::{Digest, Sha256};

use crate::common::{
    TaskId, V1_SCHEMA_VERSION, ValidationError, deserialize_schema_version, validate_schema_version,
};
use crate::identity::{PolicyId, ProtocolReference, SemanticVersion, Sha256Digest, SourceId};

/// Bounds keep untrusted receipts from growing without limit.
pub const MAX_REASON_CODES: usize = 32;
pub const MAX_SIGNALS: usize = 256;
pub const MAX_DECISIONS: usize = 4_096;
pub const MAX_LINEAGE: usize = 256;
pub const MAX_POLICY_REFS: usize = 32;
const MAX_REASON_CODE_LEN: usize = 64;

// ─── Classification ─────────────────────────────────────────────────────────

/// The one classification lattice: `Public < Internal < Confidential < Restricted`.
///
/// Secrets and credentials are `Restricted`. There is deliberately no
/// `Default`: an unclassified object must be classified by the mode's rule
/// ([`GatewayModeV1::unclassified`]), never by an implicit fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationV1 {
    Public,
    Internal,
    Confidential,
    Restricted,
}

impl ClassificationV1 {
    /// Information-flow join: a derived object is at least as sensitive as the
    /// most sensitive input.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        self.max(other)
    }
}

impl From<crate::team_context::TeamClassification> for ClassificationV1 {
    fn from(value: crate::team_context::TeamClassification) -> Self {
        use crate::team_context::TeamClassification as T;
        match value {
            T::Public => Self::Public,
            T::Internal => Self::Internal,
            T::Confidential => Self::Confidential,
            T::Restricted => Self::Restricted,
        }
    }
}

impl From<crate::knowledge::ClassificationLevel> for ClassificationV1 {
    fn from(value: crate::knowledge::ClassificationLevel) -> Self {
        use crate::knowledge::ClassificationLevel as K;
        match value {
            K::Public => Self::Public,
            K::Internal => Self::Internal,
            K::Confidential => Self::Confidential,
            K::Restricted => Self::Restricted,
        }
    }
}

impl From<crate::experiment::DataClassification> for ClassificationV1 {
    fn from(value: crate::experiment::DataClassification) -> Self {
        use crate::experiment::DataClassification as D;
        match value {
            D::Public => Self::Public,
            D::Internal => Self::Internal,
            D::Confidential => Self::Confidential,
            D::Restricted => Self::Restricted,
        }
    }
}

/// Projection of the VIA classification onto the two gateway axes. VIA mixes
/// sensitivity with placement (`LocalOnly`, `EnterprisePrivate`); the gateway
/// keeps placement as a [`DestinationConstraintV1`] so locality never depends
/// on a sensitivity estimate.
#[must_use]
pub fn project_via_classification(
    value: crate::edge_via::ViaClassificationV1,
) -> (ClassificationV1, Option<DestinationConstraintV1>) {
    use crate::edge_via::ViaClassificationV1 as V;
    match value {
        V::Public => (ClassificationV1::Public, None),
        V::Normal => (ClassificationV1::Internal, None),
        V::Sensitive => (ClassificationV1::Confidential, None),
        V::Secret => (ClassificationV1::Restricted, None),
        V::LocalOnly => (
            ClassificationV1::Confidential,
            Some(DestinationConstraintV1::LocalOnly),
        ),
        V::EnterprisePrivate => (
            ClassificationV1::Confidential,
            Some(DestinationConstraintV1::OrganizationPrivate),
        ),
    }
}

/// Operating posture of the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayModeV1 {
    /// Local-first, strong defaults, visible warnings.
    Developer,
    /// Organisation-controlled: identity and mandatory checks fail closed.
    Governed,
    /// Nothing leaves the trust boundary except to approved destinations.
    Sovereign,
}

impl GatewayModeV1 {
    /// Classification for an object no detector or label classified.
    #[must_use]
    pub fn unclassified(self) -> ClassificationV1 {
        match self {
            Self::Developer => ClassificationV1::Public,
            Self::Governed | Self::Sovereign => ClassificationV1::Internal,
        }
    }
}

// ─── Principal and destination ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKindV1 {
    Person,
    Team,
    Organization,
    Project,
    Agent,
    Session,
    Workload,
    /// No authenticated identity. Explicit, and never an authorization.
    Unknown,
}

/// Who requested the context. `Unknown` never carries an id; every other
/// kind must.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct PrincipalV1 {
    pub kind: PrincipalKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ProtocolReference>,
}

impl PrincipalV1 {
    pub fn new(kind: PrincipalKindV1, id: ProtocolReference) -> Result<Self, ValidationError> {
        if kind == PrincipalKindV1::Unknown {
            return Err(ValidationError::new(
                "an unknown principal must not carry an identity",
            ));
        }
        Ok(Self { kind, id: Some(id) })
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self {
            kind: PrincipalKindV1::Unknown,
            id: None,
        }
    }

    #[must_use]
    pub fn is_known(&self) -> bool {
        self.kind != PrincipalKindV1::Unknown
    }
}

impl<'de> Deserialize<'de> for PrincipalV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: PrincipalKindV1,
            #[serde(default)]
            id: Option<ProtocolReference>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match (wire.kind, wire.id) {
            (PrincipalKindV1::Unknown, None) => Ok(Self::unknown()),
            (PrincipalKindV1::Unknown, Some(_)) => Err(DeError::custom(
                "an unknown principal must not carry an identity",
            )),
            (_, None) => Err(DeError::custom("a known principal requires an id")),
            (kind, Some(id)) => Ok(Self { kind, id: Some(id) }),
        }
    }
}

/// Placement constraints that only ever accumulate through derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationConstraintV1 {
    /// Must never leave the local machine.
    LocalOnly,
    /// Only organisation-managed destinations (or local ones).
    OrganizationPrivate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationLocalityV1 {
    Local,
    Remote,
    /// Not established. Never satisfies a locality constraint.
    Unknown,
}

/// Where the compiled context is delivered. Models LeanCTX itself uses
/// (detectors, summarisers) are destinations too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationV1 {
    pub provider: ProtocolReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ProtocolReference>,
    pub locality: DestinationLocalityV1,
    /// True only when the destination is attested as organisation-managed.
    #[serde(default)]
    pub organization_managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_ref: Option<ProtocolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<ProtocolReference>,
}

impl DestinationV1 {
    /// Whether this destination may receive an object with `constraint`.
    #[must_use]
    pub fn satisfies(&self, constraint: DestinationConstraintV1) -> bool {
        match constraint {
            DestinationConstraintV1::LocalOnly => self.locality == DestinationLocalityV1::Local,
            DestinationConstraintV1::OrganizationPrivate => {
                self.locality == DestinationLocalityV1::Local
                    || (self.organization_managed && self.locality == DestinationLocalityV1::Remote)
            }
        }
    }

    /// Whether this destination satisfies every constraint in `constraints`.
    #[must_use]
    pub fn satisfies_all<'a>(
        &self,
        constraints: impl IntoIterator<Item = &'a DestinationConstraintV1>,
    ) -> bool {
        constraints
            .into_iter()
            .all(|constraint| self.satisfies(*constraint))
    }
}

// ─── Reason codes and policy references ─────────────────────────────────────

/// Stable, machine-readable reason: `[a-z][a-z0-9_.]{2,63}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReasonCodeV1(String);

impl ReasonCodeV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (3..=MAX_REASON_CODE_LEN).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'.');
        if !valid {
            return Err(ValidationError::new(format!(
                "reason code {value:?} must match [a-z][a-z0-9_.]{{2,63}}"
            )));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ReasonCodeV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(|e| DeError::custom(e.0))
    }
}

/// The exact policy that decided: identity, optional version, content digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRefV1 {
    pub id: PolicyId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<SemanticVersion>,
    pub digest: Sha256Digest,
}

// ─── Detector signals ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorCategoryV1 {
    Secret,
    Pii,
    PromptInjection,
    Classification,
    Policy,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityV1 {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageKindV1 {
    /// Every byte of the object was inspected.
    Complete,
    /// Only part of the object was inspected (prefix, chunk budget, …).
    Partial,
    /// The detector cannot inspect this media type.
    Unsupported,
    /// The detector failed; nothing it reported can be relied on.
    Failed,
    /// Policy did not require this detector for this object.
    NotRequired,
}

/// What a detector actually looked at. A result never implies more coverage
/// than recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectorCoverageV1 {
    pub kind: CoverageKindV1,
    pub bytes_total: u64,
    pub bytes_inspected: u64,
    pub chunks_total: u32,
    pub chunks_inspected: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ReasonCodeV1>,
}

impl DetectorCoverageV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.bytes_inspected > self.bytes_total || self.chunks_inspected > self.chunks_total {
            return Err(ValidationError::new(
                "coverage must not inspect more than the object holds",
            ));
        }
        let all_bytes = self.bytes_inspected == self.bytes_total;
        match self.kind {
            CoverageKindV1::Complete if !all_bytes => Err(ValidationError::new(
                "complete coverage must inspect every byte",
            )),
            CoverageKindV1::Partial if all_bytes => Err(ValidationError::new(
                "partial coverage must leave bytes uninspected",
            )),
            _ => Ok(()),
        }
    }

    /// True only when the detector inspected the whole object.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.kind == CoverageKindV1::Complete
    }
}

impl<'de> Deserialize<'de> for DetectorCoverageV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: CoverageKindV1,
            bytes_total: u64,
            bytes_inspected: u64,
            chunks_total: u32,
            chunks_inspected: u32,
            #[serde(default)]
            reason: Option<ReasonCodeV1>,
        }
        let w = Wire::deserialize(deserializer)?;
        let coverage = Self {
            kind: w.kind,
            bytes_total: w.bytes_total,
            bytes_inspected: w.bytes_inspected,
            chunks_total: w.chunks_total,
            chunks_inspected: w.chunks_inspected,
            reason: w.reason,
        };
        coverage.validate().map_err(|e| DeError::custom(e.0))?;
        Ok(coverage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorStatusV1 {
    Completed,
    Failed,
    TimedOut,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorRefV1 {
    pub id: ProtocolReference,
    pub version: SemanticVersion,
}

/// One detector's normalised result. Carries counts only — never the matched
/// value — so telemetry and receipts cannot leak what was detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorSignalV1 {
    pub detector: DetectorRefV1,
    pub category: DetectorCategoryV1,
    pub severity: SeverityV1,
    /// Confidence in thousandths. Not a probability unless `calibrated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_milli: Option<u16>,
    #[serde(default)]
    pub calibrated: bool,
    pub evidence_count: u32,
    pub coverage: DetectorCoverageV1,
    pub status: DetectorStatusV1,
    pub latency_us: u64,
}

impl DetectorSignalV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.coverage.validate()?;
        if self.confidence_milli.is_some_and(|c| c > 1_000) {
            return Err(ValidationError::new(
                "confidence_milli must be at most 1000",
            ));
        }
        let failed = matches!(
            self.status,
            DetectorStatusV1::Failed | DetectorStatusV1::TimedOut
        );
        if failed && self.coverage.is_complete() {
            return Err(ValidationError::new(
                "a failed or timed-out detector cannot claim complete coverage",
            ));
        }
        Ok(())
    }

    /// Whether this signal may satisfy a *required* detector: it ran to
    /// completion over the whole object.
    #[must_use]
    pub fn satisfies_requirement(&self) -> bool {
        self.status == DetectorStatusV1::Completed && self.coverage.is_complete()
    }
}

// ─── Decisions and transformations ──────────────────────────────────────────

/// Per-object outcome, ordered from least to most restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDispositionV1 {
    Allow,
    AllowMinimized,
    AllowRedacted,
    AllowSummaryOnly,
    AllowLocalModelOnly,
    /// Held until an approver releases it; not delivered meanwhile.
    AllowWithApproval,
    Quarantine,
    Deny,
}

impl ContextDispositionV1 {
    /// Whether content (possibly transformed) reaches the destination now.
    #[must_use]
    pub fn delivers_content(self) -> bool {
        self <= Self::AllowLocalModelOnly
    }

    /// Combining two rules keeps the stricter outcome.
    #[must_use]
    pub fn most_restrictive(self, other: Self) -> Self {
        self.max(other)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformationKindV1 {
    Redaction,
    Classification,
    Selection,
    Deduplication,
    StructuralExtraction,
    Compression,
    Summarization,
    Recovery,
    Reranking,
}

/// One step in an object's lineage: which transformation turned which input
/// digest into which output digest, and what it cost or saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationRecordV1 {
    pub kind: TransformationKindV1,
    pub input: Sha256Digest,
    pub output: Sha256Digest,
    pub tokens_before: u64,
    pub tokens_after: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<ReasonCodeV1>,
}

/// The gateway's decision about one object. Anything but a plain `Allow`
/// must say why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextDecisionV1 {
    pub object: Sha256Digest,
    pub disposition: ContextDispositionV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<ReasonCodeV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<DetectorSignalV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_transformations: Vec<TransformationKindV1>,
}

impl ContextDecisionV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.disposition != ContextDispositionV1::Allow && self.reason_codes.is_empty() {
            return Err(ValidationError::new(
                "every non-allow decision requires at least one reason code",
            ));
        }
        if self.reason_codes.len() > MAX_REASON_CODES || self.signals.len() > MAX_SIGNALS {
            return Err(ValidationError::new("decision exceeds its bounds"));
        }
        for signal in &self.signals {
            signal.validate()?;
        }
        Ok(())
    }
}

// ─── Passport ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindV1 {
    File,
    Shell,
    Search,
    Provider,
    Memory,
    Knowledge,
    Agent,
    Web,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRefV1 {
    pub kind: SourceKindV1,
    pub id: SourceId,
}

/// How far the source is trusted, least trusted first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevelV1 {
    Untrusted,
    Unknown,
    Internal,
    Trusted,
}

/// Security and optimisation metadata that travels with one object. Content
/// is referenced by digest, never embedded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextObjectPassportV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub object: Sha256Digest,
    pub source: SourceRefV1,
    pub principal: PrincipalV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    pub classification: ClassificationV1,
    pub trust: TrustLevelV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_refs: Vec<PolicyRefV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_signals: Vec<DetectorSignalV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destination_constraints: Vec<DestinationConstraintV1>,
    pub tokens: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<TransformationRecordV1>,
}

impl ContextObjectPassportV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        if self.policy_refs.len() > MAX_POLICY_REFS
            || self.risk_signals.len() > MAX_SIGNALS
            || self.lineage.len() > MAX_LINEAGE
        {
            return Err(ValidationError::new("passport exceeds its bounds"));
        }
        if !is_strictly_sorted(&self.destination_constraints)
            || !is_strictly_sorted(&self.policy_refs)
        {
            return Err(ValidationError::new(
                "destination constraints and policy refs must be sorted and unique",
            ));
        }
        for signal in &self.risk_signals {
            signal.validate()?;
        }
        Ok(())
    }

    /// Passport of an object derived from `inputs` by `step` (selection,
    /// summary, dedup, …). Information-flow rule: the result is at least as
    /// restricted as every input — classification is the join, constraints,
    /// policies and risk signals accumulate, trust drops to the least trusted
    /// input. Inputs must share one principal: context never silently crosses
    /// principals.
    pub fn derive(
        inputs: &[&Self],
        step: TransformationRecordV1,
        source: SourceRefV1,
        tokens: u64,
        bytes: u64,
    ) -> Result<Self, ValidationError> {
        let Some(first) = inputs.first() else {
            return Err(ValidationError::new(
                "derivation requires at least one input",
            ));
        };
        if inputs.iter().any(|p| p.principal != first.principal) {
            return Err(ValidationError::new(
                "derivation must not combine context of different principals",
            ));
        }
        let task = first.task.clone();
        let mut classification = first.classification;
        let mut trust = first.trust;
        let mut constraints = BTreeSet::new();
        let mut policies = BTreeSet::new();
        let mut signals = Vec::new();
        let mut lineage = Vec::new();
        for input in inputs {
            classification = classification.join(input.classification);
            trust = trust.min(input.trust);
            constraints.extend(input.destination_constraints.iter().copied());
            policies.extend(input.policy_refs.iter().cloned());
            for signal in &input.risk_signals {
                if !signals.contains(signal) {
                    signals.push(signal.clone());
                }
            }
            lineage.extend(input.lineage.iter().cloned());
        }
        let object = step.output.clone();
        lineage.push(step);
        let derived = Self {
            schema_version: V1_SCHEMA_VERSION,
            object,
            source,
            principal: first.principal.clone(),
            task,
            classification,
            trust,
            policy_refs: policies.into_iter().collect(),
            risk_signals: signals,
            destination_constraints: constraints.into_iter().collect(),
            tokens,
            bytes,
            lineage,
        };
        derived.validate()?;
        Ok(derived)
    }
}

fn is_strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

// ─── Context quality section ────────────────────────────────────────────────

/// How strong the quality evidence is, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityEvidenceTierV1 {
    Mechanism,
    DeterministicQuality,
    RecordedRegression,
    LiveTaskEvaluation,
    ProductionOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityStateV1 {
    Pass,
    Fail,
    Unmeasured,
}

/// Kinds of critical facts a retention probe checks — kinds only, never values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityProbeKindV1 {
    ErrorCode,
    TestResult,
    Status,
    Location,
    Path,
    Hash,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionCountsV1 {
    pub retained: u64,
    pub recoverable: u64,
    pub lost: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityRetentionV1 {
    pub critical: RetentionCountsV1,
    pub important: RetentionCountsV1,
    /// Sorted and unique.
    pub lost_critical_kinds: Vec<QualityProbeKindV1>,
    /// Critical facts could not all be checked; the round can never pass.
    pub critical_unchecked: bool,
    pub truncated: bool,
    /// Secret lines are withheld and never count as recoverable.
    pub secret_lines_withheld: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityRecoveryV1 {
    pub handles_emitted: u64,
    pub handles_verified: u64,
    pub failures: u64,
    pub critical_failures: u64,
}

/// Deterministic context-quality evidence for one round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextQualitySectionV1 {
    pub evidence_tier: QualityEvidenceTierV1,
    pub retention: QualityRetentionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<QualityRecoveryV1>,
    /// Task quality is never measured per round.
    pub task_quality: QualityStateV1,
    pub overall: QualityStateV1,
}

impl ContextQualitySectionV1 {
    /// The overall state the measured dimensions imply.
    #[must_use]
    pub fn derived_overall(&self) -> QualityStateV1 {
        let retention = &self.retention;
        let retention_failed = retention.critical_unchecked
            || retention.critical.lost > 0
            || !retention.lost_critical_kinds.is_empty();
        let recovery_failed = self
            .recovery
            .is_some_and(|recovery| recovery.critical_failures > 0 || recovery.failures > 0);
        let measured = retention.critical.retained
            + retention.critical.recoverable
            + retention.critical.lost
            + retention.important.retained
            + retention.important.recoverable
            + retention.important.lost
            > 0
            || retention.critical_unchecked
            || self.recovery.is_some();
        if retention_failed || recovery_failed {
            QualityStateV1::Fail
        } else if measured {
            QualityStateV1::Pass
        } else {
            QualityStateV1::Unmeasured
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.task_quality != QualityStateV1::Unmeasured {
            return Err(ValidationError::new(
                "task quality is never measured per round",
            ));
        }
        if !is_strictly_sorted(&self.retention.lost_critical_kinds) {
            return Err(ValidationError::new(
                "lost critical kinds must be sorted and unique",
            ));
        }
        if let Some(recovery) = self.recovery
            && (recovery.handles_verified.saturating_add(recovery.failures)
                > recovery.handles_emitted
                || recovery.critical_failures > recovery.failures)
        {
            return Err(ValidationError::new(
                "recovery counts must satisfy verified + failures <= emitted and critical <= failures",
            ));
        }
        if self.overall != self.derived_overall() {
            return Err(ValidationError::new(
                "overall quality must follow from the measured dimensions",
            ));
        }
        Ok(())
    }
}

// ─── Receipt ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcomeV1 {
    Delivered,
    Withheld,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCountsV1 {
    pub inspected: u32,
    pub permitted: u32,
    pub selected: u32,
    pub blocked: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityCountsV1 {
    pub redactions: u32,
    pub blocked_objects: u32,
    pub quarantined_objects: u32,
    pub injection_signals: u32,
    pub incomplete_coverage: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenAccountV1 {
    pub original: u64,
    pub delivered: u64,
}

/// One governed delivery: who asked, for what, what was considered, what was
/// withheld and why, what reached which destination under which policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextDecisionReceiptV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub receipt_id: ProtocolReference,
    pub mode: GatewayModeV1,
    pub principal: PrincipalV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    pub destination: DestinationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyRefV1>,
    pub sources: SourceCountsV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ContextDecisionV1>,
    pub security: SecurityCountsV1,
    pub tokens: TokenAccountV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_context: Option<Sha256Digest>,
    pub outcome: DeliveryOutcomeV1,
    pub duration_us: u64,
    /// Context-quality evidence for this round (retention, recovery). Absent
    /// when nothing was measured; absent fields keep the receipt's digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<ContextQualitySectionV1>,
}

impl ContextDecisionReceiptV1 {
    /// Receipt consistency: counts must describe the recorded decisions, and
    /// only a delivered receipt names the delivered context.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        if let Some(quality) = &self.quality {
            quality.validate()?;
        }
        if self.decisions.len() > MAX_DECISIONS {
            return Err(ValidationError::new("receipt exceeds its decision bound"));
        }
        for decision in &self.decisions {
            decision.validate()?;
        }
        let s = self.sources;
        if s.selected > s.permitted
            || u64::from(s.permitted) + u64::from(s.blocked) > u64::from(s.inspected)
        {
            return Err(ValidationError::new(
                "source counts must satisfy selected <= permitted and permitted + blocked <= inspected",
            ));
        }
        let count = |d: ContextDispositionV1| {
            self.decisions
                .iter()
                .filter(|decision| decision.disposition == d)
                .count()
        };
        if count(ContextDispositionV1::Deny) != self.security.blocked_objects as usize
            || count(ContextDispositionV1::Quarantine) != self.security.quarantined_objects as usize
        {
            return Err(ValidationError::new(
                "security counts must equal the recorded deny/quarantine decisions",
            ));
        }
        match (self.outcome, &self.final_context) {
            (DeliveryOutcomeV1::Delivered, None) => Err(ValidationError::new(
                "a delivered receipt must name the delivered context digest",
            )),
            (DeliveryOutcomeV1::Withheld | DeliveryOutcomeV1::Failed, Some(_)) => Err(
                ValidationError::new("only a delivered receipt may name a context digest"),
            ),
            _ => Ok(()),
        }
    }

    /// Deterministic content identity of the receipt (#498): SHA-256 over the
    /// canonical JSON (sorted keys, no whitespace).
    pub fn canonical_digest(&self) -> Result<Sha256Digest, ValidationError> {
        self.validate()?;
        let value = serde_json::to_value(self)
            .map_err(|e| ValidationError::new(format!("receipt serialization: {e}")))?;
        let bytes = serde_json::to_vec(&value)
            .map_err(|e| ValidationError::new(format!("receipt serialization: {e}")))?;
        sha256_digest(&bytes)
    }
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut digest, "{byte:02x}")
            .map_err(|_| ValidationError::new("gateway digest encoding failed"))?;
    }
    Sha256Digest::new(digest)
}

#[cfg(test)]
#[path = "context_gateway_tests.rs"]
mod tests;
