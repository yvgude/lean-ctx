use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;

use super::data_dir::lean_ctx_data_dir;

static DB: std::sync::LazyLock<Mutex<Option<Connection>>> =
    std::sync::LazyLock::new(|| Mutex::new(open_db()));

fn db_path() -> PathBuf {
    lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".lean-ctx"))
        .join("archives")
        .join("index.db")
}

fn open_db() -> Option<Connection> {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(&path).ok()?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
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

    let results = stmt
        .query_map(params![query, limit as i64], |row| {
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
        .unwrap_or_default();

    results
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
        std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

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
}
