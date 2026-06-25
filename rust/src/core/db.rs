//! WAL-mode SQLite connection wrapper.
//!
//! Every connection in the index pipeline should use [`WalConnection`] for
//! consistent WAL journal mode, busy timeout, and foreign-key enforcement.

use std::ops::{Deref, DerefMut};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

/// The current schema version for `code_index.db`.
///
/// Bump this whenever the DDL in `DumpEngine::create_schema` changes so that
/// loading a DB produced by an older version returns an error instead of
/// silently using a mismatched schema.
pub const SCHEMA_VERSION: i64 = 1;

/// A SQLite connection opened with WAL journal mode, a 5-second busy timeout,
/// and foreign-key enforcement.
///
/// Implements `Deref<Target = Connection>` so all `rusqlite::Connection`
/// methods are available directly.
pub struct WalConnection {
    conn: Connection,
}

impl WalConnection {
    /// Open (or create) a SQLite database at `path` with WAL mode.
    ///
    /// Pragmas set:
    /// - `PRAGMA journal_mode = WAL`
    /// - `PRAGMA busy_timeout = 5000`
    /// - `PRAGMA foreign_keys = ON`
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("open SQLite db: {}", path.as_ref().display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )
        .context("set WAL mode pragmas")?;
        Ok(Self { conn })
    }

    /// Open an in-memory database (WAL pragma is a harmless no-op).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory SQLite db")?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )
        .context("set WAL mode pragmas")?;
        Ok(Self { conn })
    }

    /// Consume the wrapper and return the inner `rusqlite::Connection`.
    #[allow(dead_code)]
    pub fn into_inner(self) -> Connection {
        self.conn
    }

    /// Borrow the inner `rusqlite::Connection`.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

impl Deref for WalConnection {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl DerefMut for WalConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;

    #[test]
    fn wal_mode_set_on_open() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_wal.db");
        let conn = WalConnection::open(&db_path).unwrap();

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            mode.to_lowercase(),
            "wal",
            "journal_mode should be WAL, got {mode:?}"
        );
    }

    #[test]
    fn busy_timeout_set() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_busy.db");
        let conn = WalConnection::open(&db_path).unwrap();

        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000, "busy_timeout should be 5000, got {timeout}");
    }

    #[test]
    fn foreign_keys_on() {
        let _iso = isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_fk.db");
        let conn = WalConnection::open(&db_path).unwrap();

        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys should be ON (1), got {fk}");
    }

    #[test]
    fn open_in_memory_works() {
        let conn = WalConnection::open_in_memory().unwrap();
        // In-memory databases cannot use WAL; SQLite silently falls back to
        // "memory". The pragma was set without error, which is the important
        // thing for code-path uniformity.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert!(
            matches!(mode.to_lowercase().as_str(), "memory" | "wal"),
            "in-memory journal_mode should be 'memory' (or 'wal' on some systems), got {mode:?}",
        );
    }
}
