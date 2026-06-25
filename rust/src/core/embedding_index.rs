//! Persistent, incremental embedding index.
//!
//! Stores pre-computed chunk embeddings alongside file content hashes.
//! On re-index, only files whose hash has changed get re-embedded,
//! avoiding expensive model inference for unchanged code.
//!
//! Storage format: `~/.lean-ctx/vectors/<project_hash>/embeddings.bin` (postcard)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::chunk_data::CodeChunk;
use super::embedding_quant::{self, QuantizedVector};
use super::hnsw::FlatEmbeddings;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingIndex {
    pub version: u32,
    pub dimensions: usize,
    /// Model identifier that generated these embeddings.
    /// Used for mismatch detection when the user switches models.
    #[serde(default)]
    pub model_id: Option<String>,
    pub entries: Vec<EmbeddingEntry>,
    pub file_hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingEntry {
    pub file_path: String,
    pub symbol_name: String,
    pub start_line: usize,
    pub end_line: usize,
    /// int8-quantized embedding (turbovec-derived) — 4× smaller on disk.
    pub quant: QuantizedVector,
    pub content_hash: String,
}

impl EmbeddingEntry {
    /// Write the dequantized embedding directly into `dest`, avoiding
    /// intermediate `Vec<f32>` allocation.
    fn write_into_flat(&self, dest: &mut Vec<f32>) {
        let q = &self.quant;
        let scale = q.scale;
        if scale == 0.0 {
            dest.resize(dest.len() + q.code.len(), 0.0);
        } else {
            for &c in &q.code {
                dest.push(f32::from(c) * scale);
            }
        }
    }
}

/// Outcome of a `build_or_update` call from the index orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingBuildOutcome {
    /// Embeddings were built or already up-to-date.
    Ready,
    /// Skipped because the embeddings feature is not enabled or disabled by config.
    Skipped,
    /// The embedding engine (ONNX model) is not available.
    /// Carries the reason so the orchestrator can show a helpful message.
    ModelNotAvailable(String),
    /// Build failed with an error.
    Failed,
}

impl EmbeddingBuildOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Skipped => "skipped",
            Self::ModelNotAvailable(_) => "model-not-available",
            Self::Failed => "failed",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::ModelNotAvailable(r) => Some(r.as_str()),
            _ => None,
        }
    }
}

/// Build or update the persistent embedding index for a project.
///
/// Called by the index orchestrator during `build-full` / `build` after the BM25
/// index is ready.  This is a *background-friendly* operation: it runs the ONNX
/// model incrementally (only chunks whose content hash changed) and persists
/// `embeddings.bin` (postcard) so subsequent `ctx_semantic_search` calls find a warm cache.
///
/// Returns [`EmbeddingBuildOutcome`] — the orchestrator uses this to set the
/// semantic component state without aborting the overall index build.
///
/// Feature-gated: when `embeddings` is not compiled in, this is a no-op that
/// returns `Skipped`.
pub fn build_or_update(root: &Path, bm25: &super::chunk_data::ChunkData) -> EmbeddingBuildOutcome {
    #[cfg(feature = "embeddings")]
    {
        // Respect the config gates so the orchestrator does not force-embed when
        // the user explicitly opted out.
        let cfg = crate::core::config::Config::load();
        if !cfg.search.dense_enabled {
            tracing::info!("[embedding_index] build_or_update skipped: search.dense_enabled=false");
            return EmbeddingBuildOutcome::Skipped;
        }
        let profile = crate::core::config::MemoryProfile::effective(&cfg);
        if !profile.embeddings_enabled() {
            tracing::info!(
                "[embedding_index] build_or_update skipped: memory_profile disables embeddings"
            );
            return EmbeddingBuildOutcome::Skipped;
        }

        // Bootstrap the model if it isn't on disk yet. `build_or_update` only
        // runs for an explicit build request (`index build` / `build-full` /
        // `build-semantic`), so a cold machine should download the model now
        // rather than dead-end. `is_available()` is just a file check, so the
        // earlier short-circuit on it meant the auto-download never started
        // (#545). `ensure_downloaded()` is pure network/file IO and never
        // initializes ORT, so the teardown-safety rationale for deferring the
        // `shared_engine()` load still holds: on download failure we return
        // without ever having touched the ONNX Runtime.
        if !crate::core::embeddings::EmbeddingEngine::is_available() {
            tracing::info!(
                "[embedding_index] embedding model absent — downloading from HuggingFace"
            );
            if let Err(e) = crate::core::embeddings::EmbeddingEngine::ensure_downloaded() {
                let reason = format!("embedding model auto-download from HuggingFace failed: {e}");
                tracing::warn!("[embedding_index] build_or_update failed: {reason}");
                return EmbeddingBuildOutcome::ModelNotAvailable(reason);
            }
        }

        let Some(engine) = crate::core::embeddings::shared_engine() else {
            let reason = "embedding model files found but engine failed to load (check logs / RUST_LOG=info)";
            tracing::info!("[embedding_index] build_or_update skipped: {reason}");
            return EmbeddingBuildOutcome::ModelNotAvailable(reason.to_string());
        };

        let model_name = engine.model_name();
        let mut idx = EmbeddingIndex::load(root)
            .unwrap_or_else(|| EmbeddingIndex::new_with_model(engine.dimensions(), model_name));

        // Detect model / dimension changes → rebuild from scratch.
        if let Some((stored, current)) = idx.model_mismatch(model_name) {
            tracing::info!(
                "[embedding_index] model changed: {stored} → {current}. Re-building from scratch."
            );
            idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
        } else if idx.dimension_mismatch(engine.dimensions()) {
            tracing::info!(
                "[embedding_index] dimension mismatch: index={}d, engine={}d. Re-building.",
                idx.dimensions,
                engine.dimensions()
            );
            idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
        }

        let mut changed_files = idx.files_needing_update(&bm25.chunks);
        changed_files.sort();
        changed_files.dedup();

        if changed_files.is_empty() {
            tracing::info!(
                "[embedding_index] all {} chunks up-to-date, nothing to embed",
                bm25.chunks.len()
            );
            return EmbeddingBuildOutcome::Ready;
        }

        let changed_set: std::collections::HashSet<&str> =
            changed_files.iter().map(String::as_str).collect();
        let mut changed_indices: Vec<usize> = Vec::new();
        let mut changed_texts: Vec<&str> = Vec::new();
        for (i, c) in bm25.chunks.iter().enumerate() {
            if changed_set.contains(c.file_path.as_str()) {
                changed_indices.push(i);
                changed_texts.push(&c.content);
            }
        }

        let count = changed_files.len();
        tracing::info!(
            "[embedding_index] embedding {count} changed files ({total} chunks in index)",
            total = bm25.chunks.len()
        );

        let batch_embeddings = match engine.embed_batch(&changed_texts) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("[embedding_index] batch embed failed: {e}");
                return EmbeddingBuildOutcome::Failed;
            }
        };

        let new_embeddings: Vec<(usize, Vec<f32>)> =
            changed_indices.into_iter().zip(batch_embeddings).collect();

        idx.update(&bm25.chunks, &new_embeddings, &changed_files, None);

        if let Err(e) = idx.save(root) {
            tracing::error!("[embedding_index] save failed: {e}");
            return EmbeddingBuildOutcome::Failed;
        }

        tracing::info!(
            "[embedding_index] successfully persisted {count} file embeddings ({total} chunks)",
            total = bm25.chunks.len()
        );
        EmbeddingBuildOutcome::Ready
    }

    #[cfg(not(feature = "embeddings"))]
    {
        let _ = (root, bm25);
        EmbeddingBuildOutcome::Skipped
    }
}

/// Current on-disk format version. Used for forward-compatibility checks.
const CURRENT_VERSION: u32 = 3;

impl EmbeddingIndex {
    pub fn new(dimensions: usize) -> Self {
        Self {
            version: CURRENT_VERSION,
            dimensions,
            model_id: None,
            entries: Vec::new(),
            file_hashes: HashMap::new(),
        }
    }

    /// Create a new index tagged with a specific model identity.
    pub fn new_with_model(dimensions: usize, model_id: &str) -> Self {
        Self {
            version: CURRENT_VERSION,
            dimensions,
            model_id: Some(model_id.to_string()),
            entries: Vec::new(),
            file_hashes: HashMap::new(),
        }
    }

    /// Check if the index was built with a different model than currently selected.
    /// Returns `Some((stored_model, current_model))` on mismatch, `None` if compatible.
    pub fn model_mismatch<'a>(&'a self, current_model: &'a str) -> Option<(&'a str, &'a str)> {
        match &self.model_id {
            Some(stored) if stored != current_model => Some((stored, current_model)),
            _ => None,
        }
    }

    /// Check if index dimensions are incompatible with the current engine.
    pub fn dimension_mismatch(&self, engine_dimensions: usize) -> bool {
        self.dimensions != engine_dimensions && !self.entries.is_empty()
    }

    /// Approximate heap memory used by this index in bytes.
    pub fn memory_usage_bytes(&self) -> usize {
        let entries_size: usize = self
            .entries
            .iter()
            .map(|e| {
                e.file_path.len()
                    + e.symbol_name.len()
                    + e.content_hash.len()
                    + e.quant.code.len()
                    + 4
                    + 48
            })
            .sum();
        let hashes_size: usize = self
            .file_hashes
            .iter()
            .map(|(k, v)| k.len() + v.len() + 32)
            .sum();
        entries_size + hashes_size
    }

    /// Drops all in-memory data to free heap. Index can be re-loaded from disk.
    pub fn unload(&mut self) {
        let usage = self.memory_usage_bytes();
        self.entries = Vec::new();
        self.file_hashes = HashMap::new();
        tracing::info!(
            "[embeddings] unloaded index, freed ~{:.1}MB",
            usage as f64 / 1_048_576.0
        );
    }

    /// Load a previously saved index, or create a new empty one.
    pub fn load_or_new(root: &Path, dimensions: usize) -> Self {
        Self::load(root).unwrap_or_else(|| Self::new(dimensions))
    }

    /// Determine which files need re-embedding based on content hashes.
    ///
    /// When the index is empty (no prior embeddings), skips hash computation
    /// entirely by returning all unique file paths from chunks directly.
    pub fn files_needing_update(&self, chunks: &[CodeChunk]) -> Vec<String> {
        // Empty index: every file needs embedding — skip O(chunks) hash iteration.
        if self.file_hashes.is_empty() {
            let mut files: Vec<String> = chunks.iter().map(|c| c.file_path.clone()).collect();
            files.sort();
            files.dedup();
            return files;
        }

        let current_hashes = compute_file_hashes(chunks);

        let mut needs_update = Vec::new();
        for (file, hash) in &current_hashes {
            match self.file_hashes.get(file) {
                Some(old_hash) if old_hash == hash => {}
                _ => needs_update.push(file.clone()),
            }
        }

        for file in self.file_hashes.keys() {
            if !current_hashes.contains_key(file) {
                needs_update.push(file.clone());
            }
        }

        needs_update
    }

    /// Number of `chunks` a re-embed pass would have to embed right now — i.e.
    /// the chunks belonging to files flagged by [`Self::files_needing_update`].
    ///
    /// Used by the hybrid/dense cold-start guard (#512): on a server that came
    /// up before the on-disk index existed, the first query would otherwise embed
    /// the *entire* corpus inline under the request watchdog, producing a runaway
    /// the watchdog abandons but cannot cancel. Counting the pending chunks up
    /// front lets the caller fall back instead of starting that embed.
    pub fn pending_chunk_count(&self, chunks: &[CodeChunk]) -> usize {
        let changed = self.files_needing_update(chunks);
        if changed.is_empty() {
            return 0;
        }
        let changed: std::collections::HashSet<&str> = changed.iter().map(String::as_str).collect();
        chunks
            .iter()
            .filter(|c| changed.contains(c.file_path.as_str()))
            .count()
    }

    /// Update the index with new embeddings for changed files.
    /// Preserves existing embeddings for unchanged files.
    ///
    /// `precomputed_hashes` can be passed to avoid re-computing file hashes
    /// when the caller already has them (e.g. from `files_needing_update`).
    /// When `None`, hashes are computed from `chunks`.
    pub fn update(
        &mut self,
        chunks: &[CodeChunk],
        new_embeddings: &[(usize, Vec<f32>)],
        changed_files: &[String],
        precomputed_hashes: Option<HashMap<String, String>>,
    ) {
        self.entries
            .retain(|e| !changed_files.contains(&e.file_path));

        for file in changed_files {
            self.file_hashes.remove(file);
        }

        let current_hashes = precomputed_hashes.unwrap_or_else(|| compute_file_hashes(chunks));
        for file in changed_files {
            if let Some(hash) = current_hashes.get(file) {
                self.file_hashes.insert(file.clone(), hash.clone());
            }
        }

        for &(chunk_idx, ref embedding) in new_embeddings {
            if let Some(chunk) = chunks.get(chunk_idx) {
                let content_hash = hash_content(&chunk.content);
                self.entries.push(EmbeddingEntry {
                    file_path: chunk.file_path.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    quant: embedding_quant::quantize(embedding),
                    content_hash,
                });
            }
        }
    }

    /// Get all embeddings in chunk order (aligned with BM25Index.chunks) as a
    /// single contiguous [`FlatEmbeddings`] allocation. Returns None if the index
    /// doesn't cover all chunks.
    ///
    /// The flat layout (_n_vectors × _dim_ in row-major order) gives sequential
    /// memory access during dot-product scoring — one dereference instead of the
    /// two-level indirection of `Arc<[Vec<f32>]>`.
    pub fn get_aligned_flat(&self, chunks: &[CodeChunk]) -> Option<FlatEmbeddings> {
        let dim = self.dimensions;
        let mut map: HashMap<(&str, usize, usize), &EmbeddingEntry> =
            HashMap::with_capacity(self.entries.len());
        for e in &self.entries {
            map.insert((e.file_path.as_str(), e.start_line, e.end_line), e);
        }

        let n = chunks.len();
        let mut data = Vec::with_capacity(n * dim);
        for chunk in chunks {
            let entry = map.get(&(chunk.file_path.as_str(), chunk.start_line, chunk.end_line))?;
            entry.write_into_flat(&mut data);
        }
        Some(FlatEmbeddings {
            data: Arc::from(data),
            dim,
        })
    }

    pub fn coverage(&self, total_chunks: usize) -> f64 {
        if total_chunks == 0 {
            return 0.0;
        }
        self.entries.len() as f64 / total_chunks as f64
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = index_dir(root);
        std::fs::create_dir_all(&dir)?;
        // Binary (postcard) — compact, fast, deterministic.
        let data = postcard::to_allocvec(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("embeddings.bin"), data)?;
        Ok(())
    }

    pub fn load(root: &Path) -> Option<Self> {
        let bin_path = index_dir(root).join("embeddings.bin");
        let data = std::fs::read(&bin_path).ok()?;
        match postcard::from_bytes::<Self>(&data) {
            // Only accept an index whose on-disk schema matches the current one.
            Ok(idx) if idx.version == CURRENT_VERSION => Some(idx),
            // A structurally-valid but stale schema (older/newer bin layout) must
            // not be trusted — postcard can silently mis-decode a changed struct.
            // Drop it and rebuild rather than serving garbage vectors.
            Ok(idx) => {
                tracing::warn!(
                    "[embeddings] index format v{} != current v{CURRENT_VERSION} — \
                     removing and rebuilding from scratch",
                    idx.version
                );
                let _ = std::fs::remove_file(&bin_path);
                None
            }
            Err(_) => {
                tracing::warn!(
                    "[embeddings] corrupt embeddings.bin — removing and will rebuild from scratch"
                );
                let _ = std::fs::remove_file(&bin_path);
                None
            }
        }
    }
}

fn index_dir(root: &Path) -> PathBuf {
    crate::core::index_namespace::vectors_dir(root)
}

fn hash_content(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

fn compute_file_hashes(chunks: &[CodeChunk]) -> HashMap<String, String> {
    let mut by_file: HashMap<&str, Vec<&CodeChunk>> = HashMap::new();
    for chunk in chunks {
        by_file
            .entry(chunk.file_path.as_str())
            .or_default()
            .push(chunk);
    }

    let mut out: HashMap<String, String> = HashMap::with_capacity(by_file.len());
    for (file, mut file_chunks) in by_file {
        file_chunks.sort_by(|a, b| {
            (a.start_line, a.end_line, a.symbol_name.as_str()).cmp(&(
                b.start_line,
                b.end_line,
                b.symbol_name.as_str(),
            ))
        });

        let mut hasher = Md5::new();
        hasher.update(file.as_bytes());
        for c in file_chunks {
            hasher.update(c.start_line.to_le_bytes());
            hasher.update(c.end_line.to_le_bytes());
            hasher.update(c.symbol_name.as_bytes());
            hasher.update([kind_tag(&c.kind)]);
            hasher.update(c.content.as_bytes());
        }
        out.insert(
            file.to_string(),
            crate::core::agent_identity::hex_encode(&hasher.finalize()),
        );
    }
    out
}

fn kind_tag(kind: &super::chunk_data::ChunkKind) -> u8 {
    use super::chunk_data::ChunkKind;
    match kind {
        ChunkKind::Function => 1,
        ChunkKind::Struct => 2,
        ChunkKind::Impl => 3,
        ChunkKind::Module => 4,
        ChunkKind::Class => 5,
        ChunkKind::Method => 6,
        ChunkKind::Other => 7,
        ChunkKind::Issue => 8,
        ChunkKind::PullRequest => 9,
        ChunkKind::WikiPage => 10,
        ChunkKind::DbSchema => 11,
        ChunkKind::ApiEndpoint => 12,
        ChunkKind::Ticket => 13,
        ChunkKind::ExternalOther => 14,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::{ChunkKind, CodeChunk};

    fn make_chunk(file: &str, name: &str, content: &str, start: usize, end: usize) -> CodeChunk {
        CodeChunk {
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind: ChunkKind::Function,
            start_line: start,
            end_line: end,
            content: content.to_string(),
            tokens: vec![name.to_string()],
            token_count: 1,
        }
    }

    fn dummy_embedding(dim: usize) -> Vec<f32> {
        vec![0.1; dim]
    }

    #[test]
    fn new_index_is_empty() {
        let idx = EmbeddingIndex::new(384);
        assert!(idx.entries.is_empty());
        assert!(idx.file_hashes.is_empty());
        assert_eq!(idx.dimensions, 384);
    }

    #[test]
    fn files_needing_update_all_new() {
        let idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        let needs = idx.files_needing_update(&chunks);
        assert_eq!(needs.len(), 2);
    }

    #[test]
    fn files_needing_update_unchanged() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];

        idx.update(
            &chunks,
            &[(0, dummy_embedding(384))],
            &["a.rs".to_string()],
            None,
        );

        let needs = idx.files_needing_update(&chunks);
        assert!(needs.is_empty(), "unchanged file should not need update");
    }

    #[test]
    fn files_needing_update_changed_content() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks_v1 = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(
            &chunks_v1,
            &[(0, dummy_embedding(384))],
            &["a.rs".to_string()],
            None,
        );

        let chunks_v2 = vec![make_chunk("a.rs", "fn_a", "fn a() { modified }", 1, 3)];
        let needs = idx.files_needing_update(&chunks_v2);
        assert!(
            needs.contains(&"a.rs".to_string()),
            "changed file should need update"
        );
    }

    #[test]
    fn files_needing_update_detects_change_in_later_chunk() {
        let mut idx = EmbeddingIndex::new(3);
        let chunks_v1 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() {}", 10, 12),
        ];
        idx.update(
            &chunks_v1,
            &[(0, vec![0.1, 0.1, 0.1]), (1, vec![0.2, 0.2, 0.2])],
            &["a.rs".to_string()],
            None,
        );

        let chunks_v2 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() { changed }", 10, 12),
        ];
        let needs = idx.files_needing_update(&chunks_v2);
        assert!(
            needs.contains(&"a.rs".to_string()),
            "changing a later chunk should trigger re-embedding"
        );
    }

    #[test]
    fn files_needing_update_deleted_file() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384)), (1, dummy_embedding(384))],
            &["a.rs".to_string(), "b.rs".to_string()],
            None,
        );

        let chunks_after = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        let needs = idx.files_needing_update(&chunks_after);
        assert!(
            needs.contains(&"b.rs".to_string()),
            "deleted file should trigger update"
        );
    }

    #[test]
    fn pending_chunk_count_cold_start_counts_every_chunk() {
        let idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() {}", 10, 12),
            make_chunk("b.rs", "fn_c", "fn c() {}", 1, 3),
        ];
        assert_eq!(
            idx.pending_chunk_count(&chunks),
            3,
            "an empty index must report every chunk as pending (cold start)"
        );
    }

    #[test]
    fn pending_chunk_count_zero_when_fully_embedded() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384)), (1, dummy_embedding(384))],
            &["a.rs".to_string(), "b.rs".to_string()],
            None,
        );
        assert_eq!(
            idx.pending_chunk_count(&chunks),
            0,
            "a fully-embedded index has no pending chunks (warm path stays inline)"
        );
    }

    #[test]
    fn pending_chunk_count_only_counts_changed_files_chunks() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks_v1 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() {}", 10, 12),
            make_chunk("b.rs", "fn_c", "fn c() {}", 1, 3),
        ];
        idx.update(
            &chunks_v1,
            &[
                (0, dummy_embedding(384)),
                (1, dummy_embedding(384)),
                (2, dummy_embedding(384)),
            ],
            &["a.rs".to_string(), "b.rs".to_string()],
            None,
        );

        // Only b.rs changed → its single chunk is pending, a.rs's two are not.
        let chunks_v2 = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("a.rs", "fn_b", "fn b() {}", 10, 12),
            make_chunk("b.rs", "fn_c", "fn c() { changed }", 1, 3),
        ];
        assert_eq!(
            idx.pending_chunk_count(&chunks_v2),
            1,
            "incremental update must only count the changed file's chunks"
        );
    }

    #[test]
    fn update_preserves_unchanged() {
        let mut idx = EmbeddingIndex::new(384);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384)), (1, dummy_embedding(384))],
            &["a.rs".to_string(), "b.rs".to_string()],
            None,
        );
        assert_eq!(idx.entries.len(), 2);

        idx.update(&chunks, &[(0, vec![0.5; 384])], &["a.rs".to_string()], None);
        assert_eq!(idx.entries.len(), 2);

        let b_entry = idx.entries.iter().find(|e| e.file_path == "b.rs").unwrap();
        let b_embed = b_entry.quant.dequantize();
        assert!(
            (b_embed[0] - 0.1).abs() < 1e-6,
            "b.rs embedding should be preserved"
        );
    }

    #[test]
    fn get_aligned_flat_ok() {
        let mut idx = EmbeddingIndex::new(2);
        let chunks = vec![
            make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3),
            make_chunk("b.rs", "fn_b", "fn b() {}", 1, 3),
        ];
        idx.update(
            &chunks,
            &[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])],
            &["a.rs".to_string(), "b.rs".to_string()],
            None,
        );

        let flat = idx.get_aligned_flat(&chunks).unwrap();
        assert_eq!(flat.n_vectors(), 2);
        assert_eq!(flat.dim, 2);
        assert!((flat.get(0)[0] - 1.0).abs() < 1e-6);
        assert!((flat.get(1)[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn get_aligned_flat_missing() {
        let idx = EmbeddingIndex::new(384);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        assert!(idx.get_aligned_flat(&chunks).is_none());
    }

    #[test]
    fn coverage_calculation() {
        let mut idx = EmbeddingIndex::new(384);
        assert!((idx.coverage(10) - 0.0).abs() < 1e-6);

        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384))],
            &["a.rs".to_string()],
            None,
        );
        assert!((idx.coverage(2) - 0.5).abs() < 1e-6);
        assert!((idx.coverage(1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let _lock = crate::core::data_dir::test_env_lock();
        let data_dir = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());

        let project_dir = tempfile::tempdir().unwrap();

        let mut idx = EmbeddingIndex::new(3);
        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(
            &chunks,
            &[(0, vec![1.0, 2.0, 3.0])],
            &["a.rs".to_string()],
            None,
        );
        idx.save(project_dir.path()).unwrap();

        let loaded = EmbeddingIndex::load(project_dir.path()).unwrap();
        assert_eq!(loaded.dimensions, 3);
        assert_eq!(loaded.entries.len(), 1);
        // int8-quantized round-trip: within one quantization step of the original.
        let recon = loaded.entries[0].quant.dequantize();
        assert!((recon[0] - 1.0).abs() < 0.02);
        assert!((recon[1] - 2.0).abs() < 0.02);
        assert!((recon[2] - 3.0).abs() < 0.02);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn new_with_model_sets_model_id() {
        let idx = EmbeddingIndex::new_with_model(768, "jina-code-v2");
        assert_eq!(idx.model_id, Some("jina-code-v2".to_string()));
        assert_eq!(idx.dimensions, 768);
    }

    #[test]
    fn model_mismatch_detection() {
        let idx = EmbeddingIndex::new_with_model(768, "all-MiniLM-L6-v2");
        assert!(idx.model_mismatch("all-MiniLM-L6-v2").is_none());
        assert!(idx.model_mismatch("jina-code-v2").is_some());

        let (stored, current) = idx.model_mismatch("jina-code-v2").unwrap();
        assert_eq!(stored, "all-MiniLM-L6-v2");
        assert_eq!(current, "jina-code-v2");
    }

    #[test]
    fn model_mismatch_none_when_no_model_id() {
        let idx = EmbeddingIndex::new(384);
        assert!(idx.model_mismatch("anything").is_none());
    }

    #[test]
    fn dimension_mismatch_detection() {
        let mut idx = EmbeddingIndex::new(384);
        assert!(!idx.dimension_mismatch(384));
        assert!(!idx.dimension_mismatch(768)); // no entries = no mismatch

        let chunks = vec![make_chunk("a.rs", "fn_a", "fn a() {}", 1, 3)];
        idx.update(
            &chunks,
            &[(0, dummy_embedding(384))],
            &["a.rs".to_string()],
            None,
        );
        assert!(!idx.dimension_mismatch(384));
        assert!(idx.dimension_mismatch(768));
    }
}
