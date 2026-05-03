use std::path::Path;

use crate::core::hybrid_search::{DenseSearchResult, HybridConfig, HybridResult};
use crate::core::vector_index::BM25Index;
#[cfg(feature = "qdrant")]
use crate::core::vector_index::ChunkKind;

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

        let inferred_qdrant = std::env::var("LEANCTX_QDRANT_URL")
            .ok()
            .is_some_and(|v| !v.trim().is_empty());

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

#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
pub fn dense_results_as_hybrid(
    backend: DenseBackendKind,
    root: &Path,
    index: &BM25Index,
    engine: &crate::core::embeddings::EmbeddingEngine,
    aligned_embeddings: &[Vec<f32>],
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

#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
pub fn hybrid_results(
    backend: DenseBackendKind,
    root: &Path,
    index: &BM25Index,
    engine: &crate::core::embeddings::EmbeddingEngine,
    aligned_embeddings: &[Vec<f32>],
    changed_files: &[String],
    query: &str,
    top_k: usize,
    config: &HybridConfig,
    filter: Option<&dyn Fn(&str) -> bool>,
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

            let mut fused =
                crate::core::hybrid_search::reciprocal_rank_fusion(&bm25, &dense, config, top_k);
            if let Some(pred) = filter {
                fused.retain(|r| pred(&r.file_path));
            }
            fused.truncate(top_k);
            Ok(fused)
        }
    }
}

#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
fn dense_results(
    backend: DenseBackendKind,
    root: &Path,
    index: &BM25Index,
    engine: &crate::core::embeddings::EmbeddingEngine,
    aligned_embeddings: &[Vec<f32>],
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
        DenseBackendKind::Qdrant => dense_results_qdrant(
            root,
            index,
            engine,
            aligned_embeddings,
            changed_files,
            query,
            top_k,
            filter,
        ),
    }
}

#[cfg(feature = "embeddings")]
fn dense_results_local(
    index: &BM25Index,
    engine: &crate::core::embeddings::EmbeddingEngine,
    aligned_embeddings: &[Vec<f32>],
    query: &str,
    top_k: usize,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<DenseSearchResult>, String> {
    use crate::core::embeddings::cosine_similarity;

    let query_embedding = engine
        .embed(query)
        .map_err(|e| format!("embedding failed: {e}"))?;

    let mut scored: Vec<(usize, f32)> = aligned_embeddings
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let Some(pred) = filter else { return true };
            index.chunks.get(*i).is_some_and(|c| pred(&c.file_path))
        })
        .map(|(i, emb)| (i, cosine_similarity(&query_embedding, emb)))
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    Ok(scored
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

#[cfg(feature = "qdrant")]
#[cfg(feature = "embeddings")]
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
        .embed(query)
        .map_err(|e| format!("embedding failed: {e}"))?;

    let hits = store.search(&collection, &query_vec, top_k)?;
    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        if let Some(pred) = filter {
            if !pred(&hit.file_path) {
                continue;
            }
        }
        let snippet = snippet_from_disk(root, &hit.file_path, hit.start_line, hit.end_line, 5);
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
fn chunk_kind_str(kind: &ChunkKind) -> &'static str {
    match kind {
        ChunkKind::Function => "Function",
        ChunkKind::Struct => "Struct",
        ChunkKind::Impl => "Impl",
        ChunkKind::Module => "Module",
        ChunkKind::Class => "Class",
        ChunkKind::Method => "Method",
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
        _ => ChunkKind::Other,
    }
}

#[cfg(feature = "qdrant")]
pub(crate) fn kind_to_str(kind: &ChunkKind) -> &'static str {
    chunk_kind_str(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) -> Option<String> {
        let old = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        old
    }

    fn restore_env(key: &str, old: Option<String>) {
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
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
