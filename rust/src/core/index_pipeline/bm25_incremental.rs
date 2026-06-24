//! Incremental BM25 chunk reuse builder.
//!
//! Reuses previous chunks for unchanged files and extracts fresh chunks for
//! changed/new files. Avoids re-chunking the entire codebase on every index
//! rebuild.

use std::collections::HashMap;

use crate::core::bm25_index::{BM25Index, CodeChunk, extract_chunks};
use crate::core::index_pipeline::incremental::FileStatus;

/// Incrementally builds a `BM25Index` from a previous index and a set of file
/// status classifications.
///
/// # Usage
///
/// ```ignore
/// let mut builder = Bm25IncrementalBuilder::from_previous(&prev_index);
/// for (rel, status, content) in classified_files { ... }
/// let new_index = builder.finalize();
/// ```
pub struct Bm25IncrementalBuilder {
    /// Previous chunks grouped by file path.
    prev_chunks_by_file: HashMap<String, Vec<CodeChunk>>,
    /// Accumulated chunks for the new index.
    chunks: Vec<CodeChunk>,
}

impl Bm25IncrementalBuilder {
    /// Create an empty builder (no previous index).
    #[must_use]
    pub fn new() -> Self {
        Self {
            prev_chunks_by_file: HashMap::new(),
            chunks: Vec::new(),
        }
    }

    /// Pre-load from a previous `BM25Index`, grouping chunks by file.
    ///
    /// Each file's chunks are sorted by `start_line` then `end_line` for
    /// deterministic output.
    #[must_use]
    pub fn from_previous(prev: &BM25Index) -> Self {
        let mut map: HashMap<String, Vec<CodeChunk>> = HashMap::new();
        for chunk in &prev.chunks {
            map.entry(chunk.file_path.clone())
                .or_default()
                .push(chunk.clone());
        }
        // Sort each file's chunks for deterministic output
        for chunks in map.values_mut() {
            chunks.sort_by(|a, b| {
                a.start_line
                    .cmp(&b.start_line)
                    .then_with(|| a.end_line.cmp(&b.end_line))
            });
        }
        Self {
            prev_chunks_by_file: map,
            chunks: Vec::new(),
        }
    }

    /// Process a single file based on its status.
    ///
    /// * `Unchanged` — reuses previous chunks (if any existed).
    /// * `Changed` / `New` — extracts fresh chunks from `content`.
    /// * `Deleted` — skips; previous chunks are naturally excluded.
    /// * `ModeSkipped` — keeps previous chunks; file still indexed in other modes.
    pub fn process_file(&mut self, rel: &str, status: FileStatus, content: Option<&str>) {
        match status {
            FileStatus::Unchanged => {
                // Reuse previous chunks
                if let Some(prev_chunks) = self.prev_chunks_by_file.remove(rel) {
                    self.chunks.extend(prev_chunks);
                }
            }
            FileStatus::Changed | FileStatus::New => {
                // Extract fresh chunks from content
                if let Some(content) = content
                    && !content.is_empty()
                {
                    let mut chunks = extract_chunks(rel, content);
                    chunks.sort_by(|a, b| {
                        a.start_line
                            .cmp(&b.start_line)
                            .then_with(|| a.end_line.cmp(&b.end_line))
                    });
                    self.chunks.extend(chunks);
                }
            }
            FileStatus::Deleted => {
                // Skip — previous chunks already removed via HashMap::remove above
            }
            FileStatus::ModeSkipped => {
                // Keep previous chunks — file still indexed in other modes
                if let Some(prev_chunks) = self.prev_chunks_by_file.remove(rel) {
                    self.chunks.extend(prev_chunks);
                }
            }
        }
    }

    /// Build the final `BM25Index` from accumulated chunks.
    #[must_use]
    pub fn finalize(self) -> BM25Index {
        let mut index = BM25Index::new();
        for chunk in self.chunks {
            index.add_chunk(chunk);
        }
        index.finalize();
        index
    }
}

impl Default for Bm25IncrementalBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::tokenize;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn make_chunk(
        file_path: &str,
        symbol_name: &str,
        start_line: usize,
        end_line: usize,
        content: &str,
    ) -> CodeChunk {
        let token_count = tokenize(content).len();
        CodeChunk {
            file_path: file_path.to_string(),
            symbol_name: symbol_name.to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line,
            end_line,
            content: content.to_string(),
            tokens: Vec::new(),
            token_count,
        }
    }

    /// Build a small index from inline chunks (test helper, mirrors
    /// `from_chunks_for_test` but without cfg(test) visibility issues).
    fn build_test_index(chunks: Vec<CodeChunk>) -> BM25Index {
        let mut idx = BM25Index::new();
        for c in chunks {
            idx.add_chunk(c);
        }
        idx.finalize();
        idx
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    #[test]
    fn unchanged_file_reuses_chunks() {
        let chunks = vec![
            make_chunk("a.rs", "foo", 1, 3, "pub fn foo() {}"),
            make_chunk("a.rs", "bar", 5, 10, "pub fn bar() {}"),
        ];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("a.rs", FileStatus::Unchanged, None);

        let index = builder.finalize();
        assert_eq!(index.chunks.len(), 2);

        let paths: Vec<&str> = index.chunks.iter().map(|c| c.file_path.as_str()).collect();
        assert!(paths.iter().all(|&p| p == "a.rs"));
    }

    #[test]
    fn changed_file_extracts_new_chunks() {
        let chunks = vec![make_chunk("a.rs", "old_fn", 1, 3, "pub fn old_fn() {}")];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file(
            "a.rs",
            FileStatus::Changed,
            Some("pub fn new_fn() {}\npub fn another() {}"),
        );

        let index = builder.finalize();
        assert_eq!(index.chunks.len(), 2);
        // Chunks are from new content, not old
        assert!(index.chunks.iter().any(|c| c.symbol_name == "new_fn"));
        assert!(index.chunks.iter().any(|c| c.symbol_name == "another"));
        assert!(!index.chunks.iter().any(|c| c.symbol_name == "old_fn"));
    }

    #[test]
    fn new_file_extracts_chunks() {
        let prev = BM25Index::new();

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file(
            "new.rs",
            FileStatus::New,
            Some("pub fn fresh() -> i32 { 42 }"),
        );

        let index = builder.finalize();
        assert_eq!(index.chunks.len(), 1);
        assert_eq!(index.chunks[0].symbol_name, "fresh");
        assert_eq!(index.chunks[0].file_path, "new.rs");
    }

    #[test]
    fn deleted_file_absent_from_output() {
        let chunks = vec![make_chunk(
            "gone.rs",
            "deprecated",
            1,
            3,
            "pub fn deprecated() {}",
        )];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("gone.rs", FileStatus::Deleted, None);

        let index = builder.finalize();
        assert!(index.chunks.is_empty());
    }

    #[test]
    fn mixed_classifications() {
        // a.rs unchanged, b.rs changed, c.rs new, d.rs deleted
        let prev_chunks = vec![
            make_chunk("a.rs", "stable", 1, 3, "pub fn stable() {}"),
            make_chunk("b.rs", "old", 1, 3, "pub fn old() {}"),
            make_chunk("d.rs", "gone", 1, 3, "pub fn gone() {}"),
        ];
        let prev = build_test_index(prev_chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("a.rs", FileStatus::Unchanged, None);
        builder.process_file(
            "b.rs",
            FileStatus::Changed,
            Some("pub fn updated() -> bool { true }"),
        );
        builder.process_file(
            "c.rs",
            FileStatus::New,
            Some("pub fn brand_new() {}\npub fn also_new() {}"),
        );
        builder.process_file("d.rs", FileStatus::Deleted, None);

        let index = builder.finalize();

        // a.rs unchanged (1 chunk), b.rs changed (1), c.rs new (2)
        assert_eq!(index.chunks.len(), 4);

        assert!(index.chunks.iter().any(|c| c.file_path == "a.rs"));
        assert!(index.chunks.iter().any(|c| c.file_path == "b.rs"));
        assert!(index.chunks.iter().any(|c| c.file_path == "c.rs"));
        assert!(!index.chunks.iter().any(|c| c.file_path == "d.rs"));

        assert!(index.chunks.iter().any(|c| c.symbol_name == "stable"));
        assert!(index.chunks.iter().any(|c| c.symbol_name == "updated"));
        assert!(!index.chunks.iter().any(|c| c.symbol_name == "old"));
    }

    #[test]
    fn all_unchanged_output_matches_previous() {
        let chunks = vec![
            make_chunk("a.rs", "alpha", 1, 3, "pub fn alpha() {}"),
            make_chunk("b.rs", "beta", 1, 3, "pub fn beta() {}"),
        ];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("a.rs", FileStatus::Unchanged, None);
        builder.process_file("b.rs", FileStatus::Unchanged, None);

        let index = builder.finalize();
        assert_eq!(index.chunks.len(), 2);

        let a_chunks: Vec<&CodeChunk> = index
            .chunks
            .iter()
            .filter(|c| c.file_path == "a.rs")
            .collect();
        let b_chunks: Vec<&CodeChunk> = index
            .chunks
            .iter()
            .filter(|c| c.file_path == "b.rs")
            .collect();
        assert_eq!(a_chunks.len(), 1);
        assert_eq!(b_chunks.len(), 1);
    }

    #[test]
    fn empty_no_prev_no_content() {
        let builder = Bm25IncrementalBuilder::new();
        // Nothing processed
        let index = builder.finalize();
        assert!(index.chunks.is_empty());
        assert_eq!(index.doc_count, 0);
    }

    #[test]
    fn sorting_within_file_by_start_line() {
        // Previous index has chunks out of order (declaration order)
        let chunks = vec![
            make_chunk("m.rs", "z_last", 50, 60, "pub fn z_last() {}"),
            make_chunk("m.rs", "a_first", 1, 10, "pub fn a_first() {}"),
        ];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("m.rs", FileStatus::Unchanged, None);

        // Only one file, chunks should be sorted by start_line
        let index = builder.finalize();
        let m_chunks: Vec<&CodeChunk> = index
            .chunks
            .iter()
            .filter(|c| c.file_path == "m.rs")
            .collect();
        // Should be 2 chunks sorted by start_line
        assert_eq!(m_chunks.len(), 2);
        // a_first has start_line=1, z_last has start_line=50
        assert_eq!(m_chunks[0].symbol_name, "a_first");
        assert_eq!(m_chunks[1].symbol_name, "z_last");
    }

    #[test]
    fn mode_skipped_keeps_previous_chunks() {
        let chunks = vec![make_chunk(
            "a.rs",
            "mode_shared",
            1,
            3,
            "pub fn mode_shared() {}",
        )];
        let prev = build_test_index(chunks);

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("a.rs", FileStatus::ModeSkipped, None);

        let index = builder.finalize();
        assert_eq!(index.chunks.len(), 1);
        assert_eq!(index.chunks[0].symbol_name, "mode_shared");
    }

    #[test]
    fn new_file_with_empty_content_produces_no_chunks() {
        let prev = BM25Index::new();

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("empty.rs", FileStatus::New, Some(""));

        let index = builder.finalize();
        assert!(index.chunks.is_empty());
    }

    #[test]
    fn changed_file_with_none_content_is_skipped() {
        let prev = BM25Index::new();

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        builder.process_file("no_content.rs", FileStatus::Changed, None);

        let index = builder.finalize();
        assert!(index.chunks.is_empty());
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let chunks = vec![
            make_chunk("b.rs", "beta", 1, 3, "pub fn beta() {}"),
            make_chunk("a.rs", "alpha", 1, 3, "pub fn alpha() {}"),
        ];
        let prev = build_test_index(chunks);

        // Run twice with same inputs
        let run = |files: &[(&str, FileStatus, Option<&str>)]| -> Vec<String> {
            let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
            for &(rel, status, content) in files {
                builder.process_file(rel, status, content);
            }
            let idx = builder.finalize();
            idx.chunks
                .iter()
                .map(|c| format!("{}:{}", c.file_path, c.symbol_name))
                .collect()
        };

        let files = &[
            ("a.rs", FileStatus::Unchanged, None),
            ("b.rs", FileStatus::Unchanged, None),
        ];

        let r1 = run(files);
        let r2 = run(files);

        assert_eq!(r1, r2);
    }

    #[test]
    fn process_file_handles_missing_in_prev_gracefully() {
        let prev = BM25Index::new();

        let mut builder = Bm25IncrementalBuilder::from_previous(&prev);
        // File never existed in prev, marked Unchanged — no-op
        builder.process_file("ghost.rs", FileStatus::Unchanged, None);

        let index = builder.finalize();
        assert!(index.chunks.is_empty());
    }

    #[test]
    fn finalize_with_no_chunks_returns_valid_empty_index() {
        let builder = Bm25IncrementalBuilder::new();
        let index = builder.finalize();
        assert!(index.chunks.is_empty());
        assert_eq!(index.doc_count, 0);
        assert!(index.search("anything", 10).is_empty());
    }
}
