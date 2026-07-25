//! Dense & hybrid search modes: inline-embed budget, engine + embedding
//! index loading, embedding alignment.

use std::path::Path;

use crate::core::bm25_index::BM25Index;
use crate::core::embedding_index::EmbeddingIndex;
#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;
use crate::core::hnsw::FlatEmbeddings;
use crate::core::hybrid_search::{HybridConfig, format_hybrid_results};

#[allow(clippy::wildcard_imports)]
use super::*;

/// #512: max chunks the hybrid/dense path will embed *inline* (under the
/// per-request watchdog) before degrading instead of embedding. A server that
/// started before the on-disk dense index existed would otherwise embed the
/// whole corpus on the first query — observed as a runaway 500%+ CPU child the
/// 120s watchdog abandons but cannot cancel. Tunable via
/// `LEAN_CTX_HYBRID_INLINE_EMBED_MAX`; `0` disables the guard (always embed
/// inline — the pre-#512 behavior).
#[cfg(feature = "embeddings")]
pub(crate) fn inline_embed_max_chunks() -> usize {
    const DEFAULT_MAX: usize = 2000;
    std::env::var("LEAN_CTX_HYBRID_INLINE_EMBED_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX)
}

/// Pure budget check for the cold-start guard (#512): `max == 0` disables it,
/// and the budget is inclusive (`pending == max` still embeds inline).
#[cfg(feature = "embeddings")]
pub(crate) fn exceeds_inline_embed_budget(pending: usize, max: usize) -> bool {
    max > 0 && pending > max
}

/// Decide whether this call would trigger a large inline embed the watchdog
/// cannot safely bound (#512). Returns the pending-chunk count when the call
/// should degrade instead of embedding inline; `None` keeps the normal path
/// (warm index, or an incremental embed of only a few changed chunks).
#[cfg(feature = "embeddings")]
pub(crate) fn cold_start_embed_guard(
    embed_idx: &EmbeddingIndex,
    index: &BM25Index,
) -> Option<usize> {
    let pending = embed_idx.pending_chunk_count(&index.chunks);
    exceeds_inline_embed_budget(pending, inline_embed_max_chunks()).then_some(pending)
}

/// One-line, deterministic hint pointing at the out-of-band dense build. Shared
/// by the hybrid fallback and the dense fail-fast so the guidance never drifts.
#[cfg(feature = "embeddings")]
pub(crate) fn dense_build_hint(pending: usize, compact: bool) -> String {
    if compact {
        format!("[dense not built: {pending} chunks pending — run: lean-ctx index build-semantic]")
    } else {
        format!(
            "[lean-ctx: dense index not built ({pending} chunks would embed inline). \
             Build it once — no per-query embed, no cold-start hang: \
             lean-ctx index build-semantic]"
        )
    }
}

/// #1259: is the default BM25 ranking running *because* the dense index was
/// never built? Cheap enough for the hot path — a file-existence check, no
/// engine load. Callers use it to label the degradation instead of reporting
/// lexical hits as "semantic". False when dense is deliberately off (#686):
/// there is nothing to build then.
pub(crate) fn dense_index_missing(root: &Path) -> bool {
    #[cfg(feature = "embeddings")]
    {
        HybridConfig::from_config().dense_enabled
            && !crate::core::index_namespace::vectors_dir(root)
                .join("embeddings.bin")
                .exists()
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = root;
        false
    }
}

pub(crate) fn hybrid_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let cfg = HybridConfig::from_config();

        // Dense disabled (#686): skip the embedding engine + index build/persist
        // and rank with BM25 + graph proximity + reranking (+ SPLADE) only — the
        // exact fallback the pipeline uses when embeddings are absent, so results
        // stay coherent while the vector footprint and embed latency disappear.
        if !cfg.dense_enabled {
            return bm25_graph_search(query, root, index, top_k, compact, filter, &cfg);
        }

        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        // #512: cold-start guard. Never embed a large corpus inline under the
        // request watchdog (it produces a runaway the watchdog abandons but
        // cannot cancel). Degrade to the BM25+graph path — the same coherent
        // fallback used when dense is disabled — and tell the user to build the
        // dense index once, out of band. Incremental embeds (few changed chunks
        // on a warm index) stay inline and fast.
        if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
            let base = bm25_graph_search(query, root, index, top_k, compact, filter, &cfg);
            return format!("{base}\n{}", dense_build_hint(pending, compact));
        }

        let (aligned, coverage, changed_files) =
            match ensure_embeddings(root, index, engine, &mut embed_idx) {
                Ok(v) => v,
                Err(e) => return format!("ERR: {e}"),
            };

        let backend = match crate::core::dense_backend::DenseBackendKind::try_from_env() {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };
        let filter_fn = |p: &str| filter.matches(p);
        let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
            .is_active()
            .then_some(&filter_fn as &dyn Fn(&str) -> bool);
        let graph_ranks = graph_rrf_ranks_for_search_root(root);
        let graph_ranks_ref = graph_ranks.as_ref();
        let mut results = match crate::core::dense_backend::hybrid_results(
            backend,
            root,
            index,
            engine,
            &aligned,
            &changed_files,
            query,
            top_k,
            &cfg,
            filter_pred,
            graph_ranks_ref,
        ) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        if cfg.splade_weight > 0.0 {
            let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, top_k);
            if !splade.is_empty() {
                boost_with_splade(&mut results, &splade, cfg.splade_weight);
            }
        }

        results.truncate(top_k);

        let header = if compact {
            format!(
                "semantic_search(hybrid,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Hybrid): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
        if filter.is_active() {
            results.retain(|x| filter.matches(&x.file_path));
        }

        let graph_ranks = graph_rrf_ranks_for_search_root(root);
        if let Some(ref graph_ranks) = graph_ranks {
            const GRAPH_RRF_K: f64 = 60.0;
            for r in &mut results {
                if let Some(&rank) = graph_ranks.get(&r.file_path) {
                    r.score += 1.0 / (GRAPH_RRF_K + rank as f64 + 1.0);
                }
            }
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        results.truncate(top_k);
        let graph_tag = if graph_ranks.is_some() { "+graph" } else { "" };
        let header = if compact {
            format!(
                "semantic_search(bm25{graph_tag},{top_k}) → {} results, {} chunks indexed\n",
                results.len(),
                index.doc_count
            )
        } else {
            format!(
                "Semantic search (BM25{graph_tag}): \"{}\" ({} results from {} indexed chunks)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
            )
        };
        format!("{header}{}", format_search_results(&results, compact))
    }
}

pub(crate) fn dense_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        // #512: explicit dense has no BM25 fallback to degrade into, so fail fast
        // with the same actionable hint rather than embed the whole corpus inline
        // under the watchdog (the cold-start runaway). A warm/incremental index
        // passes through untouched.
        if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
            return dense_build_hint(pending, compact);
        }

        let (aligned, coverage, changed_files) =
            match ensure_embeddings(root, index, engine, &mut embed_idx) {
                Ok(v) => v,
                Err(e) => return format!("ERR: {e}"),
            };

        let backend = match crate::core::dense_backend::DenseBackendKind::try_from_env() {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let filter_fn = |p: &str| filter.matches(p);
        let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
            .is_active()
            .then_some(&filter_fn as &dyn Fn(&str) -> bool);

        let candidate_k = filtered_candidate_k(top_k, filter.is_active());
        let mut results = match crate::core::dense_backend::dense_results_as_hybrid(
            backend,
            root,
            index,
            engine,
            &aligned,
            &changed_files,
            query,
            candidate_k,
            filter_pred,
        ) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };
        results.truncate(top_k);

        let header = if compact {
            format!(
                "semantic_search(dense,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Dense): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        "ERR: embeddings feature not enabled".to_string()
    }
}

#[cfg(feature = "embeddings")]
fn reject_under_hard_pressure(operation: &str) -> Result<(), String> {
    if crate::core::memory_guard::abort_requested() {
        Err(format!("{operation} cancelled during hard memory pressure"))
    } else {
        Ok(())
    }
}

#[cfg(feature = "embeddings")]
pub(crate) fn load_engine_and_index(
    root: &Path,
) -> Result<(&'static EmbeddingEngine, EmbeddingIndex), String> {
    reject_under_hard_pressure("semantic embedding load")?;
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.embeddings_enabled() {
        return Err("embeddings disabled by memory_profile=low".into());
    }

    let engine = crate::core::embeddings::shared_engine()
        .ok_or_else(|| "embedding engine load failed".to_string())?;

    let model_name = engine.model_name();
    let mut idx = EmbeddingIndex::load(root)
        .unwrap_or_else(|| EmbeddingIndex::new_with_model(engine.dimensions(), model_name));

    if let Some((stored, current)) = idx.model_mismatch(model_name) {
        tracing::warn!(
            "[embeddings] model changed: {stored} → {current}. Re-indexing all embeddings."
        );
        idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
    } else if idx.dimension_mismatch(engine.dimensions()) {
        tracing::warn!(
            "[embeddings] dimension mismatch: index={}d, engine={}d. Re-indexing.",
            idx.dimensions,
            engine.dimensions()
        );
        idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
    }

    if idx.model_id.is_none() {
        idx.model_id = Some(model_name.to_string());
    }

    Ok((engine, idx))
}

/// Aligned embedding corpus as a single contiguous [`FlatEmbeddings`] allocation,
/// plus coverage and the list of files re-embedded this call. The flat row-major
/// layout gives sequential memory access during dot-product scoring — one
/// dereference instead of the two-level indirection of `Arc<[Vec<f32>]>`.
#[cfg(feature = "embeddings")]
pub(crate) type AlignedEmbeddings = (FlatEmbeddings, f64, Vec<String>);

#[cfg(feature = "embeddings")]
pub(crate) fn ensure_embeddings(
    root: &Path,
    index: &BM25Index,
    engine: &EmbeddingEngine,
    embed_idx: &mut EmbeddingIndex,
) -> Result<AlignedEmbeddings, String> {
    // A resident index whose bodies were shrunk to snippets (post-embedding RAM
    // reclaim) must NEVER drive re-embedding: `files_needing_update` hashes
    // `c.content`, so truncated bodies would falsely flag every file as changed
    // and re-embed 5-line snippets over the full-body vectors persisted earlier
    // this session. Embeddings for exactly these chunks were already built and
    // saved before truncation, and alignment is keyed by (path, start, end) —
    // not content — so we just re-align here. If a file genuinely changed, the
    // BM25 cache fingerprint goes stale and a fresh full-content index (reloaded
    // from disk) replaces this one, restoring the normal re-embed path.
    if index.content_truncated {
        let aligned = embed_idx.get_aligned_flat(&index.chunks).ok_or_else(|| {
            "embedding alignment failed on truncated resident index; \
                 refusing to re-embed snippet-only bodies"
                .to_string()
        })?;
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage, Vec::new()));
    }

    let mut changed_files = embed_idx.files_needing_update(&index.chunks);
    changed_files.sort();
    changed_files.dedup();

    if !changed_files.is_empty() {
        let changed_set: std::collections::HashSet<&str> = changed_files
            .iter()
            .map(std::string::String::as_str)
            .collect();

        let mut changed_indices: Vec<usize> = Vec::new();
        let mut changed_texts: Vec<&str> = Vec::new();
        for (i, c) in index.chunks.iter().enumerate() {
            if changed_set.contains(c.file_path.as_str()) {
                changed_indices.push(i);
                changed_texts.push(&c.content);
            }
        }

        let batch_embeddings = engine
            .embed_batch(&changed_texts)
            .map_err(|e| format!("batch embed failed: {e}"))?;
        reject_under_hard_pressure("embedding update")?;

        let new_embeddings: Vec<(usize, Vec<f32>)> =
            changed_indices.into_iter().zip(batch_embeddings).collect();

        embed_idx.update(&index.chunks, &new_embeddings, &changed_files, None);
        embed_idx
            .save(root)
            .map_err(|e| format!("save embeddings failed: {e}"))?;
    }

    if let Some(aligned) = embed_idx.get_aligned_flat(&index.chunks) {
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage, changed_files));
    }

    // Alignment missing: rebuild everything once via batched inference.
    let mut all_files: Vec<String> = index.chunks.iter().map(|c| c.file_path.clone()).collect();
    all_files.sort();
    all_files.dedup();

    let all_texts: Vec<&str> = index.chunks.iter().map(|c| c.content.as_str()).collect();
    let batch_embeddings = engine
        .embed_batch(&all_texts)
        .map_err(|e| format!("batch embed failed: {e}"))?;
    reject_under_hard_pressure("embedding rebuild")?;

    let new_embeddings: Vec<(usize, Vec<f32>)> = batch_embeddings.into_iter().enumerate().collect();

    embed_idx.update(&index.chunks, &new_embeddings, &all_files, None);
    embed_idx
        .save(root)
        .map_err(|e| format!("save embeddings failed: {e}"))?;

    let aligned = embed_idx
        .get_aligned_flat(&index.chunks)
        .ok_or_else(|| "embedding alignment failed after full rebuild".to_string())?;
    let coverage = embed_idx.coverage(index.chunks.len());
    Ok((aligned, coverage, all_files))
}
