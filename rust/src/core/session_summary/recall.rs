//! Recall past session summaries — semantic when embeddings are loaded, else a
//! lexical token-overlap fallback (#292).

use super::record::SummaryRecord;
use super::store::SummaryStore;

/// One recalled summary with its score and the recall mode that produced it.
#[derive(Debug, Clone)]
pub struct RecallHit {
    pub record: SummaryRecord,
    pub score: f32,
    pub mode: &'static str,
}

/// Recall the `top_k` summaries most relevant to `query`.
#[must_use]
pub fn recall(project_root: &str, query: &str, top_k: usize) -> Vec<RecallHit> {
    let store = SummaryStore::load_or_create(project_root);
    if store.summaries.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }
    #[cfg(feature = "embeddings")]
    {
        if let Some(hits) = semantic(&store, query, top_k) {
            return hits;
        }
    }
    lexical(&store, query, top_k)
}

fn lexical(store: &SummaryStore, query: &str, top_k: usize) -> Vec<RecallHit> {
    store
        .search_lexical(query, top_k)
        .into_iter()
        .map(|(i, score)| RecallHit {
            record: store.summaries[i].clone(),
            score: score as f32,
            mode: "lexical",
        })
        .collect()
}

/// Semantic recall. Returns `None` (→ lexical fallback) when embeddings are
/// disabled or the model isn't already loaded — never blocks on a model load.
#[cfg(feature = "embeddings")]
fn semantic(store: &SummaryStore, query: &str, top_k: usize) -> Option<Vec<RecallHit>> {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.embeddings_enabled() {
        return None;
    }
    // Non-blocking: only use semantic recall if the model is already warm.
    let engine = crate::core::embeddings::try_shared_engine()?;
    let q = engine.embed_query(query).ok()?;

    let mut scored: Vec<RecallHit> = Vec::new();
    for rec in &store.summaries {
        if let Ok(emb) = engine.embed_query(&rec.searchable_text()) {
            scored.push(RecallHit {
                record: rec.clone(),
                score: cosine(&q, &emb),
                mode: "semantic",
            });
        }
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.record.created_at.cmp(&a.record.created_at))
    });
    scored.truncate(top_k);
    Some(scored)
}

#[cfg(feature = "embeddings")]
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
