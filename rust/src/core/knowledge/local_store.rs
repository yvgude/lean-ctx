//! In-memory Knowledge Hub store for local sessions and tests.

use super::query::KnowledgeQuery;
use super::store::{KnowledgeStore, KnowledgeStoreError, object_id, validate_object};
use super::supersession::mark_superseded;
use lean_ctx_protocol::KnowledgeObjectV1;
use std::collections::BTreeMap;

/// Deterministic in-memory implementation of [`KnowledgeStore`].
#[derive(Debug, Clone, Default)]
pub struct LocalKnowledgeStore {
    objects: BTreeMap<String, KnowledgeObjectV1>,
}

impl LocalKnowledgeStore {
    /// Create an empty local store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of objects currently held by the store.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the store contains no objects.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Return all objects in stable object-id order.
    pub fn all(&self) -> Vec<KnowledgeObjectV1> {
        self.objects.values().cloned().collect()
    }

    /// Borrow all objects in stable object-id order.
    pub fn values(&self) -> impl Iterator<Item = &KnowledgeObjectV1> {
        self.objects.values()
    }

    /// Fetch an object without cloning it.
    pub fn get_ref(&self, object_id: &str) -> Option<&KnowledgeObjectV1> {
        self.objects.get(object_id)
    }
}

impl KnowledgeStore for LocalKnowledgeStore {
    fn get(&self, object_id: &str) -> Option<KnowledgeObjectV1> {
        self.objects.get(object_id).cloned()
    }

    fn put(&mut self, object: KnowledgeObjectV1) -> Result<(), KnowledgeStoreError> {
        validate_object(&object)?;
        self.objects.insert(object_id(&object).to_owned(), object);
        Ok(())
    }

    fn query(&self, query: &KnowledgeQuery) -> Vec<KnowledgeObjectV1> {
        self.objects
            .values()
            .filter(|object| query.matches(object))
            .cloned()
            .collect()
    }

    fn supersede(
        &mut self,
        old_object_id: &str,
        replacement: KnowledgeObjectV1,
    ) -> Result<(), KnowledgeStoreError> {
        validate_object(&replacement)?;
        let replacement_id = object_id(&replacement).to_owned();
        let old = self
            .objects
            .get_mut(old_object_id)
            .ok_or_else(|| KnowledgeStoreError::NotFound(old_object_id.to_owned()))?;
        mark_superseded(old, replacement_id);
        self.objects
            .insert(object_id(&replacement).to_owned(), replacement);
        Ok(())
    }

    fn delete(&mut self, object_id: &str) -> bool {
        self.objects.remove(object_id).is_some()
    }
}

/// Explicit alias for callers that prefer the implementation-oriented name.
pub type InMemoryKnowledgeStore = LocalKnowledgeStore;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge::query::KnowledgeQuery;
    use chrono::{Duration, Utc};
    use lean_ctx_protocol::ExtensionsV1;
    use lean_ctx_protocol::knowledge::{
        AuthorityMetadata, ClassificationLevel, DataClassification, KnowledgeSourceType,
        SourceReference, ValidityWindow,
    };

    fn object(id: &str, tag: &str) -> KnowledgeObjectV1 {
        KnowledgeObjectV1 {
            schema_version: 1,
            source_ref: Some(SourceReference {
                source_type: "git".to_owned(),
                uri: "repo://lean-ctx".to_owned(),
                commit_sha: Some("abc123".to_owned()),
                access_timestamp: "2026-08-09T00:00:00Z".to_owned(),
            }),
            source_type: KnowledgeSourceType::Repository,
            authority: Some(AuthorityMetadata {
                owner: "team".to_owned(),
                confidence_level: 0.9,
                review_status: "reviewed".to_owned(),
            }),
            owner: "team".to_owned(),
            classification: Some(DataClassification {
                level: ClassificationLevel::Internal,
                retention_days: Some(30),
            }),
            validity: Some(ValidityWindow {
                valid_from: (Utc::now() - Duration::hours(1)).to_rfc3339(),
                valid_until: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
                superseded_by: None,
            }),
            supersedes: None,
            content_hash: id.to_owned(),
            evidence_digest: format!("evidence:{id}"),
            policy_ref: "policy:local".to_owned(),
            evidence_refs: vec![format!("evidence-ref:{id}")],
            extra: {
                let mut extra = ExtensionsV1::default();
                extra
                    .insert("tags", serde_json::json!([tag]))
                    .expect("extension should be valid");
                extra
            },
        }
    }

    #[test]
    fn crud_and_query_are_deterministic() {
        let mut store = LocalKnowledgeStore::new();
        store.put(object("b", "architecture")).expect("put b");
        store.put(object("a", "architecture")).expect("put a");
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("a").expect("a").object_id(), "a");

        let query = KnowledgeQuery {
            source: Some("repo://lean-ctx".to_owned()),
            classification: Some(ClassificationLevel::Internal),
            valid_at: Some(Utc::now()),
            tags: vec!["architecture".to_owned()],
        };
        let results = store.query(&query);
        assert_eq!(
            results
                .iter()
                .map(KnowledgeObjectV1::object_id)
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(store.delete("a"));
        assert!(!store.delete("a"));
    }

    #[test]
    fn supersession_keeps_old_history_and_links_replacement() {
        let mut store = LocalKnowledgeStore::new();
        store.put(object("old", "architecture")).expect("put old");
        store
            .supersede("old", object("new", "architecture"))
            .expect("supersede old");

        let old = store.get("old").expect("old history remains");
        assert_eq!(
            old.validity
                .as_ref()
                .and_then(|validity| validity.superseded_by.as_deref()),
            Some("new")
        );
        assert!(store.get("new").is_some());
    }
}
