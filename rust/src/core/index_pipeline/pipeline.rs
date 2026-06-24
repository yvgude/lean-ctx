//! Pipeline orchestrator — top-level entry point that composes all indexing
//! pipeline components into a single runnable workflow.
//!
//! # Architecture
//!
//! ```text
//! IndexPipeline (builder)
//!   └── build() → PipelineHandle
//!                   └── run()
//!                         ├── ① discover_files()
//!                         ├── ② load_with_integrity_check()
//!                         ├── ③ classify_files()
//!                         ├── ④ no-changes fast path
//!                         ├── ⑤ full build OR incremental reindex
//!                         ├── ⑥ update file metadata store
//!                         ├── ⑦ atomic dump
//!                         ├── ⑧ property graph mirror (fire-and-forget)
//!                         └── ⑨ PipelineReport
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::core::bm25_index::BM25Index;
use crate::core::config::IndexingMode;
use crate::core::embedding_index::EmbeddingBuildOutcome;
use crate::core::graph_index::ProjectIndex;
use crate::core::index_pipeline::content_pipeline::ContentPipeline;
use crate::core::index_pipeline::discovery::{discover_files, DiscoveryConfig, DiscoveredFile};
use crate::core::index_pipeline::dump_engine::DumpEngine;
use crate::core::index_pipeline::extraction::ParallelExtractor;
use crate::core::index_pipeline::semantic_edges;
use crate::core::index_pipeline::file_metadata_store::mode;
use crate::core::index_pipeline::file_metadata_store::FileMetadata;
use crate::core::index_pipeline::graph_builder::RamGraphBuilder;
use crate::core::index_pipeline::incremental::{classify_files, reindex, ClassifiedFiles, ReindexInput};

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
    /// actual build outcome (Ready, ModelNotAvailable, Failed, Skipped).
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
/// - `max_workers`: 4
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
            max_workers: 4,
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
    /// 1. **Discover** files on disk using the configured mode.
    /// 2. **Load** previous graph + BM25 indices + file metadata store.
    /// 3. **Classify** files into unchanged / changed / new / deleted /
    ///    mode_skipped.
    /// 4. **Fast path**: if nothing changed and indices loaded successfully,
    ///    return immediately.
    /// 5. **Build**: full build (no previous state / all affected) or
    ///    incremental reindex.
    /// 6. **Update** `FileMetadataStore` with new per-file state.
    /// 7. **Dump** graph + BM25 + metadata atomically to disk.
    /// 8. **Mirror** graph into property graph (background, non-blocking).
    /// 9. **Report** elapsed time and stats.
    ///
    /// # Errors
    ///
    /// Propagates errors from discovery, ingestion, extraction, reindex,
    /// metadata persistence, and dump operations.
    pub fn run(&self) -> Result<PipelineReport> {
        let start = Instant::now();

        // ------------------------------------------------------------------
        // Step 1: Discover files
        // ------------------------------------------------------------------
        let discovery_config = DiscoveryConfig {
            mode: self.mode,
            max_file_size: self.max_file_size,
        };
        let discovered = discover_files(&self.project_root, &discovery_config)
            .context("file discovery failed")?;
        let files_scanned = discovered.len();

        // ------------------------------------------------------------------
        // Step 2: Load previous state
        // ------------------------------------------------------------------
        let (prev_graph, prev_bm25, metadata_store) = DumpEngine::load_with_integrity_check(
            &self.project_root,
        )?;

        // ------------------------------------------------------------------
        // Step 3: Load stored metadata for this mode
        // ------------------------------------------------------------------
        let mode_mask = mode_mask_for(self.mode);
        let stored = metadata_store.load_for_mode(mode_mask)?;
        let all_stored = metadata_store.load_all()?;

        // ------------------------------------------------------------------
        // Step 4: Classify files
        // ------------------------------------------------------------------
        let classified = classify_files(&discovered, &stored, self.mode, &all_stored);

        // ------------------------------------------------------------------
        // Step 5: Fast path — no changes
        // ------------------------------------------------------------------
        if !classified.needs_reindex() {
            // If both indices loaded fine, short-circuit immediately.
            if let (Some(graph), Some(bm25)) = (prev_graph.as_ref(), prev_bm25.as_ref()) {
                let elapsed = start.elapsed().as_millis() as u64;
                return Ok(PipelineReport {
                    mode: self.mode,
                    files_scanned,
                    files_changed: 0,
                    files_new: 0,
                    files_deleted: 0,
                    nodes: graph.file_count(),
                    edges: graph.edge_count(),
                    chunks: bm25.chunks.len(),
                    elapsed_ms: elapsed,
                    is_incremental: true,
                    embedding_ready: EmbeddingBuildOutcome::Skipped,
                });
            }
        }

        // ------------------------------------------------------------------
        // Step 6: Build indices (full or incremental)
        // ------------------------------------------------------------------
        let has_prev = prev_graph.is_some();
        let (graph, bm25, new_metas) = if prev_graph.is_none()
            || prev_bm25.is_none()
            || classified.total_affected() == files_scanned
        {
            // ---- FULL BUILD ----
            self.full_build(&discovered)?
        } else {
            // ---- INCREMENTAL BUILD ----
            self.incremental_build(
                &discovered,
                &classified,
                prev_graph,
                prev_bm25,
            )?
        };

        // ------------------------------------------------------------------
        // Step 6.5: Post-passes (SIMILAR_TO, SEMANTICALLY_RELATED) for FULL/MODERATE
        // ------------------------------------------------------------------
        let mut graph = graph;
        if self.mode != IndexingMode::Fast {
            semantic_edges::run_post_passes(&mut graph, self.mode);
        }

        // ------------------------------------------------------------------
        // Step 7: Update file metadata store
        // ------------------------------------------------------------------
        metadata_store
            .upsert_batch(&new_metas)
            .context("batch upsert file metadata failed")?;

        if !classified.deleted.is_empty() {
            metadata_store
                .delete_batch(&classified.deleted)
                .context("batch delete file metadata failed")?;
        }

        // ------------------------------------------------------------------
        // Step 8: Atomic dump
        // ------------------------------------------------------------------
        let dump_engine = DumpEngine::new(self.project_root.clone());
        dump_engine
            .dump_graph_index(&graph)
            .context("dump graph index failed")?;
        dump_engine
            .dump_bm25_index(&bm25)
            .context("dump BM25 index failed")?;
        dump_engine
            .dump_file_metadata(&metadata_store)
            .context("dump file metadata failed")?;

        // Step 8b: Property graph mirror (fire-and-forget, non-blocking)
        {
            let root = self.project_root.to_string_lossy().to_string();
            let idx = graph.clone();
            std::thread::spawn(move || {
                if let Err(e) = crate::core::property_graph::mirror_index(&root, &idx) {
                    tracing::warn!("[pipeline] property graph mirror failed: {e}");
                }
            });
        }

        // Step 8c: Build embedding index (only for FULL/MODERATE modes — FAST
        // skips semantic passes entirely)
        let embedding_outcome = if self.mode != IndexingMode::Fast {
            crate::core::embedding_index::build_or_update(&self.project_root, &bm25)
        } else {
            EmbeddingBuildOutcome::Skipped
        };

        // Step 9: Report
        // ------------------------------------------------------------------
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(PipelineReport {
            mode: self.mode,
            files_scanned,
            files_changed: classified.changed.len(),
            files_new: classified.new.len(),
            files_deleted: classified.deleted.len(),
            nodes: graph.file_count(),
            edges: graph.edge_count(),
            chunks: bm25.chunks.len(),
            elapsed_ms: elapsed,
            is_incremental: has_prev,
            embedding_ready: embedding_outcome,
        })
    }

    /// Run the pipeline and load the resulting indices from disk.
    ///
    /// Convenience method that calls [`run()`] and then loads the graph and
    /// BM25 indices that were dumped to disk, returning them directly.
    ///
    /// # Errors
    ///
    /// Propagates errors from the pipeline run and the load-with-integrity-check.
    pub fn run_and_load(&self) -> Result<(ProjectIndex, BM25Index)> {
        self.run()?;
        let (_graph, _bm25, _metadata) =
            DumpEngine::load_with_integrity_check(&self.project_root)
                .context("loading dumped indices after pipeline run")?;
        Ok((
            _graph.unwrap_or_else(|| ProjectIndex::new(
                &self.project_root.to_string_lossy(),
            )),
            _bm25.unwrap_or_default(),
        ))
    }

    // ---- Full build ----

    /// Perform a full (non-incremental) index build from scratch.
    ///
    /// All discovered files are ingested, extracted, and indexed.
    fn full_build(
        &self,
        discovered: &[DiscoveredFile],
    ) -> Result<(ProjectIndex, BM25Index, Vec<FileMetadata>)> {
        // 1. Ingest all files into the content pipeline.
        let mut content_pipeline = ContentPipeline::new(self.max_file_size);
        for file in discovered {
            content_pipeline
                .ingest_file(file)
                .with_context(|| format!("failed to ingest {}", file.rel_path))?;
        }

        // 2. Extract signatures and BM25 chunks via parallel extractor.
        let entries = content_pipeline.into_graph_consumer().take();
        let extractor = ParallelExtractor::new(self.max_workers);
        let output = extractor
            .extract_all(&entries, self.mode)
            .context("parallel extraction failed")?;

        // 3. Build graph index from extracted signatures.
        let root_str = self.project_root.to_string_lossy();
        let mut graph_builder = RamGraphBuilder::new(&root_str);
        for (rel, sigs) in &output.graph_sigs {
            if let Some(entry) = entries.get(rel) {
                graph_builder.add_file(rel, sigs, &entry.content, &entry.content_hash);
            }
        }
        graph_builder.build_edges(&entries);
        let graph = graph_builder
            .finalize()
            .context("graph finalization failed")?;

        // 4. Build BM25 index from extracted chunks.
        let mut bm25 = BM25Index::new();
        for (_rel, chunks) in &output.bm25_chunks {
            for chunk in chunks {
                bm25.add_chunk(chunk.clone());
            }
        }
        bm25.finalize();

        // 5. Build metadata entries for all discovered files.
        let mode_mask = mode_mask_for(self.mode);
        let new_metas: Vec<FileMetadata> = discovered
            .iter()
            .map(|f| {
                let content_hash = entries
                    .get(&f.rel_path)
                    .map(|e| e.content_hash.clone())
                    .unwrap_or_default();
                let mtime_ns = f
                    .mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as i64);
                FileMetadata {
                    rel_path: f.rel_path.clone(),
                    mtime_ns,
                    size_bytes: f.size as i64,
                    content_hash,
                    mode_mask,
                }
            })
            .collect();

        Ok((graph, bm25, new_metas))
    }

    // ---- Incremental build ----

    /// Perform an incremental reindex, updating only the changed/new/deleted
    /// files while preserving unchanged data.
    #[allow(clippy::too_many_arguments)]
    fn incremental_build(
        &self,
        discovered: &[DiscoveredFile],
        classified: &ClassifiedFiles,
        prev_graph: Option<ProjectIndex>,
        prev_bm25: Option<BM25Index>,
    ) -> Result<(ProjectIndex, BM25Index, Vec<FileMetadata>)> {
        let mut content_pipeline = ContentPipeline::new(self.max_file_size);
        let extractor = ParallelExtractor::new(self.max_workers);

        let input = ReindexInput {
            classified,
            discovered,
            prev_graph,
            prev_bm25,
            content_pipeline: &mut content_pipeline,
            extractor: &extractor,
            root: &self.project_root,
        };

        let (graph, bm25) = reindex(input).context("incremental reindex failed")?;

        // Build metadata only for discovered files (unchanged + changed + new).
        let mode_mask = mode_mask_for(self.mode);
        let new_metas: Vec<FileMetadata> = discovered
            .iter()
            .map(|f| {
                let content_hash = content_pipeline
                    .get_entry(&f.rel_path)
                    .map(|e| e.content_hash.clone())
                    .unwrap_or_default();
                let mtime_ns = f
                    .mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as i64);
                FileMetadata {
                    rel_path: f.rel_path.clone(),
                    mtime_ns,
                    size_bytes: f.size as i64,
                    content_hash,
                    mode_mask,
                }
            })
            .collect();

        Ok((graph, bm25, new_metas))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map an [`IndexingMode`] to the corresponding [`mode`] bitmask.
fn mode_mask_for(mode: IndexingMode) -> u32 {
    match mode {
        IndexingMode::Full => mode::FULL,
        IndexingMode::Moderate => mode::MODERATE,
        IndexingMode::Fast => mode::FAST,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;
    use std::path::Path;

    /// Helper: write a file at `dir/rel_path` with content.
    fn write_file(dir: &Path, rel_path: &str, content: &str) {
        let abs = dir.join(rel_path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
    }

    /// Helper: create a minimal file tree for testing.
    fn create_minimal_tree(root: &Path) {
        write_file(root, "src/main.rs", "fn main() { println!(\"hello\"); }");
        write_file(
            root,
            "src/lib.rs",
            "pub fn helper() -> u32 { 42 }",
        );
        write_file(root, "README.md", "# Test Project");
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
            "all scanned files are new in first build"
        );
        assert!(report.nodes > 0, "graph should have nodes");
        assert!(report.elapsed_ms > 0, "elapsed time should be positive");
        assert!(!report.is_incremental, "first build is not incremental");
    }

    // ---------------------------------------------------------------
    // Incremental with no changes — fast path
    // ---------------------------------------------------------------

    #[test]
    fn incremental_no_changes_fast_path() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Run pipeline once to build indices.
        let handle1 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report1 = handle1.run().unwrap();

        // Run again with no changes.
        let handle2 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report2 = handle2.run().unwrap();

        // Fast path: all files unchanged, no reindex.
        assert_eq!(report2.files_changed, 0);
        assert_eq!(report2.files_new, 0);
        assert_eq!(report2.files_deleted, 0);
        // Nodes/edges/chunks should match first run.
        assert_eq!(
            report2.nodes, report1.nodes,
            "node count must match after no-change fast path"
        );
        assert_eq!(
            report2.edges, report1.edges,
            "edge count must match after no-change fast path"
        );
        assert_eq!(
            report2.chunks, report1.chunks,
            "chunk count must match after no-change fast path"
        );
        assert!(
            report2.is_incremental,
            "second run should be marked incremental"
        );
    }

    // ---------------------------------------------------------------
    // Incremental with changed file
    // ---------------------------------------------------------------

    #[test]
    fn incremental_with_changed_file() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/main.rs", "fn main() {}");
        write_file(dir.path(), "src/lib.rs", "pub fn helper() -> u32 { 42 }");

        // First build.
        let handle1 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report1 = handle1.run().unwrap();

        // Modify one file.
        write_file(
            dir.path(),
            "src/main.rs",
            "fn main() { println!(\"updated\"); }",
        );

        // Second build (incremental).
        let handle2 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report2 = handle2.run().unwrap();

        assert_eq!(report2.files_changed, 1, "one file should be changed");
        assert_eq!(report2.files_new, 0, "no new files");
        assert_eq!(report2.files_deleted, 0, "no deleted files");
        assert_eq!(
            report2.files_scanned, report1.files_scanned,
            "file count should stay the same"
        );
        assert!(report2.is_incremental, "should be incremental");
    }

    // ---------------------------------------------------------------
    // Full build produces valid indices (dump/load round trip)
    // ---------------------------------------------------------------

    #[test]
    fn full_build_produces_valid_indices() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());

        // Build indices.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        handle.run().unwrap();

        // Load back from disk.
        let (graph, bm25, store) =
            DumpEngine::load_with_integrity_check(dir.path()).unwrap();

        let g = graph.expect("graph should load from disk");
        assert!(g.file_count() > 0, "loaded graph must have files");

        let b = bm25.expect("BM25 should load from disk");
        assert!(!b.chunks.is_empty(), "loaded BM25 must have chunks");

        let all = store.load_all().unwrap();
        assert!(!all.is_empty(), "file metadata store must have entries");
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
        assert_eq!(
            report.embedding_ready,
            EmbeddingBuildOutcome::Skipped,
            "embeddings skipped without --features embeddings"
        );
    }

    // ---------------------------------------------------------------
    // Mode dispatch: FULL includes mode-skipped dirs, MODERATE/FAST skips
    // ---------------------------------------------------------------

    #[test]
    fn mode_dispatch_full_includes_all_files() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());
        // Add files in FAST_SKIP dirs.
        write_file(dir.path(), "tests/test_main.rs", "fn test_main() {}");
        write_file(dir.path(), "examples/example.rs", "fn example() {}");

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report = handle.run().unwrap();

        assert!(
            report.files_scanned >= 4,
            "FULL mode should include tests/ and examples/ dirs, got {} files",
            report.files_scanned
        );
    }

    #[test]
    fn mode_dispatch_moderate_excludes_skip_dirs() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        create_minimal_tree(dir.path());
        write_file(dir.path(), "tests/test_main.rs", "fn test_main() {}");
        write_file(dir.path(), "examples/example.rs", "fn example() {}");

        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Moderate)
            .build()
            .unwrap();
        let report = handle.run().unwrap();

        // Moderate skips tests/ and examples/ dirs, so should only find src/ files + README.
        // src/main.rs + src/lib.rs + README.md = 3
        assert_eq!(
            report.files_scanned, 3,
            "MODERATE mode should exclude tests/ and examples/ dirs, got {} files",
            report.files_scanned
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

    // ---- Test 1: Mode transition FULL → MODERATE ----

    #[test]
    fn incremental_mode_transition_full_to_moderate() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();

        // Tree: src/ (always included), tests/ + examples/ + docs/ (FAST_SKIP in Moderate)
        write_file(dir.path(), "src/main.rs", "fn main() {}");
        write_file(dir.path(), "src/lib.rs", "pub fn helper() {}");
        write_file(dir.path(), "README.md", "# Project");
        write_file(dir.path(), "tests/test_main.rs", "fn test_main() {}");
        write_file(dir.path(), "examples/example.rs", "fn example() {}");
        write_file(dir.path(), "docs/guide.md", "# Guide");

        // FULL build: all 6 files indexed.
        let handle_full = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report_full = handle_full.run().unwrap();
        assert_eq!(report_full.files_scanned, 6, "FULL must find all 6 files");
        assert_eq!(report_full.files_new, 6, "first build: all files are new");

        // MODERATE build: excludes tests/, examples/, docs/ (FAST_SKIP_DIRS).
        // Because metadata was stored under the FULL mode_mask, `load_for_mode(MODERATE)`
        // returns empty → all 3 discovered files are "new" for MODERATE →
        // total_affected == files_scanned → full_build is triggered (mode_skipped
        // files from FULL index are replaced, not preserved — this is the expected
        // behaviour when the mode transition triggers a full rebuild).
        let handle_mod = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Moderate)
            .build()
            .unwrap();
        let report_mod = handle_mod.run().unwrap();

        assert_eq!(
            report_mod.files_scanned, 3,
            "MODERATE should only find src/ + README"
        );
        assert_eq!(
            report_mod.files_new, 3,
            "3 files new for MODERATE mode_mask"
        );
        assert_eq!(report_mod.files_changed, 0);
        assert_eq!(
            report_mod.files_deleted, 0,
            "FAST_SKIP files are mode_skipped — not deleted — even in full_build"
        );
        // Note: is_incremental is true because prev_graph loaded from the FULL
        // dump artifact still on disk, even though the build internally went
        // through full_build (total_affected == files_scanned).

        // Load graph after MODERATE build — only the 3 MODERATE-discovered files
        // survive because full_build replaced the entire index.
        let (graph, bm25) = handle_mod.run_and_load().unwrap();
        assert_eq!(graph.files.len(), 3, "only 3 src/ files in MODERATE graph");
        assert!(graph.files.contains_key("src/main.rs"));
        assert!(graph.files.contains_key("src/lib.rs"));
        assert!(graph.files.contains_key("README.md"));

        let chunk_paths: Vec<&str> = bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(chunk_paths.contains(&"src/main.rs"));
        assert!(chunk_paths.contains(&"src/lib.rs"));
        assert!(chunk_paths.contains(&"README.md"));
    }

    // ---- Test 2: Deleted file is removed from indices after incremental rebuild ----

    #[test]
    fn incremental_deleted_file_removed_from_indices() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();

        write_file(dir.path(), "src/a.rs", "fn a() {}");
        write_file(dir.path(), "src/b.rs", "fn b() {}");
        write_file(dir.path(), "src/c.rs", "fn c() {}");

        // First build (FULL).
        let handle1 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report1 = handle1.run().unwrap();
        assert_eq!(report1.files_scanned, 3);
        assert_eq!(report1.files_new, 3);

        // Delete src/c.rs on disk.
        std::fs::remove_file(dir.path().join("src/c.rs")).unwrap();

        // Second build (FULL incremental).
        let handle2 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report2 = handle2.run().unwrap();

        assert_eq!(report2.files_scanned, 2, "c.rs is gone");
        assert_eq!(report2.files_deleted, 1, "one file deleted");
        assert_eq!(report2.files_new, 0);
        assert_eq!(report2.files_changed, 0);
        assert!(report2.is_incremental, "second build is incremental");

        // Load indices and verify c.rs is absent.
        let (graph, bm25) = handle2.run_and_load().unwrap();

        assert!(
            graph.files.contains_key("src/a.rs"),
            "a.rs must remain in graph"
        );
        assert!(
            graph.files.contains_key("src/b.rs"),
            "b.rs must remain in graph"
        );
        assert!(
            !graph.files.contains_key("src/c.rs"),
            "c.rs must be removed from graph"
        );
        assert_eq!(graph.files.len(), 2, "exactly 2 files in graph");

        let chunk_paths: Vec<&str> = bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(chunk_paths.contains(&"src/a.rs"));
        assert!(chunk_paths.contains(&"src/b.rs"));
        assert!(
            !chunk_paths.contains(&"src/c.rs"),
            "c.rs chunks must be removed from BM25"
        );
    }

    // ---- Test 3: FAST mode skips SIMILAR_TO / SEMANTICALLY_RELATED edges ----

    #[test]
    fn fast_mode_skips_similarity_edges() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();

        // Files with inter-dependencies so edges are generated.
        write_file(dir.path(), "src/main.rs", "mod helper;\nfn main() { helper::util(); }");
        write_file(dir.path(), "src/helper.rs", "pub fn util() -> u32 { 42 }");

        // FAST build.
        let handle = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Fast)
            .build()
            .unwrap();
        let report = handle.run().unwrap();
        assert!(report.files_scanned >= 2, "FAST should discover src/ files");

        // Load graph and verify no SIMILAR_TO or SEMANTICALLY_RELATED edges.
        let (graph, _bm25) = handle.run_and_load().unwrap();

        for edge in &graph.edges {
            assert!(
                edge.kind != "SIMILAR_TO",
                "FAST mode must not produce SIMILAR_TO edges, found: {edge:?}"
            );
            assert!(
                edge.kind != "SEMANTICALLY_RELATED",
                "FAST mode must not produce SEMANTICALLY_RELATED edges, found: {edge:?}"
            );
        }

        // Edges should exist (import/module edges are structural, not skipped).
        let edge_kinds: Vec<&str> = graph.edges.iter().map(|e| e.kind.as_str()).collect();
        assert!(
            edge_kinds.contains(&"import") || edge_kinds.contains(&"module"),
            "FAST mode should still produce structural edges (import/module), got: {edge_kinds:?}"
        );
    }

    // ---- Test 4: Full rebuild after purge produces valid output ----

    #[test]
    fn full_rebuild_after_purge() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();

        write_file(dir.path(), "src/main.rs", "fn main() {}");
        write_file(dir.path(), "src/lib.rs", "pub fn helper() {}");

        // First build.
        let handle1 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report1 = handle1.run().unwrap();
        assert_eq!(report1.files_new, 2, "first build: both files new");

        // Purge all dump artifacts (simulates starting from scratch).
        let dump_engine = DumpEngine::new(dir.path().to_path_buf());
        dump_engine.purge_all().unwrap();

        // Second build after purge — full rebuild because dump artifacts are gone.
        // Note: DumpEngine::purge_all removes graph/BM25 dump files but NOT the
        // file metadata store (graph.db), so stored metadata still exists and
        // classify_files reports all discovered files as "unchanged" (not "new").
        // The build still runs the full_build path (prev_graph was None) producing
        // a valid index.
        let handle2 = IndexPipeline::new(dir.path().to_path_buf())
            .with_mode(IndexingMode::Full)
            .build()
            .unwrap();
        let report2 = handle2.run().unwrap();

        assert_eq!(report2.files_scanned, 2, "still discovers 2 files");
        // files_new is 0 because metadata store preserved the classification
        // (files unchanged relative to stored metadata).
        assert_eq!(report2.files_changed, 0);
        assert_eq!(report2.files_deleted, 0);
        assert!(
            !report2.is_incremental,
            "after purge the build is NOT incremental (no prev_graph loaded)"
        );

        // Load indices and verify they are valid.
        let (graph, bm25) = handle2.run_and_load().unwrap();

        assert_eq!(graph.files.len(), 2, "both files in graph");
        assert!(graph.files.contains_key("src/main.rs"));
        assert!(graph.files.contains_key("src/lib.rs"));

        let chunk_paths: Vec<&str> = bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(chunk_paths.contains(&"src/main.rs"));
        assert!(chunk_paths.contains(&"src/lib.rs"));
        assert!(bm25.chunks.len() >= 2, "BM25 should have chunks for both files");
    }

    // ---- Test 5: All modes produce deterministic output ----

    #[test]
    fn all_modes_produce_deterministic_output() {
        let _iso = isolated_data_dir();

        fn run_twice(
            dir: &std::path::Path,
            mode: IndexingMode,
        ) -> (PipelineReport, PipelineReport) {
            let h1 = IndexPipeline::new(dir.to_path_buf())
                .with_mode(mode)
                .build()
                .unwrap();
            let r1 = h1.run().unwrap();

            let h2 = IndexPipeline::new(dir.to_path_buf())
                .with_mode(mode)
                .build()
                .unwrap();
            let r2 = h2.run().unwrap();

            (r1, r2)
        }

        // Create a test tree that produces edges (files with imports).
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/main.rs", "mod helper;\nfn main() { helper::util(); }");
        write_file(dir.path(), "src/helper.rs", "pub fn util() -> u32 { 42 }");
        write_file(dir.path(), "README.md", "# Project");

        // FULL mode: two runs.
        let (full_1, full_2) = run_twice(dir.path(), IndexingMode::Full);
        assert_eq!(
            full_1.files_scanned, full_2.files_scanned,
            "FULL: files_scanned must match"
        );
        assert_eq!(
            full_1.nodes, full_2.nodes,
            "FULL: nodes must match"
        );
        assert_eq!(
            full_1.edges, full_2.edges,
            "FULL: edges must match"
        );
        assert_eq!(
            full_1.chunks, full_2.chunks,
            "FULL: chunks must match"
        );

        // MODERATE mode: two runs.
        let (mod_1, mod_2) = run_twice(dir.path(), IndexingMode::Moderate);
        assert_eq!(
            mod_1.files_scanned, mod_2.files_scanned,
            "MODERATE: files_scanned must match"
        );
        assert_eq!(
            mod_1.nodes, mod_2.nodes,
            "MODERATE: nodes must match"
        );
        assert_eq!(
            mod_1.edges, mod_2.edges,
            "MODERATE: edges must match"
        );
        assert_eq!(
            mod_1.chunks, mod_2.chunks,
            "MODERATE: chunks must match"
        );

        // FAST mode: two runs.
        let (fast_1, fast_2) = run_twice(dir.path(), IndexingMode::Fast);
        assert_eq!(
            fast_1.files_scanned, fast_2.files_scanned,
            "FAST: files_scanned must match"
        );
        assert_eq!(
            fast_1.nodes, fast_2.nodes,
            "FAST: nodes must match"
        );
        assert_eq!(
            fast_1.edges, fast_2.edges,
            "FAST: edges must match"
        );
        assert_eq!(
            fast_1.chunks, fast_2.chunks,
            "FAST: chunks must match"
        );
    }
}
