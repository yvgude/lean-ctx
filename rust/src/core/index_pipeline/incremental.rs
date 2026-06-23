//! File classification engine — compares discovered files against stored
//! metadata to determine which files need re-indexing.
//!
//! # Architecture
//!
//! ```text
//! classify_files(discovered, stored, mode, all_stored)
//!   ├── Step 1: Compare discovered vs stored   → Unchanged / Changed / New
//!   ├── Step 2: stored paths not in discovered → Deleted
//!   └── Step 3: all_stored paths neither discovered nor deleted → ModeSkipped
//! ```

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::core::bm25_index::BM25Index;
use crate::core::config::IndexingMode;
use crate::core::graph_index::{build_edges_cached, ProjectIndex};
use crate::core::index_pipeline::bm25_incremental::Bm25IncrementalBuilder;
use crate::core::index_pipeline::content_pipeline::ContentPipeline;
use crate::core::index_pipeline::discovery::DiscoveredFile;
use crate::core::index_pipeline::edge_snapshot::{
    drop_edges_for_files, restore_edges, snapshot_inbound_edges,
};
use crate::core::index_pipeline::extraction::ParallelExtractor;
use crate::core::index_pipeline::file_metadata_store::FileMetadata;
use crate::core::index_pipeline::graph_builder::RamGraphBuilder;
use crate::core::signatures::extract_signatures;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Classification for a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Unchanged,
    Changed,
    New,
    Deleted,
    /// File exists on disk but is excluded by current mode's skip lists.
    /// Preserve metadata, don't drop graph nodes.
    ModeSkipped,
}

/// Results of comparing discovered files against stored metadata.
#[derive(Debug, Clone, Default)]
pub struct ClassifiedFiles {
    pub unchanged: Vec<String>,
    pub changed: Vec<String>,
    pub new: Vec<String>,
    pub deleted: Vec<String>,
    pub mode_skipped: Vec<String>,
}

impl ClassifiedFiles {
    /// Returns `true` when there are any changed, new, or deleted files.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty() || !self.new.is_empty() || !self.deleted.is_empty()
    }

    /// Total number of files that need processing (changed + new + deleted).
    #[must_use]
    pub fn total_affected(&self) -> usize {
        self.changed.len() + self.new.len() + self.deleted.len()
    }

    /// Whether a re-index pass is needed.
    #[must_use]
    pub fn needs_reindex(&self) -> bool {
        self.has_changes()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert `SystemTime` to nanoseconds since Unix epoch for comparison with
/// `FileMetadata.mtime_ns`.
///
/// Handles times before Unix epoch by returning a negative value.
fn system_time_to_nanos(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

// ---------------------------------------------------------------------------
// Core classification
// ---------------------------------------------------------------------------

/// Compare discovered files against stored metadata and produce classification
/// results.
///
/// Pure function — no side effects, fully deterministic.
///
/// # Arguments
///
/// * `discovered` — Files found on disk by the current discovery pass.
/// * `stored` — Previously indexed metadata for the current mode (from
///   `load_for_mode`).
/// * `_mode` — The active indexing mode (reserved for future use; drives the
///   mode-skipped distinction implicitly via the `stored` / `all_stored` split).
/// * `all_stored` — All previously indexed metadata across all modes (from
///   `load_all`).
#[must_use]
pub fn classify_files(
    discovered: &[DiscoveredFile],
    stored: &HashMap<String, FileMetadata>,
    _mode: IndexingMode,
    all_stored: &HashMap<String, FileMetadata>,
) -> ClassifiedFiles {
    // Build a set of discovered paths for O(1) lookups in steps 2 and 3.
    let discovered_set: std::collections::HashSet<&str> =
        discovered.iter().map(|f| f.rel_path.as_str()).collect();

    let mut result = ClassifiedFiles::default();

    // ── Step 1: Classify discovered files against stored metadata ──────────
    for file in discovered {
        let path = &file.rel_path;
        match stored.get(path) {
            None => {
                result.new.push(path.clone());
            }
            Some(meta) => {
                let file_ns = system_time_to_nanos(file.mtime);
                let size_match = file.size as i64 == meta.size_bytes;
                let mtime_match = file_ns == meta.mtime_ns;

                if mtime_match && size_match {
                    result.unchanged.push(path.clone());
                } else {
                    result.changed.push(path.clone());
                }
            }
        }
    }

    // ── Step 2: Detect deletions (stored paths not on disk) ────────────────
    //
    // Every path in `stored` must also be in `all_stored` (stored is a subset),
    // so the additional `all_stored.contains_key` check is a safety net for
    // internal consistency.
    for path in stored.keys() {
        if !discovered_set.contains(path.as_str()) {
            if all_stored.contains_key(path) {
                result.deleted.push(path.clone());
            }
        }
    }

    // ── Step 3: Detect mode-skipped files ──────────────────────────────────
    //
    // Files that were indexed by a different mode (present in `all_stored`
    // but NOT in `stored`) and are not found by the current discovery pass.
    // Their metadata is preserved so graph nodes are not dropped.
    let deleted_set: std::collections::HashSet<&str> =
        result.deleted.iter().map(|s| s.as_str()).collect();

    for path in all_stored.keys() {
        if !discovered_set.contains(path.as_str())
            && !deleted_set.contains(path.as_str())
            && !stored.contains_key(path)
        {
            result.mode_skipped.push(path.clone());
        }
    }

    result
}

// ===========================================================================
// Tests
// ===========================================================================

// ---------------------------------------------------------------------------
// Incremental reindex orchestration
// ---------------------------------------------------------------------------

/// Input to the incremental reindex function.
pub struct ReindexInput<'a> {
    /// Classification output from [`classify_files`].
    pub classified: &'a ClassifiedFiles,
    /// Discovered files (for reading changed/new content).
    pub discovered: &'a [DiscoveredFile],
    /// Previous graph index (from disk), or `None` for first build.
    pub prev_graph: Option<ProjectIndex>,
    /// Previous BM25 index (from disk), or `None` for first build.
    pub prev_bm25: Option<BM25Index>,
    /// Content pipeline for reading file content.
    pub content_pipeline: &'a mut ContentPipeline,
    /// Extractor for parallel extraction (reserved; inline extraction used).
    pub extractor: &'a ParallelExtractor,
    /// Root directory of the project.
    pub root: &'a Path,
}

/// Orchestrate an incremental reindex pass.
///
/// Takes classification results, previous indices, and a content pipeline, then
/// produces a new `(ProjectIndex, BM25Index)` with minimal re-computation.
///
/// # Algorithm
///
/// 1. Validate inputs — both previous indices must be present.
/// 2. No-op fast path when `!classified.needs_reindex()`.
/// 3. Snapshot cross-file edges from unchanged files into changed+new files.
/// 4. Drop stale graph data (edges, symbols, file entries) for all affected files.
/// 5. For each changed/new file: read via `ContentPipeline`, extract signatures
///    and BM25 chunks inline, add to `RamGraphBuilder` and `Bm25IncrementalBuilder`.
/// 6. Process unchanged and mode-skipped files through `Bm25IncrementalBuilder`
///    (reuses previous chunks).
/// 7. Finalize the `RamGraphBuilder`, merge retained (unchanged + mode_skipped)
///    graph data, rebuild edges via `build_edges_cached`, restore snapshot edges.
/// 8. Return the new `(ProjectIndex, BM25Index)`.
///
/// # Errors
///
/// - Returns an error if either previous index is `None` (full rebuild required).
/// - Returns an error if a classified file is not found in `discovered`.
/// - Propagates I/O and extraction errors.
pub fn reindex(input: ReindexInput) -> Result<(ProjectIndex, BM25Index)> {
    let classified = input.classified;

    // ------------------------------------------------------------------
    // 1. Validate: both previous indices required for incremental reindex.
    // ------------------------------------------------------------------
    let prev_graph = input
        .prev_graph
        .ok_or_else(|| anyhow::anyhow!("no previous graph index — full rebuild required"))?;
    let prev_bm25 = input
        .prev_bm25
        .ok_or_else(|| anyhow::anyhow!("no previous BM25 index — full rebuild required"))?;

    // ------------------------------------------------------------------
    // 2. No-op fast path: no changes → return previous indices directly.
    // ------------------------------------------------------------------
    if !classified.needs_reindex() {
        return Ok((prev_graph, prev_bm25));
    }

    // ------------------------------------------------------------------
    // 3. Build ordered path lists.
    // ------------------------------------------------------------------

    // Changed + new: used for edge snapshot (unchanged → changed).
    let mut changed_new_paths: Vec<String> =
        Vec::with_capacity(classified.changed.len() + classified.new.len());
    changed_new_paths.extend(classified.changed.iter().cloned());
    changed_new_paths.extend(classified.new.iter().cloned());

    // All affected: changed + new + deleted. Used for drop_edges_for_files.
    let mut all_affected_paths: Vec<String> =
        Vec::with_capacity(changed_new_paths.len() + classified.deleted.len());
    all_affected_paths.extend(changed_new_paths.iter().cloned());
    all_affected_paths.extend(classified.deleted.iter().cloned());

    // ------------------------------------------------------------------
    // 4. Snapshot edges from unchanged files into changed+new files.
    //    (Must happen before any graph mutations.)
    // ------------------------------------------------------------------
    let snapshot = snapshot_inbound_edges(&prev_graph, &changed_new_paths);

    // ------------------------------------------------------------------
    // 5. Drop stale graph data for all affected files.
    //    After this, `retained` holds unchanged + mode_skipped files
    //    with their intact edges.
    // ------------------------------------------------------------------
    let mut retained = prev_graph;
    drop_edges_for_files(&mut retained, &all_affected_paths);

    // ------------------------------------------------------------------
    // 6. Process changed and new files.
    // ------------------------------------------------------------------
    let root_str: String = input.root.to_string_lossy().to_string();
    let mut ram_builder = RamGraphBuilder::new(&root_str);
    let mut bm25_builder = Bm25IncrementalBuilder::from_previous(&prev_bm25);

    // Content cache for edge building — only holds changed/new files' content.
    // `build_edges_cached` falls back to disk-read for files not in cache.
    let mut content_cache: HashMap<String, String> =
        HashMap::with_capacity(changed_new_paths.len());

    let changed_set: HashSet<&str> =
        classified.changed.iter().map(String::as_str).collect();

    for path in &changed_new_paths {
        let discovered = input
            .discovered
            .iter()
            .find(|d| d.rel_path == *path)
            .with_context(|| format!("discovered file entry not found for: {path}"))?;

        let entry = input
            .content_pipeline
            .ingest_file(discovered)
            .with_context(|| format!("failed to read file for reindex: {path}"))?;

        // Store content for edge building.
        content_cache.insert(path.clone(), (*entry.content).clone());

        // Extract signatures for graph index.
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Use inline extraction (ParallelExtractor is reserved for batch use).
        let sigs = extract_signatures(&entry.content, ext);

        // Add to RamGraphBuilder.
        ram_builder.add_file(path, &sigs, &entry.content, &entry.content_hash);

        // Process in Bm25IncrementalBuilder.
        let status = if changed_set.contains(path.as_str()) {
            FileStatus::Changed
        } else {
            FileStatus::New
        };
        bm25_builder.process_file(path, status, Some(&entry.content));
    }

    // ------------------------------------------------------------------
    // 7. Process unchanged files — reuse previous BM25 chunks.
    // ------------------------------------------------------------------
    for path in &classified.unchanged {
        bm25_builder.process_file(path, FileStatus::Unchanged, None);
    }

    // ------------------------------------------------------------------
    // 8. Process mode-skipped files — preserve previous BM25 chunks.
    // ------------------------------------------------------------------
    for path in &classified.mode_skipped {
        bm25_builder.process_file(path, FileStatus::ModeSkipped, None);
    }

    // ------------------------------------------------------------------
    // 9. Finalize RamGraphBuilder → graph node entries for changed/new files.
    // ------------------------------------------------------------------
    let new_graph = ram_builder.finalize()?;

    // ------------------------------------------------------------------
    // 10. Merge retained graph data (unchanged + mode_skipped) with the
    //     newly built entries (changed + new).
    //
    //     The merge order ensures retained entries take a back-seat to
    //     newly-built entries (which may have updated hashes/metadata
    //     for the same path). In practice, changed/new paths never overlap
    //     with unchanged/mode_skipped.
    // ------------------------------------------------------------------
    let mut final_graph = new_graph;

    // Add retained files and symbols (merged graph has full set for edge builder).
    for (path, entry) in retained.files {
        final_graph.files.entry(path).or_insert(entry);
    }
    for (key, entry) in retained.symbols {
        final_graph.symbols.entry(key).or_insert(entry);
    }

    // ------------------------------------------------------------------
    // 11. Rebuild edges on the merged graph.
    //
    //     `build_edges_cached` clears all edges and recomputes them from
    //     file content. For files in `content_cache` (changed/new) it uses
    //     cached content; for retained files it falls back to disk reads.
    // ------------------------------------------------------------------
    build_edges_cached(&mut final_graph, &content_cache);

    // ------------------------------------------------------------------
    // 12. Restore cross-file edges from unchanged→changed files that
    //     `build_edges_cached` may have missed or computed differently.
    //     The `restore_edges` dedup prevents duplication.
    // ------------------------------------------------------------------
    let _restored = restore_edges(&mut final_graph, &snapshot);

    // ------------------------------------------------------------------
    // 13. Finalize BM25 index.
    // ------------------------------------------------------------------
    let bm25 = bm25_builder.finalize();

    Ok((final_graph, bm25))
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Create a `DiscoveredFile` with the given properties.
    fn disc(rel_path: &str, size: u64, mtime_secs: u64) -> DiscoveredFile {
        DiscoveredFile {
            path: std::path::PathBuf::from(rel_path),
            rel_path: rel_path.to_string(),
            ext: rel_path.rsplit('.').next().unwrap_or("").to_string(),
            size,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs),
        }
    }

    /// Create a `FileMetadata` with the given properties.
    fn meta(rel_path: &str, mtime_ns: i64, size_bytes: i64) -> FileMetadata {
        FileMetadata {
            rel_path: rel_path.to_string(),
            mtime_ns,
            size_bytes,
            content_hash: String::new(),
            mode_mask: 0,
        }
    }

    // ── Individual classification tests ───────────────────────────────────

    #[test]
    fn unchanged_when_mtime_and_size_match() {
        let discovered = vec![disc("src/main.rs", 1024, 1_000_000)];
        let mut stored = HashMap::new();
        stored.insert(
            "src/main.rs".to_string(),
            meta("src/main.rs", 1_000_000_000_000_000, 1024),
        );
        let all_stored = stored.clone();

        let result = classify_files(&discovered, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.unchanged, vec!["src/main.rs"]);
        assert!(result.changed.is_empty());
        assert!(result.new.is_empty());
        assert!(result.deleted.is_empty());
        assert!(result.mode_skipped.is_empty());
        assert!(!result.has_changes());
    }

    #[test]
    fn changed_when_mtime_differs() {
        let discovered = vec![disc("src/main.rs", 1024, 1_000_001)];
        let mut stored = HashMap::new();
        stored.insert(
            "src/main.rs".to_string(),
            meta("src/main.rs", 1_000_000_000_000_000, 1024),
        );
        let all_stored = stored.clone();

        let result = classify_files(&discovered, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.changed, vec!["src/main.rs"]);
        assert!(result.unchanged.is_empty());
    }

    #[test]
    fn changed_when_size_differs() {
        let discovered = vec![disc("src/main.rs", 2048, 1_000_000)];
        let mut stored = HashMap::new();
        stored.insert(
            "src/main.rs".to_string(),
            meta("src/main.rs", 1_000_000_000_000_000, 1024),
        );
        let all_stored = stored.clone();

        let result = classify_files(&discovered, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.changed, vec!["src/main.rs"]);
        assert!(result.unchanged.is_empty());
    }

    #[test]
    fn new_when_not_in_stored() {
        let discovered = vec![disc("src/new.rs", 100, 500)];
        let stored: HashMap<String, FileMetadata> = HashMap::new();
        let all_stored: HashMap<String, FileMetadata> = HashMap::new();

        let result = classify_files(&discovered, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.new, vec!["src/new.rs"]);
    }

    #[test]
    fn deleted_when_in_stored_but_not_discovered() {
        let discovered: Vec<DiscoveredFile> = vec![];
        let mut stored = HashMap::new();
        stored.insert(
            "src/gone.rs".to_string(),
            meta("src/gone.rs", 1_000_000_000_000_000, 512),
        );
        let all_stored = stored.clone();

        let result = classify_files(&discovered, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.deleted, vec!["src/gone.rs"]);
        assert!(result.unchanged.is_empty());
        assert!(result.new.is_empty());
    }

    #[test]
    fn mode_skipped_when_in_all_stored_but_not_stored_or_discovered() {
        let discovered: Vec<DiscoveredFile> = vec![];
        let stored: HashMap<String, FileMetadata> = HashMap::new();
        let mut all_stored = HashMap::new();
        // File was indexed by a different mode (e.g. Full) but current mode
        // (e.g. Fast) does not pick it up.
        all_stored.insert(
            "tests/test.rs".to_string(),
            meta("tests/test.rs", 1_000_000_000_000_000, 256),
        );

        let result = classify_files(&discovered, &stored, IndexingMode::Fast, &all_stored);

        assert_eq!(result.mode_skipped, vec!["tests/test.rs"]);
        assert!(result.deleted.is_empty());
    }

    // ── Edge cases ────────────────────────────────────────────────────────

    #[test]
    fn empty_inputs() {
        let result = classify_files(
            &[],
            &HashMap::new(),
            IndexingMode::Full,
            &HashMap::new(),
        );

        assert!(result.unchanged.is_empty());
        assert!(result.changed.is_empty());
        assert!(result.new.is_empty());
        assert!(result.deleted.is_empty());
        assert!(result.mode_skipped.is_empty());
        assert!(!result.has_changes());
        assert_eq!(result.total_affected(), 0);
        assert!(!result.needs_reindex());
    }

    #[test]
    fn all_unchanged_no_changes_flagged() {
        let files: Vec<_> = (0..5)
            .map(|i| disc(&format!("file_{}.rs", i), 100 + i, 1_000_000 + i as u64))
            .collect();
        let mut stored = HashMap::new();
        for i in 0..5 {
            stored.insert(
                format!("file_{}.rs", i),
                meta(
                    &format!("file_{}.rs", i),
                    (1_000_000 + i as i64) * 1_000_000_000,
                    100 + i as i64,
                ),
            );
        }
        let all_stored = stored.clone();

        let result = classify_files(&files, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.unchanged.len(), 5);
        assert!(result.changed.is_empty());
        assert!(result.new.is_empty());
        assert!(result.deleted.is_empty());
        assert!(result.mode_skipped.is_empty());
        assert!(!result.has_changes());
    }

    #[test]
    fn large_batch_10k_entries() {
        let count = 10_000;
        let files: Vec<_> = (0..count)
            .map(|i| disc(&format!("file_{}.rs", i), 100, 1_000_000))
            .collect();
        let mut stored = HashMap::new();
        for i in 0..count {
            stored.insert(
                format!("file_{}.rs", i),
                meta(&format!("file_{}.rs", i), 1_000_000_000_000_000, 100),
            );
        }
        let all_stored = stored.clone();

        let result = classify_files(&files, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(result.unchanged.len(), count);
        assert_eq!(result.total_affected(), 0);
    }

    #[test]
    fn mixed_classifications() {
        // 2 unchanged, 1 changed, 1 new, 1 deleted, 1 mode_skipped
        let discovered = vec![
            disc("unchanged.rs", 100, 1_000_000),
            disc("also_unchanged.rs", 200, 2_000_000),
            disc("changed.rs", 999, 3_000_000), // size differs from stored
            disc("new.rs", 50, 4_000_000),       // not in stored
        ];

        let mut stored = HashMap::new();
        stored.insert(
            "unchanged.rs".to_string(),
            meta("unchanged.rs", 1_000_000_000_000_000, 100),
        );
        stored.insert(
            "also_unchanged.rs".to_string(),
            meta("also_unchanged.rs", 2_000_000_000_000_000, 200),
        );
        stored.insert(
            "changed.rs".to_string(),
            meta("changed.rs", 3_000_000_000_000_000, 100), // size 100 != 999
        );
        stored.insert(
            "deleted.rs".to_string(),
            meta("deleted.rs", 5_000_000_000_000_000, 300),
        );

        let mut all_stored = stored.clone();
        all_stored.insert(
            "mode_skipped.rs".to_string(),
            meta("mode_skipped.rs", 6_000_000_000_000_000, 400),
        );

        let result =
            classify_files(&discovered, &stored, IndexingMode::Fast, &all_stored);

        assert_eq!(result.unchanged.len(), 2);
        assert!(result.unchanged.contains(&"unchanged.rs".to_string()));
        assert!(result.unchanged.contains(&"also_unchanged.rs".to_string()));

        assert_eq!(result.changed, vec!["changed.rs"]);
        assert_eq!(result.new, vec!["new.rs"]);
        assert_eq!(result.deleted, vec!["deleted.rs"]);
        assert_eq!(result.mode_skipped, vec!["mode_skipped.rs"]);

        assert!(result.has_changes());
        assert_eq!(result.total_affected(), 3);
        assert!(result.needs_reindex());
    }

    // ── Determinism ───────────────────────────────────────────────────────

    #[test]
    fn deterministic_same_inputs_same_outputs() {
        let files = vec![disc("a.rs", 100, 1), disc("b.rs", 200, 2)];
        let mut stored = HashMap::new();
        stored.insert("a.rs".to_string(), meta("a.rs", 1_000_000_000, 100));
        stored.insert("b.rs".to_string(), meta("b.rs", 2_000_000_000, 200));
        let all_stored = stored.clone();

        let r1 = classify_files(&files, &stored, IndexingMode::Full, &all_stored);
        let r2 = classify_files(&files, &stored, IndexingMode::Full, &all_stored);

        assert_eq!(r1.unchanged, r2.unchanged);
        assert_eq!(r1.changed, r2.changed);
        assert_eq!(r1.new, r2.new);
        assert_eq!(r1.deleted, r2.deleted);
        assert_eq!(r1.mode_skipped, r2.mode_skipped);
    }
}

// ---------------------------------------------------------------------------
// Reindex integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod reindex_tests {
    use std::sync::Arc;
    use std::time::UNIX_EPOCH;

    use tempfile::TempDir;

    use super::*;

    use crate::core::bm25_index::extract_chunks;
    use crate::core::index_pipeline::content_pipeline::ContentEntry;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Write a file to `dir/rel_path` and return a `DiscoveredFile` for it.
    fn make_file(dir: &std::path::Path, rel_path: &str, content: &str) -> DiscoveredFile {
        let abs_path = dir.join(rel_path);
        let parent = abs_path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&abs_path, content).unwrap();
        let meta = abs_path.metadata().unwrap();
        DiscoveredFile {
            path: abs_path,
            rel_path: rel_path.to_string(),
            ext: rel_path
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_string(),
            size: meta.len(),
            mtime: meta.modified().unwrap(),
        }
    }

    /// Build a `ProjectIndex` from a list of (rel_path, content) pairs
    /// using `RamGraphBuilder`, with edge building enabled.
    fn build_prev_graph(
        dir: &std::path::Path,
        files: &[(&str, &str)],
    ) -> ProjectIndex {
        let root_str = dir.to_string_lossy().to_string();
        let mut builder = RamGraphBuilder::new(&root_str);

        for (path, content) in files {
            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let sigs = extract_signatures(content, ext);
            let hash = compute_hash(content);
            builder.add_file(path, &sigs, content, &hash);
        }

        // Build edges from all files' content.
        let mut entries: HashMap<String, ContentEntry> = HashMap::new();
        for (path, content) in files {
            entries.insert(path.to_string(), make_cached_entry(content));
        }
        builder.build_edges(&entries);

        builder.finalize().unwrap()
    }

    /// Build a `BM25Index` from a list of (rel_path, content) pairs.
    fn build_prev_bm25(files: &[(&str, &str)]) -> BM25Index {
        let mut idx = BM25Index::new();
        for (path, content) in files {
            let chunks = extract_chunks(path, content);
            for chunk in chunks {
                idx.add_chunk(chunk);
            }
        }
        idx.finalize();
        idx
    }

    /// Deterministic content hash matching `graph_index::compute_hash`.
    fn compute_hash(content: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish().to_string()
    }

    /// Create a `ContentEntry` for edge-building helpers.
    fn make_cached_entry(content: &str) -> ContentEntry {
        ContentEntry {
            content: Arc::new(content.to_string()),
            mtime: UNIX_EPOCH,
            size: content.len() as u64,
            content_hash: String::new(),
        }
    }

    /// Build a full `ReindexInput` and call `reindex`.
    fn run_reindex(
        dir: &TempDir,
        classified: &ClassifiedFiles,
        discovered: &[DiscoveredFile],
        prev_graph: Option<ProjectIndex>,
        prev_bm25: Option<BM25Index>,
    ) -> Result<(ProjectIndex, BM25Index)> {
        let mut pipeline = ContentPipeline::new(10_485_760);
        let extractor = ParallelExtractor::new(1);
        let input = ReindexInput {
            classified,
            discovered,
            prev_graph,
            prev_bm25,
            content_pipeline: &mut pipeline,
            extractor: &extractor,
            root: dir.path(),
        };
        reindex(input)
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    #[test]
    fn no_changes_returns_previous_indices() {
        let dir = TempDir::new().unwrap();
        make_file(dir.path(), "a.rs", "fn a() {}");

        let files = [("a.rs", "fn a() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        let discovered = vec![make_file(dir.path(), "a.rs", "fn a() {}")];
        let classified = ClassifiedFiles {
            unchanged: vec!["a.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph.clone()),
            Some(bm25.clone()),
        )
        .expect("reindex should succeed with no changes");

        // File count and chunk count match previous.
        assert_eq!(out_graph.files.len(), 1);
        assert!(out_graph.files.contains_key("a.rs"));
        assert_eq!(out_bm25.chunks.len(), 1);
    }

    #[test]
    fn single_changed_file_processed() {
        let dir = TempDir::new().unwrap();
        // Initial content
        make_file(dir.path(), "a.rs", "fn old() {}");
        let files = [("a.rs", "fn old() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // Changed content on disk
        let discovered = vec![make_file(dir.path(), "a.rs", "fn new() {}")];

        let classified = ClassifiedFiles {
            changed: vec!["a.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("reindex should succeed with changed file");

        assert!(out_graph.files.contains_key("a.rs"), "changed file must remain in graph");

        // BM25 chunks should contain new symbol, not old symbol.
        let chunk_syms: Vec<&str> = out_bm25
            .chunks
            .iter()
            .map(|c| c.symbol_name.as_str())
            .collect();
        assert!(
            chunk_syms.contains(&"new"),
            "BM25 chunks should contain new symbol 'new', got: {chunk_syms:?}"
        );
        assert!(
            !chunk_syms.contains(&"old"),
            "BM25 chunks should NOT contain old symbol 'old'"
        );
    }

    #[test]
    fn single_new_file_added() {
        let dir = TempDir::new().unwrap();
        // Initial state
        make_file(dir.path(), "existing.rs", "fn existing() {}");
        let files = [("existing.rs", "fn existing() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // New file appears on disk
        make_file(dir.path(), "new.rs", "fn brand_new() {}");
        let discovered = vec![
            make_file(dir.path(), "existing.rs", "fn existing() {}"),
            make_file(dir.path(), "new.rs", "fn brand_new() {}"),
        ];

        let classified = ClassifiedFiles {
            unchanged: vec!["existing.rs".to_string()],
            new: vec!["new.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("reindex should succeed with new file");

        assert!(
            out_graph.files.contains_key("existing.rs"),
            "existing file must remain"
        );
        assert!(
            out_graph.files.contains_key("new.rs"),
            "new file must be added"
        );

        let chunk_paths: Vec<&str> =
            out_bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(chunk_paths.contains(&"new.rs"), "new file must have BM25 chunks");
        assert!(chunk_paths.contains(&"existing.rs"), "existing file must keep BM25 chunks");
    }

    #[test]
    fn single_deleted_file_removed() {
        let dir = TempDir::new().unwrap();
        let files = [
            ("keep.rs", "fn keep() {}"),
            ("remove.rs", "fn remove() {}"),
        ];
        for (p, c) in &files {
            make_file(dir.path(), p, c);
        }
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // Only keep.rs discovered (remove.rs is gone).
        let discovered = vec![make_file(dir.path(), "keep.rs", "fn keep() {}")];

        let classified = ClassifiedFiles {
            unchanged: vec!["keep.rs".to_string()],
            deleted: vec!["remove.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("reindex should succeed with deleted file");

        assert!(
            out_graph.files.contains_key("keep.rs"),
            "keep.rs must remain"
        );
        assert!(
            !out_graph.files.contains_key("remove.rs"),
            "remove.rs must be gone from graph"
        );

        let chunk_paths: Vec<&str> =
            out_bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(chunk_paths.contains(&"keep.rs"));
        assert!(!chunk_paths.contains(&"remove.rs"));
    }

    #[test]
    fn mixed_changed_new_deleted() {
        let dir = TempDir::new().unwrap();

        // Initial: a.rs, b.rs, c.rs
        for (p, c) in [("a.rs", "fn a() {}"), ("b.rs", "fn b() {}"), ("c.rs", "fn c() {}")] {
            make_file(dir.path(), p, c);
        }
        let files = [("a.rs", "fn a() {}"), ("b.rs", "fn b() {}"), ("c.rs", "fn c() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // a.rs: unchanged, b.rs: changed, c.rs: deleted, d.rs: new
        make_file(dir.path(), "b.rs", "fn b_updated() {}");
        make_file(dir.path(), "d.rs", "fn d_new() {}");

        let discovered = vec![
            make_file(dir.path(), "a.rs", "fn a() {}"),
            make_file(dir.path(), "b.rs", "fn b_updated() {}"),
            make_file(dir.path(), "d.rs", "fn d_new() {}"),
        ];

        let classified = ClassifiedFiles {
            unchanged: vec!["a.rs".to_string()],
            changed: vec!["b.rs".to_string()],
            new: vec!["d.rs".to_string()],
            deleted: vec!["c.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("mixed reindex should succeed");

        // Graph assertions
        assert!(out_graph.files.contains_key("a.rs"), "unchanged file present");
        assert!(out_graph.files.contains_key("b.rs"), "changed file present");
        assert!(out_graph.files.contains_key("d.rs"), "new file present");
        assert!(!out_graph.files.contains_key("c.rs"), "deleted file gone");

        // BM25 assertions
        let chunk_paths: Vec<&str> = out_bm25
            .chunks
            .iter()
            .map(|c| c.file_path.as_str())
            .collect();
        assert!(chunk_paths.contains(&"a.rs"), "unchanged chunks kept");
        assert!(
            !chunk_paths.contains(&"c.rs"),
            "deleted chunks removed"
        );
        assert!(chunk_paths.contains(&"b.rs"), "changed chunks present");
        assert!(chunk_paths.contains(&"d.rs"), "new chunks present");

        // b.rs should have updated symbol name
        let b_syms: Vec<&str> = out_bm25
            .chunks
            .iter()
            .filter(|c| c.file_path == "b.rs")
            .map(|c| c.symbol_name.as_str())
            .collect();
        assert!(
            b_syms.contains(&"b_updated"),
            "changed file should have new symbol 'b_updated', got {b_syms:?}"
        );
    }

    #[test]
    fn mode_skipped_files_preserved() {
        let dir = TempDir::new().unwrap();

        // Initial: keep.rs (unchanged), skipped.rs (mode_skipped)
        for (p, c) in [("keep.rs", "fn keep() {}"), ("skipped.rs", "fn skipped() {}")] {
            make_file(dir.path(), p, c);
        }
        let files = [("keep.rs", "fn keep() {}"), ("skipped.rs", "fn skipped() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // Only keep.rs is discovered this time.
        let discovered = vec![make_file(dir.path(), "keep.rs", "fn keep() {}")];

        let classified = ClassifiedFiles {
            unchanged: vec!["keep.rs".to_string()],
            mode_skipped: vec!["skipped.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("reindex with mode_skipped should succeed");

        // Both files must still be in the graph.
        assert!(
            out_graph.files.contains_key("keep.rs"),
            "keep.rs must remain in graph"
        );
        assert!(
            out_graph.files.contains_key("skipped.rs"),
            "mode_skipped file must remain in graph"
        );

        // Both files must still have BM25 chunks.
        let chunk_paths: Vec<&str> = out_bm25
            .chunks
            .iter()
            .map(|c| c.file_path.as_str())
            .collect();
        assert!(chunk_paths.contains(&"keep.rs"));
        assert!(chunk_paths.contains(&"skipped.rs"));
    }

    #[test]
    fn all_unchanged_returns_previous_directly() {
        let dir = TempDir::new().unwrap();
        make_file(dir.path(), "a.rs", "fn a() {}");

        let files = [("a.rs", "fn a() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        let discovered = vec![make_file(dir.path(), "a.rs", "fn a() {}")];
        let classified = ClassifiedFiles {
            unchanged: vec!["a.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph.clone()),
            Some(bm25.clone()),
        )
        .expect("reindex should succeed with all unchanged");

        // Same file count and chunk count.
        assert_eq!(out_graph.file_count(), graph.file_count());
        assert_eq!(out_bm25.chunks.len(), bm25.chunks.len());
    }

    #[test]
    fn empty_previous_returns_error() {
        let dir = TempDir::new().unwrap();
        make_file(dir.path(), "a.rs", "fn a() {}");

        let classified = ClassifiedFiles {
            new: vec!["a.rs".to_string()],
            ..Default::default()
        };
        let discovered = vec![make_file(dir.path(), "a.rs", "fn a() {}")];

        // No previous graph → error.
        let result = run_reindex(&dir, &classified, &discovered, None, None);
        assert!(
            result.is_err(),
            "reindex without previous state must return error"
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("full rebuild"),
            "error must mention full rebuild, got: {msg}"
        );
    }

    #[test]
    fn cross_file_edges_preserved() {
        let dir = TempDir::new().unwrap();

        // lib.rs exports a function; main.rs uses it (creates an import edge).
        make_file(dir.path(), "lib.rs", "pub fn helper() -> u32 { 42 }");
        make_file(dir.path(), "main.rs", "mod lib;\nfn main() { lib::helper(); }");

        let files = [
            ("lib.rs", "pub fn helper() -> u32 { 42 }"),
            ("main.rs", "mod lib;\nfn main() { lib::helper(); }"),
        ];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // Change lib.rs but keep main.rs as found on disk.
        make_file(dir.path(), "lib.rs", "pub fn helper() -> u32 { 99 }");

        let discovered = vec![
            make_file(dir.path(), "lib.rs", "pub fn helper() -> u32 { 99 }"),
            make_file(dir.path(), "main.rs", "mod lib;\nfn main() { lib::helper(); }"),
        ];

        let classified = ClassifiedFiles {
            changed: vec!["lib.rs".to_string()],
            unchanged: vec!["main.rs".to_string()],
            ..Default::default()
        };

        let (out_graph, _out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("reindex should preserve cross-file edges");

        // Edge check: there should be an edge from main.rs to lib.rs (module edge).
        let has_edge = out_graph.edges.iter().any(|e| {
            (e.from == "main.rs" || e.from == "lib.rs")
                && (e.to == "lib.rs" || e.to == "main.rs")
                && (e.kind == "import" || e.kind == "module")
        });
        assert!(
            has_edge,
            "cross-file edge between main.rs and lib.rs must survive reindex"
        );
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let dir = TempDir::new().unwrap();
        make_file(dir.path(), "a.rs", "fn a() {}");
        make_file(dir.path(), "b.rs", "fn b() {}");
        make_file(dir.path(), "c.rs", "fn c() {}");

        let files = [("a.rs", "fn a() {}"), ("b.rs", "fn b() {}"), ("c.rs", "fn c() {}")];
        let graph = build_prev_graph(dir.path(), &files);
        let bm25 = build_prev_bm25(&files);

        // Change b.rs
        make_file(dir.path(), "b.rs", "fn b_updated() {}");

        let discovered = vec![
            make_file(dir.path(), "a.rs", "fn a() {}"),
            make_file(dir.path(), "b.rs", "fn b_updated() {}"),
            make_file(dir.path(), "c.rs", "fn c() {}"),
        ];

        let classified = ClassifiedFiles {
            unchanged: vec!["a.rs".to_string(), "c.rs".to_string()],
            changed: vec!["b.rs".to_string()],
            ..Default::default()
        };

        let run = || -> (Vec<String>, Vec<String>) {
            let mut pipeline = ContentPipeline::new(10_485_760);
            let extractor = ParallelExtractor::new(1);
            let input = ReindexInput {
                classified: &classified,
                discovered: &discovered,
                prev_graph: Some(graph.clone()),
                prev_bm25: Some(bm25.clone()),
                content_pipeline: &mut pipeline,
                extractor: &extractor,
                root: dir.path(),
            };
            let (g, b) = reindex(input).expect("deterministic reindex should succeed");

            let file_paths: Vec<String> = {
                let mut keys: Vec<String> = g.files.keys().cloned().collect();
                keys.sort();
                keys
            };
            let chunk_sigs: Vec<String> = b
                .chunks
                .iter()
                .map(|c| format!("{}::{}", c.file_path, c.symbol_name))
                .collect();
            (file_paths, chunk_sigs)
        };

        let (r1_files, r1_chunks) = run();
        let (r2_files, r2_chunks) = run();

        assert_eq!(r1_files, r2_files, "file sets must be deterministic");
        assert_eq!(r1_chunks, r2_chunks, "chunk sets must be deterministic");
    }

    #[test]
    fn large_batch_1000_files_10_changed() {
        let dir = TempDir::new().unwrap();

        // Create 1000 files.
        let all_files: Vec<(String, String)> = (0..1000)
            .map(|i| {
                let name = format!("file_{i}.rs");
                let content = format!("fn func_{i}() {{}}");
                make_file(dir.path(), &name, &content);
                (name, content)
            })
            .collect();

        // Build previous indices with all files.
        let prev_pairs: Vec<(&str, &str)> = all_files
            .iter()
            .map(|(n, c)| (n.as_str(), c.as_str()))
            .collect();
        let graph = build_prev_graph(dir.path(), &prev_pairs);
        let bm25 = build_prev_bm25(&prev_pairs);

        // Change 10 files.
        let changed_indices: std::collections::HashSet<usize> =
            [10, 50, 100, 150, 200, 300, 400, 500, 600, 700].into_iter().collect();
        let mut changed_paths: Vec<String> = Vec::new();
        let mut unchanged_paths: Vec<String> = Vec::new();

        for (i, (name, _)) in all_files.iter().enumerate() {
            if changed_indices.contains(&i) {
                let new_content = format!("fn func_{i}_updated() {{}}");
                make_file(dir.path(), name, &new_content);
                changed_paths.push(name.clone());
            } else {
                unchanged_paths.push(name.clone());
            }
        }

        // Build discovered list (all files on disk).
        let discovered: Vec<DiscoveredFile> = (0..1000)
            .map(|i| {
                let name = format!("file_{i}.rs");
                let content = if changed_indices.contains(&i) {
                    format!("fn func_{i}_updated() {{}}")
                } else {
                    format!("fn func_{i}() {{}}")
                };
                make_file(dir.path(), &name, &content)
            })
            .collect();

        unchanged_paths.sort();
        changed_paths.sort();

        let classified = ClassifiedFiles {
            unchanged: unchanged_paths,
            changed: changed_paths,
            ..Default::default()
        };

        let (out_graph, out_bm25) = run_reindex(
            &dir,
            &classified,
            &discovered,
            Some(graph),
            Some(bm25),
        )
        .expect("large batch reindex should succeed");

        // All 1000 files present.
        assert_eq!(out_graph.files.len(), 1000);

        // All 1000 files have BM25 chunks.
        let mut chunk_paths: Vec<&str> =
            out_bm25.chunks.iter().map(|c| c.file_path.as_str()).collect();
        chunk_paths.sort();
        chunk_paths.dedup();
        assert_eq!(chunk_paths.len(), 1000, "all 1000 files must have BM25 chunks");
    }
}
