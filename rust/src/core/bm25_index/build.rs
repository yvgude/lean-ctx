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

/// Pure, thread-safe per-chunk preparation: enrich → tokenize → lowercase.
/// Mirrors the first half of [`BM25Index::add_chunk`] so the cheap sequential
/// merge only has to update the shared maps.
fn prepare_chunk(chunk: CodeChunk) -> PreparedChunk {
    let enriched = enrich_for_bm25(&chunk);
    let tokens = tokenize(&enriched);
    let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    let token_count = tokens.len();
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
        std::borrow::Cow::Owned(arc.to_string())
    } else {
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
        // Order-preserving: `map().collect()` into a `Vec` keeps the input order,
        // so the merge below sees files in the same sorted order the sequential
        // path iterates — the foundation of identical output.
        let prepared: Vec<Option<PreparedFile>> = files
            .par_iter()
            .map(|rel| prepare_file(root, rel, content_hint))
            .collect();

        let mut index = Self::new();
        for pf in prepared.into_iter().flatten() {
            for pc in pf.chunks {
                index.add_prepared(pc);
            }
            index.files.insert(pf.rel, pf.state);
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

        // Order-preserving `par_iter().collect()` keeps the input file order, so
        // the merge below sees files in the same order the sequential rebuild
        // iterates — the foundation of identical output.
        let prepared: Vec<Option<PreparedFile>> = files
            .par_iter()
            .map(|rel| prepare_incremental_file(root, prev, old_by_file, &empty_hint, rel))
            .collect();

        let mut index = Self::new();
        for pf in prepared.into_iter().flatten() {
            for pc in pf.chunks {
                index.add_prepared(pc);
            }
            index.files.insert(pf.rel, pf.state);
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

        for (i, rel) in files.iter().enumerate() {
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
                cache_hits += 1;
                std::borrow::Cow::Owned(arc.to_string())
            } else {
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
