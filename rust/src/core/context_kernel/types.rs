// SPDX-License-Identifier: Apache-2.0

//! Shared types for the Context Control Kernel.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::core::context_field::{ContextItemId, Provenance, TokenBudget, ViewCosts};

/// Universal context candidate supplied by any context store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextObjectV1 {
    pub id: ContextItemId,
    pub kind: ContextObjectKind,
    pub source: String,
    pub content_ref: String,
    pub title: String,
    pub content: Option<String>,
    pub freshness: Freshness,
    pub confidence: f32,
    pub sensitivity: SensitivityLevel,
    pub token_estimate: usize,
    pub view_costs: ViewCosts,
    pub provenance: Provenance,
    pub semantic_fingerprint: Option<String>,
    pub metadata: HashMap<String, String>,
}

impl Default for ContextObjectV1 {
    fn default() -> Self {
        Self {
            id: ContextItemId(String::new()),
            kind: ContextObjectKind::Fact,
            source: String::new(),
            content_ref: String::new(),
            title: String::new(),
            content: None,
            freshness: Freshness::default(),
            confidence: 0.0,
            sensitivity: SensitivityLevel::Internal,
            token_estimate: 0,
            view_costs: ViewCosts::default(),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }
}

impl ContextObjectV1 {
    /// Creates a materialized knowledge fact candidate.
    pub fn new_fact(
        id: ContextItemId,
        key: impl Into<String>,
        value: impl Into<String>,
        confidence: f32,
    ) -> Self {
        let key = key.into();
        let value = value.into();
        let token_estimate = value.split_whitespace().count();

        Self {
            content_ref: id.as_str().to_owned(),
            id,
            kind: ContextObjectKind::Fact,
            source: "knowledge".to_owned(),
            title: key,
            content: Some(value),
            freshness: Freshness::default(),
            confidence: confidence.clamp(0.0, 1.0),
            sensitivity: SensitivityLevel::Internal,
            token_estimate,
            view_costs: ViewCosts::from_full_tokens(token_estimate),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }

    /// Creates a lazy file candidate for a source path.
    pub fn new_file(id: ContextItemId, path: impl Into<String>, tokens: usize) -> Self {
        let path = path.into();

        Self {
            content_ref: path.clone(),
            id,
            kind: ContextObjectKind::File,
            source: "file".to_owned(),
            title: path,
            content: None,
            freshness: Freshness::default(),
            confidence: 1.0,
            sensitivity: SensitivityLevel::Internal,
            token_estimate: tokens,
            view_costs: ViewCosts::from_full_tokens(tokens),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }

    /// Creates a materialized episodic-memory candidate.
    pub fn new_episode(
        id: ContextItemId,
        summary: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        let summary = summary.into();
        let outcome = outcome.into();
        let token_estimate =
            summary.split_whitespace().count() + outcome.split_whitespace().count();

        Self {
            content_ref: id.as_str().to_owned(),
            id,
            kind: ContextObjectKind::Episode,
            source: "episodic".to_owned(),
            title: summary,
            content: Some(outcome),
            freshness: Freshness::default(),
            confidence: 1.0,
            sensitivity: SensitivityLevel::Internal,
            token_estimate,
            view_costs: ViewCosts::from_full_tokens(token_estimate),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }
}

/// Kinds of context objects that can be retrieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextObjectKind {
    File,
    Fact,
    Episode,
    Procedure,
    SessionItem,
    SearchChunk,
}

/// Freshness metadata associated with a context object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Freshness {
    pub created_at: String,
    pub ttl_secs: Option<u64>,
    pub stale: bool,
}

/// Access sensitivity assigned to a context object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevel {
    Public,
    Internal,
    Confidential,
    Restricted,
}

impl From<SensitivityLevel> for lean_ctx_protocol::context_gateway::ClassificationV1 {
    fn from(level: SensitivityLevel) -> Self {
        match level {
            SensitivityLevel::Public => Self::Public,
            SensitivityLevel::Internal => Self::Internal,
            SensitivityLevel::Confidential => Self::Confidential,
            SensitivityLevel::Restricted => Self::Restricted,
        }
    }
}

/// Side-effect policy for candidate enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectPolicy {
    ReadOnly,
    MutatesStats,
}

/// Query context passed to candidate providers.
#[derive(Debug, Clone)]
pub struct RetrievalContext {
    pub query: String,
    pub task: Option<String>,
    pub project_root: String,
    pub budget: TokenBudget,
    pub max_candidates: usize,
}

/// Common interface for context stores that enumerate candidates.
pub trait CandidateProvider: Send + Sync {
    /// Returns the provider's stable identifier.
    fn provider_id(&self) -> &str;

    /// Enumerates context candidates for a retrieval request.
    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1>;

    /// Declares whether enumeration mutates provider statistics.
    fn side_effect_policy(&self) -> SideEffectPolicy;
}

/// Kernel-owned binding of one object to its registered provider and source metadata.
///
/// This is intentionally internal: public V1 projections continue to expose only
/// their existing alias-keyed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ContextOriginV1 {
    pub provider_id: String,
    pub source: String,
    pub content_ref: String,
    pub sensitivity: SensitivityLevel,
    pub provenance: Provenance,
    /// True only after this enumerated origin passes kernel admission checks.
    #[serde(default)]
    pub admitted: bool,
}

/// A compiled plan for context delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPlanV1 {
    pub plan_id: String,
    pub intent: String,
    pub budget: PlanBudget,
    pub selected: Vec<PlanEntry>,
    pub excluded: Vec<ExcludedEntry>,
    pub deferred: Vec<DeferredEntry>,
    pub provider_stats: HashMap<String, ProviderStat>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) origins: BTreeMap<String, Vec<ContextOriginV1>>,
}

impl ContextPlanV1 {
    /// Creates an empty plan that preserves the supplied token budget.
    pub fn empty(intent: &str, budget: TokenBudget) -> Self {
        Self {
            plan_id: format!("plan:{intent}"),
            intent: intent.to_owned(),
            budget: PlanBudget {
                total_tokens: budget.total,
                used_tokens: budget.used,
                remaining_tokens: budget.remaining(),
            },
            selected: Vec::new(),
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
            origins: BTreeMap::new(),
        }
    }
}

/// A selected object and delivery rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub object_id: String,
    pub provider: String,
    pub view: String,
    pub tokens: usize,
    pub phi: f64,
    pub reason: String,
}

/// An object rejected during planning and its rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedEntry {
    pub object_id: String,
    pub provider: String,
    pub reason: String,
}

/// An object retained for later delivery and its rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredEntry {
    pub object_id: String,
    pub provider: String,
    pub reason: String,
}

/// Token accounting for a context plan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanBudget {
    pub total_tokens: usize,
    pub used_tokens: usize,
    pub remaining_tokens: usize,
}

/// Per-provider candidate and token accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderStat {
    pub candidates_offered: usize,
    pub candidates_selected: usize,
    pub tokens_used: usize,
}

/// Feedback artifact created after context delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReceiptV1 {
    pub receipt_id: String,
    pub plan_id: String,
    /// Task lineage identifier when receipt originated at MCP ingress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub delivered_tokens: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub outcome: ReceiptOutcome,
    pub quality_signals: Vec<QualitySignal>,
    pub feedback_attribution: HashMap<String, f64>,
}

impl ContextReceiptV1 {
    /// Creates an initial receipt for a completed plan delivery.
    pub fn from_plan(plan: &ContextPlanV1, delivered: usize) -> Self {
        Self {
            receipt_id: format!("receipt:{}", plan.plan_id),
            plan_id: plan.plan_id.clone(),
            task_id: None,
            delivered_tokens: delivered,
            cache_hits: 0,
            cache_misses: 0,
            outcome: ReceiptOutcome::Unknown,
            quality_signals: Vec::new(),
            feedback_attribution: HashMap::new(),
        }
    }
}

/// Result classification for context delivery feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    Accepted,
    Rejected,
    Partial,
    Unknown,
}

/// A numerical quality observation associated with a receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualitySignal {
    pub signal_type: String,
    pub value: f64,
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn new_fact_creates_materialized_knowledge_object() {
        let object = ContextObjectV1::new_fact(
            ContextItemId::from_knowledge("decision", "architecture"),
            "architecture",
            "Use a context kernel.",
            1.5,
        );

        assert_eq!(object.kind, ContextObjectKind::Fact);
        assert_eq!(object.source, "knowledge");
        assert_eq!(object.content.as_deref(), Some("Use a context kernel."));
        assert_eq!(object.confidence, 1.0);
    }

    #[test]
    fn new_file_creates_lazy_file_object() {
        let object =
            ContextObjectV1::new_file(ContextItemId::from_file("src/lib.rs"), "src/lib.rs", 250);

        assert_eq!(object.kind, ContextObjectKind::File);
        assert_eq!(object.content, None);
        assert_eq!(object.token_estimate, 250);
        assert_eq!(
            object
                .view_costs
                .get(&crate::core::context_field::ViewKind::Full),
            250
        );
    }

    #[test]
    fn empty_plan_preserves_budget_accounting() {
        let plan = ContextPlanV1::empty(
            "retrieve facts",
            TokenBudget {
                total: 500,
                used: 125,
            },
        );

        assert_eq!(plan.budget.total_tokens, 500);
        assert_eq!(plan.budget.used_tokens, 125);
        assert_eq!(plan.budget.remaining_tokens, 375);
        assert!(plan.selected.is_empty());
    }

    #[test]
    fn receipt_references_source_plan() {
        let plan = ContextPlanV1::empty(
            "retrieve facts",
            TokenBudget {
                total: 500,
                used: 0,
            },
        );
        let receipt = ContextReceiptV1::from_plan(&plan, 120);

        assert_eq!(receipt.plan_id, plan.plan_id);
        assert_eq!(receipt.delivered_tokens, 120);
        assert_eq!(receipt.outcome, ReceiptOutcome::Unknown);
    }

    #[test]
    fn sensitivity_level_serialization_roundtrips() {
        let serialized = serde_json::to_string(&SensitivityLevel::Confidential)
            .expect("serialize sensitivity level");
        let decoded: SensitivityLevel =
            serde_json::from_str(&serialized).expect("deserialize sensitivity level");

        assert_eq!(decoded, SensitivityLevel::Confidential);
    }

    #[test]
    fn context_object_kind_has_all_variants() {
        let kinds = [
            ContextObjectKind::File,
            ContextObjectKind::Fact,
            ContextObjectKind::Episode,
            ContextObjectKind::Procedure,
            ContextObjectKind::SessionItem,
            ContextObjectKind::SearchChunk,
        ];

        assert_eq!(kinds.len(), 6);
    }
}
