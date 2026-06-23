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
//!                         └── ⑦ atomic dump → PipelineReport
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::core::bm25_index::BM25Index;
use crate::core::graph_index::ProjectIndex;
use crate::core::config::IndexingMode;
use crate::core::index_pipeline::content_pipeline::ContentPipeline;
use crate::core::index_pipeline::discovery::{discover_files, DiscoveryConfig, DiscoveredFile};
use crate::core::index_pipeline::dump_engine::DumpEngine;
use crate::core::index_pipeline::extraction::ParallelExtractor;
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
    pub embedding_ready: bool,
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
    /// 8. **Report** elapsed time and stats.
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
                    embedding_ready: false,
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

        // ------------------------------------------------------------------
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
            embedding_ready: false,
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
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
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
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
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
        assert!(
            !report.embedding_ready,
            "embedding not ready after pipeline build"
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
}
