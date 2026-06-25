use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::Mutex;

use super::data_dir::lean_ctx_data_dir;

static DB: std::sync::LazyLock<Mutex<Option<Connection>>> =
    std::sync::LazyLock::new(|| Mutex::new(open_db()));

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
#[must_use]
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

fn open_db() -> Option<Connection> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(&path).ok()?;
    conn.execute_batch(
        // `busy_timeout` lets a checkpoint wait for a concurrent reader instead of
        // bailing immediately, and an explicit `wal_autocheckpoint` keeps the WAL
        // bounded even when several lean-ctx processes (daemon + MCP + CLI) hold
        // the same DB open. Without these, a stale reader (e.g. an orphaned
        // daemon) blocked autocheckpoint and the WAL grew to 256 MB in the field.
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;
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
    )
    .ok()?;
    Some(conn)
}

pub fn index_entry(archive_id: &str, tool: &str, command: &str, content: &str) {
    let guard = DB.lock().ok();
    let Some(conn) = guard.as_ref().and_then(|g| g.as_ref()) else {
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
    // held back autocheckpoint and the sidecar ballooned, reclaim it now.
    if wal_bytes() > WAL_TRUNCATE_THRESHOLD_BYTES {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

/// Enforces the on-disk size cap by deleting the oldest archive entries (by
/// `created_at`) in batches until the DB is back under budget, then reclaims
/// space with VACUUM. Operates on an already-locked connection.
fn enforce_cap_locked(conn: &Connection) {
    let cap = max_db_bytes();
    if db_size_bytes() <= cap {
        return;
    }
    // Delete in batches of ~10% of current rows (min 50) until under cap or empty.
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
            // Drop the backing `.txt`/`.meta.json` too — deleting only the DB row
            // would orphan the (much larger) content file on disk (#417).
            super::archive::remove_files(id);
        }
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;");
        if db_size_bytes() <= cap {
            break;
        }
    }
}

/// Public entry point to enforce the archive DB size cap on demand (e.g. from
/// idle maintenance or `doctor`). Returns the resulting size in bytes.
pub fn enforce_cap() -> u64 {
    if let Ok(guard) = DB.lock()
        && let Some(conn) = guard.as_ref()
    {
        enforce_cap_locked(conn);
    }
    db_size_bytes()
}

pub fn remove_entry(archive_id: &str) {
    let guard = DB.lock().ok();
    let Some(conn) = guard.as_ref().and_then(|g| g.as_ref()) else {
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
    let guard = DB.lock().ok();
    let Some(conn) = guard.as_ref().and_then(|g| g.as_ref()) else {
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
    let guard = DB.lock().ok();
    let Some(conn) = guard.as_ref().and_then(|g| g.as_ref()) else {
        return 0;
    };
    conn.query_row("SELECT COUNT(*) FROM archive_meta", [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_roundtrip() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
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
}
