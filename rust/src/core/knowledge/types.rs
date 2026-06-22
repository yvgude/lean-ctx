use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::memory_boundary::FactPrivacy;
use crate::core::sensitivity::SensitivityLevel;

/// `source_session` marker for facts written by the cognition loop's observation
/// synthesis step (#802). Lets recall distinguish synthesized entity-summaries
/// from user-supplied findings (both are `Observation` archetype).
pub const COGNITION_SYNTHESIS_SOURCE: &str = "cognition-synthesis";

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
pub struct ProjectKnowledge {
    pub project_root: String,
    pub project_hash: String,
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub history: Vec<ConsolidatedInsight>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub judged_pairs: Vec<JudgedPair>,
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
