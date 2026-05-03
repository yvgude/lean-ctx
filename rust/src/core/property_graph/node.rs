//! Node types and CRUD operations for graph nodes.

use rusqlite::{params, Connection, OptionalExtension};

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

pub fn upsert(conn: &Connection, node: &Node) -> anyhow::Result<i64> {
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

pub fn get_by_path(conn: &Connection, file_path: &str) -> anyhow::Result<Option<Node>> {
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

pub fn get_by_symbol(
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

pub fn remove_by_file(conn: &Connection, file_path: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM edges WHERE source_id IN (SELECT id FROM nodes WHERE file_path = ?1)
         OR target_id IN (SELECT id FROM nodes WHERE file_path = ?1)",
        params![file_path],
    )?;
    conn.execute("DELETE FROM nodes WHERE file_path = ?1", params![file_path])?;
    Ok(())
}

pub fn count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?;
    Ok(c as usize)
}
