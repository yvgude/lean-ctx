//! Database schema initialization and migration for the code graph.

use rusqlite::Connection;

pub fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;

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
    }

    #[test]
    fn schema_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
    }
}
