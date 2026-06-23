//! File metadata store — persisted via the property graph SQLite database.
//!
//! Tracks per-file modification time, size, content hash, and processing mode
//! mask so the index pipeline can incrementally re-index only changed files.

use std::collections::HashMap;

use rusqlite::{params, Connection};

/// Bitmask values for `mode_mask`.
///
/// - `FULL`     (0x01): full semantic index
/// - `MODERATE` (0x02): moderate index
/// - `FAST`     (0x04): fast (structural-only) index
pub mod mode {
    pub const FULL: u32 = 0x01;
    pub const MODERATE: u32 = 0x02;
    pub const FAST: u32 = 0x04;
}

/// Per-file metadata row.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    pub rel_path: String,
    pub mtime_ns: i64,
    pub size_bytes: i64,
    pub content_hash: String,
    pub mode_mask: u32,
}

/// CRUD store for `file_metadata` backed by a SQLite connection.
///
/// The store **takes ownership** of the connection rather than borrowing it,
/// keeping the interface self-contained.
pub struct FileMetadataStore {
    db: Connection,
}

impl FileMetadataStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let db = Connection::open(path)?;
        Ok(Self { db })
    }

    /// Wrap an existing connection.
    pub fn new(db: Connection) -> Self {
        Self { db }
    }

    /// Insert or replace a single metadata row.
    pub fn upsert(&self, meta: &FileMetadata) -> anyhow::Result<()> {
        let mut stmt = self.db.prepare_cached(
            "INSERT OR REPLACE INTO file_metadata (path, mtime_ns, size_bytes, content_hash, mode_mask)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        stmt.execute(params![
            meta.rel_path,
            meta.mtime_ns,
            meta.size_bytes,
            meta.content_hash,
            meta.mode_mask,
        ])?;
        Ok(())
    }

    /// Batch upsert inside a single transaction.
    ///
    /// Returns `Err` if **any** row fails — the transaction is rolled back
    /// so the write is all-or-nothing.
    pub fn upsert_batch(&self, metas: &[FileMetadata]) -> anyhow::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO file_metadata (path, mtime_ns, size_bytes, content_hash, mode_mask)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for meta in metas {
                stmt.execute(params![
                    meta.rel_path,
                    meta.mtime_ns,
                    meta.size_bytes,
                    meta.content_hash,
                    meta.mode_mask,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load every row into a `HashMap` keyed by `rel_path`.
    pub fn load_all(&self) -> anyhow::Result<HashMap<String, FileMetadata>> {
        let mut stmt = self
            .db
            .prepare_cached("SELECT path, mtime_ns, size_bytes, content_hash, mode_mask FROM file_metadata")?;
        let rows = stmt.query_map([], |row| {
            Ok(FileMetadata {
                rel_path: row.get(0)?,
                mtime_ns: row.get(1)?,
                size_bytes: row.get(2)?,
                content_hash: row.get(3)?,
                mode_mask: row.get::<_, i64>(4)? as u32,
            })
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let meta = row?;
            map.insert(meta.rel_path.clone(), meta);
        }
        Ok(map)
    }

    /// Load rows whose `mode_mask` has **any** of the bits in `mode_mask` set.
    ///
    /// For example `load_for_mode(mode::FULL)` returns rows where `(mode_mask & 0x01) != 0`.
    pub fn load_for_mode(&self, mode_mask: u32) -> anyhow::Result<HashMap<String, FileMetadata>> {
        let mut stmt = self.db.prepare_cached(
            "SELECT path, mtime_ns, size_bytes, content_hash, mode_mask FROM file_metadata
             WHERE (mode_mask & ?1) != 0",
        )?;
        let rows = stmt.query_map(params![mode_mask as i64], |row| {
            Ok(FileMetadata {
                rel_path: row.get(0)?,
                mtime_ns: row.get(1)?,
                size_bytes: row.get(2)?,
                content_hash: row.get(3)?,
                mode_mask: row.get::<_, i64>(4)? as u32,
            })
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let meta = row?;
            map.insert(meta.rel_path.clone(), meta);
        }
        Ok(map)
    }

    /// Delete a single row by path.
    pub fn delete(&self, path: &str) -> anyhow::Result<()> {
        let mut stmt = self
            .db
            .prepare_cached("DELETE FROM file_metadata WHERE path = ?1")?;
        stmt.execute(params![path])?;
        Ok(())
    }

    /// Delete multiple rows inside a single transaction.
    pub fn delete_batch(&self, paths: &[String]) -> anyhow::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM file_metadata WHERE path = ?1")?;
            for p in paths {
                stmt.execute(params![p])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Expose the underlying connection for schema initialization, etc.
    pub fn connection(&self) -> &Connection {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> FileMetadataStore {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_metadata (
                path         TEXT NOT NULL PRIMARY KEY,
                mtime_ns     INTEGER NOT NULL,
                size_bytes   INTEGER NOT NULL,
                content_hash TEXT NOT NULL DEFAULT '',
                mode_mask    INTEGER NOT NULL DEFAULT 0,
                updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        FileMetadataStore::new(conn)
    }

    fn sample(path: &str, mode: u32) -> FileMetadata {
        FileMetadata {
            rel_path: path.to_string(),
            mtime_ns: 1_000_000_000,
            size_bytes: 1024,
            content_hash: "abc123".to_string(),
            mode_mask: mode,
        }
    }

    // -- upsert -------------------------------------------------------------

    #[test]
    fn upsert_inserts_new_row() {
        let store = setup();
        let meta = sample("src/main.rs", mode::FULL);
        store.upsert(&meta).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("src/main.rs").unwrap().content_hash, "abc123");
    }

    #[test]
    fn upsert_updates_existing_row() {
        let store = setup();
        store.upsert(&sample("src/main.rs", mode::FULL)).unwrap();

        let updated = FileMetadata {
            rel_path: "src/main.rs".to_string(),
            mtime_ns: 2_000_000_000,
            size_bytes: 2048,
            content_hash: "def456".to_string(),
            mode_mask: mode::FAST,
        };
        store.upsert(&updated).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        let row = all.get("src/main.rs").unwrap();
        assert_eq!(row.content_hash, "def456");
        assert_eq!(row.mtime_ns, 2_000_000_000);
        assert_eq!(row.mode_mask, mode::FAST);
    }

    // -- batch upsert -------------------------------------------------------

    #[test]
    fn upsert_batch_inserts_many() {
        let store = setup();
        let metas: Vec<_> = (0..100)
            .map(|i| FileMetadata {
                rel_path: format!("src/file_{}.rs", i),
                mtime_ns: i as i64,
                size_bytes: 100,
                content_hash: format!("hash_{}", i),
                mode_mask: if i % 2 == 0 { mode::FULL } else { mode::FAST },
            })
            .collect();

        store.upsert_batch(&metas).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 100);
    }

    #[test]
    fn upsert_batch_atomic_rollback_on_failure() {
        let store = setup();
        // Insert one row first.
        store.upsert(&sample("keep_me.rs", mode::FULL)).unwrap();

        // Build a batch — the actual rows don't matter since the db is read-only.
        let metas = vec![sample("a.rs", mode::FULL)];

        // Force the connection into an error state.
        store
            .connection()
            .execute_batch("PRAGMA query_only = 1;")
            .unwrap();
        let result = store.upsert_batch(&metas);
        // After query_only, writes should fail.
        assert!(result.is_err(), "write on query_only db should fail");

        // The original row must still be present (rollback).
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1, "atomicity violated: rollback expected");
        assert!(all.contains_key("keep_me.rs"));
    }

    // -- load_for_mode ------------------------------------------------------

    #[test]
    fn load_for_mode_returns_only_matching() {
        let store = setup();
        store
            .upsert(&sample("full.rs", mode::FULL))
            .unwrap();
        store
            .upsert(&sample("moderate.rs", mode::MODERATE))
            .unwrap();
        store.upsert(&sample("fast.rs", mode::FAST)).unwrap();
        store
            .upsert(&sample("all.rs", mode::FULL | mode::MODERATE | mode::FAST))
            .unwrap();

        let full = store.load_for_mode(mode::FULL).unwrap();
        assert_eq!(full.len(), 2); // full.rs + all.rs
        assert!(full.contains_key("full.rs"));
        assert!(full.contains_key("all.rs"));

        let fast = store.load_for_mode(mode::FAST).unwrap();
        assert_eq!(fast.len(), 2); // fast.rs + all.rs
    }

    // -- delete -------------------------------------------------------------

    #[test]
    fn delete_removes_row() {
        let store = setup();
        store.upsert(&sample("gone.rs", mode::FULL)).unwrap();
        store.upsert(&sample("stay.rs", mode::FULL)).unwrap();

        store.delete("gone.rs").unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all.contains_key("stay.rs"));
    }

    #[test]
    fn delete_batch_removes_multiple() {
        let store = setup();
        for i in 0..10 {
            store
                .upsert(&sample(&format!("file_{}.rs", i), mode::FULL))
                .unwrap();
        }

        let to_remove: Vec<String> = (0..5).map(|i| format!("file_{}.rs", i)).collect();
        store.delete_batch(&to_remove).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 5);
        for i in 5..10 {
            assert!(all.contains_key(&format!("file_{}.rs", i)));
        }
    }

    // -- 1000-row batch -----------------------------------------------------

    #[test]
    fn upsert_batch_1000_rows() {
        let store = setup();
        let metas: Vec<_> = (0..1000)
            .map(|i| FileMetadata {
                rel_path: format!("file_{}.rs", i),
                mtime_ns: i as i64,
                size_bytes: (i * 100) % 65536,
                content_hash: format!("{:016x}", i),
                mode_mask: if i % 3 == 0 {
                    mode::FULL
                } else if i % 3 == 1 {
                    mode::MODERATE
                } else {
                    mode::FAST
                },
            })
            .collect();

        store.upsert_batch(&metas).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1000);

        // Quick spot-check
        assert_eq!(all.get("file_0.rs").unwrap().mtime_ns, 0);
        assert_eq!(all.get("file_999.rs").unwrap().mtime_ns, 999);
    }

    // -- load_all on empty store -------------------------------------------

    #[test]
    fn load_all_empty() {
        let store = setup();
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    // -- delete non-existent path -------------------------------------------

    #[test]
    fn delete_non_existent_is_noop() {
        let store = setup();
        store.delete("no_such_file.rs").unwrap();
        // Should not error or change anything.
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }
}
