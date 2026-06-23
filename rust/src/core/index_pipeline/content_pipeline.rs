//! Shared content pipeline for the indexing pipeline.
//!
//! Reads files once and feeds the result to both the graph index builder and
//! the BM25 builder simultaneously, preventing redundant disk I/O.
//!
//! # Architecture
//!
//! ```text
//! ContentPipeline::ingest_file(file)
//!   ├── checks max_file_size (skip if too large)
//!   ├── checks global shared content_cache (hit → cache, return)
//!   ├── reads file content via std::fs::read_to_string (cache miss)
//!   ├── computes content hash (std hash, deterministic)
//!   ├── populates ContentEntry { content, mtime, size, hash }
//!   ├── stores in self.content_cache (HashMap<String, ContentEntry>)
//!   ├── inserts into global shared cache (content_cache::insert)
//!   └── returns &ContentEntry
//!
//! ContentPipeline::into_graph_consumer() → GraphConsumer { entries }
//! ContentPipeline::into_bm25_consumer()  → BM25Consumer  { entries }
//! ```

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::core::content_cache;
use crate::core::index_pipeline::discovery::DiscoveredFile;

// ---------------------------------------------------------------------------
// ContentEntry
// ---------------------------------------------------------------------------

/// Cached content for a single file, produced by [`ContentPipeline::ingest_file`].
#[derive(Debug, Clone)]
pub struct ContentEntry {
    /// Full file content, shared via `Arc` so the two consumer types never copy.
    pub content: Arc<String>,
    /// Last modification time (from the file system).
    pub mtime: SystemTime,
    /// File size in bytes.
    pub size: u64,
    /// Deterministic content hash (std `DefaultHasher` over the full text).
    /// Not cryptographically secure — collision-resistant enough for change
    /// detection across a single session.
    pub content_hash: String,
}

// ---------------------------------------------------------------------------
// ContentPipeline
// ---------------------------------------------------------------------------

/// Reads files once and caches them for multiple downstream consumers.
///
/// Typical usage:
/// ```ignore
/// let mut pipeline = ContentPipeline::new(10_485_760);  // 10 MiB limit
/// for file in discovered_files {
///     pipeline.ingest_file(&file)?;
/// }
/// let graph_consumer = pipeline.into_graph_consumer();
/// let bm25_consumer  = pipeline.into_bm25_consumer();
/// ```
pub struct ContentPipeline {
    /// Map from `rel_path` (project-relative) to content entry.
    content_cache: HashMap<String, ContentEntry>,
    /// Maximum file size in bytes. Files larger than this are skipped.
    max_file_size: u64,
}

impl ContentPipeline {
    /// Create a new pipeline with the given per-file size limit.
    ///
    /// `max_file_size` is the **per-file** cap — any file whose `size` exceeds
    /// this value is silently skipped by [`ingest_file`](Self::ingest_file).
    #[must_use]
    pub fn new(max_file_size: u64) -> Self {
        Self {
            content_cache: HashMap::new(),
            max_file_size,
        }
    }

    /// Read a discovered file into the pipeline cache.
    ///
    /// Returns the [`ContentEntry`] on success. Files larger than
    /// `max_file_size` are skipped (returns `Ok` with a sentinel entry — a
    /// zero-length entry with empty hash, so callers can detect it was seen).
    ///
    /// # Errors
    ///
    /// - I/O errors from reading the file.
    /// - Non-UTF-8 content (wraps the `std::fs::read_to_string` error).
    pub fn ingest_file(&mut self, file: &DiscoveredFile) -> Result<ContentEntry> {
        // Size gate — skip silently (no error).
        if file.size > self.max_file_size {
            let entry = ContentEntry {
                content: Arc::new(String::new()),
                mtime: file.mtime,
                size: file.size,
                content_hash: String::new(),
            };
            // Still cache a sentinel so get_entry reports the file was seen
            // (with a zero-length content and empty hash).
            self.content_cache
                .insert(file.rel_path.clone(), entry.clone());
            return Ok(entry);
        }

        // Check the global shared content cache first — another component (e.g. the
        // BM25 index build, ctx_search, or a previous pipeline run) may have already
        // read this file.  content_cache::get validates against (mtime, size), so a
        // cache-hit is guaranteed fresh.
        let cache_state = content_cache::FileState {
            mtime_ms: system_time_to_millis(file.mtime),
            size_bytes: file.size,
        };
        if let Some(cached) = content_cache::get(&file.path, cache_state) {
            let cached_string: String = cached.to_string();
            let content_arc: Arc<String> = Arc::new(cached_string);
            let content_hash = compute_content_hash(content_arc.as_str());
            let entry = ContentEntry {
                content: Arc::clone(&content_arc),
                mtime: file.mtime,
                size: file.size,
                content_hash,
            };
            self.content_cache
                .insert(file.rel_path.clone(), entry.clone());
            return Ok(entry);
        }

        // Cache miss — read from disk.
        let content_text =
            std::fs::read_to_string(&file.path)
                .with_context(|| format!("failed to read {}", file.path.display()))?;

        let content_arc: Arc<String> = Arc::new(content_text);

        // Compute deterministic hash.
        let content_hash = compute_content_hash(content_arc.as_str());

        let mtime = file.mtime;

        let entry = ContentEntry {
            content: Arc::clone(&content_arc),
            mtime,
            size: file.size,
            content_hash,
        };

        // Store in our local cache.
        self.content_cache
            .insert(file.rel_path.clone(), entry.clone());

        // Also propagate to the global shared cache (content_cache::insert
        // takes Arc<str>, so we convert).
        let content_arc_str: Arc<str> = Arc::from(content_arc.as_str());
        let cache_state = content_cache::FileState {
            mtime_ms: system_time_to_millis(mtime),
            size_bytes: file.size,
        };
        content_cache::insert(&file.path, cache_state, content_arc_str);

        Ok(entry)
    }

    /// Look up a cached entry by its project-relative path.
    #[must_use]
    pub fn get_entry(&self, path: &str) -> Option<&ContentEntry> {
        self.content_cache.get(path)
    }

    /// Number of cached entries.
    #[must_use]
    pub fn content_count(&self) -> usize {
        self.content_cache.len()
    }

    /// Total bytes of all cached file contents.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.content_cache
            .values()
            .map(|e| e.content.len() as u64)
            .sum()
    }

    /// Remove all entries from the pipeline cache.
    pub fn clear(&mut self) {
        self.content_cache.clear();
    }

    /// Consume the pipeline and return a [`GraphConsumer`] wrapping the cached
    /// entries. After this call the pipeline is consumed and cannot be used
    /// again.
    #[must_use]
    pub fn into_graph_consumer(self) -> GraphConsumer {
        GraphConsumer {
            entries: self.content_cache,
        }
    }

    /// Consume the pipeline and return a [`BM25Consumer`] wrapping the cached
    /// entries. After this call the pipeline is consumed and cannot be used
    /// again.
    #[must_use]
    pub fn into_bm25_consumer(self) -> BM25Consumer {
        BM25Consumer {
            entries: self.content_cache,
        }
    }
}

// ---------------------------------------------------------------------------
// Consumer types
// ---------------------------------------------------------------------------

/// Consumes content entries for graph index building.
///
/// Obtained via [`ContentPipeline::into_graph_consumer`].
#[derive(Debug)]
pub struct GraphConsumer {
    entries: HashMap<String, ContentEntry>,
}

impl GraphConsumer {
    /// Borrow the underlying entries map.
    #[must_use]
    pub fn entries(&self) -> &HashMap<String, ContentEntry> {
        &self.entries
    }

    /// Consume the consumer and return the underlying map.
    #[must_use]
    pub fn take(self) -> HashMap<String, ContentEntry> {
        self.entries
    }
}

/// Consumes content entries for BM25 chunk building.
///
/// Obtained via [`ContentPipeline::into_bm25_consumer`].
#[derive(Debug)]
pub struct BM25Consumer {
    entries: HashMap<String, ContentEntry>,
}

impl BM25Consumer {
    /// Borrow the underlying entries map.
    #[must_use]
    pub fn entries(&self) -> &HashMap<String, ContentEntry> {
        &self.entries
    }

    /// Consume the consumer and return the underlying map.
    #[must_use]
    pub fn take(self) -> HashMap<String, ContentEntry> {
        self.entries
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deterministic content hash using std `DefaultHasher`.
///
/// The result is a decimal string representation of the 64-bit hash value.
/// This is **not** cryptographically secure but is collision-resistant enough
/// for change detection within a single indexing session.
fn compute_content_hash(content: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish().to_string()
}

/// Convert [`SystemTime`] to milliseconds since UNIX_EPOCH.
///
/// Falls back to `0` when the system clock is before the epoch (unlikely on
/// modern hardware).
fn system_time_to_millis(t: SystemTime) -> u64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// Helper: write a file at `dir/name` with `body` and return a
    /// [`DiscoveredFile`] for it.
    fn make_file(dir: &Path, name: &str, body: &str) -> DiscoveredFile {
        let abs_path = dir.join(name);
        std::fs::create_dir_all(abs_path.parent().unwrap()).unwrap();
        std::fs::write(&abs_path, body).unwrap();
        let meta = abs_path.metadata().unwrap();
        DiscoveredFile {
            path: abs_path,
            rel_path: name.replace('\\', "/"),
            ext: name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_lowercase(),
            size: meta.len(),
            mtime: meta.modified().unwrap(),
        }
    }

    /// Helper: repeatedly write until file exceeds `target_size` bytes.
    fn make_large_file(dir: &Path, name: &str, target_size: u64) -> DiscoveredFile {
        let abs_path = dir.join(name);
        std::fs::create_dir_all(abs_path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&abs_path).unwrap();
        let chunk = b"x".repeat(1024);
        while abs_path.metadata().unwrap().len() < target_size {
            f.write_all(&chunk).unwrap();
        }
        drop(f);
        let meta = abs_path.metadata().unwrap();
        DiscoveredFile {
            path: abs_path,
            rel_path: name.replace('\\', "/"),
            ext: name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_lowercase(),
            size: meta.len(),
            mtime: meta.modified().unwrap(),
        }
    }

    // -- ingest_file returns content with matching hash --

    #[test]
    fn ingest_file_returns_content_with_matching_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "src/main.rs", "fn main() { println!(\"hi\"); }");

        let mut pipeline = ContentPipeline::new(1_000_000);
        let entry = pipeline.ingest_file(&file).unwrap();

        assert_eq!(
            entry.content_hash,
            compute_content_hash("fn main() { println!(\"hi\"); }"),
            "hash must match the ingested content"
        );
        assert_eq!(&*entry.content, "fn main() { println!(\"hi\"); }");
        assert_eq!(entry.size, file.size);
    }

    // -- get_entry returns cached entry for previously ingested file --

    #[test]
    fn get_entry_returns_cached_entry() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "a.rs", "pub fn a() {}");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&file).unwrap();

        let entry = pipeline.get_entry("a.rs").expect("entry must be cached");
        assert_eq!(&*entry.content, "pub fn a() {}");
    }

    // -- get_entry returns None for unknown path --

    #[test]
    fn get_entry_returns_none_for_missing_path() {
        let pipeline = ContentPipeline::new(1_000_000);
        assert!(pipeline.get_entry("nonexistent.rs").is_none());
    }

    // -- content_cache has entry after ingest (via shared cache) --

    #[test]
    fn shared_cache_has_entry_after_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "shared.rs", "// shared cache test");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&file).unwrap();

        let state = content_cache::FileState {
            mtime_ms: system_time_to_millis(file.mtime),
            size_bytes: file.size,
        };
        let cached = content_cache::get(&file.path, state);
        assert!(
            cached.is_some(),
            "shared content_cache must have the entry after ingest"
        );
        assert_eq!(&*cached.unwrap(), "// shared cache test");
    }

    // -- Re-ingesting same file updates cache entry --

    #[test]
    fn re_ingesting_same_file_updates_cache() {
        let dir = tempfile::tempdir().unwrap();
        let abs_path = dir.path().join("update.rs");
        std::fs::write(&abs_path, "version one content").unwrap();

        let meta = abs_path.metadata().unwrap();
        let file_v1 = DiscoveredFile {
            path: abs_path.clone(),
            rel_path: "update.rs".to_string(),
            ext: "rs".to_string(),
            size: meta.len(),
            mtime: meta.modified().unwrap(),
        };

        let mut pipeline = ContentPipeline::new(1_000_000);
        let entry_v1 = pipeline.ingest_file(&file_v1).unwrap();
        assert_eq!(&*entry_v1.content, "version one content");

        // Modify the file with different content length so FileState differs.
        std::fs::write(&abs_path, "v2-shorter").unwrap();
        let meta = abs_path.metadata().unwrap();
        let file_v2 = DiscoveredFile {
            path: abs_path,
            rel_path: "update.rs".to_string(),
            ext: "rs".to_string(),
            size: meta.len(),
            mtime: meta.modified().unwrap(),
        };

        let entry_v2 = pipeline.ingest_file(&file_v2).unwrap();
        assert_eq!(&*entry_v2.content, "v2-shorter", "re-ingest must return new content");
        assert_ne!(
            entry_v1.content_hash, entry_v2.content_hash,
            "hash must change when content changes"
        );
    }

    // -- Large file exceeding max_file_size is skipped --

    #[test]
    fn large_file_exceeding_max_size_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // Target size: 10 KiB with a 100-byte limit → definitely skipped.
        let file = make_large_file(dir.path(), "giant.rs", 10_240);

        let mut pipeline = ContentPipeline::new(100); // tiny limit
        let entry = pipeline.ingest_file(&file).unwrap();

        assert!(
            entry.content.is_empty(),
            "content must be empty for oversized files"
        );
        assert!(
            entry.content_hash.is_empty(),
            "hash must be empty for oversized files"
        );
        assert_eq!(entry.size, file.size);
        // The sentinel entry is still cached so downstream code can detect it.
        assert!(pipeline.get_entry("giant.rs").is_some());
    }

    // -- into_graph_consumer and into_bm25_consumer produce correct types --

    #[test]
    fn into_graph_consumer_produces_correct_entries() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = make_file(dir.path(), "a.rs", "fn a() {}");
        let f2 = make_file(dir.path(), "b.rs", "fn b() {}");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&f1).unwrap();
        pipeline.ingest_file(&f2).unwrap();

        let graph = pipeline.into_graph_consumer();
        assert_eq!(graph.entries().len(), 2);
        assert!(graph.entries().contains_key("a.rs"));
        assert!(graph.entries().contains_key("b.rs"));

        let map = graph.take();
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn into_bm25_consumer_produces_correct_entries() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = make_file(dir.path(), "x.rs", "fn x() {}");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&f1).unwrap();

        let bm25 = pipeline.into_bm25_consumer();
        assert_eq!(bm25.entries().len(), 1);
        assert!(bm25.entries().contains_key("x.rs"));

        let map = bm25.take();
        assert_eq!(map.len(), 1);
        assert_eq!(&*map.get("x.rs").unwrap().content, "fn x() {}");
    }

    // -- clear() empties the cache --

    #[test]
    fn clear_empties_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "c.rs", "// clear test");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&file).unwrap();
        assert_eq!(pipeline.content_count(), 1);

        pipeline.clear();
        assert_eq!(pipeline.content_count(), 0);
        assert!(pipeline.get_entry("c.rs").is_none());
    }

    // -- content_count and total_bytes reflect cached entries --

    #[test]
    fn content_count_and_total_bytes_are_accurate() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = make_file(dir.path(), "small.rs", "short");
        let f2 = make_file(dir.path(), "medium.rs", "this is a bit longer content");

        let mut pipeline = ContentPipeline::new(1_000_000);
        pipeline.ingest_file(&f1).unwrap();
        pipeline.ingest_file(&f2).unwrap();

        assert_eq!(pipeline.content_count(), 2);
        assert_eq!(pipeline.total_bytes(), (f1.size + f2.size) as u64);
    }

    // -- ingest_file handles root-level file --

    #[test]
    fn ingest_file_handles_root_level_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "Cargo.toml", "[package]\nname = \"test\"\n");

        let mut pipeline = ContentPipeline::new(1_000_000);
        let entry = pipeline.ingest_file(&file).unwrap();

        assert!(!entry.content.is_empty());
        assert_eq!(
            pipeline.get_entry("Cargo.toml").unwrap().content_hash,
            entry.content_hash
        );
    }

    // -- ingest_file reads from shared cache when pre-populated --

    #[test]
    fn ingest_file_hits_shared_cache_when_prepopulated() {
        let dir = tempfile::tempdir().unwrap();
        let file = make_file(dir.path(), "cached.rs", "// from shared cache");

        // Pre-populate the global shared cache before creating the pipeline.
        let cache_state = content_cache::FileState {
            mtime_ms: system_time_to_millis(file.mtime),
            size_bytes: file.size,
        };
        let pre_content: Arc<str> = Arc::from("// from shared cache");
        content_cache::insert(&file.path, cache_state, pre_content);

        // Now ingest_file should hit the cache instead of reading from disk.
        let hits_before = content_cache::stats().hits;
        let mut pipeline = ContentPipeline::new(1_000_000);
        let entry = pipeline.ingest_file(&file).unwrap();
        let hits_after = content_cache::stats().hits;

        assert_eq!(&*entry.content, "// from shared cache");
        assert!(
            hits_after > hits_before,
            "ingest_file must produce a shared cache hit when cache is pre-populated (hits: {hits_before} → {hits_after})"
        );
    }

    // -- ingest_file misses shared cache when file changed since cached --

    #[test]
    fn ingest_file_misses_stale_cache_when_file_changed() {
        let dir = tempfile::tempdir().unwrap();
        let abs_path = dir.path().join("stale_cache.rs");

        // Write v1 and pre-populate cache.
        std::fs::write(&abs_path, "v1 content that is longer").unwrap();
        let meta_v1 = abs_path.metadata().unwrap();
        let state_v1 = content_cache::FileState {
            mtime_ms: system_time_to_millis(meta_v1.modified().unwrap()),
            size_bytes: meta_v1.len(),
        };
        content_cache::insert(&abs_path, state_v1, Arc::from("v1 content that is longer"));

        // Modify the file (v2) with different size so FileState is guaranteed
        // to differ — mtime alone is not reliable on fast filesystems.
        std::fs::write(&abs_path, "v2 shorter").unwrap();

        // Create DiscoveredFile for v2 (with current mtime/size).
        let meta_v2 = abs_path.metadata().unwrap();
        let file_v2 = DiscoveredFile {
            path: abs_path.clone(),
            rel_path: "stale_cache.rs".to_string(),
            ext: "rs".to_string(),
            size: meta_v2.len(),
            mtime: meta_v2.modified().unwrap(),
        };

        let mut pipeline = ContentPipeline::new(1_000_000);
        let entry = pipeline.ingest_file(&file_v2).unwrap();

        // Must read v2 from disk, not the stale cached v1.
        assert_eq!(&*entry.content, "v2 shorter", "must read updated content, not stale cached v1");
    }

    // -- Deterministic hash property (same content → same hash) --

    #[test]
    fn same_content_produces_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = make_file(dir.path(), "one.rs", "fn same() {}");
        let f2 = make_file(dir.path(), "two.rs", "fn same() {}");

        let mut pipeline = ContentPipeline::new(1_000_000);
        let e1 = pipeline.ingest_file(&f1).unwrap();
        let e2 = pipeline.ingest_file(&f2).unwrap();

        assert_eq!(
            e1.content_hash, e2.content_hash,
            "same content must produce identical hash"
        );
    }

    // -- Different content produces different hash --

    #[test]
    fn different_content_produces_different_hash() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = make_file(dir.path(), "alpha.rs", "fn alpha() {}");
        let f2 = make_file(dir.path(), "beta.rs", "fn beta() {}");

        let mut pipeline = ContentPipeline::new(1_000_000);
        let e1 = pipeline.ingest_file(&f1).unwrap();
        let e2 = pipeline.ingest_file(&f2).unwrap();

        assert_ne!(
            e1.content_hash, e2.content_hash,
            "different content must produce different hash"
        );
    }
}
