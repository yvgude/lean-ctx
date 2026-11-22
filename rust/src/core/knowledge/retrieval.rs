//! Retrieval contract and the local reference implementation.

use super::local_store::LocalKnowledgeStore;
use super::query::KnowledgeQuery;
use super::snapshot::KnowledgeSnapshot;
use lean_ctx_protocol::knowledge::{ClassificationLevel, KnowledgeObjectV1};
use serde::{Deserialize, Serialize};

/// Policy inputs understood by the portable retrieval surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalPolicy {
    /// Maximum number of objects returned by a retrieval.
    pub max_items: usize,
    /// Empty means every classification is allowed.
    #[serde(default)]
    pub allowed_classifications: Vec<ClassificationLevel>,
    /// If true, superseded and out-of-window objects are excluded.
    pub required_validity: bool,
    /// Reserved portable ranking hint; enterprise implementations may use it.
    pub recency_weight: f32,
}

impl Default for RetrievalPolicy {
    fn default() -> Self {
        Self {
            max_items: usize::MAX,
            allowed_classifications: Vec::new(),
            required_validity: true,
            recency_weight: 0.0,
        }
    }
}

/// Detailed retrieval accounting suitable for a task-local snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub items: Vec<KnowledgeObjectV1>,
    pub total_available: usize,
    pub budget_used: usize,
    pub excluded_reasons: Vec<String>,
}

/// Portable retrieval interface implemented by local and enterprise adapters.
pub trait KnowledgeRetriever {
    /// Retrieve at most `budget` valid objects for a task.
    fn retrieve_for_task(
        &self,
        task_id: &str,
        budget: usize,
        policy: &RetrievalPolicy,
    ) -> Vec<KnowledgeObjectV1>;
}

impl LocalKnowledgeStore {
    /// Retrieve with accounting details from the local reference store.
    pub fn retrieve_for_task_with_result(
        &self,
        _task_id: &str,
        budget: usize,
        policy: &RetrievalPolicy,
    ) -> RetrievalResult {
        let now_query = if policy.required_validity {
            Some(KnowledgeQuery::valid_at(lean_ctx_time::UtcDateTime::now()))
        } else {
            None
        };
        let cap = budget.min(policy.max_items);
        let mut excluded_reasons = Vec::new();
        let mut eligible = Vec::new();

        for object in self.values() {
            if let Some(query) = &now_query {
                if !query.matches(object) {
                    excluded_reasons.push(format!("{}:invalid_or_superseded", object.object_id()));
                    continue;
                }
            }
            if !policy.allowed_classifications.is_empty()
                && object.classification.as_ref().is_none_or(|classification| {
                    !policy
                        .allowed_classifications
                        .contains(&classification.level)
                })
            {
                excluded_reasons.push(format!("{}:classification", object.object_id()));
                continue;
            }
            eligible.push(object.clone());
        }

        let total_available = eligible.len();
        if eligible.len() > cap {
            excluded_reasons.push(format!("budget:{cap}"));
            eligible.truncate(cap);
        }

        // The local reference implementation deliberately keeps stable store order;
        // `recency_weight` is available to richer adapters without changing this MVP.
        let _recency_weight = policy.recency_weight;
        RetrievalResult {
            budget_used: eligible.len(),
            items: eligible,
            total_available,
            excluded_reasons,
        }
    }

    /// Produce a receipt-ready proof of the objects selected for a task.
    pub fn snapshot_for_task(
        &self,
        task_id: &str,
        budget: usize,
        policy: &RetrievalPolicy,
        policy_version: &str,
    ) -> KnowledgeSnapshot {
        let result = self.retrieve_for_task_with_result(task_id, budget, policy);
        KnowledgeSnapshot::from_items(task_id, policy_version, &result.items)
    }
}

impl KnowledgeRetriever for LocalKnowledgeStore {
    fn retrieve_for_task(
        &self,
        task_id: &str,
        budget: usize,
        policy: &RetrievalPolicy,
    ) -> Vec<KnowledgeObjectV1> {
        self.retrieve_for_task_with_result(task_id, budget, policy)
            .items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::store::KnowledgeStore;
    use lean_ctx_protocol::knowledge::{
        ClassificationLevel, DataClassification, KnowledgeSourceType, SourceReference,
        ValidityWindow,
    };
    use lean_ctx_time::{Duration, UtcDateTime};

    fn object(id: &str, level: ClassificationLevel, valid: bool) -> KnowledgeObjectV1 {
        let now = UtcDateTime::now();
        KnowledgeObjectV1 {
            schema_version: 1,
            source_ref: Some(SourceReference {
                source_type: "test".to_owned(),
                uri: "memory://knowledge".to_owned(),
                commit_sha: None,
                access_timestamp: now.to_rfc3339(),
            }),
            source_type: KnowledgeSourceType::Other,
            authority: None,
            owner: "test".to_owned(),
            classification: Some(DataClassification {
                level,
                retention_days: None,
            }),
            validity: valid.then(|| ValidityWindow {
                valid_from: (now - Duration::minutes(1)).to_rfc3339(),
                valid_until: Some((now + Duration::minutes(1)).to_rfc3339()),
                superseded_by: None,
            }),
            supersedes: None,
            content_hash: id.to_owned(),
            evidence_digest: format!("digest:{id}"),
            policy_ref: "policy:test".to_owned(),
            evidence_refs: Vec::new(),
            extra: Default::default(),
        }
    }

    #[test]
    fn local_retrieval_applies_policy_and_budget() {
        let mut store = LocalKnowledgeStore::new();
        store
            .put(object("public", ClassificationLevel::Public, true))
            .expect("put public");
        store
            .put(object("internal", ClassificationLevel::Internal, true))
            .expect("put internal");
        store
            .put(object("expired", ClassificationLevel::Public, false))
            .expect("put expired");

        let policy = RetrievalPolicy {
            max_items: 1,
            allowed_classifications: vec![ClassificationLevel::Public],
            required_validity: true,
            recency_weight: 0.5,
        };
        let result = store.retrieve_for_task_with_result("task-1", 4, &policy);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].object_id(), "public");
        assert_eq!(result.total_available, 1);
        assert_eq!(result.budget_used, 1);
        assert!(
            result
                .excluded_reasons
                .iter()
                .any(|reason| reason.contains("invalid"))
        );
    }
}
