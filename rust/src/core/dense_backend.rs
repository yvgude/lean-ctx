#[cfg(feature = "qdrant")]
use std::path::Path;

#[cfg(feature = "qdrant")]
use crate::core::bm25_index::ChunkKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenseBackendKind {
    Local,
    #[cfg(feature = "qdrant")]
    Qdrant,
}

impl DenseBackendKind {
    pub fn try_from_env() -> Result<Self, String> {
        let explicit = std::env::var("LEANCTX_DENSE_BACKEND")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());

        let inferred_qdrant =
            std::env::var("LEANCTX_QDRANT_URL").is_ok_and(|v| !v.trim().is_empty());

        let requested = explicit.or_else(|| inferred_qdrant.then_some("qdrant".to_string()));

        match requested.as_deref() {
            None | Some("local") => Ok(Self::Local),
            Some("qdrant") => {
                #[cfg(feature = "qdrant")]
                {
                    Ok(Self::Qdrant)
                }
                #[cfg(not(feature = "qdrant"))]
                {
                    Err("Dense backend 'qdrant' requested, but feature 'qdrant' is not enabled. Rebuild with --features qdrant.".to_string())
                }
            }
            Some(other) => Err(format!(
                "Unknown LEANCTX_DENSE_BACKEND={other:?} (expected 'local' or 'qdrant')"
            )),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "local",
            #[cfg(feature = "qdrant")]
            Self::Qdrant => "qdrant",
        }
    }
}

// ---------------------------------------------------------------------------
// Embedding-dependent operations — only compiled with `embeddings` feature.
// ---------------------------------------------------------------------------
#[cfg(feature = "embeddings")]
mod embed {
    use std::path::Path;

    use crate::core::bm25_index::BM25Index;
    use crate::core::hnsw::FlatEmbeddings;
    use crate::core::hybrid_search::{DenseSearchResult, HybridConfig, HybridResult};

    use super::DenseBackendKind;

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dense_results_as_hybrid(
        backend: DenseBackendKind,
        root: &Path,
        index: &BM25Index,
        engine: &crate::core::embeddings::EmbeddingEngine,
        aligned_embeddings: &FlatEmbeddings,
        changed_files: &[String],
        query: &str,
        top_k: usize,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Vec<HybridResult>, String> {
        let dense = dense_results(
            backend,
            root,
            index,
            engine,
            aligned_embeddings,
            changed_files,
            query,
            top_k,
            filter,
        )?;

        Ok(dense
            .into_iter()
            .map(|d| HybridResult {
                file_path: d.file_path,
                symbol_name: d.symbol_name,
                kind: d.kind,
                start_line: d.start_line,
                end_line: d.end_line,
                snippet: d.snippet,
                rrf_score: d.similarity as f64,
                bm25_score: None,
                dense_score: Some(d.similarity),
                bm25_rank: None,
                dense_rank: None,
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn hybrid_results(
        backend: DenseBackendKind,
        root: &Path,
        index: &BM25Index,
        engine: &crate::core::embeddings::EmbeddingEngine,
        aligned_embeddings: &FlatEmbeddings,
        changed_files: &[String],
        query: &str,
        top_k: usize,
        config: &HybridConfig,
        filter: Option<&dyn Fn(&str) -> bool>,
        graph_file_ranks: Option<&std::collections::HashMap<String, usize>>,
    ) -> Result<Vec<HybridResult>, String> {
        match backend {
            DenseBackendKind::Local => {
                let _ = (root, changed_files);
                let mut results = crate::core::hybrid_search::hybrid_search(
                    query,
                    index,
                    Some(engine),
                    Some(aligned_embeddings),
                    top_k,
                    config,
                    graph_file_ranks,
                );
                if let Some(pred) = filter {
                    results.retain(|r| pred(&r.file_path));
                }
                results.truncate(top_k);
                Ok(results)
            }
            #[cfg(feature = "qdrant")]
            DenseBackendKind::Qdrant => {
                let bm25_k = config.bm25_candidates.max(top_k);
                let dense_k = config.dense_candidates.max(top_k);

                let mut bm25 = index.search(query, bm25_k);
                if let Some(pred) = filter {
                    bm25.retain(|r| pred(&r.file_path));
                }

                let dense = dense_results(
                    backend,
                    root,
                    index,
                    engine,
                    aligned_embeddings,
                    changed_files,
                    query,
                    dense_k,
                    filter,
                )?;

                let mut fused = crate::core::hybrid_search::reciprocal_rank_fusion(
                    &bm25,
                    &dense,
                    config,
                    top_k,
                    graph_file_ranks,
                );
                if let Some(pred) = filter {
                    fused.retain(|r| pred(&r.file_path));
                }
                fused.truncate(top_k);
                Ok(fused)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dense_results(
        backend: DenseBackendKind,
        root: &Path,
        index: &BM25Index,
        engine: &crate::core::embeddings::EmbeddingEngine,
        aligned_embeddings: &FlatEmbeddings,
        changed_files: &[String],
        query: &str,
        top_k: usize,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Vec<DenseSearchResult>, String> {
        match backend {
            DenseBackendKind::Local => {
                let _ = (root, changed_files);
                dense_results_local(index, engine, aligned_embeddings, query, top_k, filter)
            }
            #[cfg(feature = "qdrant")]
            DenseBackendKind::Qdrant => {
                let vecs: Vec<Vec<f32>> = (0..aligned_embeddings.n_vectors())
                    .map(|i| aligned_embeddings.get_vec(i))
                    .collect();
                dense_results_qdrant(
                    root,
                    index,
                    engine,
                    &vecs,
                    changed_files,
                    query,
                    top_k,
                    filter,
                )
            }
        }
    }

    fn dense_results_local(
        index: &BM25Index,
        engine: &crate::core::embeddings::EmbeddingEngine,
        aligned_embeddings: &FlatEmbeddings,
        query: &str,
        top_k: usize,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Vec<DenseSearchResult>, String> {
        let query_embedding = engine
            .embed_query(query)
            .map_err(|e| format!("embedding failed: {e}"))?;

        let top = top_k_by_similarity(&query_embedding, aligned_embeddings, top_k, |i| {
            let Some(pred) = filter else { return true };
            index.chunks.get(i).is_some_and(|c| pred(&c.file_path))
        });

        Ok(top
            .into_iter()
            .filter_map(|(idx, sim)| {
                let chunk = index.chunks.get(idx)?;
                let snippet = chunk.content.lines().take(5).collect::<Vec<_>>().join("\n");
                Some(DenseSearchResult {
                    chunk_idx: idx,
                    similarity: sim,
                    file_path: chunk.file_path.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    kind: chunk.kind.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    snippet,
                })
            })
            .collect())
    }

    /// Min-heap based Top-K selection over a flat embedding buffer.
    /// O(n log k) instead of O(n log n) full sort. The filter is applied inline
    /// during the scan so only matching chunks are considered — post-filtering
    /// cannot drop below `top_k` results regardless of filter selectivity.
    ///
    /// Uses sequential memory access (one dereference) via `FlatEmbeddings::get`,
    /// unlike the old `Arc<[Vec<f32>]>` layout which had two-level indirection.
    fn top_k_by_similarity(
        query: &[f32],
        embeddings: &FlatEmbeddings,
        k: usize,
        filter: impl Fn(usize) -> bool,
    ) -> Vec<(usize, f32)> {
        use std::cmp::Ordering;
        use std::collections::BinaryHeap;

        #[derive(PartialEq)]
        struct MinEntry(f32, usize);

        impl Eq for MinEntry {}
        impl PartialOrd for MinEntry {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for MinEntry {
            fn cmp(&self, other: &Self) -> Ordering {
                other
                    .0
                    .partial_cmp(&self.0)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| self.1.cmp(&other.1))
            }
        }

        let n = embeddings.n_vectors();
        let mut heap: BinaryHeap<MinEntry> = BinaryHeap::with_capacity(k + 1);

        for i in 0..n {
            if !filter(i) {
                continue;
            }
            let emb = embeddings.get(i);
            let sim = crate::core::embeddings::cosine_similarity(query, emb);
            if heap.len() < k {
                heap.push(MinEntry(sim, i));
            } else if let Some(min) = heap.peek()
                && sim > min.0
            {
                heap.pop();
                heap.push(MinEntry(sim, i));
            }
        }

        let mut result: Vec<(usize, f32)> = heap.into_iter().map(|e| (e.1, e.0)).collect();
        result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        result
    }

    #[cfg(feature = "qdrant")]
    fn dense_results_qdrant(
        root: &Path,
        index: &BM25Index,
        engine: &crate::core::embeddings::EmbeddingEngine,
        aligned_embeddings: &[Vec<f32>],
        changed_files: &[String],
        query: &str,
        top_k: usize,
        filter: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Vec<DenseSearchResult>, String> {
        let store = crate::core::qdrant_store::QdrantStore::from_env()?;
        let collection = store.collection_name(root, engine.dimensions())?;
        let created_new = store.ensure_collection(&collection, engine.dimensions())?;
        store.sync_index(
            &collection,
            index,
            aligned_embeddings,
            changed_files,
            created_new,
        )?;

        let query_vec = engine
            .embed_query(query)
            .map_err(|e| format!("embedding failed: {e}"))?;

        let hits = store.search(&collection, &query_vec, top_k)?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(pred) = filter
                && !pred(&hit.file_path)
            {
                continue;
            }
            let snippet =
                super::snippet_from_disk(root, &hit.file_path, hit.start_line, hit.end_line, 5);
            out.push(DenseSearchResult {
                chunk_idx: 0,
                similarity: hit.score,
                file_path: hit.file_path,
                symbol_name: hit.symbol_name,
                kind: hit.kind,
                start_line: hit.start_line,
                end_line: hit.end_line,
                snippet,
            });
        }
        Ok(out)
    }
}

#[cfg(feature = "embeddings")]
pub(crate) use embed::*;

#[cfg(feature = "qdrant")]
fn snippet_from_disk(
    root: &Path,
    rel_path: &str,
    start_line: usize,
    end_line: usize,
    max_lines: usize,
) -> String {
    let Ok(path) = crate::core::pathjail::jail_path(&root.join(rel_path), root) else {
        return String::new();
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = start_line.saturating_sub(1).min(lines.len());
    let end = end_line.max(start_line).min(lines.len());
    let mut slice = &lines[start..end];
    if slice.len() > max_lines {
        slice = &slice[..max_lines];
    }
    slice.join("\n")
}

#[cfg(feature = "qdrant")]
pub(crate) fn kind_to_str(kind: &ChunkKind) -> &'static str {
    match kind {
        ChunkKind::Function => "Function",
        ChunkKind::Struct => "Struct",
        ChunkKind::Impl => "Impl",
        ChunkKind::Module => "Module",
        ChunkKind::Class => "Class",
        ChunkKind::Method => "Method",
        ChunkKind::Issue => "Issue",
        ChunkKind::PullRequest => "PullRequest",
        ChunkKind::WikiPage => "WikiPage",
        ChunkKind::DbSchema => "DbSchema",
        ChunkKind::ApiEndpoint => "ApiEndpoint",
        ChunkKind::Ticket => "Ticket",
        ChunkKind::ExternalOther => "ExternalOther",
        ChunkKind::Other => "Other",
    }
}

#[cfg(feature = "qdrant")]
pub(crate) fn kind_from_str(s: &str) -> ChunkKind {
    match s {
        "Function" => ChunkKind::Function,
        "Struct" => ChunkKind::Struct,
        "Impl" => ChunkKind::Impl,
        "Module" => ChunkKind::Module,
        "Class" => ChunkKind::Class,
        "Method" => ChunkKind::Method,
        "Issue" => ChunkKind::Issue,
        "PullRequest" => ChunkKind::PullRequest,
        "WikiPage" => ChunkKind::WikiPage,
        "DbSchema" => ChunkKind::DbSchema,
        "ApiEndpoint" => ChunkKind::ApiEndpoint,
        "Ticket" => ChunkKind::Ticket,
        "ExternalOther" => ChunkKind::ExternalOther,
        _ => ChunkKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) -> Option<String> {
        let old = std::env::var(key).ok();
        match value {
            Some(v) => crate::test_env::set_var(key, v),
            None => crate::test_env::remove_var(key),
        }
        old
    }

    fn restore_env(key: &str, old: Option<String>) {
        match old {
            Some(v) => crate::test_env::set_var(key, v),
            None => crate::test_env::remove_var(key),
        }
    }

    #[test]
    fn dense_backend_defaults_to_local() {
        let _g = ENV_LOCK.lock().unwrap();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", None);

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Local);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }

    #[test]
    fn dense_backend_unknown_value_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", Some("wat"));
        let old_url = set_env("LEANCTX_QDRANT_URL", None);

        let err = DenseBackendKind::try_from_env().unwrap_err();
        assert!(err.contains("Unknown LEANCTX_DENSE_BACKEND"));

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }

    #[cfg(feature = "qdrant")]
    #[test]
    fn dense_backend_infers_qdrant_from_url() {
        let _g = ENV_LOCK.lock().unwrap();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", Some("http://127.0.0.1:6333"));

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Qdrant);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }

    #[cfg(not(feature = "qdrant"))]
    #[test]
    fn dense_backend_qdrant_requires_feature() {
        let _g = ENV_LOCK.lock().unwrap();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", Some("qdrant"));
        let old_url = set_env("LEANCTX_QDRANT_URL", None);

        let err = DenseBackendKind::try_from_env().unwrap_err();
        assert!(err.contains("feature 'qdrant' is not enabled"));

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }
}
