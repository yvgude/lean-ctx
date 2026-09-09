//! Parallel, deterministic BM25 index construction (#933).
//!
//! The full directory build was a sequential parse + tokenize loop over every
//! file that pinned a single core for ~2.85 s on the lean-ctx repo. Fanning that
//! work across a rayon pool brings it down to ~0.69 s (~4x), measured.
//!
//! The per-file work — tree-sitter chunking + tokenization — is pure and
//! thread-safe (the parser is `thread_local!`, the shared `content_cache` is
//! `Mutex`-guarded), so we fan it across a rayon pool with an order-preserving
//! `collect`, then **merge the results sequentially in the original sorted file
//! order**. The merge replays exactly what [`BM25Index::add_chunk`] does, so the
//! resulting index is identical to the sequential build (same chunk order, same
//! inverted postings, same `doc_freqs`) — upholding the determinism contract
//! (#498). See `tests.rs` (`parallel_build_matches_sequential_*`).

#[allow(clippy::wildcard_imports)]
use super::*;
use rayon::prelude::*;

/// Below this file count the rayon pool setup outweighs the win, so the
/// sequential path is used instead (it also carries the memory-pressure
/// early-break). Output is identical either way.
pub(super) const PARALLEL_MIN_FILES: usize = 32;

/// Upper bound for batch size — the parallel paths never exceed this even with
/// generous headroom. Lowered from 2000 to 500 after reports of 30 GB RSS
/// spikes: `par_iter().collect()` materializes the entire batch atomically,
/// so every file in the batch is live in RAM simultaneously.
const MAX_BATCH_FILES: usize = 500;

/// Minimum progress unit under low headroom.
const MIN_BATCH_FILES: usize = 1;

/// Conservative estimate of peak transient memory per file during BM25
/// preparation: file content (~20 KB avg) + tree-sitter chunks + lowered
/// token vectors + content-cache Arc. Higher than graph-scan because
/// chunk extraction and tokenization run concurrently per batch element.
const EST_TRANSIENT_PER_FILE: u64 = 256 * 1024;

fn effective_batch_size(max_files: usize) -> usize {
    crate::core::memory_guard::adaptive_batch_size(
        MIN_BATCH_FILES,
        max_files.clamp(1, MAX_BATCH_FILES),
        EST_TRANSIENT_PER_FILE,
    )
}

const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;

/// A chunk with its lowercased index tokens precomputed off the hot merge path.
struct PreparedChunk {
    chunk: CodeChunk,
    /// Lowercased tokens in `tokenize(enrich_for_bm25(chunk))` order — drives the
    /// inverted index and `doc_freqs` exactly as [`BM25Index::add_chunk`] would.
    lowered: Vec<String>,
}

/// All prepared chunks for a single file, plus the file state to record.
struct PreparedFile {
    rel: String,
    state: IndexedFileState,
    chunks: Vec<PreparedChunk>,
}

/// Between-batch guardian check for the parallel paths (#685). Returns `true`
/// when the build must stop now — either the guardian requested an abort
/// (Hard/Critical RSS) or soft pressure is on. Logs once with the position so
/// operators can see why an index ended up partial.
fn parallel_build_must_stop(what: &str, files_done: usize) -> bool {
    if crate::core::memory_guard::abort_requested() {
        tracing::warn!(
            "[{what}: aborting parallel build after {files_done} files due to critical memory pressure]"
        );
        return true;
    }
    if crate::core::memory_guard::is_under_pressure() {
        tracing::warn!(
            "[{what}: stopping parallel build after {files_done} files due to memory pressure]"
        );
        return true;
    }
    false
}

/// Pure, thread-safe per-chunk preparation: enrich → tokenize → lowercase.
/// Mirrors the first half of [`BM25Index::add_chunk`] so the cheap sequential
/// merge only has to update the shared maps.
fn prepare_chunk(mut chunk: CodeChunk) -> PreparedChunk {
    let enriched = enrich_for_bm25(&chunk);
    let tokens = tokenize(&enriched);
    let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    let token_count = tokens.len();

    // #790: truncate content AFTER tokenization (full text used for BM25 scoring
    // above). Must mirror add_chunk's SNIPPET_LINES to keep parallel ≡ sequential.
    const SNIPPET_LINES: usize = 10;
    if chunk.content.lines().nth(SNIPPET_LINES).is_some() {
        chunk.content = chunk
            .content
            .lines()
            .take(SNIPPET_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        chunk.content.shrink_to_fit();
    }

    PreparedChunk {
        chunk: CodeChunk {
            token_count,
            tokens: Vec::new(),
            ..chunk
        },
        lowered,
    }
}

/// Pure, thread-safe per-file work: resolve content (binary / hint / cache /
/// disk), extract + sort chunks, then prepare each. Returns `None` for files the
/// sequential build would `continue` past (missing, oversized, unreadable, or
/// empty after extraction) — keeping the two paths in lock-step.
fn prepare_file(
    root: &Path,
    rel: &str,
    content_hint: &HashMap<String, String>,
) -> Option<PreparedFile> {
    if crate::core::memory_guard::abort_requested() {
        return None;
    }
    let abs = root.join(rel);
    let state = IndexedFileState::from_path(&abs)?;
    if state.size_bytes > MAX_FILE_SIZE_BYTES {
        return None;
    }

    let cache_state = crate::core::content_cache::FileState {
        mtime_ms: state.mtime_ms,
        size_bytes: state.size_bytes,
    };
    let content: std::borrow::Cow<'_, str> = if crate::core::extractors::is_binary_document(&abs) {
        // Binary document (PDF, …): extract clean text from raw bytes. Skipped if
        // extraction yields nothing. Never populates the UTF-8 content cache.
        match std::fs::read(&abs) {
            Ok(bytes) => {
                let text = crate::core::extractors::extract(&abs, &bytes).text;
                if text.is_empty() {
                    return None;
                }
                std::borrow::Cow::Owned(text)
            }
            Err(_) => return None,
        }
    } else if let Some(cached) = content_hint.get(rel) {
        std::borrow::Cow::Borrowed(cached.as_str())
    } else if let Some(arc) = crate::core::content_cache::get(&abs, cache_state) {
        crate::core::cache::record_search_content_read(true);
        std::borrow::Cow::Owned(arc.to_string())
    } else {
        crate::core::cache::record_search_content_read(false);
        match std::fs::read_to_string(&abs) {
            Ok(c) => {
                crate::core::content_cache::insert(
                    &abs,
                    cache_state,
                    std::sync::Arc::from(c.as_str()),
                );
                std::borrow::Cow::Owned(c)
            }
            Err(_) => return None,
        }
    };

    if crate::core::memory_guard::abort_requested() {
        return None;
    }

    // #1739: bundled/minified payloads explode the chunker (one chunk per
    // nested scope, each carrying its full subtree text). Applied at the same
    // point in all three build paths so parallel stays identical to sequential.
    if looks_minified(&content) {
        tracing::debug!("[bm25: skipping minified payload {rel}]");
        return None;
    }

    let mut chunks = extract_chunks(rel, &content);
    chunks.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.end_line.cmp(&b.end_line))
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
    });

    Some(PreparedFile {
        rel: rel.to_string(),
        state,
        chunks: chunks.into_iter().map(prepare_chunk).collect(),
    })
}

/// Per-file work for the incremental rebuild, mirroring the sequential branch:
/// reuse the previous chunks when the file is unchanged and they carry content,
/// otherwise re-`prepare_file`. Crucially, reused chunks are re-prepared (enrich
/// → tokenize → lowercase) here too, so the *whole* tokenization — not just the
/// changed files — runs on the rayon pool, off the serial merge. The reuse guard
/// (`state` match + non-empty first chunk) and the changed-file fallthrough match
/// `rebuild_incremental_sequential` exactly, keeping the two paths in lock-step.
fn prepare_incremental_file(
    root: &Path,
    prev: &BM25Index,
    old_by_file: &HashMap<String, Vec<CodeChunk>>,
    content_hint: &HashMap<String, String>,
    rel: &str,
) -> Option<PreparedFile> {
    if crate::core::memory_guard::abort_requested() {
        return None;
    }
    let abs = root.join(rel);
    let state = IndexedFileState::from_path(&abs)?;

    let unchanged = prev.files.get(rel).is_some_and(|old| *old == state);
    if unchanged
        && let Some(chunks) = old_by_file.get(rel)
        && chunks.first().is_some_and(|c| !c.content.is_empty())
    {
        return Some(PreparedFile {
            rel: rel.to_string(),
            state,
            chunks: chunks.iter().cloned().map(prepare_chunk).collect(),
        });
    }

    // Changed / new / previously-empty: full prepare. The size guard and content
    // resolution live in `prepare_file`; for a changed file the resident content
    // cache fails its (mtime, size) validation and falls through to a fresh disk
    // read, so the bytes match the sequential path's direct read.
    prepare_file(root, rel, content_hint)
}

impl BM25Index {
    /// Deterministic parallel full build. Fans per-file parse + tokenize across a
    /// rayon pool (order-preserving `par_iter().collect()`), then merges
    /// sequentially in the sorted file order so the index is identical to
    /// [`Self::build_sequential`] for the same `files`.
    pub(crate) fn build_parallel(
        root: &Path,
        content_hint: &HashMap<String, String>,
        files: &[String],
    ) -> Self {
        Self::build_parallel_batched(root, content_hint, files, MAX_BATCH_FILES)
    }

    /// Batch-size-injectable core of [`Self::build_parallel`]. `max_batch_size`
    /// caps the upper bound; actual batch sizes adapt to RSS headroom each
    /// iteration. Testable without a 2000-file corpus.
    pub(super) fn build_parallel_batched(
        root: &Path,
        content_hint: &HashMap<String, String>,
        files: &[String],
        max_batch_size: usize,
    ) -> Self {
        let mut index = Self::new();
        let max_batch_size = max_batch_size.clamp(1, MAX_BATCH_FILES);
        let root_key = root.to_string_lossy().to_string();
        let total = files.len() as u64;
        crate::core::index_progress::report_bm25(&root_key, 0, total);
        let mut files_done = 0;
        while files_done < files.len() {
            if parallel_build_must_stop("bm25", files_done) {
                break;
            }
            let adaptive_size = effective_batch_size(max_batch_size);
            let batch_end = (files_done + adaptive_size).min(files.len());
            let prepared: Vec<Option<PreparedFile>> = files[files_done..batch_end]
                .par_iter()
                .map(|rel| prepare_file(root, rel, content_hint))
                .collect();
            for pf in prepared.into_iter().flatten() {
                for pc in pf.chunks {
                    index.add_prepared(pc);
                }
                index.files.insert(pf.rel, pf.state);
            }
            files_done = batch_end;
            crate::core::index_progress::report_bm25(&root_key, files_done as u64, total);
            crate::core::memory_guard::jemalloc_purge();
        }
        index.finalize();
        index
    }

    /// Deterministic parallel incremental rebuild (#581). Fans **all** per-file
    /// tokenization across the rayon pool — changed files through the full
    /// `prepare_file`, unchanged files by re-`prepare_chunk`-ing their reused
    /// chunks — then merges sequentially in file order via `add_prepared`. The
    /// result is identical to [`Self::rebuild_incremental_sequential`] for the same
    /// inputs (same file order, same chunk order, same postings / `doc_freqs`),
    /// upholding the determinism contract (#498). See `tests.rs`
    /// (`parallel_incremental_matches_sequential`).
    pub(crate) fn rebuild_incremental_parallel(
        root: &Path,
        prev: &BM25Index,
        old_by_file: &HashMap<String, Vec<CodeChunk>>,
        files: &[String],
    ) -> Self {
        // No per-build content hint for a rebuild; `prepare_file` falls back to the
        // resident content cache (validated) then disk.
        let empty_hint: HashMap<String, String> = HashMap::new();

        let mut index = Self::new();
        let root_key = root.to_string_lossy().to_string();
        let total = files.len() as u64;
        crate::core::index_progress::report_bm25(&root_key, 0, total);
        let mut files_done = 0;
        while files_done < files.len() {
            if parallel_build_must_stop("bm25-incr", files_done) {
                break;
            }
            let batch_size = effective_batch_size(MAX_BATCH_FILES);
            let batch_end = (files_done + batch_size).min(files.len());
            let prepared: Vec<Option<PreparedFile>> = files[files_done..batch_end]
                .par_iter()
                .map(|rel| prepare_incremental_file(root, prev, old_by_file, &empty_hint, rel))
                .collect();
            for pf in prepared.into_iter().flatten() {
                for pc in pf.chunks {
                    index.add_prepared(pc);
                }
                index.files.insert(pf.rel, pf.state);
            }
            files_done = batch_end;
            crate::core::index_progress::report_bm25(&root_key, files_done as u64, total);
            crate::core::memory_guard::jemalloc_purge();
        }
        index.finalize();
        index
    }

    /// Sequential full build with incremental memory-pressure guards. Used for
    /// small corpora and as the safe fallback when memory is tight.
    pub(crate) fn build_sequential(
        root: &Path,
        content_hint: &HashMap<String, String>,
        files: &[String],
    ) -> Self {
        let mut index = Self::new();
        let mut cache_hits = 0usize;
        let root_key = root.to_string_lossy().to_string();
        let total = files.len() as u64;
        crate::core::index_progress::report_bm25(&root_key, 0, total);

        for (i, rel) in files.iter().enumerate() {
            if i.is_multiple_of(16) || i + 1 == files.len() {
                crate::core::index_progress::report_bm25(&root_key, (i + 1) as u64, total);
            }
            if i.is_multiple_of(500) && crate::core::memory_guard::is_under_pressure() {
                tracing::warn!(
                    "[bm25: stopping build at file {i}/{} due to memory pressure]",
                    files.len()
                );
                break;
            }
            if crate::core::memory_guard::abort_requested() {
                tracing::warn!("[bm25: aborting build due to critical memory pressure]");
                break;
            }

            let abs = root.join(rel);
            let Some(state) = IndexedFileState::from_path(&abs) else {
                continue;
            };
            if state.size_bytes > MAX_FILE_SIZE_BYTES {
                continue;
            }

            // Content sources, cheapest first: an explicit per-build hint, then
            // the shared resident content cache (populated by the search-index
            // build / ctx_search, issue #148) validated by `(mtime, size)`, then
            // a one-time disk read that also publishes into the shared cache.
            let cache_state = crate::core::content_cache::FileState {
                mtime_ms: state.mtime_ms,
                size_bytes: state.size_bytes,
            };
            let content = if crate::core::extractors::is_binary_document(&abs) {
                match std::fs::read(&abs) {
                    Ok(bytes) => {
                        let text = crate::core::extractors::extract(&abs, &bytes).text;
                        if text.is_empty() {
                            continue;
                        }
                        std::borrow::Cow::Owned(text)
                    }
                    Err(_) => continue,
                }
            } else if let Some(cached) = content_hint.get(rel) {
                cache_hits += 1;
                std::borrow::Cow::Borrowed(cached.as_str())
            } else if let Some(arc) = crate::core::content_cache::get(&abs, cache_state) {
                crate::core::cache::record_search_content_read(true);
                cache_hits += 1;
                std::borrow::Cow::Owned(arc.to_string())
            } else {
                crate::core::cache::record_search_content_read(false);
                match std::fs::read_to_string(&abs) {
                    Ok(c) => {
                        crate::core::content_cache::insert(
                            &abs,
                            cache_state,
                            std::sync::Arc::from(c.as_str()),
                        );
                        std::borrow::Cow::Owned(c)
                    }
                    Err(_) => continue,
                }
            };

            // #1739: see `prepare_file` — same predicate, same position.
            if looks_minified(&content) {
                tracing::debug!("[bm25: skipping minified payload {rel}]");
                continue;
            }

            let mut chunks = extract_chunks(rel, &content);
            chunks.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then_with(|| a.end_line.cmp(&b.end_line))
                    .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            });
            for chunk in chunks {
                index.add_chunk(chunk);
            }
            index.files.insert(rel.clone(), state);
        }

        if cache_hits > 0 {
            tracing::info!(
                "[bm25: reused {cache_hits}/{} file contents from graph scan cache]",
                files.len()
            );
        }

        index.finalize();
        index
    }

    /// Merge a [`PreparedChunk`] into the index. Replays [`Self::add_chunk`]'s
    /// inverted-index / `doc_freqs` updates using the precomputed tokens, so a
    /// parallel build reaches the same state as the sequential one.
    fn add_prepared(&mut self, prepared: PreparedChunk) {
        let idx = self.chunks.len();
        for lower in &prepared.lowered {
            let postings = self.inverted.entry(lower.clone()).or_default();
            if postings.last().map(|(last_idx, _)| *last_idx) != Some(idx) {
                *self.doc_freqs.entry(lower.clone()).or_insert(0) += 1;
            }
            postings.push((idx, 1.0));
        }
        self.chunks.push(prepared.chunk);
    }
}
