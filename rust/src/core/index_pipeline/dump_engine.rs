//! Dump engine — atomic snapshot + crash recovery for the index pipeline.
//!
//! Serialises in-memory indices (graph, BM25) to disk with tmp→rename atomicity
//! and bounded decompression.  Each dump is self-contained: graph uses
//! postcard+zstd → `.zst`, BM25 delegates to [`BM25Index::save`], and file
//! metadata runs a WAL checkpoint on the SQLite property-graph DB.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::bm25_index::BM25Index;
use crate::core::graph_index::ProjectIndex;
use crate::core::index_namespace;
use crate::core::index_pipeline::file_metadata_store::FileMetadataStore;

/// Atomic dump engine for index snapshots.
///
/// Every write goes through a `.tmp` → rename sequence so partial writes from
/// crashes or OOM never leave a corrupt artifact.  [`load_with_integrity_check`]
/// pairs with this to detect and recover from such scenarios.
pub struct DumpEngine {
    pub project_root: PathBuf,
}

impl DumpEngine {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    /// Serialize graph index to `vectors_dir/project_index.bin.zst`.
    ///
    /// Pipeline: postcard → zstd (level 9) → `.zst.tmp` → atomic rename.
    pub fn dump_graph_index(&self, graph: &ProjectIndex) -> Result<()> {
        let dir = index_namespace::vectors_dir(&self.project_root);
        std::fs::create_dir_all(&dir)?;

        let data = postcard::to_allocvec(graph)
            .map_err(|e| anyhow::anyhow!("postcard serialize graph index: {e}"))?;
        let compressed = zstd::encode_all(data.as_slice(), 9)
            .map_err(|e| anyhow::anyhow!("zstd compress graph index: {e}"))?;

        let tmp = dir.join("project_index.bin.zst.tmp");
        let target = dir.join("project_index.bin.zst");

        std::fs::write(&tmp, &compressed)?;
        std::fs::rename(&tmp, &target)?;

        Ok(())
    }

    /// Persist BM25 index to `vectors_dir/bm25_index.bin.zst`.
    ///
    /// Delegates to [`BM25Index::save`] which uses the same tmp→rename pattern.
    pub fn dump_bm25_index(&self, bm25: &BM25Index) -> Result<()> {
        let _outcome = bm25
            .save(&self.project_root)
            .context("BM25 save failed")?;
        Ok(())
    }

    /// Fsync + WAL checkpoint the property-graph SQLite database.
    ///
    /// Runs `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL is fully checkpointed
    /// into the main DB file, ensuring file_metadata rows are durable.
    pub fn dump_file_metadata(&self, store: &FileMetadataStore) -> Result<()> {
        store
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("WAL checkpoint failed")?;
        Ok(())
    }

    /// Load all three index components with integrity checks.
    ///
    /// Returns `None` for any component whose on-disk artifact is missing or
    /// corrupted.  Callers should trigger a full rebuild for the failed parts.
    ///
    /// Steps:
    /// 1. Remove leftover `.tmp` files from prior crashes.
    /// 2. Load graph index from `project_index.bin.zst` (bounded decompression).
    /// 3. Load BM25 index via [`BM25Index::load`].
    /// 4. Open [`FileMetadataStore`] from the property-graph DB (creates schema
    ///    if absent).
    pub fn load_with_integrity_check(
        root: &Path,
    ) -> Result<(Option<ProjectIndex>, Option<BM25Index>, FileMetadataStore)> {
        let dir = index_namespace::vectors_dir(root);

        // 1. Clean up leftover .tmp files from crashes
        cleanup_tmp_files(&dir);

        // 2. Load graph index
        let graph = load_graph_index(&dir);

        // 3. Load BM25 index
        let bm25 = BM25Index::load(root);

        // 4. Open file metadata store
        let fm_store = open_file_metadata_store(root)?;

        Ok((graph, bm25, fm_store))
    }

    /// Delete all dump artifacts from disk, leaving the property graph DB
    /// (`graph.db`, file_catalog, nodes, edges) intact.
    pub fn purge_all(&self) -> Result<()> {
        let dir = index_namespace::vectors_dir(&self.project_root);
        if !dir.exists() {
            return Ok(());
        }

        let artifacts = [
            "project_index.bin.zst",
            "bm25_index.bin.zst",
            "bm25_index.bin",
            "bm25_index.json",
        ];
        for name in &artifacts {
            let path = dir.join(name);
            if path.exists() {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!("[dump_engine] failed to remove {}: {e}", path.display());
                }
            }
        }
        cleanup_tmp_files(&dir);
        Ok(())
    }
}

// ── File helpers ────────────────────────────────────────────────────────────

/// Remove any leftover `.tmp` files from a prior crash or interrupted write.
fn cleanup_tmp_files(dir: &Path) {
    if !dir.exists() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Load [`ProjectIndex`] from `project_index.bin.zst` with bounded decompression.
///
/// Decompression is capped at 500 MB to prevent OOM from a corrupted or
/// malicious file.  Returns `None` on any failure (missing file, corrupt data,
/// decompression bomb).
fn load_graph_index(dir: &Path) -> Option<ProjectIndex> {
    let zst_path = dir.join("project_index.bin.zst");
    if !zst_path.exists() {
        return None;
    }
    let compressed = std::fs::read(&zst_path).ok()?;
    const MAX_DECOMPRESSED: u64 = 500 * 1024 * 1024; // 500 MB
    let data = bounded_zstd_decode(&compressed, MAX_DECOMPRESSED)?;
    match postcard::from_bytes(&data) {
        Ok(idx) => Some(idx),
        Err(e) => {
            tracing::warn!("[dump_engine] graph index deserialize failed: {e}");
            None
        }
    }
}

/// Bounded zstd decompression that stops after `max_bytes` output bytes.
///
/// Prevents decompression bombs (a small `.zst` expanding to gigabytes).
fn bounded_zstd_decode(compressed: &[u8], max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut decoder = zstd::Decoder::new(compressed).ok()?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    let mut total = 0u64;
    loop {
        let n = decoder.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            tracing::warn!(
                "[dump_engine] decompressed output exceeds limit \
                 ({:.0} MB > {:.0} MB), aborting",
                total as f64 / (1024.0 * 1024.0),
                max_bytes as f64 / (1024.0 * 1024.0)
            );
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(buf)
}

/// Open the [`FileMetadataStore`] from the property-graph DB.
///
/// Creates the DB and `file_metadata` table if they do not exist, so the store
/// is always usable after this call.
fn open_file_metadata_store(root: &Path) -> Result<FileMetadataStore> {
    let graph_dir = crate::core::property_graph::graph_dir(&root.to_string_lossy());
    std::fs::create_dir_all(&graph_dir)?;
    let db_path = graph_dir.join("graph.db");

    let store = FileMetadataStore::open(&db_path)?;
    // Ensure the file_metadata table exists (idempotent).
    store.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS file_metadata (
            path         TEXT NOT NULL PRIMARY KEY,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            mode_mask    INTEGER NOT NULL DEFAULT 0,
            updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    // Validate the store is readable
    if let Err(e) = store.load_all() {
        tracing::warn!(
            "[dump_engine] file_metadata store integrity check failed: {e}"
        );
    }

    Ok(store)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    fn sample_graph(project_root: &str) -> ProjectIndex {
        let mut idx = ProjectIndex::new(project_root);
        idx.files.insert(
            "src/main.rs".to_string(),
            crate::core::graph_index::FileEntry {
                path: "src/main.rs".to_string(),
                hash: "a1b2c3".to_string(),
                language: "rust".to_string(),
                line_count: 42,
                token_count: 120,
                exports: vec!["run".to_string()],
                summary: "Entry point".to_string(),
            },
        );
        idx
    }

    fn sample_bm25() -> BM25Index {
        BM25Index::from_chunks_for_test(vec![
            crate::core::bm25_index::CodeChunk {
                file_path: "src/main.rs".to_string(),
                symbol_name: "run".to_string(),
                kind: crate::core::bm25_index::ChunkKind::Function,
                start_line: 1,
                end_line: 10,
                content: "fn run() { println!(\"hello\"); }".to_string(),
                tokens: vec![],
                token_count: 6,
            },
        ])
    }

    #[test]
    fn dump_produces_files_at_expected_paths() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        let bm25 = sample_bm25();

        engine.dump_graph_index(&graph).unwrap();
        engine.dump_bm25_index(&bm25).unwrap();

        let dir = index_namespace::vectors_dir(root.path());
        assert!(dir.join("project_index.bin.zst").exists());
        assert!(dir.join("bm25_index.bin.zst").exists());
    }

    #[test]
    fn tmp_files_cleaned_after_successful_dump() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());

        engine.dump_graph_index(&graph).unwrap();

        let dir = index_namespace::vectors_dir(root.path());
        assert!(!dir.join("project_index.bin.zst.tmp").exists());
    }

    #[test]
    fn load_after_dump_recovers_same_indices() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        let bm25 = sample_bm25();

        engine.dump_graph_index(&graph).unwrap();
        engine.dump_bm25_index(&bm25).unwrap();

        let (loaded_graph, loaded_bm25, _store) =
            DumpEngine::load_with_integrity_check(root.path()).unwrap();

        let lg = loaded_graph.expect("graph should load");
        assert_eq!(lg.file_count(), graph.file_count());
        assert!(lg.files.contains_key("src/main.rs"));

        let lb = loaded_bm25.expect("bm25 should load");
        assert_eq!(lb.chunks.len(), 1);
        assert_eq!(lb.chunks[0].symbol_name, "run");
    }

    #[test]
    fn purge_all_removes_dump_artifacts() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        let bm25 = sample_bm25();

        engine.dump_graph_index(&graph).unwrap();
        engine.dump_bm25_index(&bm25).unwrap();

        engine.purge_all().unwrap();

        let dir = index_namespace::vectors_dir(root.path());
        assert!(!dir.join("project_index.bin.zst").exists());
        assert!(!dir.join("bm25_index.bin.zst").exists());
    }

    #[test]
    fn crash_recovery_cleans_leftover_tmp() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        // Simulate crash: write only a .tmp file
        let dir = index_namespace::vectors_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project_index.bin.zst.tmp"), b"garbage").unwrap();

        // load_with_integrity_check should clean it up
        let _ = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        assert!(!dir.join("project_index.bin.zst.tmp").exists());
    }

    #[test]
    fn empty_indices_dump_and_load() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let empty_graph = ProjectIndex::new(root.path().to_str().unwrap());
        let empty_bm25 = BM25Index::new();

        engine.dump_graph_index(&empty_graph).unwrap();
        engine.dump_bm25_index(&empty_bm25).unwrap();

        let (graph, bm25, _store) =
            DumpEngine::load_with_integrity_check(root.path()).unwrap();

        let g = graph.expect("empty graph should load");
        assert_eq!(g.file_count(), 0);

        let b = bm25.expect("empty bm25 should load");
        assert_eq!(b.chunks.len(), 0);
    }

    #[test]
    fn load_returns_none_for_missing_artifacts() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let (graph, bm25, _store) =
            DumpEngine::load_with_integrity_check(root.path()).unwrap();

        assert!(graph.is_none(), "no graph artifact should return None");
        assert!(bm25.is_none(), "no bm25 artifact should return None");
    }

    #[test]
    fn purge_all_on_clean_dir_is_noop() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        // Should not error on a clean / non-existent vectors dir
        engine.purge_all().unwrap();
    }

    #[test]
    fn dump_file_metadata_checkpoint() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let store = open_file_metadata_store(root.path()).unwrap();
        store
            .upsert(&crate::core::index_pipeline::file_metadata_store::FileMetadata {
                rel_path: "src/test.rs".to_string(),
                mtime_ns: 1_000_000_000,
                size_bytes: 100,
                content_hash: "abc".to_string(),
                mode_mask: 0x01,
            })
            .unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_file_metadata(&store).unwrap();

        // Data still readable after checkpoint
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("src/test.rs").unwrap().content_hash, "abc");
    }

    #[test]
    fn purge_all_preserves_property_graph_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        // Open the store to create graph.db
        let _store = open_file_metadata_store(root.path()).unwrap();
        let graph_dir = crate::core::property_graph::graph_dir(
            &root.path().to_string_lossy(),
        );
        let db_path = graph_dir.join("graph.db");
        assert!(
            db_path.exists(),
            "property graph DB should have been created"
        );

        // Dump and purge
        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        engine.dump_graph_index(&graph).unwrap();
        engine.purge_all().unwrap();

        // Property graph DB must survive
        assert!(
            db_path.exists(),
            "purge_all must not delete graph.db"
        );
    }
}
