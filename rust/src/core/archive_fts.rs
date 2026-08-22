use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::Mutex;

use super::data_dir::lean_ctx_data_dir;

struct CachedConnection {
    path: PathBuf,
    connection: Option<Connection>,
}

impl CachedConnection {
    fn new() -> Self {
        let path = db_path();
        let connection = open_db_at(&path);
        Self { path, connection }
    }

    fn current(&mut self) -> Option<&Connection> {
        let path = db_path();
        if path != self.path {
            self.connection = open_db_at(&path);
            self.path = path;
        }
        self.connection.as_ref()
    }
}

static DB: std::sync::LazyLock<Mutex<CachedConnection>> =
    std::sync::LazyLock::new(|| Mutex::new(CachedConnection::new()));

/// Default maximum on-disk size for the archive FTS database. Overridable via
/// `LEAN_CTX_ARCHIVE_DB_MAX_MB`. Without enforcement this DB grew unbounded
/// (observed 576 MB in the field — see EPIC 6 / #2364).
const DEFAULT_MAX_DB_MB: u64 = 500;

/// Run cap enforcement roughly every N inserts to amortize the VACUUM cost.
const ENFORCE_EVERY_N_INSERTS: usize = 200;

/// If the `-wal` sidecar ever exceeds this size, force a TRUNCATE checkpoint on
/// the next write regardless of insert count. This bounds the footprint when a
/// concurrent reader in another lean-ctx process has been holding back
/// autocheckpoint (observed 256 MB WAL caused by a stale/orphaned daemon).
const WAL_TRUNCATE_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

fn max_db_bytes() -> u64 {
    std::env::var("LEAN_CTX_ARCHIVE_DB_MAX_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MAX_DB_MB)
        .saturating_mul(1024 * 1024)
}

fn db_path() -> PathBuf {
    lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".lean-ctx"))
        .join("archives")
        .join("index.db")
}

/// Current on-disk size of the archive DB in bytes (including WAL). Used by
/// `doctor` to surface the footprint budget.
pub fn db_size_bytes() -> u64 {
    let base = db_path();
    let mut total = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            base.clone()
        } else {
            PathBuf::from(format!("{}{suffix}", base.display()))
        };
        if let Ok(meta) = std::fs::metadata(&p) {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Current size of just the `-wal` sidecar file in bytes.
fn wal_bytes() -> u64 {
    let wal = PathBuf::from(format!("{}-wal", db_path().display()));
    std::fs::metadata(&wal).map_or(0, |m| m.len())
}

/// Maximum attempts to open the DB when hitting transient WAL lock contention.
/// Mirrors the retry strategy in `property_graph::CodeGraph::open` (#1409).
const DB_OPEN_MAX_ATTEMPTS: u32 = 8;

#[cfg(test)]
fn open_db() -> Option<Connection> {
    let path = db_path();
    open_db_at(&path)
}

fn open_db_at(path: &std::path::Path) -> Option<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // #1409: Multiple MCP stdio processes (Devin/Windsurf) can race on WAL init.
    // PRAGMA journal_mode=WAL + CREATE TABLE can fail with SQLITE_BUSY even with
    // busy_timeout set, because the busy handler is not invoked for initial schema
    // DDL (same issue as graph.db — see property_graph/mod.rs:92). Retry with
    // exponential backoff.
    for attempt in 0..DB_OPEN_MAX_ATTEMPTS {
        match try_open_db(&path) {
            Ok(conn) => return Some(conn),
            Err(e) => {
                if attempt + 1 < DB_OPEN_MAX_ATTEMPTS && is_transient_sqlite_error(&e) {
                    std::thread::sleep(std::time::Duration::from_millis(
                        50 * u64::from(attempt + 1),
                    ));
                    continue;
                }
                tracing::warn!(
                    "archive_fts: failed to open index.db after {} attempts: {e}",
                    attempt + 1
                );
                return None;
            }
        }
    }
    None
}

fn try_open_db(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=10000;
         PRAGMA wal_autocheckpoint=1000;
         CREATE TABLE IF NOT EXISTS archive_meta (
             archive_id TEXT PRIMARY KEY,
             tool TEXT NOT NULL,
             command TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS archive_fts USING fts5(
             tool,
             command,
             content,
             archive_id UNINDEXED
         );",
    )?;
    Ok(conn)
}

fn is_transient_sqlite_error(e: &rusqlite::Error) -> bool {
    use rusqlite::ffi;
    matches!(
        e,
        rusqlite::Error::SqliteFailure(
            ffi::Error {
                code: ffi::ErrorCode::DatabaseBusy | ffi::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

pub fn index_entry(archive_id: &str, tool: &str, command: &str, content: &str) {
    let Some(mut guard) = DB.lock().ok() else {
        return;
    };
    let Some(conn) = guard.current() else {
        return;
    };

    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM archive_meta WHERE archive_id = ?1",
            params![archive_id],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if exists {
        return;
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    let _ = conn.execute(
        "INSERT OR IGNORE INTO archive_meta (archive_id, tool, command, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![archive_id, tool, command, created_at],
    );
    let _ = conn.execute(
        "INSERT INTO archive_fts (archive_id, tool, command, content) VALUES (?1, ?2, ?3, ?4)",
        params![archive_id, tool, command, content],
    );

    // Amortized cap enforcement: only check periodically, since size checks +
    // VACUUM are not free.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_meta", [], |row| row.get(0))
        .unwrap_or(0);
    if (count as usize).is_multiple_of(ENFORCE_EVERY_N_INSERTS) {
        enforce_cap_locked(conn);
    }

    // Bound the WAL even between cap-enforcement passes: if a concurrent reader
    // held back autocheckpoint and the sidecar ballooned, try to reclaim it now.
    // #1409: best-effort — don't block on lock contention from stale processes.
    if wal_bytes() > WAL_TRUNCATE_THRESHOLD_BYTES {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }
}

/// Enforces the on-disk size cap by deleting the oldest archive entries (by
/// `created_at`) in batches until the DB is back under budget, then reclaims
/// space with VACUUM. Operates on an already-locked connection.
///
/// #1409: VACUUM and checkpoint are best-effort — if they fail with BUSY (another
/// process holds the WAL), we skip them rather than blocking tool calls. The cap
/// enforcement (deletes) still runs; space reclamation happens on the next
/// successful pass.
fn enforce_cap_locked(conn: &Connection) {
    let cap = max_db_bytes();
    if db_size_bytes() <= cap {
        return;
    }
    for _ in 0..50 {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM archive_meta", [], |row| row.get(0))
            .unwrap_or(0);
        if count == 0 {
            break;
        }
        let batch = (count / 10).max(50);
        let ids: Vec<String> = conn
            .prepare("SELECT archive_id FROM archive_meta ORDER BY created_at ASC LIMIT ?1")
            .and_then(|mut stmt| {
                let rows = stmt.query_map(params![batch], |row| row.get::<_, String>(0))?;
                Ok(rows.flatten().collect::<Vec<_>>())
            })
            .unwrap_or_default();
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            let _ = conn.execute(
                "DELETE FROM archive_meta WHERE archive_id = ?1",
                params![id],
            );
            let _ = conn.execute("DELETE FROM archive_fts WHERE archive_id = ?1", params![id]);
            super::archive::remove_files(id);
        }
        // Best-effort reclamation: skip if another process holds the WAL lock.
        if conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .is_ok()
        {
            let _ = conn.execute_batch("VACUUM;");
        }
        if db_size_bytes() <= cap {
            break;
        }
    }
}

/// Public entry point to enforce the archive DB size cap on demand (e.g. from
/// idle maintenance or `doctor`). Returns the resulting size in bytes.
pub fn enforce_cap() -> u64 {
    if let Ok(mut guard) = DB.lock()
        && let Some(conn) = guard.current()
    {
        enforce_cap_locked(conn);
    }
    db_size_bytes()
}

pub fn remove_entry(archive_id: &str) {
    let Some(mut guard) = DB.lock().ok() else {
        return;
    };
    let Some(conn) = guard.current() else {
        return;
    };
    let _ = conn.execute(
        "DELETE FROM archive_meta WHERE archive_id = ?1",
        params![archive_id],
    );
    let _ = conn.execute(
        "DELETE FROM archive_fts WHERE archive_id = ?1",
        params![archive_id],
    );
}

#[derive(Debug, Clone)]
pub struct FtsResult {
    pub archive_id: String,
    pub tool: String,
    pub command: String,
    pub snippet: String,
    pub rank: f64,
}

pub fn search(query: &str, limit: usize) -> Vec<FtsResult> {
    let Some(mut guard) = DB.lock().ok() else {
        return Vec::new();
    };
    let Some(conn) = guard.current() else {
        return Vec::new();
    };

    let Ok(mut stmt) = conn.prepare(
        "SELECT archive_id, tool, command, snippet(archive_fts, 2, '»', '«', '…', 40), rank
         FROM archive_fts
         WHERE archive_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    ) else {
        return Vec::new();
    };

    stmt.query_map(params![query, limit as i64], |row| {
        Ok(FtsResult {
            archive_id: row.get(0)?,
            tool: row.get(1)?,
            command: row.get(2)?,
            snippet: row.get(3)?,
            rank: row.get(4)?,
        })
    })
    .ok()
    .map(|rows| rows.flatten().collect::<Vec<_>>())
    .unwrap_or_default()
}

pub fn entry_count() -> usize {
    let Some(mut guard) = DB.lock().ok() else {
        return 0;
    };
    let Some(conn) = guard.current() else {
        return 0;
    };
    conn.query_row("SELECT COUNT(*) FROM archive_meta", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0) as usize
}

#[cfg(test)]
pub mod tests {
    use super::*;

    struct EnvGuard(Option<std::ffi::OsString>);

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.0 {
                crate::test_env::set_var("LEAN_CTX_DATA_DIR", previous);
            } else {
                crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
            }
        }
    }

    #[test]
    fn fts_roundtrip() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _env = EnvGuard(std::env::var_os("LEAN_CTX_DATA_DIR"));
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        // Force re-open by directly testing open_db
        let conn = open_db().expect("should open");
        conn.execute(
            "INSERT INTO archive_meta (archive_id, tool, command, created_at) VALUES ('t1', 'shell', 'git log', '2026-01-01')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO archive_fts (archive_id, tool, command, content) VALUES ('t1', 'shell', 'git log', 'commit abc refactored the parser module')",
            [],
        ).unwrap();

        let mut stmt = conn
            .prepare("SELECT archive_id FROM archive_fts WHERE archive_fts MATCH 'parser'")
            .unwrap();
        let ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(ids, vec!["t1"]);
    }

    #[test]
    fn open_db_bounds_the_wal() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _env = EnvGuard(std::env::var_os("LEAN_CTX_DATA_DIR"));
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        let conn = open_db().expect("should open");

        // WAL journal mode is required for the FTS write path.
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        // A bounded (non-zero) autocheckpoint is what keeps the sidecar from
        // growing unbounded when another process holds the DB open.
        let autocheckpoint: i64 = conn
            .query_row("PRAGMA wal_autocheckpoint;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(autocheckpoint, 1000);
    }

    #[test]
    fn cached_connection_tracks_the_active_data_dir() {
        let _lock = crate::core::data_dir::test_env_lock();
        let original_path = db_path();
        let _env = EnvGuard(std::env::var_os("LEAN_CTX_DATA_DIR"));
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        crate::test_env::set_var("LEAN_CTX_DATA_DIR", first.path());
        index_entry(
            "mes1609_first",
            "ctx_shell",
            "first",
            "mes1609_fts_first_only",
        );
        assert_eq!(
            search("mes1609_fts_first_only", 10)[0].archive_id,
            "mes1609_first"
        );

        crate::test_env::set_var("LEAN_CTX_DATA_DIR", second.path());
        index_entry(
            "mes1609_second",
            "ctx_shell",
            "second",
            "mes1609_fts_second_only",
        );
        assert_eq!(
            search("mes1609_fts_second_only", 10)[0].archive_id,
            "mes1609_second"
        );
        assert!(search("mes1609_fts_first_only", 10).is_empty());

        crate::test_env::set_var(
            "LEAN_CTX_DATA_DIR",
            original_path.parent().unwrap().parent().unwrap(),
        );
        assert!(search("mes1609_fts_first_only", 10).is_empty());
        assert!(search("mes1609_fts_second_only", 10).is_empty());
    }
}
