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
            "gotcha" | "gotchas" | "known_bugs" | "known_issues" | "bug" | "bugs" | "blocker"
            | "blockers" => Self::Gotcha,
            "convention" | "conventions" | "style" => Self::Convention,
            "dependency" | "dependencies" | "deps" => Self::Dependency,
            "pattern" | "patterns" => Self::Pattern,
            "workflow" | "workflows" => Self::Workflow,
            "preference" | "preferences" | "pref" => Self::Preference,
            "observation" | "finding" | "findings" => Self::Observation,
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
    pub(crate) withheld: Vec<KnowledgeFact>,
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
