//! Consolidation engine for provider data — hippocampal sleep replay.
//!
//! Converts provider results into long-term context artifacts:
//!   1. BM25/embedding index chunks (for future searches)
//!   2. Cross-source graph edges (for related-file discovery)
//!   3. Knowledge facts (for semantic memory)
//!   4. Session cache entries (for fast re-reads at ~13 tokens)
//!
//! This is the "sleep replay" mechanism: raw episodic data (provider API
//! responses) is consolidated into durable semantic representations.
//!
//! Scientific basis: Hippocampal memory consolidation (Kitamura, Science 2017).
//! Fast hippocampal (session cache) traces are replayed to build slow
//! neocortical (knowledge + graph + index) representations.

use crate::core::content_chunk::ContentChunk;
use crate::core::cross_source_edges;
use crate::core::graph_index::IndexEdge;
use crate::core::knowledge_provider_extract::{self, ExtractedFact};

/// Result of a consolidation run — tells the caller what was created.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationResult {
    pub chunks_indexed: usize,
    pub edges_created: usize,
    pub facts_extracted: usize,
    pub cache_entries_stored: usize,
}

/// Consolidate a batch of ContentChunks into all long-term stores.
///
/// This is the main entry point. It does NOT perform I/O itself — it returns
/// the artifacts that the caller should persist. This keeps the consolidation
/// logic pure and testable.
pub fn consolidate(chunks: &[ContentChunk]) -> ConsolidationArtifacts {
    let external_chunks: Vec<&ContentChunk> = chunks.iter().filter(|c| c.is_external()).collect();

    if external_chunks.is_empty() {
        return ConsolidationArtifacts::default();
    }

    let edges = cross_source_edges::extract_cross_source_edges(chunks);

    let facts = knowledge_provider_extract::extract_facts(chunks);

    let cache_entries: Vec<CacheableProviderResult> = external_chunks
        .iter()
        .map(|c| CacheableProviderResult {
            uri: c.file_path.clone(),
            content: c.content.clone(),
            token_count: c.token_count,
        })
        .collect();

    ConsolidationArtifacts {
        bm25_chunks: chunks.to_vec(),
        edges,
        facts,
        cache_entries,
    }
}

/// Pure artifacts produced by consolidation — no side effects yet.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationArtifacts {
    pub bm25_chunks: Vec<ContentChunk>,
    pub edges: Vec<IndexEdge>,
    pub facts: Vec<ExtractedFact>,
    pub cache_entries: Vec<CacheableProviderResult>,
}

impl ConsolidationArtifacts {
    pub fn is_empty(&self) -> bool {
        self.bm25_chunks.is_empty()
            && self.edges.is_empty()
            && self.facts.is_empty()
            && self.cache_entries.is_empty()
    }

    pub fn summary(&self) -> ConsolidationResult {
        ConsolidationResult {
            chunks_indexed: self.bm25_chunks.iter().filter(|c| c.is_external()).count(),
            edges_created: self.edges.len(),
            facts_extracted: self.facts.len(),
            cache_entries_stored: self.cache_entries.len(),
        }
    }
}

/// A provider result ready to be stored in the session cache.
#[derive(Debug, Clone)]
pub struct CacheableProviderResult {
    pub uri: String,
    pub content: String,
    pub token_count: usize,
}

/// Apply consolidation artifacts to the live systems.
///
/// This function performs the actual side effects: writing to BM25, graph,
/// knowledge, and session cache. Designed to be called from a background
/// thread or after a provider query returns.
pub fn apply_artifacts(
    artifacts: &ConsolidationArtifacts,
    bm25: Option<&mut crate::core::bm25_index::BM25Index>,
    graph_edges: Option<&mut Vec<IndexEdge>>,
    session_cache: Option<&mut crate::core::cache::SessionCache>,
) -> ConsolidationResult {
    apply_artifacts_with_pg(artifacts, bm25, graph_edges, session_cache, None)
}

pub fn apply_artifacts_with_pg(
    artifacts: &ConsolidationArtifacts,
    bm25: Option<&mut crate::core::bm25_index::BM25Index>,
    graph_edges: Option<&mut Vec<IndexEdge>>,
    session_cache: Option<&mut crate::core::cache::SessionCache>,
    property_graph: Option<&crate::core::property_graph::CodeGraph>,
) -> ConsolidationResult {
    let mut result = ConsolidationResult::default();

    if let Some(index) = bm25 {
        result.chunks_indexed = index.ingest_content_chunks(artifacts.bm25_chunks.clone());
    }

    if let Some(edges) = graph_edges {
        result.edges_created = cross_source_edges::merge_edges(edges, artifacts.edges.clone());
    }

    if let Some(pg) = property_graph {
        write_edges_to_property_graph(pg, &artifacts.edges);
    }

    result.facts_extracted = artifacts.facts.len();

    if let Some(cache) = session_cache {
        for entry in &artifacts.cache_entries {
            cache.store(&entry.uri, &entry.content);
            result.cache_entries_stored += 1;
        }
    }

    result
}

fn write_edges_to_property_graph(pg: &crate::core::property_graph::CodeGraph, edges: &[IndexEdge]) {
    use crate::core::property_graph::{Edge, EdgeKind, Node};
    for edge in edges {
        let Ok(src_id) = pg.upsert_node(&Node::file(&edge.from)) else {
            continue;
        };
        let Ok(tgt_id) = pg.upsert_node(&Node::file(&edge.to)) else {
            continue;
        };
        let kind = EdgeKind::parse(&edge.kind);
        let _ = pg.upsert_edge(&Edge::new(src_id, tgt_id, kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::{BM25Index, ChunkKind};
    use crate::core::cache::SessionCache;
    use crate::core::content_chunk::ContentChunk;

    fn sample_chunks() -> Vec<ContentChunk> {
        vec![
            ContentChunk::from_provider(
                "github",
                "issues",
                "42",
                "Auth token bug",
                ChunkKind::Issue,
                "Token expires too early in src/auth.rs".into(),
                vec!["src/auth.rs".into()],
                Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
            ),
            ContentChunk::from_provider(
                "github",
                "pull_requests",
                "100",
                "Fix auth expiry",
                ChunkKind::PullRequest,
                "Fixes token lifetime calculation in src/auth.rs".into(),
                vec!["src/auth.rs".into()],
                Some(serde_json::json!({"state": "open"})),
            ),
        ]
    }

    #[test]
    fn consolidate_produces_all_artifact_types() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);

        assert!(!artifacts.is_empty());
        assert_eq!(artifacts.bm25_chunks.len(), 2);
        assert!(!artifacts.edges.is_empty());
        assert!(!artifacts.facts.is_empty());
        assert_eq!(artifacts.cache_entries.len(), 2);
    }

    #[test]
    fn consolidate_empty_input_produces_empty_artifacts() {
        let artifacts = consolidate(&[]);
        assert!(artifacts.is_empty());
    }

    #[test]
    fn consolidate_code_only_produces_empty_external_artifacts() {
        let code = ContentChunk::from(crate::core::bm25_index::CodeChunk {
            file_path: "src/main.rs".into(),
            symbol_name: "main".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 5,
            content: "fn main() {}".into(),
            tokens: vec![],
            token_count: 0,
        });
        let artifacts = consolidate(&[code]);
        assert!(artifacts.edges.is_empty());
        assert!(artifacts.facts.is_empty());
        assert!(artifacts.cache_entries.is_empty());
    }

    #[test]
    fn consolidation_summary_counts_correctly() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);
        let summary = artifacts.summary();

        assert_eq!(summary.chunks_indexed, 2);
        assert!(summary.edges_created > 0);
        assert!(summary.facts_extracted > 0);
        assert_eq!(summary.cache_entries_stored, 2);
    }

    #[test]
    fn apply_artifacts_to_bm25() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);

        let mut index = BM25Index::new();

        let result = apply_artifacts(&artifacts, Some(&mut index), None, None);
        assert_eq!(result.chunks_indexed, 2);
        assert_eq!(index.doc_count, 2);
        assert_eq!(index.external_chunk_count(), 2);
    }

    #[test]
    fn apply_artifacts_to_graph() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);

        let mut edges: Vec<IndexEdge> = Vec::new();
        let result = apply_artifacts(&artifacts, None, Some(&mut edges), None);

        assert!(result.edges_created > 0);
        assert!(!edges.is_empty());
        assert!(edges.iter().any(|e| e.to == "src/auth.rs"));
    }

    #[test]
    fn apply_artifacts_to_session_cache() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);

        let mut cache = SessionCache::new();
        let result = apply_artifacts(&artifacts, None, None, Some(&mut cache));

        assert_eq!(result.cache_entries_stored, 2);
        assert!(cache.get("github://issues/42").is_some());
        assert!(cache.get("github://pull_requests/100").is_some());
    }

    #[test]
    fn apply_artifacts_to_all_systems() {
        let chunks = sample_chunks();
        let artifacts = consolidate(&chunks);

        let mut index = BM25Index::new();
        let mut edges: Vec<IndexEdge> = Vec::new();
        let mut cache = SessionCache::new();

        let result = apply_artifacts(
            &artifacts,
            Some(&mut index),
            Some(&mut edges),
            Some(&mut cache),
        );

        assert!(result.chunks_indexed > 0);
        assert!(result.edges_created > 0);
        assert!(result.facts_extracted > 0);
        assert!(result.cache_entries_stored > 0);
    }
}
