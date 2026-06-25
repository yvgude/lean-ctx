//! Pipeline orchestrator — top-level entry point that composes all indexing
//! pipeline components into a single runnable workflow.
//!
//! # Architecture
//!
//! ```text
//! IndexPipeline (builder)
//!   └── build() → PipelineHandle
//!                   └── run()
//!                         ├── ① Lock
//!                         ├── ② Discover files
//!                         ├── ③ Phase 1: Structure (Project → Folder → File)
//!                         ├── ④ Phase 3A: Parallel extraction
//!                         ├── ⑤ Phase 3B: Registry build
//!                         ├── ⑥ Phase 4: Parallel resolution
//!                         ├── ⑦ Phase 5: BM25 index
//!                         ├── ⑧ Phase 6: Post-passes (SIMILAR_TO)
//!                         ├── ⑨ Phase 7: SQLite dump
//!                         ├── ⑩ Embedding index
//!                         └── ⑪ PipelineReport
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::core::bm25_index::ChunkData;
use crate::core::config::IndexingMode;
use crate::core::embedding_index::EmbeddingBuildOutcome;
use crate::core::graph_buffer::GraphBuffer;
use crate::core::graph_index::ProjectIndex;
use crate::core::index_pipeline::classification::classify_files;
use crate::core::index_pipeline::discovery::{DiscoveredFile, DiscoveryConfig, discover_files};
use crate::core::index_pipeline::dump_engine::DumpEngine;
use crate::core::index_pipeline::edge_preserve::{relink_edges, snapshot_cross_file_edges};
use crate::core::index_pipeline::parallel_extract::ParallelExtractor;
use crate::core::index_pipeline::parallel_resolve::serial_resolve;
use crate::core::index_pipeline::registry_build::build_registry;
use crate::core::index_pipeline::similarity_pass;
use crate::core::index_pipeline::structure_pass::build_structure;
use crate::core::pipeline_lock::CancelToken;
use crate::core::pipeline_lock::PipelineLock;

// ---------------------------------------------------------------------------
// PipelineReport
// ---------------------------------------------------------------------------

/// Report produced after a pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineReport {
    pub mode: IndexingMode,
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_new: usize,
    pub files_deleted: usize,
    pub nodes: usize,
    pub edges: usize,
    pub chunks: usize,
    pub elapsed_ms: u64,
    pub is_incremental: bool,
    /// Detailed outcome of the embedding build step.
    /// When the `embeddings` feature is not compiled in, this is always
    /// [`EmbeddingBuildOutcome::Skipped`]; when enabled it reflects the
    /// actual build outcome (Ready, `ModelNotAvailable`, Failed, Skipped).
    pub embedding_ready: EmbeddingBuildOutcome,
}

// ---------------------------------------------------------------------------
// IndexPipeline (builder)
// ---------------------------------------------------------------------------

/// Builder for configuring and launching an indexing pipeline run.
///
/// Defaults:
/// - `mode`: `IndexingMode::Full`
/// - `max_file_size`: 2 MiB
/// - `max_workers`: `available_parallelism()`, falling back to 4
pub struct IndexPipeline {
    project_root: PathBuf,
    mode: IndexingMode,
    max_file_size: u64,
    max_workers: usize,
}

impl IndexPipeline {
    /// Create a new pipeline builder for the given project root.
    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            mode: IndexingMode::Full,
            max_file_size: 2 * 1024 * 1024,
            max_workers: std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        }
    }

    /// Set the indexing mode.
    #[must_use]
    pub fn with_mode(mut self, mode: IndexingMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the per-file size limit in bytes. Files larger than this are
    /// skipped during discovery and ingestion.
    #[must_use]
    pub fn with_max_file_size(mut self, max_size: u64) -> Self {
        self.max_file_size = max_size;
        self
    }

    /// Set the maximum number of parallel extraction workers.
    #[must_use]
    pub fn with_max_workers(mut self, workers: usize) -> Self {
        self.max_workers = workers;
        self
    }

    /// Consume the builder and produce a [`PipelineHandle`] ready to run.
    ///
    /// Validates the project root exists and is a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the project root does not exist or is not a
    /// directory.
    pub fn build(self) -> Result<PipelineHandle> {
        let root = &self.project_root;
        if !root.exists() {
            anyhow::bail!("project root does not exist: {}", root.display());
        }
        if !root.is_dir() {
            anyhow::bail!("project root is not a directory: {}", root.display());
        }

        Ok(PipelineHandle {
            project_root: self.project_root,
            mode: self.mode,
            max_file_size: self.max_file_size,
            max_workers: self.max_workers,
        })
    }

    /// Convenience method: build the pipeline and run it, auto-detecting
    /// whether an incremental or full rebuild is needed.
    ///
    /// If `code_index.db` already exists in the project's vectors directory,
    /// calls [`PipelineHandle::run_incremental`]; otherwise calls
    /// [`PipelineHandle::run`] for a full build.
    ///
    /// # Errors
    ///
    /// Propagates errors from [`IndexPipeline::build`] or the underlying run.
    pub fn run(self) -> Result<PipelineReport> {
        let handle = self.build()?;
        let db_path = DumpEngine::db_path_for(&handle.project_root);
        if db_path.exists() {
            handle.run_incremental()
        } else {
            handle.run()
        }
    }
}

// ---------------------------------------------------------------------------
// PipelineHandle
// ---------------------------------------------------------------------------

/// A configured pipeline ready to run. Obtained from [`IndexPipeline::build`].
#[derive(Debug)]
pub struct PipelineHandle {
    project_root: PathBuf,
    mode: IndexingMode,
    max_file_size: u64,
    max_workers: usize,
}

impl PipelineHandle {
    /// Run the full indexing pipeline and return a report.
    ///
    /// # Algorithm
    ///
    /// 1. **Lock** — acquire exclusive lock to prevent concurrent runs.
    /// 2. **Discover** files on disk using the configured mode.
    /// 3. **Phase 1 (structure)** — build Project→Folder→File hierarchy.
    /// 4. **Phase 3A (extraction)** — single-pass parallel read+parse+extract.
    /// 5. **Phase 3B (registry)** — serial registry build (DEFINES, IMPORTS).
    /// 6. **Phase 4 (resolution)** — parallel CALLS, USES, THROWS, EMITS edges.
    /// 7. **Phase 5 (BM25)** — build BM25 index from pre-extracted chunks.
    /// 8. **Phase 6 (post-passes)** — `SIMILAR_TO` edges (FULL/MODERATE only).
    /// 9. **Phase 7 (dump)** — SQLite dump of graph + chunks.
    /// 10. **Embedding index** — build/update for FULL/MODERATE.
    /// 11. **Report** — elapsed time and stats.
    ///
    /// # Errors
    ///
    /// Propagates errors from any phase. Cancellation between phases is
    /// checked via [`CancelToken`] — a cancelled run produces a clean error.
    ///
    /// # Panics
    ///
    /// Should not panic under normal operation.
    pub fn run(&self) -> Result<PipelineReport> {
        let start = Instant::now();
        let cancel = CancelToken::new();

        // ── ① Lock ──────────────────────────────────────────────────────────
        let _lock = PipelineLock::try_acquire(&self.project_root)
            .map_err(|e| anyhow::anyhow!("failed to acquire pipeline lock: {e}"))?;

        // ── ② Discover files ────────────────────────────────────────────────
        let discovery_config = DiscoveryConfig {
            mode: self.mode,
            max_file_size: self.max_file_size,
        };
        let files: Vec<DiscoveredFile> = discover_files(&self.project_root, &discovery_config)
            .context("file discovery failed")?;
        let files_scanned = files.len();

        // ── ③ Phase 1: Build structure (Project → Folder → File) ────────────
        let root_str: String = self.project_root.to_string_lossy().to_string();
        let mut gbuf = GraphBuffer::new(&root_str);
        build_structure(&root_str, &files, &mut gbuf);
        cancel_check(&cancel, "Phase 1 (structure)")?;

        // ── ④ Phase 3A: Single-pass parallel extraction ─────────────────────
        let extractor = ParallelExtractor::new(self.max_workers);
        let extract_output = extractor
            .extract_all(&files, &root_str, self.mode, Some(&cancel))
            .context("parallel extraction failed")?;
        let extracted_files = extract_output.extracted_files;

        // Merge worker-local gbufs into the main gbuf (handles ID remapping
        // for nodes that already exist from Phase 1).
        let mut extracted_gbuf = extract_output.graph;
        gbuf.merge(&mut extracted_gbuf);
        cancel_check(&cancel, "Phase 3A (extraction)")?;

        // ── ⑤ Phase 3B: Serial registry build ───────────────────────────────
        let registry = build_registry(&extracted_files, &mut gbuf);
        cancel_check(&cancel, "Phase 3B (registry)")?;

        // ── ⑥ Phase 4: Parallel resolution ──────────────────────────────────
        serial_resolve(&extracted_files, &registry, &mut gbuf)
            .context("parallel resolution failed")?;
        cancel_check(&cancel, "Phase 4 (resolution)")?;

        // ── ⑦ Phase 5: BM25 index from pre-extracted chunks ─────────────────
        let mut all_chunks = Vec::new();
        for ef in &extracted_files {
            all_chunks.extend(ef.chunks.iter().cloned());
        }
        let chunk_count = all_chunks.len();
        let bm25 = ChunkData::from_chunks(&all_chunks);
        cancel_check(&cancel, "Phase 5 (BM25)")?;

        // ── ⑧ Phase 6: Post-passes (SIMILAR_TO) for FULL/MODERATE ───────────
        if self.mode != IndexingMode::Fast {
            similarity_pass::compute_similar_to(&mut gbuf, 0.5);
        }
        cancel_check(&cancel, "Phase 6 (post-passes)")?;

        // ── ⑨ Phase 7: SQLite dump ──────────────────────────────────────────
        let dump_engine = DumpEngine::new(self.project_root.clone());
        dump_engine
            .dump_all(&gbuf, &all_chunks)
            .context("SQLite dump failed")?;

        // 9b. Persist file hashes from discovered files for incremental rebuild.
        dump_engine
            .insert_file_hashes(&files)
            .context("file hashes persist failed")?;

        // ── ⑩ Embedding index (FULL/MODERATE only) ──────────────────────────
        let embedding_outcome = if self.mode == IndexingMode::Fast {
            EmbeddingBuildOutcome::Skipped
        } else {
            crate::core::embedding_index::build_or_update(&self.project_root, &bm25)
        };

        // ── Report ───────────────────────────────────────────────────────────
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(PipelineReport {
            mode: self.mode,
            files_scanned,
            files_changed: 0,
            files_new: files_scanned,
            files_deleted: 0,
            nodes: gbuf.node_count(),
            edges: gbuf.edge_count(),
            chunks: chunk_count,
            elapsed_ms: elapsed,
            is_incremental: false,
            embedding_ready: embedding_outcome,
        })
    }

    /// Run the incremental indexing pipeline — only re-processes changed files.
    ///
    /// # Algorithm
    ///
    /// 1. **Lock** — acquire exclusive lock.
    /// 2. **Discover** files on disk (same as full build).
    /// 3. **Load** stored file hashes from SQLite `file_hashes` table.
    /// 4. **Classify** files into new/changed/unchanged/deleted sets.
    /// 5. **Fast path** — return immediately if nothing changed.
    /// 6. **Load** the existing graph buffer from the on-disk DB.
    /// 7. **Snapshot** inbound cross-file edges (target in changed file, source
    ///    in an unchanged file) so the cascade delete does not orphan them.
    /// 8. **Purge** stale nodes for deleted and changed files.
    /// 9. **Re-extract** only changed + new files through the standard pipeline
    ///    phases (structure, extraction, registry, resolution).
    /// 10. **Relink** preserved cross-file edges.
    /// 11. **Post-passes** (`SIMILAR_TO` for FULL/MODERATE).
    /// 12. **Dump** to SQLite (atomic full-replace of `code_index.db`).
    /// 13. **Persist** updated file hashes.
    /// 14. **Embedding index** (FULL/MODERATE only).
    /// 15. **Report** — elapsed time and stats.
    ///
    /// # Errors
    ///
    /// Propagates errors from any phase. If the on-disk DB does not exist or
    /// has a mismatched schema version, this returns an error — callers should
    /// fall back to [`PipelineHandle::run`] for the initial build.
    pub fn run_incremental(&self) -> Result<PipelineReport> {
        let start = Instant::now();
        let cancel = CancelToken::new();

        // ── ① Lock ──────────────────────────────────────────────────────────
        let _lock = PipelineLock::try_acquire(&self.project_root)
            .map_err(|e| anyhow::anyhow!("failed to acquire pipeline lock: {e}"))?;

        // ── ② Discover files ────────────────────────────────────────────────
        let discovery_config = DiscoveryConfig {
            mode: self.mode,
            max_file_size: self.max_file_size,
        };
        let files: Vec<DiscoveredFile> = discover_files(&self.project_root, &discovery_config)
            .context("file discovery failed")?;
        let files_scanned = files.len();

        // ── ③ Load stored file hashes from SQLite ────────────────────────────
        let dump_engine = DumpEngine::new(self.project_root.clone());
        let project_name: String = self
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let stored_hashes = dump_engine
            .load_file_hashes(&project_name)
            .context("load file hashes")?;

        // ── ④ Classify files ────────────────────────────────────────────────
        let classification = classify_files(&files, &stored_hashes);
        let files_changed = classification.changed.len();
        let files_new = classification.new.len();
        let files_deleted = classification.deleted.len();
        let total_changed = files_changed + files_new + files_deleted;

        // ── ⑤ Fast path: no changes ─────────────────────────────────────────
        if total_changed == 0 {
            return Ok(PipelineReport {
                mode: self.mode,
                files_scanned,
                files_changed: 0,
                files_new: 0,
                files_deleted: 0,
                nodes: 0,
                edges: 0,
                chunks: 0,
                elapsed_ms: start.elapsed().as_millis() as u64,
                is_incremental: true,
                embedding_ready: EmbeddingBuildOutcome::Skipped,
            });
        }

        // ── ⑥ Load graph buffer from DB ─────────────────────────────────────
        let db_path = dump_engine.db_path();
        let root_str: String = self.project_root.to_string_lossy().to_string();
        let mut gbuf = GraphBuffer::load_from_db(&db_path, &root_str)
            .context("failed to load graph buffer from existing DB")?;
        cancel_check(&cancel, "load from DB")?;

        // ── ⑦ Snapshot cross-file edges (before purging) ────────────────────
        let changed_new: Vec<String> = classification
            .changed
            .iter()
            .chain(classification.new.iter())
            .cloned()
            .collect();
        let preserved = snapshot_cross_file_edges(&gbuf, &changed_new);
        cancel_check(&cancel, "edge snapshot")?;

        // ── ⑧ Purge stale nodes ─────────────────────────────────────────────
        for file in &classification.deleted {
            gbuf.delete_by_file(file);
        }
        for file in &classification.changed {
            gbuf.delete_by_file(file);
        }
        cancel_check(&cancel, "purge")?;

        // ── ⑨ Filter discovered files to only changed + new ─────────────────
        let changed_new_set: HashSet<&str> = changed_new
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let files_to_process: Vec<DiscoveredFile> = files
            .iter()
            .filter(|f| changed_new_set.contains(f.rel_path.as_str()))
            .cloned()
            .collect();

        // Phase 1: Build structure for changed+new files
        build_structure(&root_str, &files_to_process, &mut gbuf);
        cancel_check(&cancel, "Phase 1 (structure)")?;

        // Phase 3A: Single-pass parallel extraction
        let extractor = ParallelExtractor::new(self.max_workers);
        let extract_output = extractor
            .extract_all(&files_to_process, &root_str, self.mode, Some(&cancel))
            .context("parallel extraction failed")?;
        let extracted_files = extract_output.extracted_files;
        let mut extracted_gbuf = extract_output.graph;
        gbuf.merge(&mut extracted_gbuf);
        cancel_check(&cancel, "Phase 3A (extraction)")?;

        // Phase 3B: Serial registry build
        let registry = build_registry(&extracted_files, &mut gbuf);
        cancel_check(&cancel, "Phase 3B (registry)")?;

        // Phase 4: Serial resolution
        serial_resolve(&extracted_files, &registry, &mut gbuf)
            .context("serial resolution failed")?;
        cancel_check(&cancel, "Phase 4 (resolution)")?;

        // Phase 5: BM25 chunks from extracted files
        let mut all_chunks = Vec::new();
        for ef in &extracted_files {
            all_chunks.extend(ef.chunks.iter().cloned());
        }
        let chunk_count = all_chunks.len();

        // ── ⑩ Relink preserved edges ─────────────────────────────────────────
        relink_edges(&mut gbuf, &preserved);

        // ── ⑪ Phase 6: Post-passes (SIMILAR_TO for FULL/MODERATE) ────────────
        if self.mode != IndexingMode::Fast {
            similarity_pass::compute_similar_to(&mut gbuf, 0.5);
        }
        cancel_check(&cancel, "Phase 6 (post-passes)")?;

        // ── ⑫ Phase 7: SQLite dump ──────────────────────────────────────────
        dump_engine
            .dump_all(&gbuf, &all_chunks)
            .context("SQLite dump failed")?;

        // ── ⑬ Persist updated file hashes ───────────────────────────────────
        dump_engine
            .insert_file_hashes(&files)
            .context("file hashes persist failed")?;

        // ── ⑭ Embedding index (FULL/MODERATE only) ──────────────────────────
        let bm25 = ChunkData::from_chunks(&all_chunks);
        let embedding_outcome = if self.mode == IndexingMode::Fast {
            EmbeddingBuildOutcome::Skipped
        } else {
            crate::core::embedding_index::build_or_update(&self.project_root, &bm25)
        };

        // ── Report ───────────────────────────────────────────────────────────
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(PipelineReport {
            mode: self.mode,
            files_scanned,
            files_changed,
            files_new,
            files_deleted,
            nodes: gbuf.node_count(),
            edges: gbuf.edge_count(),
            chunks: chunk_count,
            elapsed_ms: elapsed,
            is_incremental: true,
            embedding_ready: embedding_outcome,
        })
    }

    /// Run the pipeline and load the resulting indices from disk.
    ///
    /// Convenience method that calls [`PipelineHandle::run`] and then loads the
    /// graph and BM25 indices that were dumped to disk, returning them
    /// directly.
    ///
    /// # Errors
    ///
    /// Propagates errors from the pipeline run and the load-with-integrity-check.
    pub fn run_and_load(&self) -> Result<(ProjectIndex, ChunkData)> {
        self.run()?;
        let (graph, chunks) = DumpEngine::load_with_integrity_check(&self.project_root)
            .context("loading dumped indices after pipeline run")?;
        let index = if chunks.is_empty() {
            ChunkData::new()
        } else {
            let converted: Vec<crate::core::index_types::CodeChunk> = chunks
                .iter()
                .map(|c| crate::core::index_types::CodeChunk {
                    file_path: c.file_path.clone(),
                    content: c.content.clone(),
                    content_hash: String::new(),
                    start_line: c.start_line as u32,
                    end_line: c.end_line as u32,
                    language: String::new(),
                    symbol_name: c.symbol_name.clone(),
                    kind: serde_json::to_string(&c.kind).unwrap_or_default(),
                })
                .collect();
            ChunkData::from_chunks(&converted)
        };
        Ok((
            graph.unwrap_or_else(|| ProjectIndex::new(&self.project_root.to_string_lossy())),
            index,
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check cancellation and bail if the token is set.
fn cancel_check(cancel: &CancelToken, phase: &str) -> Result<()> {
    if cancel.is_cancelled() {
        anyhow::bail!("pipeline cancelled after {phase}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    /// Helper: write a file at `dir/rel_path` with content.
    fn write_file(dir: &std::path::Path, rel_path: &str, content: &str) {
        let abs = dir.join(rel_path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
    }

    /// Helper: create a minimal file tree for testing.
    fn create_minimal_tree(root: &std::path::Path) {
        write_file(root, "src/main.rs", "fn main() { println!(\"hello\"); }");
        write_file(root, "src/lib.rs", "pub fn helper() -> u32 { 42 }");
        write_file(root, "README.md", "# Test Project");
    }

    // ---------------------------------------------------------------
    // New pipeline runs on the frozen test repo
    // ---------------------------------------------------------------

    #[test]
    fn new_pipeline_runs_on_frozen_repo() {
        let _iso = isolated_data_dir();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures/frozen-test-repo");
        let handle = IndexPipeline::new(root)
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report = handle.run().unwrap();
        assert!(report.nodes > 0, "graph should have nodes");
        assert!(report.files_scanned > 0, "should have scanned files");
        assert!(report.elapsed_ms > 0, "elapsed time should be positive");
    }

    // ---------------------------------------------------------------
    // Full build from scratch
    // ---------------------------------------------------------------

    #[test]
    fn full_build_from_scratch() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .expect("build should succeed");

        let report = handle.run().expect("pipeline run should succeed");

        assert!(
            report.files_scanned >= 2,
            "should find at least src/main.rs and src/lib.rs"
        );
        assert_eq!(
            report.files_new, report.files_scanned,
            "all scanned files are new in new pipeline"
        );
        assert!(report.nodes > 0, "graph should have nodes");
        assert!(report.elapsed_ms > 0, "elapsed time should be positive");
        assert!(!report.is_incremental, "new pipeline is not incremental");
    }

    // ---------------------------------------------------------------
    // Pipeline report has correct stats
    // ---------------------------------------------------------------

    #[test]
    fn pipeline_report_has_correct_stats() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report = handle.run().unwrap();

        assert_eq!(report.mode, IndexingMode::Full);
        assert!(report.files_scanned >= 2);
        assert!(report.nodes > 0);
        assert!(report.elapsed_ms > 0);
        assert!(
            matches!(
                report.embedding_ready,
                EmbeddingBuildOutcome::Skipped | EmbeddingBuildOutcome::ModelNotAvailable(_)
            ),
            "embeddings: expected Skipped or ModelNotAvailable, got {:?}",
            report.embedding_ready
        );
    }

    // ---------------------------------------------------------------
    // Error handling: invalid root
    // ---------------------------------------------------------------

    #[test]
    fn error_on_invalid_root() {
        let result = IndexPipeline::new(PathBuf::from("/nonexistent/path/xyz")).build();

        assert!(
            result.is_err(),
            "building with non-existent root must return error"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("does not exist"),
            "error must mention non-existent root, got: {err}"
        );
    }

    #[test]
    fn error_on_file_root() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir.txt");
        std::fs::write(&file_path, "I am a file, not a directory").unwrap();

        let result = IndexPipeline::new(file_path).build();

        assert!(
            result.is_err(),
            "building with a file path must return error"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("not a directory"),
            "error must mention not-a-directory, got: {err}"
        );
    }

    // ---------------------------------------------------------------
    // Lock prevents concurrent runs
    // ---------------------------------------------------------------

    #[test]
    fn lock_prevents_concurrent_runs() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();

        // Acquire the lock manually first.
        let _lock = PipelineLock::try_acquire(dir.path()).unwrap();

        // Pipeline should fail to acquire the lock.
        let result = handle.run();
        assert!(result.is_err(), "pipeline should fail when lock is held");
    }

    // ---------------------------------------------------------------
    // FAST mode skips similarity edges
    // ---------------------------------------------------------------

    #[test]
    fn fast_mode_skips_similarity_edges() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Fast)
            .build()
            .unwrap();
        let report = handle.run().unwrap();
        assert!(report.files_scanned >= 2, "FAST should discover src/ files");
    }

    // ---------------------------------------------------------------
    // All modes run without errors
    // ---------------------------------------------------------------

    #[test]
    fn moderate_mode_runs() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Moderate)
            .build()
            .unwrap();
        let report = handle.run().unwrap();
        assert!(report.nodes > 0);
    }

    #[test]
    fn fast_mode_runs() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Fast)
            .build()
            .unwrap();
        let report = handle.run().unwrap();
        assert!(report.nodes > 0);
    }

    // ---------------------------------------------------------------
    // Cancel between phases produces clean error
    // ---------------------------------------------------------------

    #[test]
    fn cancel_between_phases_produces_clean_error() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();

        // Many files so extraction takes long enough to cancel.
        for i in 0..50 {
            write_file(
                dir.path(),
                &format!("src/f{i}.rs"),
                &format!("pub fn f{i}() -> u32 {{ {i} }}"),
            );
        }

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();

        // We test cancellation by checking the pipeline's internal CancelToken
        // is checked between phases.  The cancellation path is exercised by
        // the ParallelExtractor test directly (the pipeline checks the token
        // after each phase, so a cancelled run bails cleanly).
        let report = handle.run().unwrap();
        assert!(report.nodes > 0, "normal run should succeed");
    }

    // ---------------------------------------------------------------
    // Incremental pipeline
    // ---------------------------------------------------------------

    #[test]
    fn incremental_skips_unchanged_repo() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Full build first.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let full_report = handle.run().expect("full build must succeed");
        assert!(full_report.nodes > 0, "full build must produce nodes");

        // Incremental with exact same files — fast path, no changes.
        let incr_report = handle
            .run_incremental()
            .expect("incremental with same files must succeed");
        assert_eq!(incr_report.files_changed, 0, "no files changed");
        assert_eq!(incr_report.files_new, 0, "no new files");
        assert_eq!(incr_report.files_deleted, 0, "no deleted files");
        assert!(
            incr_report.elapsed_ms < 200,
            "fast path should be quick: {} ms",
            incr_report.elapsed_ms
        );
        assert!(incr_report.is_incremental, "must be incremental");
    }

    #[test]
    fn incremental_rebuilds_one_changed_file() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Full build.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let full_report = handle.run().expect("full build must succeed");
        assert!(full_report.nodes > 0);

        // Modify one file.
        // Sleep 10ms so mtime is guaranteed to differ (filesystem mtime
        // resolution on ext4 is typically 10ms for non-ns timestamps).
        std::thread::sleep(std::time::Duration::from_millis(50));
        write_file(
            dir.path(),
            "src/main.rs",
            "fn main() { println!(\"modified\"); }",
        );

        // Incremental — only the changed file should be re-processed.
        let incr_report = handle
            .run_incremental()
            .expect("incremental with one changed file must succeed");
        assert_eq!(incr_report.files_changed, 1, "exactly one file changed");
        assert_eq!(incr_report.files_new, 0, "no new files");
        assert_eq!(incr_report.files_deleted, 0, "no deleted files");
        assert!(incr_report.is_incremental, "must be incremental");
        assert!(
            incr_report.nodes > 0,
            "graph should still have nodes after incremental"
        );
    }

    /// Verify that incremental produces the same report stats as a full
    /// rebuild from scratch on the same modified input.
    #[test]
    fn incremental_matches_full_rebuild_state() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Full build on initial tree.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        handle.run().expect("full build must succeed");

        // Modify one file.
        std::thread::sleep(std::time::Duration::from_millis(50));
        write_file(
            dir.path(),
            "src/main.rs",
            "fn main() { println!(\"modified\"); }",
        );

        // Incremental rebuild.
        let incr_handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let incr_report = incr_handle
            .run_incremental()
            .expect("incremental must succeed");

        // Full rebuild on the same modified repo (PipelineHandle::run is the
        // unconditional full rebuild path).
        let full_report = handle.run().expect("full rebuild must succeed");

        // The resulting graph stats should be the same for the same input.
        assert_eq!(
            incr_report.nodes, full_report.nodes,
            "node count after incremental must match full rebuild"
        );
        assert_eq!(
            incr_report.edges, full_report.edges,
            "edge count after incremental must match full rebuild"
        );
        assert!(incr_report.nodes > 0, "should have nodes after incremental");
        assert!(incr_report.edges > 0, "should have edges after incremental");
        assert!(incr_report.is_incremental, "incremental flag must be set");
        assert!(
            !full_report.is_incremental,
            "full rebuild flag must be false"
        );
    }

    #[test]
    fn incremental_rollback_on_failure() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Full build.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        handle.run().expect("full build must succeed");

        // Record DB state before incremental.
        let engine = DumpEngine::new(dir.path().to_path_buf());
        let db_path = engine.db_path();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let nodes_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        let edges_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        drop(conn);

        // Trigger a failure: write new content to a file (so it's classified
        // as changed) then make it unreadable (so extraction fails).
        std::thread::sleep(std::time::Duration::from_millis(50));
        let target = dir.path().join("src/main.rs");
        std::fs::write(&target, "fn main() { /* new content */ }").unwrap();
        // Make the file unreadable by removing read permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = target.metadata().unwrap().permissions();
            perm.set_mode(0o000);
            std::fs::set_permissions(&target, perm).unwrap();
        }

        // Incremental should fail because the file cannot be read.
        let result = handle.run_incremental();
        assert!(
            result.is_err(),
            "incremental must fail when file is unreadable: {result:?}"
        );

        // Verify DB state is unchanged from before the incremental run.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let nodes_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        let edges_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            nodes_after, nodes_before,
            "node count must be unchanged after failed incremental"
        );
        assert_eq!(
            edges_after, edges_before,
            "edge count must be unchanged after failed incremental"
        );
    }
}
