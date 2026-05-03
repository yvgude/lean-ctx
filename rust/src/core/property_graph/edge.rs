//! Edge types and CRUD operations for graph edges.

use rusqlite::{params, Connection};

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

pub fn upsert(conn: &Connection, edge: &Edge) -> anyhow::Result<()> {
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

pub fn from_node(conn: &Connection, node_id: i64) -> anyhow::Result<Vec<Edge>> {
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

pub fn to_node(conn: &Connection, node_id: i64) -> anyhow::Result<Vec<Edge>> {
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

pub fn count(conn: &Connection) -> anyhow::Result<usize> {
    let c: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
    Ok(c as usize)
}
