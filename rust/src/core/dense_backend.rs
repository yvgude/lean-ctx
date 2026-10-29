#[cfg(feature = "embeddings")]
use std::path::Path;

#[cfg(feature = "embeddings")]
use crate::core::bm25_index::BM25Index;
#[cfg(any(feature = "qdrant", feature = "pgvector"))]
use crate::core::bm25_index::ChunkKind;
#[cfg(feature = "embeddings")]
use crate::core::hnsw::FlatEmbeddings;
#[cfg(feature = "embeddings")]
use crate::core::hybrid_search::{DenseSearchResult, HybridConfig, HybridResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenseBackendKind {
    Local,
    #[cfg(feature = "qdrant")]
    Qdrant,
    #[cfg(feature = "pgvector")]
    Pgvector,
}

impl DenseBackendKind {
    pub fn try_from_env() -> Result<Self, String> {
        let explicit = std::env::var("LEANCTX_DENSE_BACKEND")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());

        let inferred_qdrant =
            std::env::var("LEANCTX_QDRANT_URL").is_ok_and(|v| !v.trim().is_empty());
        let inferred_pgvector =
            std::env::var("LEANCTX_PGVECTOR_URL").is_ok_and(|v| !v.trim().is_empty());

        let requested = explicit
            .or_else(|| inferred_qdrant.then_some("qdrant".to_string()))
            .or_else(|| inferred_pgvector.then_some("pgvector".to_string()));

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
            Some("pgvector") => {
                #[cfg(feature = "pgvector")]
                {
                    Ok(Self::Pgvector)
                }
                #[cfg(not(feature = "pgvector"))]
                {
                    Err("Dense backend 'pgvector' requested, but feature 'pgvector' is not enabled. Rebuild with --features pgvector.".to_string())
                }
            }
            Some(other) => Err(format!(
                "Unknown LEANCTX_DENSE_BACKEND={other:?} (expected 'local', 'qdrant' or 'pgvector')"
            )),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "local",
            #[cfg(feature = "qdrant")]
            Self::Qdrant => "qdrant",
            #[cfg(feature = "pgvector")]
            Self::Pgvector => "pgvector",
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
    aligned_embeddings: &FlatEmbeddings,
    changed_files: &[String],
    query: &str,
    top_k: usize,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<HybridResult>, String> {
    // Pack and role pinned for the whole search; a change while it ran
    // returns nothing (`source_gate::pinned`).
    let dense =
        crate::core::bm25_index::source_gate::pinned(gate_root(backend, root, index), || {
            dense_results(
                backend,
                root,
                index,
                engine,
                aligned_embeddings,
                changed_files,
                query,
                top_k,
                filter,
            )
        })
        .unwrap_or_else(|| Ok(Vec::new()))?;

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

/// The root whose gate a search of `backend` decides under: the root the
/// index was handed out for (none for a view built in the running call), and
/// for a remote backend always the project root its rows belong to.
#[cfg(feature = "embeddings")]
fn gate_root<'a>(
    backend: DenseBackendKind,
    root: &'a Path,
    index: &'a BM25Index,
) -> Option<&'a Path> {
    if matches!(backend, DenseBackendKind::Local) {
        index.source_root.as_deref()
    } else {
        Some(root)
    }
}

/// Lexical + dense search with fusion. Pack and role are pinned for the
/// whole search — both candidate lists and every gate decision — and a
/// change of either while it ran returns nothing (`source_gate::pinned`).
#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
pub fn hybrid_results(
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
    crate::core::bm25_index::source_gate::pinned(gate_root(backend, root, index), || {
        hybrid_results_pinned(
            backend,
            root,
            index,
            engine,
            aligned_embeddings,
            changed_files,
            query,
            top_k,
            config,
            filter,
            graph_file_ranks,
        )
    })
    .unwrap_or_else(|| Ok(Vec::new()))
}

#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
fn hybrid_results_pinned(
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
        DenseBackendKind::Qdrant => remote_hybrid_results(
            backend,
            root,
            index,
            engine,
            aligned_embeddings,
            changed_files,
            query,
            top_k,
            config,
            filter,
            graph_file_ranks,
        ),
        #[cfg(feature = "pgvector")]
        DenseBackendKind::Pgvector => remote_hybrid_results(
            backend,
            root,
            index,
            engine,
            aligned_embeddings,
            changed_files,
            query,
            top_k,
            config,
            filter,
            graph_file_ranks,
        ),
    }
}

/// Shared BM25+dense RRF pipeline for remote vector backends (qdrant, pgvector):
/// the backends differ only in where `dense_results` fetches from.
#[cfg(all(feature = "embeddings", any(feature = "qdrant", feature = "pgvector")))]
#[allow(clippy::too_many_arguments)]
fn remote_hybrid_results(
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

#[cfg(feature = "embeddings")]
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
        #[cfg(feature = "pgvector")]
        DenseBackendKind::Pgvector => {
            let vecs: Vec<Vec<f32>> = (0..aligned_embeddings.n_vectors())
                .map(|i| aligned_embeddings.get_vec(i))
                .collect();
            dense_results_pgvector(
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

#[cfg(feature = "embeddings")]
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

    // Serve-time source gate (G04): the vector of a withdrawn source may still
    // be the nearest one, but its chunk is not served. A gated search fetches
    // a bounded surplus and cuts after the gate, so such a chunk neither
    // occupies a slot nor moves the cut.
    let mut gate = index.source_gate();
    let fetch = if gate.is_some() {
        crate::core::bm25_index::source_gate::overfetch(top_k)
    } else {
        top_k
    };
    let top = top_k_by_similarity(&query_embedding, aligned_embeddings, fetch, |i| {
        let Some(pred) = filter else { return true };
        index.chunks.get(i).is_some_and(|c| pred(&c.file_path))
    });

    Ok(top
        .into_iter()
        .filter_map(|(idx, sim)| {
            let chunk = index.chunks.get(idx)?;
            if !crate::core::bm25_index::source_gate::chunk_served(&mut gate, chunk) {
                return None;
            }
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
        .take(top_k)
        .collect())
}

/// Min-heap based Top-K selection over a flat embedding buffer.
/// O(n log k) instead of O(n log n) full sort. The filter is applied inline
/// during the scan so only matching chunks are considered — post-filtering
/// cannot drop below `top_k` results regardless of filter selectivity.
///
/// Uses sequential memory access (one dereference) via `FlatEmbeddings::get`,
/// unlike the old `Arc<[Vec<f32>]>` layout which had two-level indirection.
#[cfg(feature = "embeddings")]
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
        .embed_query(query)
        .map_err(|e| format!("embedding failed: {e}"))?;

    // A row of a source the admitted reader no longer returns is not served,
    // path and symbol name included (G04). Where a source authority is
    // required the backend ranks admitted sources only (`remote_candidates`).
    let mut gate = crate::core::bm25_index::source_gate::SourceGate::new(root, "ctx_search")
        .with_origins(&index.provider_origins);
    let hits = remote_candidates(
        &mut gate,
        index,
        query,
        top_k,
        |limit, sources| match sources {
            Some(sources) => store.search_within(&collection, &query_vec, limit, sources),
            None => store.search(&collection, &query_vec, limit),
        },
    )?;
    let mut out = Vec::with_capacity(top_k.min(hits.len()));
    let mut undigested = Vec::new();
    for hit in hits {
        if out.len() >= top_k {
            break;
        }
        if let Some(pred) = filter
            && !pred(&hit.file_path)
        {
            continue;
        }
        let row = RemoteRow {
            source: &hit.file_path,
            start_line: hit.start_line,
            end_line: hit.end_line,
            content_digest: hit.content_digest.as_deref(),
        };
        let Some(snippet) = remote_row_snippet(&mut gate, index, root, &row, &mut undigested)
        else {
            continue;
        };
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
    forget_deleted_sources(root, &gate, |gone| {
        store.sync_index(&collection, &BM25Index::new(), &[], gone, false)
    });
    resync_undigested(undigested, |sources| {
        store.sync_index(&collection, index, aligned_embeddings, sources, false)
    });
    Ok(out)
}

#[cfg(feature = "pgvector")]
#[cfg(feature = "embeddings")]
#[allow(clippy::too_many_arguments)]
fn dense_results_pgvector(
    root: &Path,
    index: &BM25Index,
    engine: &crate::core::embeddings::EmbeddingEngine,
    aligned_embeddings: &[Vec<f32>],
    changed_files: &[String],
    query: &str,
    top_k: usize,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<Vec<DenseSearchResult>, String> {
    let store = crate::core::pgvector_store::PgvectorStore::from_env()?;
    let table = store.table_name(root, engine.dimensions())?;
    let created_new = store.ensure_table(&table, engine.dimensions())?;
    store.sync_index(
        &table,
        index,
        aligned_embeddings,
        changed_files,
        created_new,
    )?;

    let query_vec = engine
        .embed_query(query)
        .map_err(|e| format!("embedding failed: {e}"))?;

    // As `dense_results_qdrant`: admitted sources only where a source
    // authority is required, a bounded surplus cut after the gate otherwise.
    let mut gate = crate::core::bm25_index::source_gate::SourceGate::new(root, "ctx_search")
        .with_origins(&index.provider_origins);
    let hits = remote_candidates(
        &mut gate,
        index,
        query,
        top_k,
        |limit, sources| match sources {
            Some(sources) => store.search_within(&table, &query_vec, limit, sources),
            None => store.search(&table, &query_vec, limit),
        },
    )?;
    let mut out = Vec::with_capacity(top_k.min(hits.len()));
    let mut undigested = Vec::new();
    for hit in hits {
        if out.len() >= top_k {
            break;
        }
        if let Some(pred) = filter
            && !pred(&hit.file_path)
        {
            continue;
        }
        let row = RemoteRow {
            source: &hit.file_path,
            start_line: hit.start_line,
            end_line: hit.end_line,
            content_digest: hit.content_digest.as_deref(),
        };
        let Some(snippet) = remote_row_snippet(&mut gate, index, root, &row, &mut undigested)
        else {
            continue;
        };
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
    forget_deleted_sources(root, &gate, |gone| {
        store.sync_index(&table, &BM25Index::new(), &[], gone, false)
    });
    resync_undigested(undigested, |sources| {
        store.sync_index(&table, index, aligned_embeddings, sources, false)
    });
    Ok(out)
}

/// The rows a remote dense search is served from, best first.
///
/// Without protection (Community developer mode): the backend's nearest
/// rows with a bounded surplus; the caller gates them and cuts afterwards —
/// today's behaviour.
///
/// Where a source authority is required the backend must not rank what may
/// not be served, and no unrestricted request is sent at all. The admitted
/// sources are decided FIRST, from what the Engine holds locally — the
/// sources of `index`, the index the remote table is synchronised from —
/// through the gate, in the deterministic bounded order of
/// `BM25Index::admitted_sources` (lexical candidates of the query, then the
/// rest by path, at most the gate's bound). The ONE request is then
/// restricted, in the backend, to those sources (`search_within`: a payload
/// filter in Qdrant, a bound variable in PostgreSQL, on the `file_path`
/// every row has carried since the tables exist). Rows of refused,
/// undecided or locally unknown sources are not ranked: no score, no place
/// before the limit.
///
/// No admitted source: no request, nothing served. A restriction the
/// backend cannot take (too large to bind) is an error, never an unfiltered
/// ranking. A corpus of more sources than the bound is searched in its
/// first sources of that order only (truncated closed). Cost under
/// protection: up to the bound in admitted source reads before the request
/// (the lexical search of the same call decides through its own gate).
#[cfg(all(feature = "embeddings", any(feature = "qdrant", feature = "pgvector")))]
fn remote_candidates<Hit>(
    gate: &mut crate::core::bm25_index::source_gate::SourceGate,
    index: &BM25Index,
    query: &str,
    top_k: usize,
    search: impl Fn(usize, Option<&[String]>) -> Result<Vec<Hit>, String>,
) -> Result<Vec<Hit>, String> {
    let fetch = crate::core::bm25_index::source_gate::overfetch(top_k);
    if !gate.protected() {
        return search(fetch, None);
    }
    let admitted = index.admitted_sources(query, gate);
    if admitted.is_empty() {
        return Ok(Vec::new());
    }
    search(fetch, Some(&admitted))
}

/// What a remote row says about the chunk it was written for.
#[cfg(any(feature = "qdrant", feature = "pgvector"))]
struct RemoteRow<'a> {
    source: &'a str,
    start_line: usize,
    end_line: usize,
    /// `external_origin::chunk_digest` of the chunk, on rows written since
    /// the stores keep it.
    content_digest: Option<&'a str>,
}

/// The snippet of a remote row that may be served now; `None` when it may
/// not.
///
/// A row of a project file is served while the admitted reader still
/// returns the lines it names. A row of an external chunk (`scheme://…`)
/// has no file to read; where a source authority is required it is served
/// only as the chunk the local index holds: the row's content digest must
/// be the digest of a chunk of that source in `index`, and the gate must
/// serve that chunk — for an origin bound to one text that is the digest in
/// the origin on record. A row without a digest (written before the stores
/// kept one) is not served there and its source is noted in `undigested`
/// for re-synchronisation; it is never served unverified.
#[cfg(any(feature = "qdrant", feature = "pgvector"))]
fn remote_row_snippet(
    gate: &mut crate::core::bm25_index::source_gate::SourceGate,
    index: &BM25Index,
    root: &Path,
    row: &RemoteRow<'_>,
    undigested: &mut Vec<String>,
) -> Option<String> {
    use crate::core::bm25_index::external_origin::chunk_digest;
    if row.source.contains("://") && gate.protected() {
        let Some(digest) = row.content_digest else {
            undigested.push(row.source.to_owned());
            return None;
        };
        let chunk = index.chunks.iter().find(|chunk| {
            chunk.file_path == row.source && chunk_digest(&chunk.content) == digest
        })?;
        return gate
            .serves(&chunk.file_path, &chunk.content)
            .then(|| chunk.content.lines().take(5).collect::<Vec<_>>().join("\n"));
    }
    let snippet = snippet_from_disk(root, row.source, row.start_line, row.end_line, 5);
    gate.serves(row.source, &snippet).then_some(snippet)
}

/// Write the rows of external sources that were found without a content
/// digest again, through the store's replace path (`sync_index` deletes the
/// source's rows and upserts its current chunks, now with their digest).
/// Bounded and best effort: until it succeeds those rows stay unserved.
#[cfg(any(feature = "qdrant", feature = "pgvector"))]
fn resync_undigested(
    mut sources: Vec<String>,
    resync: impl FnOnce(&[String]) -> Result<(), String>,
) {
    /// Sources one search writes again.
    const MAX_RESYNCED_SOURCES: usize = 8;
    sources.sort();
    sources.dedup();
    sources.truncate(MAX_RESYNCED_SOURCES);
    if !sources.is_empty() && resync(&sources).is_err() {
        tracing::debug!("[dense] rows without a content digest could not be written again");
    }
}

/// Remove the rows of the sources `gate` refused that no longer exist from a
/// remote table, through the store's own replace path (`sync_index` deletes a
/// changed file's rows and upserts its chunks — none, for an empty index).
/// Only a deleted source: admission is the reader's, the table is shared, so
/// a refusal for this reader must not remove a row for another. Bounded and
/// best effort — a failed delete leaves rows the gate keeps refusing.
#[cfg(any(feature = "qdrant", feature = "pgvector"))]
fn forget_deleted_sources(
    root: &Path,
    gate: &crate::core::bm25_index::source_gate::SourceGate,
    delete: impl FnOnce(&[String]) -> Result<(), String>,
) {
    /// Deleted sources one search removes from the remote table.
    const MAX_DELETED_SOURCES: usize = 8;
    let mut gone: Vec<String> = gate
        .refused_sources()
        .filter(|source| !source.contains("://") && !root.join(source).exists())
        .map(str::to_owned)
        .collect();
    gone.sort();
    gone.truncate(MAX_DELETED_SOURCES);
    if !gone.is_empty() && delete(&gone).is_err() {
        tracing::debug!("[dense] rows of deleted sources could not be removed");
    }
}

#[cfg(any(feature = "qdrant", feature = "pgvector"))]
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
    // Snippets are served to the model: admitted text only (G5).
    let Some(content) = crate::core::context_admission::stores::admitted_store_text(&path) else {
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

#[cfg(any(feature = "qdrant", feature = "pgvector"))]
pub fn kind_to_str(kind: &ChunkKind) -> &'static str {
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

#[cfg(any(feature = "qdrant", feature = "pgvector"))]
pub fn kind_from_str(s: &str) -> ChunkKind {
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
pub mod tests {
    use super::*;
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
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", None);
        let old_pg = set_env("LEANCTX_PGVECTOR_URL", None);

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Local);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
        restore_env("LEANCTX_PGVECTOR_URL", old_pg);
    }

    #[test]
    fn dense_backend_unknown_value_errors() {
        let _g = crate::core::data_dir::test_env_lock();
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
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", Some("http://127.0.0.1:6333"));
        let old_pg = set_env("LEANCTX_PGVECTOR_URL", None);

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Qdrant);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
        restore_env("LEANCTX_PGVECTOR_URL", old_pg);
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn dense_backend_infers_pgvector_from_url() {
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", None);
        let old_pg = set_env("LEANCTX_PGVECTOR_URL", Some("postgres://localhost/lctx"));

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Pgvector);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
        restore_env("LEANCTX_PGVECTOR_URL", old_pg);
    }

    #[cfg(all(feature = "qdrant", feature = "pgvector"))]
    #[test]
    fn dense_backend_qdrant_wins_when_both_urls_set() {
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", None);
        let old_url = set_env("LEANCTX_QDRANT_URL", Some("http://127.0.0.1:6333"));
        let old_pg = set_env("LEANCTX_PGVECTOR_URL", Some("postgres://localhost/lctx"));

        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Qdrant);

        // Explicit selection overrides inference order.
        crate::test_env::set_var("LEANCTX_DENSE_BACKEND", "pgvector");
        let got = DenseBackendKind::try_from_env().unwrap();
        assert_eq!(got, DenseBackendKind::Pgvector);

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
        restore_env("LEANCTX_PGVECTOR_URL", old_pg);
    }

    #[cfg(not(feature = "qdrant"))]
    #[test]
    fn dense_backend_qdrant_requires_feature() {
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", Some("qdrant"));
        let old_url = set_env("LEANCTX_QDRANT_URL", None);

        let err = DenseBackendKind::try_from_env().unwrap_err();
        assert!(err.contains("feature 'qdrant' is not enabled"));

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }

    #[cfg(not(feature = "pgvector"))]
    #[test]
    fn dense_backend_pgvector_requires_feature() {
        let _g = crate::core::data_dir::test_env_lock();
        let old_backend = set_env("LEANCTX_DENSE_BACKEND", Some("pgvector"));
        let old_url = set_env("LEANCTX_QDRANT_URL", None);

        let err = DenseBackendKind::try_from_env().unwrap_err();
        assert!(err.contains("feature 'pgvector' is not enabled"));

        restore_env("LEANCTX_DENSE_BACKEND", old_backend);
        restore_env("LEANCTX_QDRANT_URL", old_url);
    }
}
