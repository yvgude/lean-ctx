//! Database schema initialization and migration for the code graph.

use rusqlite::Connection;

pub(super) fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size  = -8000;
        PRAGMA mmap_size   = 268435456;
        PRAGMA temp_store  = MEMORY;

        CREATE TABLE IF NOT EXISTS nodes (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            kind       TEXT NOT NULL,
            name       TEXT NOT NULL,
            file_path  TEXT NOT NULL,
            line_start INTEGER,
            line_end   INTEGER,
            metadata   TEXT,
            UNIQUE(kind, name, file_path)
        );

        CREATE INDEX IF NOT EXISTS idx_nodes_file
            ON nodes(file_path);
        CREATE INDEX IF NOT EXISTS idx_nodes_name
            ON nodes(name);
        CREATE INDEX IF NOT EXISTS idx_nodes_kind
            ON nodes(kind);
        CREATE INDEX IF NOT EXISTS idx_nodes_kind_file
            ON nodes(kind, file_path);

        CREATE TABLE IF NOT EXISTS edges (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            kind      TEXT NOT NULL,
            metadata  TEXT,
            UNIQUE(source_id, target_id, kind)
        );

        CREATE INDEX IF NOT EXISTS idx_edges_source
            ON edges(source_id);
        CREATE INDEX IF NOT EXISTS idx_edges_target
            ON edges(target_id);
        CREATE INDEX IF NOT EXISTS idx_edges_kind
            ON edges(kind);
        CREATE INDEX IF NOT EXISTS idx_edges_source_kind
            ON edges(source_id, kind);
        CREATE INDEX IF NOT EXISTS idx_edges_target_kind
            ON edges(target_id, kind);

        CREATE TABLE IF NOT EXISTS file_catalog (
            path        TEXT PRIMARY KEY,
            hash        TEXT NOT NULL,
            language    TEXT NOT NULL DEFAULT '',
            line_count  INTEGER NOT NULL DEFAULT 0,
            token_count INTEGER NOT NULL DEFAULT 0,
            exports     TEXT NOT NULL DEFAULT '[]',
            summary     TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS cross_source_edges (
            from_path TEXT NOT NULL,
            to_path   TEXT NOT NULL,
            kind      TEXT NOT NULL,
            weight    REAL NOT NULL DEFAULT 1.0,
            PRIMARY KEY (from_path, to_path, kind)
        );

        CREATE INDEX IF NOT EXISTS idx_cross_source_from
            ON cross_source_edges(from_path);
        CREATE INDEX IF NOT EXISTS idx_cross_source_to
            ON cross_source_edges(to_path);

        CREATE TABLE IF NOT EXISTS file_metadata (
            path         TEXT NOT NULL PRIMARY KEY,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            mode_mask    INTEGER NOT NULL DEFAULT 0,
            updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();

        assert!(tables.contains(&"nodes".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(tables.contains(&"file_catalog".to_string()));
        assert!(tables.contains(&"cross_source_edges".to_string()));
        assert!(tables.contains(&"file_metadata".to_string()));
    }

    #[test]
    fn schema_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
    }
}
