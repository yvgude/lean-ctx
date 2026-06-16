use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct FileCatalogEntry {
    pub path: String,
    pub hash: String,
    pub language: String,
    pub line_count: usize,
    pub token_count: usize,
    pub exports: Vec<String>,
    pub summary: String,
}

pub(super) fn upsert(conn: &Connection, entry: &FileCatalogEntry) -> anyhow::Result<()> {
    let exports_json = serde_json::to_string(&entry.exports).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO file_catalog (path, hash, language, line_count, token_count, exports, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET
             hash = excluded.hash,
             language = excluded.language,
             line_count = excluded.line_count,
             token_count = excluded.token_count,
             exports = excluded.exports,
             summary = excluded.summary",
        params![
            entry.path,
            entry.hash,
            entry.language,
            entry.line_count as i64,
            entry.token_count as i64,
            exports_json,
            entry.summary,
        ],
    )?;
    Ok(())
}

pub(super) fn get(conn: &Connection, path: &str) -> anyhow::Result<Option<FileCatalogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT path, hash, language, line_count, token_count, exports, summary
         FROM file_catalog WHERE path = ?1",
    )?;

    let result = stmt
        .query_row(params![path], |row| {
            let exports_str: String = row.get(5)?;
            let exports: Vec<String> = serde_json::from_str(&exports_str).unwrap_or_default();
            Ok(FileCatalogEntry {
                path: row.get(0)?,
                hash: row.get(1)?,
                language: row.get(2)?,
                line_count: row.get::<_, i64>(3)? as usize,
                token_count: row.get::<_, i64>(4)? as usize,
                exports,
                summary: row.get(6)?,
            })
        })
        .ok();
    Ok(result)
}

pub(super) fn count(conn: &Connection) -> anyhow::Result<usize> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM file_catalog", [], |row| row.get(0))?;
    Ok(n as usize)
}

pub(super) fn all_paths(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM file_catalog ORDER BY path")?;
    let paths = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(paths)
}
