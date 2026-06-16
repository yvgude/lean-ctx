//! Node types and CRUD operations for graph nodes.

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Symbol,
    Module,
    Commit,
    Test,
    CIRun,
    Knowledge,
    Issue,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Symbol => "symbol",
            Self::Module => "module",
            Self::Commit => "commit",
            Self::Test => "test",
            Self::CIRun => "ci_run",
            Self::Knowledge => "knowledge",
            Self::Issue => "issue",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "symbol" => Self::Symbol,
            "module" => Self::Module,
            "commit" => Self::Commit,
            "test" => Self::Test,
            "ci_run" => Self::CIRun,
            "knowledge" => Self::Knowledge,
            "issue" => Self::Issue,
            _ => Self::File,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: Option<i64>,
    pub kind: NodeKind,
    pub name: String,
    pub file_path: String,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    pub metadata: Option<String>,
}

impl Node {
    pub fn file(path: &str) -> Self {
        Self {
            id: None,
            kind: NodeKind::File,
            name: path.to_string(),
            file_path: path.to_string(),
            line_start: None,
            line_end: None,
            metadata: None,
        }
    }

    pub fn symbol(name: &str, file_path: &str, kind: NodeKind) -> Self {
        Self {
            id: None,
            kind,
            name: name.to_string(),
            file_path: file_path.to_string(),
            line_start: None,
            line_end: None,
            metadata: None,
        }
    }

    pub fn with_lines(mut self, start: usize, end: usize) -> Self {
        self.line_start = Some(start);
        self.line_end = Some(end);
        self
    }

    pub fn with_metadata(mut self, meta: &str) -> Self {
        self.metadata = Some(meta.to_string());
        self
    }

    pub fn commit(hash: &str, message: &str) -> Self {
        Self {
            id: None,
            kind: NodeKind::Commit,
            name: hash.to_string(),
            file_path: String::new(),
            line_start: None,
            line_end: None,
            metadata: Some(message.to_string()),
        }
    }

    pub fn test(path: &str, test_name: &str) -> Self {
        Self {
            id: None,
            kind: NodeKind::Test,
            name: test_name.to_string(),
            file_path: path.to_string(),
            line_start: None,
            line_end: None,
            metadata: None,
        }
    }

    pub fn knowledge(id: &str, summary: &str) -> Self {
        Self {
            id: None,
            kind: NodeKind::Knowledge,
            name: id.to_string(),
            file_path: String::new(),
            line_start: None,
            line_end: None,
            metadata: Some(summary.to_string()),
        }
    }

    pub fn issue(id: &str, title: &str) -> Self {
        Self {
            id: None,
            kind: NodeKind::Issue,
            name: id.to_string(),
            file_path: String::new(),
            line_start: None,
            line_end: None,
            metadata: Some(title.to_string()),
        }
    }
}

pub(super) fn upsert(conn: &Connection, node: &Node) -> anyhow::Result<i64> {
    conn.execute(
        "INSERT INTO nodes (kind, name, file_path, line_start, line_end, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(kind, name, file_path) DO UPDATE SET
            line_start = excluded.line_start,
            line_end = excluded.line_end,
            metadata = excluded.metadata",
        params![
            node.kind.as_str(),
            node.name,
            node.file_path,
            node.line_start.map(|v| v as i64),
            node.line_end.map(|v| v as i64),
            node.metadata,
        ],
    )?;

    let id: i64 = conn.query_row(
        "SELECT id FROM nodes WHERE kind = ?1 AND name = ?2 AND file_path = ?3",
        params![node.kind.as_str(), node.name, node.file_path],
        |row| row.get(0),
    )?;

    Ok(id)
}

pub(super) fn get_by_path(conn: &Connection, file_path: &str) -> anyhow::Result<Option<Node>> {
    let result = conn
        .query_row(
            "SELECT id, kind, name, file_path, line_start, line_end, metadata
             FROM nodes WHERE kind = 'file' AND file_path = ?1",
            params![file_path],
            |row| {
                Ok(Node {
                    id: Some(row.get(0)?),
                    kind: NodeKind::parse(&row.get::<_, String>(1)?),
                    name: row.get(2)?,
                    file_path: row.get(3)?,
                    line_start: row.get::<_, Option<i64>>(4)?.map(|v| v as usize),
                    line_end: row.get::<_, Option<i64>>(5)?.map(|v| v as usize),
                    metadata: row.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

pub(super) fn get_by_symbol(
    conn: &Connection,
    name: &str,
    file_path: &str,
) -> anyhow::Result<Option<Node>> {
    let result = conn
        .query_row(
            "SELECT id, kind, name, file_path, line_start, line_end, metadata
             FROM nodes WHERE name = ?1 AND file_path = ?2 AND kind != 'file'",
            params![name, file_path],
            |row| {
                Ok(Node {
                    id: Some(row.get(0)?),
                    kind: NodeKind::parse(&row.get::<_, String>(1)?),
                    name: row.get(2)?,
                    file_path: row.get(3)?,
                    line_start: row.get::<_, Option<i64>>(4)?.map(|v| v as usize),
                    line_end: row.get::<_, Option<i64>>(5)?.map(|v| v as usize),
                    metadata: row.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

pub(super) fn remove_by_file(conn: &Connection, file_path: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM edges WHERE source_id IN (SELECT id FROM nodes WHERE file_path = ?1)
         OR target_id IN (SELECT id FROM nodes WHERE file_path = ?1)",
        params![file_path],
    )?;
    conn.execute("DELETE FROM nodes WHERE file_path = ?1", params![file_path])?;
    Ok(())
}

pub(super) fn find_symbols(
    conn: &Connection,
    name: &str,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
) -> anyhow::Result<Vec<Node>> {
    let name_lower = name.to_lowercase();
    let mut sql = String::from(
        "SELECT id, kind, name, file_path, line_start, line_end, metadata
         FROM nodes WHERE kind != 'file'
         AND LOWER(name) LIKE '%' || ?1 || '%'",
    );
    let mut param_idx = 2;
    if file_filter.is_some() {
        sql.push_str(&format!(" AND file_path LIKE '%' || ?{param_idx} || '%'"));
        param_idx += 1;
    }
    if kind_filter.is_some() {
        sql.push_str(&format!(" AND kind = ?{param_idx}"));
    }
    sql.push_str(" ORDER BY file_path, line_start LIMIT 100");

    let mut stmt = conn.prepare(&sql)?;

    let params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = {
        let mut v: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(name_lower)];
        if let Some(f) = file_filter {
            v.push(Box::new(f.to_string()));
        }
        if let Some(k) = kind_filter {
            v.push(Box::new(k.to_string()));
        }
        v
    };
    let refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(std::convert::AsRef::as_ref).collect();

    let rows = stmt.query_map(refs.as_slice(), |row| {
        Ok(Node {
            id: Some(row.get(0)?),
            kind: NodeKind::parse(&row.get::<_, String>(1)?),
            name: row.get(2)?,
            file_path: row.get(3)?,
            line_start: row.get::<_, Option<i64>>(4)?.map(|v| v as usize),
            line_end: row.get::<_, Option<i64>>(5)?.map(|v| v as usize),
            metadata: row.get(6)?,
        })
    })?;

    let mut results = Vec::new();
    for r in rows {
        results.push(r?);
    }
    Ok(results)
}

pub(super) fn symbol_count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE kind != 'file'",
        [],
        |row| row.get(0),
    )?;
    Ok(c as usize)
}

pub(super) fn all_edges_flat(
    conn: &Connection,
) -> anyhow::Result<Vec<(String, String, String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT n1.file_path, n2.file_path, e.kind, e.weight
         FROM edges e
         JOIN nodes n1 ON e.source_id = n1.id
         JOIN nodes n2 ON e.target_id = n2.id
         WHERE n1.kind = 'file' AND n2.kind = 'file'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
        ))
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

pub(super) fn count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?;
    Ok(c as usize)
}
