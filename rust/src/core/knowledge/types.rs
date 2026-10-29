use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::core::memory_boundary::FactPrivacy;
use crate::core::sensitivity::SensitivityLevel;

/// `source_session` marker for facts written by the cognition loop's observation
/// synthesis step (#802). Lets recall distinguish synthesized entity-summaries
/// from user-supplied findings (both are `Observation` archetype).
pub const COGNITION_SYNTHESIS_SOURCE: &str = "cognition-synthesis";

/// `source_session` marker for digest facts written by the cognition loop's
/// cluster-compaction step (#971). A digest replaces a cluster of low-value,
/// mutually-similar facts; the originals are archived (recoverable). Excluded
/// from being compacted again so digests never cannibalize each other.
pub const COMPACTION_DIGEST_SOURCE: &str = "cognition-compaction";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeArchetype {
    Pattern,
    Preference,
    Architecture,
    Gotcha,
    Convention,
    Dependency,
    Workflow,
    Observation,
    Decision,
    #[default]
    Fact,
}

impl KnowledgeArchetype {
    pub fn salience_bonus(&self) -> u32 {
        match self {
            Self::Architecture => 15,
            Self::Decision => 12,
            Self::Gotcha => 14,
            Self::Convention => 8,
            Self::Dependency => 6,
            Self::Pattern => 10,
            Self::Workflow => 7,
            Self::Preference => 5,
            Self::Observation => 3,
            Self::Fact => 0,
        }
    }

    /// Whether this archetype is objective *evidence* (vs. *inference*). Hindsight's
    /// central idea: evidence (the external world, structural facts) should be
    /// treated differently from inference (decisions, preferences, synthesized
    /// observations). Used by archetype-aware decay so evidence persists longer.
    pub fn is_evidence(&self) -> bool {
        matches!(
            self,
            Self::Architecture | Self::Dependency | Self::Convention | Self::Gotcha | Self::Fact
        )
    }

    /// Ebbinghaus stability multiplier (≥ 1.0 slows decay). Structural evidence is
    /// more durable than inference; only applied when `archetype_aware_decay` is on
    /// (default off), so the baseline tuning is unchanged.
    pub fn stability_multiplier(&self) -> f32 {
        match self {
            Self::Architecture => 1.5,
            Self::Dependency => 1.4,
            Self::Convention => 1.3,
            Self::Gotcha => 1.25,
            Self::Fact => 1.2,
            Self::Pattern => 1.1,
            Self::Workflow | Self::Decision | Self::Observation => 1.0,
            Self::Preference => 0.9,
        }
    }

    /// Stable lowercase token used as the Open Knowledge Format (OKF) `type`
    /// field and in `leanctx_archetype`. Single words, so snake_case == the
    /// serde name; kept as an explicit method to avoid a serde round-trip on
    /// the export hot path and to give a symmetric [`Self::from_type_str`].
    pub fn as_type_str(&self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Preference => "preference",
            Self::Architecture => "architecture",
            Self::Gotcha => "gotcha",
            Self::Convention => "convention",
            Self::Dependency => "dependency",
            Self::Workflow => "workflow",
            Self::Observation => "observation",
            Self::Decision => "decision",
            Self::Fact => "fact",
        }
    }

    /// Inverse of [`Self::as_type_str`]. Foreign OKF `type` values that lean-ctx
    /// does not model fall back to [`Self::Fact`] — OKF only mandates `type`, so
    /// an unknown producer type is imported as a plain fact rather than rejected.
    pub fn from_type_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "pattern" => Self::Pattern,
            "preference" => Self::Preference,
            "architecture" => Self::Architecture,
            "gotcha" => Self::Gotcha,
            "convention" => Self::Convention,
            "dependency" => Self::Dependency,
            "workflow" => Self::Workflow,
            "observation" => Self::Observation,
            "decision" => Self::Decision,
            _ => Self::Fact,
        }
    }

    pub fn infer_from_category(category: &str) -> Self {
        match category.to_lowercase().as_str() {
            // `data_model`/`schema` are structural (provider-extracted); they join arch.
            "architecture" | "arch" | "data_model" | "schema" => Self::Architecture,
            "decision" | "decisions" => Self::Decision,
            // bugs/blockers (provider + auto-capture) are pitfalls → Gotcha.
            // An approach tried and given up (K1: reverts, "dead ends") is one too.
            "gotcha" | "gotchas" | "known_bugs" | "known_issues" | "bug" | "bugs" | "blocker"
            | "blockers" | "dead_end" | "dead_ends" | "dead-end" | "dead-ends" => Self::Gotcha,
            "convention" | "conventions" | "style" => Self::Convention,
            "dependency" | "dependencies" | "deps" => Self::Dependency,
            "pattern" | "patterns" => Self::Pattern,
            "workflow" | "workflows" | "procedure" | "procedures" => Self::Workflow,
            "preference" | "preferences" | "pref" => Self::Preference,
            "observation" | "finding" | "findings" | "experience" | "experiences" => {
                Self::Observation
            }
            "solution-decision" | "solution_decision" | "solution-debt" | "solution_debt" => {
                Self::Decision
            }
            _ => Self::Fact,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FidelityScore {
    pub structural: f64,
    pub semantic: f64,
    pub computed_at: DateTime<Utc>,
}

impl Default for FidelityScore {
    fn default() -> Self {
        Self {
            structural: 0.0,
            semantic: 0.0,
            computed_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionDecisionMeta {
    pub kind: SolutionDecisionKind,
    pub chosen: String,
    pub alternatives: Vec<String>,
    pub rationale: Option<String>,
    pub status: SolutionStatus,
    pub scope: Vec<String>,
    pub loc_impact: Option<i32>,
    pub upgrade_condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SolutionDecisionKind {
    StdlibChosen,
    NativeUsed,
    Reuse,
    YagniSkip,
    OneLineSolution,
    DebtAccepted,
}

impl SolutionDecisionKind {
    /// Convert a supported Solution Intelligence category into its decision kind.
    pub fn from_category(category: &str) -> Option<Self> {
        match category.trim().to_ascii_lowercase().as_str() {
            "stdlib" | "standard-library" | "standard_library" => Some(Self::StdlibChosen),
            "native" | "platform" => Some(Self::NativeUsed),
            "reuse" => Some(Self::Reuse),
            "yagni" => Some(Self::YagniSkip),
            "one-line" | "one_line" | "oneline" => Some(Self::OneLineSolution),
            "debt" | "solution-debt" | "solution_debt" => Some(Self::DebtAccepted),
            _ => None,
        }
    }

    /// Stable key used by the persisted Solution Intelligence tracker.
    pub const fn tracker_key(&self) -> &'static str {
        match self {
            Self::StdlibChosen => "stdlib",
            Self::NativeUsed => "native",
            Self::Reuse => "reuse",
            Self::YagniSkip => "yagni",
            Self::OneLineSolution => "oneline",
            Self::DebtAccepted => "debt",
        }
    }
}

impl fmt::Display for SolutionDecisionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::StdlibChosen => "stdlib chosen",
            Self::NativeUsed => "native used",
            Self::Reuse => "reuse",
            Self::YagniSkip => "YAGNI skip",
            Self::OneLineSolution => "one-line solution",
            Self::DebtAccepted => "debt accepted",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SolutionStatus {
    Accepted,
    Deferred,
    Resolved,
}

#[derive(Debug, Clone, Default)]
pub struct KnowledgeIndex {
    pub(super) token_positions: BTreeMap<String, Vec<usize>>,
    pub(super) session_token_positions: BTreeMap<String, Vec<usize>>,
    pub(super) category_positions: BTreeMap<String, Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledge {
    pub project_root: String,
    pub project_hash: String,
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub history: Vec<ConsolidatedInsight>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub judged_pairs: Vec<JudgedPair>,
    /// Ephemeral recall index. It is rebuilt after JSON load and never persisted,
    /// preserving the on-disk knowledge format for existing projects.
    #[serde(skip)]
    pub index: KnowledgeIndex,
    /// Records excluded from this read view. Never serialized or exposed by a
    /// formatter; the checked persistence path retains them without modification.
    #[serde(skip)]
    pub(crate) withheld: Vec<WithheldKnowledge>,
}

/// A complete typed record hidden from this read view and retained only so the
/// checked save path can detect stale views without serializing hidden data.
// Short-lived and few per view: boxing every withheld fact would only add an
// allocation per record to make the rarer variants smaller.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum WithheldKnowledge {
    Fact(KnowledgeFact),
    Pattern(ProjectPattern),
    History(ConsolidatedInsight),
    JudgedPair(JudgedPair),
}

impl std::fmt::Debug for WithheldKnowledge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug output is also a disclosure boundary, including nested derives.
        formatter.write_str("WithheldKnowledge(..)")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "source", rename_all = "snake_case")]
pub enum FactOrigin {
    #[default]
    Unverified,
    Local,
    Provider(Box<crate::core::providers::provenance::ProviderOrigin>),
    Derived(Vec<FactOrigin>),
}

/// Whether a fact rests on a source (V01): `verified` when a provider or a
/// derivation from sourced facts backs it, `assumption` otherwise — what an
/// agent or a person wrote down without evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Verified,
    Assumption,
}

impl FactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Assumption => "assumption",
        }
    }
}

/// Validity class of a fact (AP01 §4, D04): what its reuse is bound to.
///
/// - `content`: bound to source files at a revision. Served only while each
///   source still delivers the bytes the fact was bound to; in another
///   checkout of the same project only when the same files there are
///   byte-identical (`ctx_knowledge::linked`).
/// - `convention`: an explicitly marked rule or decision of the project
///   (naming rule, process decision). Not bound to a revision: served in
///   every checkout linked to the same project (D01), until it is replaced.
///
/// A fact without a class (written before classes existed, or never
/// classified) keeps the behaviour it had: it belongs to its own checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactValidity {
    Content,
    Convention,
}

impl FactValidity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Convention => "convention",
        }
    }

    /// The class a caller names (`validity` of `ctx_knowledge remember`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "content" => Some(Self::Content),
            "convention" => Some(Self::Convention),
            _ => None,
        }
    }

    /// The class an explicit `remember` of `category` gets when its writer
    /// names neither a class nor sources: conventions and decisions are the
    /// project's rules; every other category stays unclassified.
    pub fn for_remembered_category(category: &str) -> Option<Self> {
        matches!(
            category.trim().to_lowercase().as_str(),
            "convention" | "conventions" | "konvention" | "konventionen" | "decision" | "decisions"
        )
        .then_some(Self::Convention)
    }
}

impl FactOrigin {
    /// A source backs this origin: a provider, or a derivation whose every
    /// input is sourced. Local writes and unknown origins are not evidence.
    pub fn is_sourced(&self) -> bool {
        match self {
            Self::Provider(_) => true,
            Self::Derived(inputs) => !inputs.is_empty() && inputs.iter().all(Self::is_sourced),
            Self::Local | Self::Unverified => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgedPair {
    pub key_a: String,
    pub key_b: String,
    pub verdict: String,
    pub judged_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFact {
    #[serde(default)]
    pub origin: FactOrigin,
    pub category: String,
    pub key: String,
    pub value: String,
    pub source_session: String,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub last_confirmed: DateTime<Utc>,
    #[serde(default)]
    pub retrieval_count: u32,
    #[serde(default)]
    pub last_retrieved: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confirmation_count: u32,
    #[serde(default)]
    pub feedback_up: u32,
    #[serde(default)]
    pub feedback_down: u32,
    #[serde(default)]
    pub last_feedback: Option<DateTime<Utc>>,
    #[serde(default)]
    pub privacy: FactPrivacy,
    /// Per-item sensitivity classification (#212). Defaults to `Public`; set at
    /// creation from content and enforced by the policy floor at injection time.
    #[serde(default)]
    pub sensitivity: SensitivityLevel,
    #[serde(default)]
    pub imported_from: Option<String>,
    #[serde(default)]
    pub archetype: KnowledgeArchetype,
    #[serde(default)]
    pub fidelity: Option<FidelityScore>,
    #[serde(default)]
    pub revision_count: u32,
    /// The recorded status (V01). Absent on facts written before it existed
    /// and on facts nobody classified; [`KnowledgeFact::status`] then
    /// derives it from the origin. Omitted when absent (byte-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FactStatus>,
    /// Validity class (AP01 §4). Absent on facts written before it existed
    /// and on unclassified facts, which keep today's behaviour. Omitted when
    /// absent (byte-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validity: Option<FactValidity>,
    /// Code anchors (J2, C0 1.9 `CodeAnchorV1`): the code this fact is
    /// about, so a change there makes it *to verify*. Omitted when empty
    /// (byte-compatible).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<super::anchors::CodeAnchor>,
    /// Review state (C0 1.9 fact states): set when what the fact rests on
    /// moved under it — an anchor that no longer holds, a removed import
    /// source, a contradiction. Orthogonal to [`FactStatus`] (evidence): a
    /// verified fact can be to verify. Omitted when absent (byte-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<FactReview>,
    /// What the Hive capture gate reads (v4 V1): node type, procedure
    /// steps and commands, references, evidence. Every field is omitted
    /// when empty (byte-compatible).
    #[serde(flatten)]
    pub capture: FactCapture,
}

/// Structured fields of a fact for the Hive capture gate (SPEC-ENGINE
/// §3.1.3), written by `ctx_knowledge remember type= steps= commands=
/// refs= evidence=`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactCapture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<NodeType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ProcedureCommand>,
    /// `file:`, `sym:`, `test:`, `commit:`, `node:` or `https://` references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceLevel>,
    /// When a `remember` last changed these fields (e.g. a second
    /// `remember` of the same statement that adds `refs`): the capture gate
    /// considers the fact again although it was created before its
    /// watermark. `None` for facts whose fields no `remember` set.
    #[serde(
        default,
        rename = "capture_changed_at",
        skip_serializing_if = "Option::is_none"
    )]
    pub changed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Node types a fact can be captured as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Fact,
    Decision,
    Gotcha,
    Procedure,
    Finding,
}

impl NodeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Gotcha => "gotcha",
            Self::Procedure => "procedure",
            Self::Finding => "finding",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fact" => Self::Fact,
            "decision" => Self::Decision,
            "gotcha" => Self::Gotcha,
            "procedure" => Self::Procedure,
            "finding" => Self::Finding,
            _ => return None,
        })
    }
}

/// How a captured claim is backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    Observed,
    Stated,
    Imported,
}

impl EvidenceLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Stated => "stated",
            Self::Imported => "imported",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "observed" => Self::Observed,
            "stated" => Self::Stated,
            "imported" => Self::Imported,
            _ => return None,
        })
    }
}

/// A command of a procedure: data only, never executed by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureCommand {
    pub shell: String,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_hint: Option<String>,
}

/// Why a fact needs a person's look again (C0 1.9 fact states).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactReview {
    pub state: FactReviewState,
    pub reason: FactReviewReason,
    pub since: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactReviewState {
    ToVerify,
    Contradicted,
}

/// The contract's reasons; `source_removed` is an imported fact whose
/// source section is gone (K1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactReviewReason {
    AnchorChanged,
    AnchorMoved,
    AnchorMissing,
    Contradiction,
    SourceRemoved,
}

impl FactReviewReason {
    /// The reason of an anchor state that does not hold (`None` when current).
    pub fn of_anchor(state: &super::anchors::AnchorState) -> Option<Self> {
        match state {
            super::anchors::AnchorState::Current => None,
            super::anchors::AnchorState::Changed => Some(Self::AnchorChanged),
            super::anchors::AnchorState::Moved { .. } => Some(Self::AnchorMoved),
            super::anchors::AnchorState::Missing => Some(Self::AnchorMissing),
        }
    }
}

impl KnowledgeFact {
    /// The fact's status: as recorded, else `verified` only when its origin
    /// names a source, else `assumption`.
    pub fn status(&self) -> FactStatus {
        self.status.unwrap_or(if self.origin.is_sourced() {
            FactStatus::Verified
        } else {
            FactStatus::Assumption
        })
    }

    /// Mark the fact for review; an existing mark keeps its `since`.
    pub fn mark_review(
        &mut self,
        state: FactReviewState,
        reason: FactReviewReason,
        now: DateTime<Utc>,
    ) {
        match &mut self.review {
            Some(review) if review.state == state && review.reason == reason => {}
            slot => {
                *slot = Some(FactReview {
                    state,
                    reason,
                    since: now,
                });
            }
        }
    }

    /// A person or a fresh write confirmed the fact again.
    pub fn clear_review(&mut self) {
        self.review = None;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub existing_key: String,
    pub existing_value: String,
    pub new_value: String,
    pub category: String,
    pub severity: ContradictionSeverity,
    pub resolution: String,
}

/// Outcome of a write-time admission decision (#970), returned by
/// `ProjectKnowledge::remember_admitted`. It is the difference between "the agent
/// asked to store X" and "what lean-ctx actually persisted".
#[derive(Debug, Clone)]
pub enum AdmissionResult {
    /// Inserted / confirmed / superseded the normal way; carries any
    /// contradiction the write resolved.
    Stored(Option<Contradiction>),
    /// The value was a near-duplicate of an existing same-category fact under a
    /// different key and was merged into it (a confirmation bump) instead of
    /// growing the store. Carries the target's identity, its new confirmation
    /// count, and the value finally kept.
    Merged {
        category: String,
        key: String,
        confirmations: u32,
        value: String,
    },
    /// Content salience was below the configured floor; nothing was written.
    RejectedLowSalience { salience: u32, floor: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContradictionSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPattern {
    pub pattern_type: String,
    pub description: String,
    pub examples: Vec<String>,
    pub source_session: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedInsight {
    pub summary: String,
    pub from_sessions: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod archetype_tests {
    use super::*;

    #[test]
    fn infer_archetype_from_category() {
        assert_eq!(
            KnowledgeArchetype::infer_from_category("architecture"),
            KnowledgeArchetype::Architecture
        );
        assert_eq!(
            KnowledgeArchetype::infer_from_category("gotcha"),
            KnowledgeArchetype::Gotcha
        );
        for (category, archetype) in [
            ("dead_end", KnowledgeArchetype::Gotcha),
            ("procedure", KnowledgeArchetype::Workflow),
            ("experience", KnowledgeArchetype::Observation),
        ] {
            assert_eq!(KnowledgeArchetype::infer_from_category(category), archetype);
        }
        assert_eq!(
            KnowledgeArchetype::infer_from_category("random"),
            KnowledgeArchetype::Fact
        );
    }

    #[test]
    fn solution_categories_infer_decision_archetype() {
        for category in [
            "solution-decision",
            "solution_decision",
            "solution-debt",
            "solution_debt",
        ] {
            assert_eq!(
                KnowledgeArchetype::infer_from_category(category),
                KnowledgeArchetype::Decision,
                "category {category}"
            );
        }
    }

    #[test]
    fn salience_bonus_ordering() {
        assert!(
            KnowledgeArchetype::Architecture.salience_bonus()
                > KnowledgeArchetype::Fact.salience_bonus()
        );
        assert!(
            KnowledgeArchetype::Gotcha.salience_bonus()
                > KnowledgeArchetype::Convention.salience_bonus()
        );
    }

    #[test]
    fn default_archetype_is_fact() {
        assert_eq!(KnowledgeArchetype::default(), KnowledgeArchetype::Fact);
    }

    #[test]
    fn fidelity_structural_computation() {
        let fact = KnowledgeFact {
            anchors: Vec::new(),
            review: None,
            capture: Default::default(),
            origin: crate::core::knowledge::FactOrigin::Local,
            category: "test".into(),
            key: "k".into(),
            value: "v".into(),
            source_session: "sess1".into(),
            confidence: 0.9,
            created_at: Utc::now(),
            last_confirmed: Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 3,
            feedback_up: 2,
            feedback_down: 0,
            last_feedback: None,
            privacy: FactPrivacy::default(),
            sensitivity: SensitivityLevel::default(),
            imported_from: None,
            archetype: KnowledgeArchetype::default(),
            fidelity: None,
            revision_count: 0,
            status: None,
            validity: None,
        };
        let fidelity = fact.compute_structural_fidelity();
        assert!(fidelity >= 0.8);
    }

    #[test]
    fn backward_compatible_deserialization() {
        let json = r#"{
            "category": "test",
            "key": "k",
            "value": "v",
            "source_session": "s",
            "confidence": 0.8,
            "created_at": "2024-01-01T00:00:00Z",
            "last_confirmed": "2024-01-01T00:00:00Z"
        }"#;
        let fact: KnowledgeFact = serde_json::from_str(json).unwrap();
        assert_eq!(fact.archetype, KnowledgeArchetype::Fact);
        assert!(fact.fidelity.is_none());
    }
}
