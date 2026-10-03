//! Database schema initialization and migration for the code graph.

use rusqlite::Connection;

fn has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in columns {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The PropertyGraph is derived data. Replace pre-FileId code tables in place;
/// callers rebuild them because the engine-version bump marks the graph stale.
fn migrate_legacy_paths(conn: &Connection) -> anyhow::Result<()> {
    if has_column(conn, "nodes", "file_path")? {
        conn.execute_batch("DROP TABLE IF EXISTS edges; DROP TABLE nodes;")?;
    }
    if has_column(conn, "file_catalog", "path")? {
        conn.execute_batch("DROP TABLE file_catalog;")?;
    }
    Ok(())
}

pub(super) fn initialize(conn: &Connection) -> anyhow::Result<()> {
    migrate_legacy_paths(conn)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size  = -8000;
        PRAGMA mmap_size   = 268435456;
        PRAGMA temp_store  = MEMORY;

        CREATE TABLE IF NOT EXISTS paths (
            id   INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS nodes (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            kind       TEXT NOT NULL,
            name       TEXT NOT NULL,
            file_id    INTEGER NOT NULL REFERENCES paths(id),
            line_start INTEGER,
            line_end   INTEGER,
            metadata   TEXT,
            UNIQUE(kind, name, file_id)
        );

        CREATE INDEX IF NOT EXISTS idx_nodes_file
            ON nodes(file_id);
        CREATE INDEX IF NOT EXISTS idx_nodes_name
            ON nodes(name);
        CREATE INDEX IF NOT EXISTS idx_nodes_kind
            ON nodes(kind);
        CREATE INDEX IF NOT EXISTS idx_nodes_kind_file
            ON nodes(kind, file_id);

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
            file_id     INTEGER PRIMARY KEY REFERENCES paths(id),
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

        -- Semantic backend answers per call site. One row per site and
        -- operation: a new caller hash overwrites, so the table stays bounded
        -- by the code size. Valid only while caller_hash matches.
        CREATE TABLE IF NOT EXISTS semantic_resolutions (
            caller_file   TEXT NOT NULL,
            line          INTEGER NOT NULL,
            col           INTEGER NOT NULL,
            op            TEXT NOT NULL,
            caller_hash   TEXT NOT NULL,
            backend       TEXT NOT NULL,
            outcome       TEXT NOT NULL,
            target_file   TEXT,
            target_line   INTEGER,
            target_symbol TEXT,
            context       TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (caller_file, line, col, op)
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
        assert!(tables.contains(&"paths".to_string()));
    }

    #[test]
    fn schema_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
    }

    #[test]
    fn legacy_path_schema_is_replaced() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
                id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
                name TEXT NOT NULL, file_path TEXT NOT NULL
             );
             CREATE TABLE edges (
                id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL,
                target_id INTEGER NOT NULL, kind TEXT NOT NULL
             );
             CREATE TABLE file_catalog (path TEXT PRIMARY KEY, hash TEXT NOT NULL);
             INSERT INTO nodes VALUES (1, 'file', 'old.rs', 'old.rs');
             INSERT INTO file_catalog VALUES ('old.rs', 'hash');",
        )
        .unwrap();

        initialize(&conn).unwrap();
        initialize(&conn).unwrap();

        assert!(has_column(&conn, "nodes", "file_id").unwrap());
        assert!(!has_column(&conn, "nodes", "file_path").unwrap());
        assert!(has_column(&conn, "file_catalog", "file_id").unwrap());
        let nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(nodes, 0);
    }
}
