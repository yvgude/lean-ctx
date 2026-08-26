//! Frozen compatibility contracts for historical knowledge routing.
//!
//! These Product-shaped selection types remain wire-compatible for existing
//! control-plane consumers, but they are not Engine mechanism authority. New
//! Engine integrations must use factual source/view/search/recovery/capability
//! contracts instead of extending this module.
//!
//! Removal gate: a protocol-major release after `ControlPlaneRequest` no longer
//! embeds `ContextBundleV1` and one complete compatibility window has elapsed.

use crate::common::{ValidationError, deserialize_milliunit, validate_milliunit};
use serde::{Deserialize, Serialize};

/// Relative acquisition cost of a knowledge source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    #[default]
    Negligible,
    Low,
    Medium,
    High,
}

/// Operations supported by a knowledge source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCapabilities {
    pub search: bool,
    pub exact_get: bool,
    pub delta_sync: bool,
    pub live_query: bool,
    pub graph_edges: bool,
}

/// Describes one source available to the knowledge router.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSourceManifestV1 {
    pub source_id: String,
    pub display_name: String,
    pub kinds: Vec<String>,
    pub capabilities: SourceCapabilities,
    pub freshness_typical_ms: u64,
    pub cost_class: CostClass,
}

impl KnowledgeSourceManifestV1 {
    /// Validate the manifest's structural invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Ranked, content-addressed context candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCandidateV1 {
    pub candidate_id: String,
    pub task_id: String,
    pub source_id: String,
    pub kind: String,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub relevance_milli: u16,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub confidence_milli: u16,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub freshness_milli: u16,
    pub estimated_tokens: u64,
    pub content_hash: String,
}

impl ContextCandidateV1 {
    /// Validate candidate ranking and freshness bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_milliunit(self.relevance_milli, "relevance_milli")?;
        validate_milliunit(self.confidence_milli, "confidence_milli")?;
        validate_milliunit(self.freshness_milli, "freshness_milli")
    }
}

/// Context bundle selected for a task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBundleV1 {
    pub bundle_id: String,
    pub task_id: String,
    pub candidates: Vec<String>,
    pub total_tokens: u64,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub coverage_milli: u16,
    pub strategy: String,
}

impl ContextBundleV1 {
    /// Validate bundle coverage bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_milliunit(self.coverage_milli, "coverage_milli")
    }
}

/// Accounting receipt for a context-routing decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextReceiptV1 {
    pub receipt_id: String,
    pub task_id: String,
    pub bundle_id: String,
    pub budget_tokens: u64,
    pub materialized_tokens: u64,
    pub candidates_considered: u32,
    pub candidates_selected: u32,
    pub sources_used: Vec<String>,
    pub degraded_reasons: Vec<String>,
}

impl ContextReceiptV1 {
    /// Validate candidate and token accounting invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.materialized_tokens > self.budget_tokens {
            return Err(ValidationError::new(
                "materialized_tokens exceeds budget_tokens",
            ));
        }
        if self.candidates_selected > self.candidates_considered {
            return Err(ValidationError::new(
                "candidates_selected exceeds candidates_considered",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn manifest() -> KnowledgeSourceManifestV1 {
        KnowledgeSourceManifestV1 {
            source_id: "docs".to_owned(),
            display_name: "Documentation".to_owned(),
            kinds: vec!["reference".to_owned()],
            capabilities: SourceCapabilities {
                search: true,
                exact_get: true,
                delta_sync: false,
                live_query: false,
                graph_edges: true,
            },
            freshness_typical_ms: 60_000,
            cost_class: CostClass::Low,
        }
    }

    fn candidate() -> ContextCandidateV1 {
        ContextCandidateV1 {
            candidate_id: "candidate-1".to_owned(),
            task_id: "task-1".to_owned(),
            source_id: "docs".to_owned(),
            kind: "reference".to_owned(),
            relevance_milli: 950,
            confidence_milli: 900,
            freshness_milli: 800,
            estimated_tokens: 500,
            content_hash: "sha256:content".to_owned(),
        }
    }

    fn bundle() -> ContextBundleV1 {
        ContextBundleV1 {
            bundle_id: "bundle-1".to_owned(),
            task_id: "task-1".to_owned(),
            candidates: vec!["candidate-1".to_owned()],
            total_tokens: 500,
            coverage_milli: 850,
            strategy: "relevance_first".to_owned(),
        }
    }

    fn receipt() -> ContextReceiptV1 {
        ContextReceiptV1 {
            receipt_id: "receipt-1".to_owned(),
            task_id: "task-1".to_owned(),
            bundle_id: "bundle-1".to_owned(),
            budget_tokens: 1_000,
            materialized_tokens: 500,
            candidates_considered: 4,
            candidates_selected: 1,
            sources_used: vec!["docs".to_owned()],
            degraded_reasons: Vec::new(),
        }
    }

    #[test]
    fn serialization_round_trip() {
        let values = (manifest(), candidate(), bundle(), receipt());
        let json = serde_json::to_string(&values).expect("routing values should serialize");
        let decoded: (
            KnowledgeSourceManifestV1,
            ContextCandidateV1,
            ContextBundleV1,
            ContextReceiptV1,
        ) = serde_json::from_str(&json).expect("routing values should deserialize");
        assert_eq!(values, decoded);
        values.0.validate().expect("manifest should be valid");
        values.1.validate().expect("candidate should be valid");
        values.2.validate().expect("bundle should be valid");
        values.3.validate().expect("receipt should be valid");
    }

    #[test]
    fn validation_rejects_invalid_milliunits_and_accounting() {
        let mut candidate = candidate();
        candidate.relevance_milli = 1001;
        assert!(candidate.validate().is_err());

        let mut bundle = bundle();
        bundle.coverage_milli = 1001;
        assert!(bundle.validate().is_err());

        let mut receipt = receipt();
        receipt.materialized_tokens = 1_001;
        assert!(receipt.validate().is_err());
        receipt.materialized_tokens = 500;
        receipt.candidates_selected = 5;
        assert!(receipt.validate().is_err());

        let invalid_json = r#"{
            "candidate_id": "candidate-1",
            "task_id": "task-1",
            "source_id": "docs",
            "kind": "reference",
            "relevance_milli": 1001,
            "confidence_milli": 900,
            "freshness_milli": 800,
            "estimated_tokens": 500,
            "content_hash": "sha256:content"
        }"#;
        assert!(serde_json::from_str::<ContextCandidateV1>(invalid_json).is_err());
    }

    #[test]
    fn defaults_are_stable_and_valid() {
        let values = (
            KnowledgeSourceManifestV1::default(),
            ContextCandidateV1::default(),
            ContextBundleV1::default(),
            ContextReceiptV1::default(),
        );
        assert_eq!(values.0, KnowledgeSourceManifestV1::default());
        assert_eq!(values.1, ContextCandidateV1::default());
        assert_eq!(values.2, ContextBundleV1::default());
        assert_eq!(values.3, ContextReceiptV1::default());
        values
            .0
            .validate()
            .expect("default manifest should be valid");
        values
            .1
            .validate()
            .expect("default candidate should be valid");
        values.2.validate().expect("default bundle should be valid");
        values
            .3
            .validate()
            .expect("default receipt should be valid");
        assert_eq!(CostClass::default(), CostClass::Negligible);
    }

    #[test]
    fn frozen_candidate_schema_has_no_product_expansion() {
        let value = serde_json::to_value(candidate()).expect("candidate should serialize");
        let keys = value
            .as_object()
            .expect("candidate should be an object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = [
            "candidate_id",
            "confidence_milli",
            "content_hash",
            "estimated_tokens",
            "freshness_milli",
            "kind",
            "relevance_milli",
            "source_id",
            "task_id",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(keys, expected);
    }
}
