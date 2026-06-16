//! Daemon-safe storage maintenance.
//!
//! Unlike the interactive `lean-ctx cache prune` (which prints per-file output),
//! these routines are silent (tracing only) so they can run inside the MCP
//! daemon without corrupting the stdio protocol. They enforce the disk budget
//! that the field had been silently exceeding (see EPIC 6 / #2364): unbounded
//! archive FTS growth and accumulated quarantined BM25 indexes.

use std::path::PathBuf;

/// Result of a quiet maintenance pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaintenanceResult {
    pub quarantined_removed: u32,
    pub bytes_freed: u64,
    pub archive_entries_pruned: u32,
    pub archive_db_bytes_after: u64,
}

const QUARANTINED_FILES: &[&str] = &[
    "bm25_index.json.quarantined",
    "bm25_index.bin.quarantined",
    "bm25_index.bin.zst.quarantined",
];

/// Remove accumulated quarantined BM25 index files. These are dead weight: an
/// index is only quarantined when it failed a load/size check and was replaced.
fn prune_quarantined_bm25() -> (u32, u64) {
    let mut removed = 0u32;
    let mut freed = 0u64;
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return (removed, freed);
    };
    let vectors_dir = data_dir.join("vectors");
    let Ok(entries) = std::fs::read_dir(&vectors_dir) else {
        return (removed, freed);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        for q_name in QUARANTINED_FILES {
            let q: PathBuf = dir.join(q_name);
            if q.exists() {
                if let Ok(meta) = std::fs::metadata(&q) {
                    freed = freed.saturating_add(meta.len());
                }
                if std::fs::remove_file(&q).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    (removed, freed)
}

/// Run a silent maintenance pass: prune quarantined BM25 indexes and enforce
/// the archive FTS size cap. Safe to call from the MCP daemon.
pub fn run_quiet() -> MaintenanceResult {
    let (quarantined_removed, bytes_freed) = prune_quarantined_bm25();
    // Enforce the archive TTL + on-disk size budget (prunes `.txt`/`.meta.json`
    // + FTS rows together), then backstop the DB cap. Without this the archive
    // grew unbounded on disk and exhausted host RAM via the page cache (#417).
    let archive_entries_pruned = crate::core::archive::cleanup();
    let archive_db_bytes_after = crate::core::archive_fts::enforce_cap();
    if quarantined_removed > 0 || archive_entries_pruned > 0 {
        tracing::info!(
            "storage maintenance: pruned {quarantined_removed} quarantined BM25 index file(s) \
             (freed {bytes_freed} bytes) + {archive_entries_pruned} archive entry/entries; \
             archive DB now {archive_db_bytes_after} bytes"
        );
    }
    MaintenanceResult {
        quarantined_removed,
        bytes_freed,
        archive_entries_pruned,
        archive_db_bytes_after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_removes_quarantined_files() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

        let idx_dir = tmp.path().join("vectors").join("proj_abc");
        std::fs::create_dir_all(&idx_dir).unwrap();
        std::fs::write(idx_dir.join("bm25_index.json.quarantined"), b"dead").unwrap();
        std::fs::write(idx_dir.join("bm25_index.bin"), b"live").unwrap();

        let (removed, freed) = prune_quarantined_bm25();
        assert_eq!(removed, 1);
        assert!(freed >= 4);
        assert!(!idx_dir.join("bm25_index.json.quarantined").exists());
        assert!(
            idx_dir.join("bm25_index.bin").exists(),
            "live index must be preserved"
        );

        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
    }
}
