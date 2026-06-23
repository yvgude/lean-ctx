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
use std::time::SystemTime;

use crate::core::config::IndexingMode;
use crate::core::index_pipeline::discovery::DiscoveredFile;
use crate::core::index_pipeline::file_metadata_store::FileMetadata;

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
