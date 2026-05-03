//! Embedding-based Knowledge Retrieval for `ctx_knowledge`.
//!
//! Wraps `ProjectKnowledge` with a vector index for semantic recall.
//! Facts are automatically embedded on `remember` and searched via
//! cosine similarity on `recall`, with hybrid exact + semantic ranking.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::knowledge::{KnowledgeFact, ProjectKnowledge};
use crate::core::memory_policy::MemoryPolicy;

#[cfg(feature = "embeddings")]
use super::embeddings::{cosine_similarity, EmbeddingEngine};

const ALPHA_SEMANTIC: f32 = 0.6;
const BETA_CONFIDENCE: f32 = 0.25;
const GAMMA_RECENCY: f32 = 0.15;
const MAX_RECENCY_DAYS: f32 = 90.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEmbedding {
    pub category: String,
    pub key: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEmbeddingIndex {
    pub project_hash: String,
    pub entries: Vec<FactEmbedding>,
}

impl KnowledgeEmbeddingIndex {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            entries: Vec::new(),
        }
    }

    pub fn upsert(&mut self, category: &str, key: &str, embedding: Vec<f32>) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.category == category && e.key == key)
        {
            existing.embedding = embedding;
        } else {
            self.entries.push(FactEmbedding {
                category: category.to_string(),
                key: key.to_string(),
                embedding,
            });
        }
    }

    pub fn remove(&mut self, category: &str, key: &str) {
        self.entries
            .retain(|e| !(e.category == category && e.key == key));
    }

    #[cfg(feature = "embeddings")]
    pub fn semantic_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<(&FactEmbedding, f32)> {
        let mut scored: Vec<(&FactEmbedding, f32)> = self
            .entries
            .iter()
            .map(|e| {
                let sim = cosine_similarity(query_embedding, &e.embedding);
                (e, sim)
            })
            .collect();

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.category.cmp(&b.0.category))
                .then_with(|| a.0.key.cmp(&b.0.key))
        });
        scored.truncate(top_k);
        scored
    }

    fn index_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("knowledge")
            .join(project_hash);
        Some(dir.join("embeddings.json"))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::index_path(project_hash)?;
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::index_path(&self.project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let json = serde_json::to_string(self).map_err(|e| format!("{e}"))?;
        std::fs::write(path, json).map_err(|e| format!("{e}"))
    }
}

pub fn reset(project_hash: &str) -> Result<(), String> {
    let path = KnowledgeEmbeddingIndex::index_path(project_hash)
        .ok_or_else(|| "Cannot determine data directory".to_string())?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("{e}"))?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct ScoredFact<'a> {
    pub fact: &'a KnowledgeFact,
    pub score: f32,
    pub semantic_score: f32,
    pub confidence_score: f32,
    pub recency_score: f32,
}

#[cfg(feature = "embeddings")]
pub fn semantic_recall<'a>(
    knowledge: &'a ProjectKnowledge,
    index: &KnowledgeEmbeddingIndex,
    engine: &EmbeddingEngine,
    query: &str,
    top_k: usize,
) -> Vec<ScoredFact<'a>> {
    let Ok(query_embedding) = engine.embed(query) else {
        return lexical_fallback(knowledge, query, top_k);
    };

    let semantic_hits = index.semantic_search(&query_embedding, top_k * 2);

    let mut results: Vec<ScoredFact<'a>> = Vec::new();

    for (entry, sim) in &semantic_hits {
        if let Some(fact) = knowledge
            .facts
            .iter()
            .find(|f| f.category == entry.category && f.key == entry.key && f.is_current())
        {
            let confidence_score = fact.quality_score();
            let recency_score = recency_decay(fact);
            let score = ALPHA_SEMANTIC * sim
                + BETA_CONFIDENCE * confidence_score
                + GAMMA_RECENCY * recency_score;

            results.push(ScoredFact {
                fact,
                score,
                semantic_score: *sim,
                confidence_score,
                recency_score,
            });
        }
    }

    let exact_matches = knowledge.recall(query);
    for fact in exact_matches {
        let already_included = results
            .iter()
            .any(|r| r.fact.category == fact.category && r.fact.key == fact.key);
        if !already_included {
            results.push(ScoredFact {
                fact,
                score: 1.0,
                semantic_score: 1.0,
                confidence_score: fact.quality_score(),
                recency_score: recency_decay(fact),
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.confidence_score
                    .partial_cmp(&a.confidence_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.recency_score
                    .partial_cmp(&a.recency_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.fact.category.cmp(&b.fact.category))
            .then_with(|| a.fact.key.cmp(&b.fact.key))
            .then_with(|| a.fact.value.cmp(&b.fact.value))
    });
    results.truncate(top_k);
    results
}

#[cfg(feature = "embeddings")]
pub fn semantic_recall_semantic_only<'a>(
    knowledge: &'a ProjectKnowledge,
    index: &KnowledgeEmbeddingIndex,
    engine: &EmbeddingEngine,
    query: &str,
    top_k: usize,
) -> Vec<ScoredFact<'a>> {
    let Ok(query_embedding) = engine.embed(query) else {
        return Vec::new();
    };

    let semantic_hits = index.semantic_search(&query_embedding, top_k * 2);
    let mut results: Vec<ScoredFact<'a>> = Vec::new();

    for (entry, sim) in &semantic_hits {
        if let Some(fact) = knowledge
            .facts
            .iter()
            .find(|f| f.category == entry.category && f.key == entry.key && f.is_current())
        {
            let confidence_score = fact.quality_score();
            let recency_score = recency_decay(fact);
            let score = ALPHA_SEMANTIC * sim
                + BETA_CONFIDENCE * confidence_score
                + GAMMA_RECENCY * recency_score;

            results.push(ScoredFact {
                fact,
                score,
                semantic_score: *sim,
                confidence_score,
                recency_score,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.confidence_score
                    .partial_cmp(&a.confidence_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.recency_score
                    .partial_cmp(&a.recency_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.fact.category.cmp(&b.fact.category))
            .then_with(|| a.fact.key.cmp(&b.fact.key))
            .then_with(|| a.fact.value.cmp(&b.fact.value))
    });
    results.truncate(top_k);
    results
}

pub fn compact_against_knowledge(
    index: &mut KnowledgeEmbeddingIndex,
    knowledge: &ProjectKnowledge,
    policy: &MemoryPolicy,
) {
    use std::collections::HashMap;

    let mut current: HashMap<(&str, &str), &KnowledgeFact> = HashMap::new();
    for f in &knowledge.facts {
        if f.is_current() {
            current.insert((f.category.as_str(), f.key.as_str()), f);
        }
    }

    let mut kept: Vec<(FactEmbedding, &KnowledgeFact)> = index
        .entries
        .iter()
        .filter_map(|e| {
            current
                .get(&(e.category.as_str(), e.key.as_str()))
                .map(|f| (e.clone(), *f))
        })
        .collect();

    kept.sort_by(|(ea, fa), (eb, fb)| {
        fb.confidence
            .partial_cmp(&fa.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| fb.last_confirmed.cmp(&fa.last_confirmed))
            .then_with(|| fb.retrieval_count.cmp(&fa.retrieval_count))
            .then_with(|| ea.category.cmp(&eb.category))
            .then_with(|| ea.key.cmp(&eb.key))
    });

    let max = policy.embeddings.max_facts;
    if kept.len() > max {
        kept.truncate(max);
    }

    index.entries = kept.into_iter().map(|(e, _)| e).collect();
}

fn lexical_fallback<'a>(
    knowledge: &'a ProjectKnowledge,
    query: &str,
    top_k: usize,
) -> Vec<ScoredFact<'a>> {
    knowledge
        .recall(query)
        .into_iter()
        .take(top_k)
        .map(|fact| ScoredFact {
            fact,
            score: fact.confidence,
            semantic_score: 0.0,
            confidence_score: fact.confidence,
            recency_score: recency_decay(fact),
        })
        .collect()
}

fn recency_decay(fact: &KnowledgeFact) -> f32 {
    let days_old = chrono::Utc::now()
        .signed_duration_since(fact.last_confirmed)
        .num_days() as f32;
    (1.0 - days_old / MAX_RECENCY_DAYS).max(0.0)
}

#[cfg(feature = "embeddings")]
pub fn embed_and_store(
    index: &mut KnowledgeEmbeddingIndex,
    engine: &EmbeddingEngine,
    category: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let text = format!("{category} {key}: {value}");
    let embedding = engine.embed(&text).map_err(|e| format!("{e}"))?;
    index.upsert(category, key, embedding);
    Ok(())
}

pub fn format_scored_facts(results: &[ScoredFact<'_>]) -> String {
    if results.is_empty() {
        return "No matching facts found.".to_string();
    }

    let mut output = String::new();
    for (i, scored) in results.iter().enumerate() {
        let f = scored.fact;
        let stars = if f.confidence >= 0.9 {
            "★★★★"
        } else if f.confidence >= 0.7 {
            "★★★"
        } else if f.confidence >= 0.5 {
            "★★"
        } else {
            "★"
        };

        if i > 0 {
            output.push('|');
        }
        output.push_str(&format!(
            "{}:{}={}{} [s:{:.0}%]",
            f.category,
            f.key,
            f.value,
            stars,
            scored.score * 100.0
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_removes_index_file() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(
            "LEAN_CTX_DATA_DIR",
            tmp.path().to_string_lossy().to_string(),
        );

        let idx = KnowledgeEmbeddingIndex {
            project_hash: "projhash".to_string(),
            entries: vec![FactEmbedding {
                category: "arch".to_string(),
                key: "db".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
            }],
        };
        idx.save().expect("save");
        assert!(KnowledgeEmbeddingIndex::load("projhash").is_some());

        reset("projhash").expect("reset");
        assert!(KnowledgeEmbeddingIndex::load("projhash").is_none());

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn compact_drops_missing_or_archived_facts() {
        let mut knowledge = ProjectKnowledge::new("/tmp/project");
        let now = chrono::Utc::now();
        knowledge.facts.push(KnowledgeFact {
            category: "arch".to_string(),
            key: "db".to_string(),
            value: "Postgres".to_string(),
            source_session: "s".to_string(),
            confidence: 0.9,
            created_at: now,
            last_confirmed: now,
            retrieval_count: 5,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        });
        knowledge.facts.push(KnowledgeFact {
            category: "arch".to_string(),
            key: "old".to_string(),
            value: "Old".to_string(),
            source_session: "s".to_string(),
            confidence: 0.9,
            created_at: now,
            last_confirmed: now,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: Some(now),
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        });

        let mut idx = KnowledgeEmbeddingIndex::new(&knowledge.project_hash);
        idx.upsert("arch", "db", vec![1.0, 0.0, 0.0]);
        idx.upsert("arch", "old", vec![0.0, 1.0, 0.0]);
        idx.upsert("ops", "deploy", vec![0.0, 0.0, 1.0]);

        compact_against_knowledge(&mut idx, &knowledge, &MemoryPolicy::default());
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].category, "arch");
        assert_eq!(idx.entries[0].key, "db");
    }

    #[test]
    fn index_upsert_and_remove() {
        let mut idx = KnowledgeEmbeddingIndex::new("test");
        idx.upsert("arch", "db", vec![1.0, 0.0, 0.0]);
        assert_eq!(idx.entries.len(), 1);

        idx.upsert("arch", "db", vec![0.0, 1.0, 0.0]);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].embedding[1], 1.0);

        idx.upsert("arch", "cache", vec![0.0, 0.0, 1.0]);
        assert_eq!(idx.entries.len(), 2);

        idx.remove("arch", "db");
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].key, "cache");
    }

    #[test]
    fn recency_decay_recent() {
        let fact = KnowledgeFact {
            category: "test".to_string(),
            key: "k".to_string(),
            value: "v".to_string(),
            source_session: "s".to_string(),
            confidence: 0.9,
            created_at: chrono::Utc::now(),
            last_confirmed: chrono::Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        };
        let decay = recency_decay(&fact);
        assert!(
            decay > 0.95,
            "Recent fact should have high recency: {decay}"
        );
    }

    #[test]
    fn recency_decay_old() {
        let old_date = chrono::Utc::now() - chrono::Duration::days(100);
        let fact = KnowledgeFact {
            category: "test".to_string(),
            key: "k".to_string(),
            value: "v".to_string(),
            source_session: "s".to_string(),
            confidence: 0.5,
            created_at: old_date,
            last_confirmed: old_date,
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 1,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        };
        let decay = recency_decay(&fact);
        assert_eq!(decay, 0.0, "100-day-old fact should have 0 recency");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn semantic_search_ranking() {
        let mut idx = KnowledgeEmbeddingIndex::new("test");
        idx.upsert("arch", "db", vec![1.0, 0.0, 0.0]);
        idx.upsert("arch", "cache", vec![0.0, 1.0, 0.0]);
        idx.upsert("ops", "deploy", vec![0.5, 0.5, 0.0]);

        let query = vec![1.0, 0.0, 0.0];
        let results = idx.semantic_search(&query, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0.key, "db");
    }

    #[test]
    fn format_scored_empty() {
        assert_eq!(format_scored_facts(&[]), "No matching facts found.");
    }

    #[test]
    fn format_scored_output() {
        let fact = KnowledgeFact {
            category: "arch".to_string(),
            key: "db".to_string(),
            value: "PostgreSQL".to_string(),
            source_session: "s1".to_string(),
            confidence: 0.95,
            created_at: chrono::Utc::now(),
            last_confirmed: chrono::Utc::now(),
            retrieval_count: 0,
            last_retrieved: None,
            valid_from: None,
            valid_until: None,
            supersedes: None,
            confirmation_count: 3,
            feedback_up: 0,
            feedback_down: 0,
            last_feedback: None,
        };
        let scored = vec![ScoredFact {
            fact: &fact,
            score: 0.85,
            semantic_score: 0.9,
            confidence_score: 0.95,
            recency_score: 1.0,
        }];
        let output = format_scored_facts(&scored);
        assert!(output.contains("arch:db=PostgreSQL"));
        assert!(output.contains("★★★★"));
        assert!(output.contains("[s:85%]"));
    }
}
