//! Edge types and CRUD operations for graph edges.

use rusqlite::{Connection, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    Imports,
    Calls,
    Defines,
    Exports,
    TypeRef,
    TestedBy,
    ChangedIn,
    BuiltIn,
    MentionedIn,
    Affects,
    Breaks,
    /// Implicit module/package/re-export relationship (from graph_index)
    Module,
    /// Git co-change correlation (files frequently changed together)
    Cochange,
    /// Implementation → interface/trait it implements (semantic backend)
    Implements,
    /// Sibling/orphan rescue edge (fallback connectivity)
    Sibling,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::Defines => "defines",
            Self::Exports => "exports",
            Self::TypeRef => "type_ref",
            Self::TestedBy => "tested_by",
            Self::ChangedIn => "changed_in",
            Self::BuiltIn => "built_in",
            Self::MentionedIn => "mentioned_in",
            Self::Affects => "affects",
            Self::Breaks => "breaks",
            Self::Module => "module",
            Self::Cochange => "cochange",
            Self::Sibling => "sibling",
            Self::Implements => "implements",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "calls" => Self::Calls,
            "defines" => Self::Defines,
            "exports" => Self::Exports,
            "type_ref" => Self::TypeRef,
            "tested_by" => Self::TestedBy,
            "changed_in" => Self::ChangedIn,
            "built_in" => Self::BuiltIn,
            "mentioned_in" => Self::MentionedIn,
            "affects" => Self::Affects,
            "breaks" => Self::Breaks,
            "module" => Self::Module,
            "cochange" => Self::Cochange,
            "sibling" => Self::Sibling,
            "implements" => Self::Implements,
            _ => Self::Imports,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub id: Option<i64>,
    pub source_id: i64,
    pub target_id: i64,
    pub kind: EdgeKind,
    pub metadata: Option<String>,
}

impl Edge {
    pub fn new(source_id: i64, target_id: i64, kind: EdgeKind) -> Self {
        Self {
            id: None,
            source_id,
            target_id,
            kind,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, meta: &str) -> Self {
        self.metadata = Some(meta.to_string());
        self
    }
}

pub(super) fn upsert(conn: &Connection, edge: &Edge) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO edges (source_id, target_id, kind, metadata)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(source_id, target_id, kind) DO UPDATE SET
            metadata = excluded.metadata",
        params![
            edge.source_id,
            edge.target_id,
            edge.kind.as_str(),
            edge.metadata,
        ],
    )?;
    Ok(())
}

pub(super) fn from_node(conn: &Connection, node_id: i64) -> anyhow::Result<Vec<Edge>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_id, kind, metadata
         FROM edges WHERE source_id = ?1",
    )?;
    let edges = stmt
        .query_map(params![node_id], |row| {
            Ok(Edge {
                id: Some(row.get(0)?),
                source_id: row.get(1)?,
                target_id: row.get(2)?,
                kind: EdgeKind::parse(&row.get::<_, String>(3)?),
                metadata: row.get(4)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    Ok(edges)
}

pub(super) fn to_node(conn: &Connection, node_id: i64) -> anyhow::Result<Vec<Edge>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_id, kind, metadata
         FROM edges WHERE target_id = ?1",
    )?;
    let edges = stmt
        .query_map(params![node_id], |row| {
            Ok(Edge {
                id: Some(row.get(0)?),
                source_id: row.get(1)?,
                target_id: row.get(2)?,
                kind: EdgeKind::parse(&row.get::<_, String>(3)?),
                metadata: row.get(4)?,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();
    Ok(edges)
}

pub(super) fn count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
    Ok(c as usize)
}

pub(super) fn metadata_of(
    conn: &Connection,
    source_id: i64,
    target_id: i64,
    kind: &EdgeKind,
) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    // `None` for both "no such edge" and "edge without metadata".
    Ok(conn
        .query_row(
            "SELECT metadata FROM edges WHERE source_id = ?1 AND target_id = ?2 AND kind = ?3",
            params![source_id, target_id, kind.as_str()],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

pub(super) fn file_edges_of_kind(
    conn: &Connection,
    kind: &EdgeKind,
) -> anyhow::Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT ps.path, pt.path, e.metadata
         FROM edges e
         JOIN nodes ns ON ns.id = e.source_id
         JOIN nodes nt ON nt.id = e.target_id
         JOIN paths ps ON ps.id = ns.file_id
         JOIN paths pt ON pt.id = nt.file_id
         WHERE e.kind = ?1 AND ns.kind = 'file' AND nt.kind = 'file'
         ORDER BY ps.path, pt.path",
    )?;
    let rows = stmt
        .query_map(params![kind.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(super) fn remove_file_edge(
    conn: &Connection,
    source: &str,
    target: &str,
    kind: &EdgeKind,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM edges WHERE kind = ?3
           AND source_id IN (SELECT n.id FROM nodes n JOIN paths p ON p.id = n.file_id
                             WHERE n.kind = 'file' AND p.path = ?1)
           AND target_id IN (SELECT n.id FROM nodes n JOIN paths p ON p.id = n.file_id
                             WHERE n.kind = 'file' AND p.path = ?2)",
        params![source, target, kind.as_str()],
    )?;
    Ok(())
}
