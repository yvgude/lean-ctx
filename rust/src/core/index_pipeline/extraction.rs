//! Parallel extraction of graph signatures and BM25 chunks from content entries.
//!
//! Uses rayon to process files concurrently, collecting per-thread results
//! without shared mutable state.

use std::collections::HashMap;

use anyhow::Result;
use rayon::prelude::*;

use crate::core::bm25_index::CodeChunk;
use crate::core::config::IndexingMode;
use crate::core::index_pipeline::content_pipeline::ContentEntry;
use crate::core::signatures::Signature;

/// Output from a parallel extraction pass.
///
/// Contains per-file graph signatures and BM25 chunks, both sorted by
/// `rel_path` for deterministic output.
#[derive(Debug, Default)]
pub struct ExtractionOutput {
    /// Per-file graph signatures, sorted by rel_path.
    pub graph_sigs: Vec<(String, Vec<Signature>)>,
    /// Per-file BM25 chunks, sorted by rel_path.
    pub bm25_chunks: Vec<(String, Vec<CodeChunk>)>,
}

/// Parallel extractor for graph signatures and BM25 chunks.
///
/// Processes multiple files concurrently using rayon, merging per-thread
/// results at the end without shared mutable state.
pub struct ParallelExtractor {
    /// Maximum number of worker threads for parallel extraction.
    /// Passed through to the rayon thread pool builder. A value of 0
    /// is clamped to 1 (sequential execution).
    pub max_workers: usize,
}

impl ParallelExtractor {
    /// Create a new extractor with the given worker thread limit.
    ///
    /// `max_workers` controls the size of the scoped rayon thread pool
    /// used for extraction. A value of 0 or 1 runs the workload on a
    /// single thread, effectively sequential but with pool overhead.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self { max_workers }
    }

    /// Run parallel extraction over all content entries.
    ///
    /// For each entry this extracts:
    /// - Graph signatures via [`crate::core::signatures::extract_signatures`]
    /// - BM25 chunks via [`crate::core::bm25_index::extract_chunks`]
    ///
    /// The results are sorted by `rel_path` for deterministic output (the
    /// same set of input files always produces the same ordered result).
    ///
    /// The `_mode` parameter is reserved for future use (e.g. skipping
    /// deep extraction in fast indexing mode) and is currently unused.
    ///
    /// # Errors
    ///
    /// Returns an error if the rayon thread pool cannot be built.
    pub fn extract_all(
        &self,
        entries: &HashMap<String, ContentEntry>,
        _mode: IndexingMode,
    ) -> Result<ExtractionOutput> {
        if entries.is_empty() {
            return Ok(ExtractionOutput::default());
        }

        // Sort keys for deterministic ordering across runs.
        let mut sorted_keys: Vec<&String> = entries.keys().collect();
        sorted_keys.sort();

        // Collect (rel_path, &str) pairs to avoid holding the HashMap borrow
        // across the parallel section (each thread only needs a &str).
        let items: Vec<(&str, &str)> = sorted_keys
            .iter()
            .map(|k| (k.as_str(), entries[*k].content.as_str()))
            .collect();

        // Build a scoped thread pool so max_workers is respected without
        // affecting the global rayon pool (which may be shared by other
        // parts of the system).
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.max_workers.max(1))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build rayon thread pool: {e}"))?;

        let results: Vec<(String, Vec<Signature>, Vec<CodeChunk>)> = pool.install(|| {
            items
                .par_iter()
                .map(|&(rel_path, content)| {
                    let ext = std::path::Path::new(rel_path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let sigs = crate::core::signatures::extract_signatures(content, ext);
                    let chunks = crate::core::bm25_index::extract_chunks(rel_path, content);
                    (rel_path.to_string(), sigs, chunks)
                })
                .collect()
        });

        // Partition the merged results into the two output vectors while
        // preserving the sorted order established above.
        let mut graph_sigs: Vec<(String, Vec<Signature>)> = Vec::with_capacity(results.len());
        let mut bm25_chunks: Vec<(String, Vec<CodeChunk>)> = Vec::with_capacity(results.len());

        for (rel_path, sigs, chunks) in results {
            graph_sigs.push((rel_path.clone(), sigs));
            bm25_chunks.push((rel_path, chunks));
        }

        Ok(ExtractionOutput {
            graph_sigs,
            bm25_chunks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::UNIX_EPOCH;

    // ── helpers ──────────────────────────────────────────────────────────

    fn make_entry(content: &str) -> ContentEntry {
        ContentEntry {
            content: Arc::new(content.to_string()),
            mtime: UNIX_EPOCH,
            size: content.len() as u64,
            content_hash: String::new(),
        }
    }

    /// Run sequential extraction for a reference result.
    fn extract_sequential(
        entries: &HashMap<String, ContentEntry>,
    ) -> (Vec<(String, Vec<Signature>)>, Vec<(String, Vec<CodeChunk>)>) {
        let mut sorted: Vec<(&String, &ContentEntry)> = entries.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));

        let mut sigs = Vec::with_capacity(sorted.len());
        let mut chunks = Vec::with_capacity(sorted.len());

        for (path, entry) in sorted {
            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let s = crate::core::signatures::extract_signatures(&entry.content, ext);
            let c = crate::core::bm25_index::extract_chunks(path, &entry.content);
            sigs.push((path.clone(), s));
            chunks.push((path.clone(), c));
        }

        (sigs, chunks)
    }

    // ── tests ────────────────────────────────────────────────────────────

    #[test]
    fn empty_entries_returns_empty_output() {
        let extractor = ParallelExtractor::new(4);
        let entries = HashMap::new();
        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();
        assert!(output.graph_sigs.is_empty());
        assert!(output.bm25_chunks.is_empty());
    }

    #[test]
    fn single_file_produces_both_sigs_and_chunks() {
        let extractor = ParallelExtractor::new(2);
        let mut entries = HashMap::new();
        entries.insert(
            "test.rs".to_string(),
            make_entry("pub fn hello() -> bool { true }"),
        );
        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        assert_eq!(output.graph_sigs.len(), 1);
        assert_eq!(output.graph_sigs[0].0, "test.rs");
        assert!(
            output.graph_sigs[0]
                .1
                .iter()
                .any(|s| s.name == "hello"),
            "expected 'hello' signature in graph_sigs"
        );

        assert_eq!(output.bm25_chunks.len(), 1);
        assert_eq!(output.bm25_chunks[0].0, "test.rs");
        assert!(
            !output.bm25_chunks[0].1.is_empty(),
            "expected at least one BM25 chunk"
        );
    }

    #[test]
    fn multiple_files_produce_correct_count() {
        let extractor = ParallelExtractor::new(4);
        let mut entries = HashMap::new();
        entries.insert("a.rs".to_string(), make_entry("fn a() {}"));
        entries.insert("b.rs".to_string(), make_entry("fn b() {}"));
        entries.insert("c.rs".to_string(), make_entry("fn c() {}"));

        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        assert_eq!(output.graph_sigs.len(), 3);
        assert_eq!(output.bm25_chunks.len(), 3);

        let paths: Vec<&str> = output.graph_sigs.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.rs"));
        assert!(paths.contains(&"b.rs"));
        assert!(paths.contains(&"c.rs"));
    }

    #[test]
    fn output_is_sorted_by_rel_path() {
        let extractor = ParallelExtractor::new(4);
        let mut entries = HashMap::new();
        entries.insert("z.rs".to_string(), make_entry("fn z() {}"));
        entries.insert("a.rs".to_string(), make_entry("fn a() {}"));
        entries.insert("m.rs".to_string(), make_entry("fn m() {}"));

        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        let paths: Vec<&str> = output.graph_sigs.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "m.rs", "z.rs"]);
    }

    #[test]
    fn parallel_output_matches_sequential_output() {
        let extractor = ParallelExtractor::new(4);
        let mut entries = HashMap::new();
        entries.insert(
            "main.rs".to_string(),
            make_entry("fn main() { println!(\"hello\"); }"),
        );
        entries.insert(
            "lib.rs".to_string(),
            make_entry("pub fn helper() -> u32 { 42 }"),
        );

        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();
        let (seq_sigs, seq_chunks) = extract_sequential(&entries);

        // Compare graph signatures per-file: same number of sigs per path.
        assert_eq!(output.graph_sigs.len(), seq_sigs.len());
        for ((p1, s1), (p2, s2)) in output.graph_sigs.iter().zip(seq_sigs.iter()) {
            assert_eq!(p1, p2, "path mismatch");
            assert_eq!(s1.len(), s2.len(), "signature count mismatch for {p1}");
            // Signature names must match (in order since output is sorted).
            for (sig_a, sig_b) in s1.iter().zip(s2.iter()) {
                assert_eq!(sig_a.name, sig_b.name, "signature name mismatch in {p1}");
            }
        }

        // Compare BM25 chunks per-file.
        assert_eq!(output.bm25_chunks.len(), seq_chunks.len());
        for ((p1, c1), (p2, c2)) in output.bm25_chunks.iter().zip(seq_chunks.iter()) {
            assert_eq!(p1, p2, "path mismatch");
            assert_eq!(c1.len(), c2.len(), "chunk count mismatch for {p1}");
        }
    }

    #[test]
    fn max_workers_is_configurable() {
        let extractor_1 = ParallelExtractor::new(1);
        let extractor_8 = ParallelExtractor::new(8);

        let mut entries = HashMap::new();
        entries.insert("a.rs".to_string(), make_entry("fn a() {}"));
        entries.insert("b.rs".to_string(), make_entry("fn b() {}"));

        let out1 = extractor_1
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();
        let out8 = extractor_8
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        assert_eq!(out1.graph_sigs.len(), out8.graph_sigs.len());
        assert_eq!(out1.bm25_chunks.len(), out8.bm25_chunks.len());
    }

    #[test]
    fn non_rust_file_extracts_correct_signatures() {
        let extractor = ParallelExtractor::new(2);
        let mut entries = HashMap::new();
        entries.insert(
            "script.py".to_string(),
            make_entry("def calculate(x, y):\n    return x + y\n\nclass Worker:\n    pass"),
        );
        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        assert_eq!(output.graph_sigs.len(), 1);
        let sigs = &output.graph_sigs[0].1;
        assert!(
            sigs.iter().any(|s| s.name == "calculate"),
            "expected 'calculate' function in Python extraction"
        );
        assert!(
            sigs.iter().any(|s| s.name == "Worker"),
            "expected 'Worker' class in Python extraction"
        );
    }

    #[test]
    fn ts_file_extracts_typescript_signatures() {
        let extractor = ParallelExtractor::new(2);
        let mut entries = HashMap::new();
        entries.insert(
            "app.ts".to_string(),
            make_entry("export function greet(name: string): string {\n  return `Hello ${name}`;\n}\n\nexport class Greeter {\n  greet(name: string) { return greet(name); }\n}"),
        );
        let output = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        assert_eq!(output.graph_sigs.len(), 1);
        let sigs = &output.graph_sigs[0].1;
        assert!(
            sigs.iter().any(|s| s.name == "greet"),
            "expected 'greet' signature in TS extraction"
        );
        assert!(
            sigs.iter().any(|s| s.name == "Greeter"),
            "expected 'Greeter' signature in TS extraction"
        );
    }

    #[test]
    fn extraction_is_deterministic() {
        let extractor = ParallelExtractor::new(4);
        let mut entries = HashMap::new();
        entries.insert(
            "foo.rs".to_string(),
            make_entry("pub struct Foo;\nimpl Foo {\n  pub fn bar() -> u32 { 0 }\n}"),
        );
        entries.insert(
            "baz.rs".to_string(),
            make_entry("fn baz() {}\nfn qux() {}"),
        );

        let out1 = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();
        let out2 = extractor
            .extract_all(&entries, IndexingMode::Full)
            .unwrap();

        // Same number of results (order is deterministic by sorting).
        assert_eq!(out1.graph_sigs.len(), out2.graph_sigs.len());
        assert_eq!(out1.bm25_chunks.len(), out2.bm25_chunks.len());

        // Paths in same order.
        let paths1: Vec<&str> = out1.graph_sigs.iter().map(|(p, _)| p.as_str()).collect();
        let paths2: Vec<&str> = out2.graph_sigs.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths1, paths2);

        // Same number of signatures per file.
        for ((p1, s1), (p2, s2)) in out1.graph_sigs.iter().zip(out2.graph_sigs.iter()) {
            assert_eq!(p1, p2);
            assert_eq!(s1.len(), s2.len(), "signature count differs for {p1}");
        }
    }
}
