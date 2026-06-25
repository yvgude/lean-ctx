//! File classification for incremental re-indexing.
//!
//! Compares a [`crate::core::index_pipeline::discovery::DiscoveredFile`] list against stored
//! [`FileHash`]es to produce a disjoint [`Classification`] — new, changed,
//! unchanged, and deleted files.
//!
//! ## Invariant
//!
//! All four sets in [`Classification`] are pairwise disjoint. A file is
//! classified into exactly one bucket:
//!
//! | Category | Condition |
//! |----------|-----------|
//! | `new` | discovered but absent from stored hashes |
//! | `changed` | discovered + stored, but mtime or size differs |
//! | `unchanged` | discovered + stored, mtime and size match |
//! | `deleted` | stored but absent from discovery |
//!
//! Callers that need the full set of files to re-parse should union
//! `new` and `changed`.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::core::index_pipeline::discovery::DiscoveredFile;
use crate::core::index_types::{Classification, FileHash};

// ---------------------------------------------------------------------------
// Core entry point
// ---------------------------------------------------------------------------

/// Classify discovered files against stored hashes.
///
/// * `discovered` — files found on disk during the current discovery pass.
/// * `stored` — file hashes loaded from a previous index.
///
/// Returns a [`Classification`] with four disjoint sets. See module docs for
/// the classification rules.
#[must_use]
pub fn classify_files(discovered: &[DiscoveredFile], stored: &[FileHash]) -> Classification {
    let mut classification = Classification::default();

    // Build a lookup from stored hashes: rel_path → &FileHash
    let stored_map: HashMap<&str, &FileHash> =
        stored.iter().map(|fh| (fh.rel_path.as_str(), fh)).collect();

    // Track which stored rel_paths were matched by discovery
    let mut seen: HashMap<&str, bool> = HashMap::new();
    for fh in stored {
        seen.insert(fh.rel_path.as_str(), false);
    }

    for file in discovered {
        if file.rel_path.is_empty() {
            continue;
        }

        let rel = file.rel_path.as_str();

        match stored_map.get(rel) {
            None => {
                // Not in stored → new file
                classification.new.push(file.rel_path.clone());
            }
            Some(stored_fh) => {
                seen.insert(rel, true);
                // Compare mtime (in nanoseconds) and size
                let mtime_ns = system_time_to_nanos(file.mtime);
                if mtime_ns == stored_fh.mtime_ns && file.size as i64 == stored_fh.size {
                    classification.unchanged.push(file.rel_path.clone());
                } else {
                    classification.changed.push(file.rel_path.clone());
                }
            }
        }
    }

    // Stored files that were not seen in discovery → deleted
    for fh in stored {
        if !seen.get(fh.rel_path.as_str()).copied().unwrap_or(false) {
            classification.deleted.push(fh.rel_path.clone());
        }
    }

    classification
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a [`SystemTime`] to nanoseconds since `UNIX_EPOCH`.
///
/// Returns 0 when the time is before `UNIX_EPOCH` or conversion fails.
fn system_time_to_nanos(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_discovered(rel_path: &str, mtime_ns: i64, size: u64) -> DiscoveredFile {
        DiscoveredFile {
            path: PathBuf::from(rel_path),
            rel_path: rel_path.to_string(),
            ext: "rs".to_string(),
            size,
            mtime: std::time::UNIX_EPOCH + std::time::Duration::from_nanos(mtime_ns as u64),
        }
    }

    fn make_stored(rel_path: &str, mtime_ns: i64, size: i64) -> FileHash {
        FileHash {
            project: String::new(),
            rel_path: rel_path.to_string(),
            mtime_ns,
            size,
            sha256: String::new(),
        }
    }

    #[test]
    fn classify_files_new_unchanged_changed_deleted() {
        let discovered = vec![
            make_discovered("src/new.rs", 100, 50), // new — not in stored
            make_discovered("src/unchanged.rs", 200, 80), // unchanged
            make_discovered("src/changed.rs", 999, 80), // changed (mtime differs)
        ];
        let stored = vec![
            make_stored("src/unchanged.rs", 200, 80),
            make_stored("src/changed.rs", 100, 80),
            make_stored("src/deleted.rs", 300, 60), // deleted — not discovered
        ];

        let c = classify_files(&discovered, &stored);

        assert_eq!(c.new, vec!["src/new.rs"]);
        assert_eq!(c.unchanged, vec!["src/unchanged.rs"]);
        assert_eq!(c.changed, vec!["src/changed.rs"]);
        assert_eq!(c.deleted, vec!["src/deleted.rs"]);

        // Invariant: changed ∩ unchanged = ∅
        for p in &c.changed {
            assert!(
                !c.unchanged.contains(p),
                "{p} is in both changed and unchanged"
            );
        }
    }

    #[test]
    fn classify_files_all_new() {
        let discovered = vec![
            make_discovered("a.rs", 10, 100),
            make_discovered("b.rs", 20, 200),
        ];
        let stored: Vec<FileHash> = vec![];

        let c = classify_files(&discovered, &stored);
        assert_eq!(c.new.len(), 2);
        assert!(c.unchanged.is_empty());
        assert!(c.changed.is_empty());
        assert!(c.deleted.is_empty());
    }

    #[test]
    fn classify_files_all_unchanged() {
        let discovered = vec![
            make_discovered("a.rs", 10, 100),
            make_discovered("b.rs", 20, 200),
        ];
        let stored = vec![make_stored("a.rs", 10, 100), make_stored("b.rs", 20, 200)];

        let c = classify_files(&discovered, &stored);
        assert!(c.new.is_empty());
        assert_eq!(c.unchanged.len(), 2);
        assert!(c.changed.is_empty());
        assert!(c.deleted.is_empty());
    }

    #[test]
    fn classify_files_all_deleted() {
        let discovered: Vec<DiscoveredFile> = vec![];
        let stored = vec![make_stored("a.rs", 10, 100), make_stored("b.rs", 20, 200)];

        let c = classify_files(&discovered, &stored);
        assert!(c.new.is_empty());
        assert!(c.unchanged.is_empty());
        assert!(c.changed.is_empty());
        assert_eq!(c.deleted.len(), 2);
    }

    #[test]
    fn classify_files_disjoint_invariant() {
        // Stress test: all categories should be disjoint
        let discovered = vec![
            make_discovered("shared.rs", 100, 50),
            make_discovered("only_disc.rs", 200, 60),
        ];
        let stored = vec![
            make_stored("shared.rs", 100, 50),
            make_stored("only_stored.rs", 300, 70),
        ];

        let c = classify_files(&discovered, &stored);

        let all: Vec<&String> = c
            .new
            .iter()
            .chain(c.changed.iter())
            .chain(c.unchanged.iter())
            .chain(c.deleted.iter())
            .collect();

        let unique: std::collections::HashSet<&String> = all.iter().copied().collect();
        assert_eq!(
            all.len(),
            unique.len(),
            "all classification sets must be disjoint"
        );
    }

    #[test]
    fn classify_files_empty_inputs() {
        let c = classify_files(&[], &[]);
        assert!(c.new.is_empty());
        assert!(c.changed.is_empty());
        assert!(c.unchanged.is_empty());
        assert!(c.deleted.is_empty());
    }

    #[test]
    fn classify_files_size_change_detected() {
        let discovered = vec![make_discovered("f.rs", 100, 999)];
        let stored = vec![make_stored("f.rs", 100, 100)];

        let c = classify_files(&discovered, &stored);
        assert_eq!(c.changed.len(), 1, "size change should be detected");
        assert!(c.unchanged.is_empty());
    }

    #[test]
    fn classify_files_mtime_change_detected() {
        let discovered = vec![make_discovered("f.rs", 500, 100)];
        let stored = vec![make_stored("f.rs", 100, 100)];

        let c = classify_files(&discovered, &stored);
        assert_eq!(c.changed.len(), 1, "mtime change should be detected");
        assert!(c.unchanged.is_empty());
    }
}
