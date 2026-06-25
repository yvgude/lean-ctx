//! Versioned, committable graph snapshots — Context-as-Code (GL#451).
//!
//! Exports the property graph as deterministic JSON Lines: one header, one
//! line per node, one line per edge, all stably sorted and free of local
//! AUTOINCREMENT ids (edges reference nodes by their (kind, name, file)
//! identity). The same graph always serializes to the same bytes, so the
//! snapshot can live in git, diff cleanly, and merge across team members.

use std::collections::HashMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{CodeGraph, Edge, EdgeKind, Node, NodeKind};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotHeader {
    leanctx_graph_snapshot: u32,
    nodes: usize,
    edges: usize,
}

/// Node identity + payload, id-free. Field order = serialization order.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct SnapNode {
    kind: String,
    file: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_start: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_end: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<String>,
}

/// Edge with endpoints referenced by node identity, id-free.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct SnapEdge {
    kind: String,
    source: (String, String, String),
    target: (String, String, String),
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<String>,
}

#[derive(Debug, Default)]
pub struct ImportStats {
    pub nodes: usize,
    pub edges: usize,
    pub skipped_edges: usize,
}

#[derive(Debug, Default)]
pub struct DriftReport {
    pub only_local: usize,
    pub only_snapshot: usize,
    pub common: usize,
}

impl DriftReport {
    #[must_use]
    pub fn in_sync(&self) -> bool {
        self.only_local == 0 && self.only_snapshot == 0
    }
}

fn collect_nodes(graph: &CodeGraph) -> anyhow::Result<Vec<SnapNode>> {
    let conn = graph.connection();
    let mut stmt =
        conn.prepare("SELECT kind, file_path, name, line_start, line_end, metadata FROM nodes")?;
    let mut nodes: Vec<SnapNode> = stmt
        .query_map([], |row| {
            Ok(SnapNode {
                kind: row.get(0)?,
                file: row.get(1)?,
                name: row.get(2)?,
                line_start: row.get::<_, Option<i64>>(3)?.map(|v| v as usize),
                line_end: row.get::<_, Option<i64>>(4)?.map(|v| v as usize),
                meta: row.get(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    nodes.sort();
    Ok(nodes)
}

fn collect_edges(graph: &CodeGraph) -> anyhow::Result<Vec<SnapEdge>> {
    let conn = graph.connection();
    let mut stmt = conn.prepare(
        "SELECT e.kind, e.metadata,
                s.kind, s.file_path, s.name,
                t.kind, t.file_path, t.name
         FROM edges e
         JOIN nodes s ON s.id = e.source_id
         JOIN nodes t ON t.id = e.target_id",
    )?;
    let mut edges: Vec<SnapEdge> = stmt
        .query_map([], |row| {
            Ok(SnapEdge {
                kind: row.get(0)?,
                meta: row.get(1)?,
                source: (row.get(2)?, row.get(3)?, row.get(4)?),
                target: (row.get(5)?, row.get(6)?, row.get(7)?),
            })
        })?
        .collect::<Result<_, _>>()?;
    edges.sort();
    Ok(edges)
}

/// Serialize the whole graph as deterministic JSON Lines.
pub fn export_snapshot(graph: &CodeGraph) -> anyhow::Result<String> {
    let nodes = collect_nodes(graph)?;
    let edges = collect_edges(graph)?;

    let mut out = String::new();
    out.push_str(&serde_json::to_string(&SnapshotHeader {
        leanctx_graph_snapshot: SNAPSHOT_VERSION,
        nodes: nodes.len(),
        edges: edges.len(),
    })?);
    out.push('\n');
    for n in &nodes {
        out.push_str(&format!("{{\"n\":{}}}\n", serde_json::to_string(n)?));
    }
    for e in &edges {
        out.push_str(&format!("{{\"e\":{}}}\n", serde_json::to_string(e)?));
    }
    Ok(out)
}

/// Merge a snapshot into the local graph: nodes and edges are upserted, the
/// local graph is never truncated (local-first merge — newer local scan data
/// wins on conflicting node payloads via the upsert).
pub fn import_snapshot(graph: &CodeGraph, content: &str) -> anyhow::Result<ImportStats> {
    let (nodes, edges) = parse_snapshot(content)?;
    let mut stats = ImportStats::default();
    let mut id_by_identity: HashMap<(String, String, String), i64> = HashMap::new();

    for n in &nodes {
        let node = Node {
            id: None,
            kind: NodeKind::parse(&n.kind),
            name: n.name.clone(),
            file_path: n.file.clone(),
            line_start: n.line_start,
            line_end: n.line_end,
            metadata: n.meta.clone(),
        };
        let id = graph.upsert_node(&node)?;
        id_by_identity.insert((n.kind.clone(), n.file.clone(), n.name.clone()), id);
        stats.nodes += 1;
    }

    for e in &edges {
        let source = resolve_endpoint(graph, &mut id_by_identity, &e.source);
        let target = resolve_endpoint(graph, &mut id_by_identity, &e.target);
        match (source, target) {
            (Some(s), Some(t)) => {
                graph.upsert_edge(&Edge {
                    id: None,
                    source_id: s,
                    target_id: t,
                    kind: EdgeKind::parse(&e.kind),
                    metadata: e.meta.clone(),
                })?;
                stats.edges += 1;
            }
            _ => stats.skipped_edges += 1,
        }
    }

    Ok(stats)
}

/// Compare the local graph against a snapshot, line-set based.
pub fn check_snapshot(graph: &CodeGraph, content: &str) -> anyhow::Result<DriftReport> {
    let local = export_snapshot(graph)?;
    let local_set: std::collections::HashSet<&str> =
        local.lines().skip(1).filter(|l| !l.is_empty()).collect();
    let snap_set: std::collections::HashSet<&str> =
        content.lines().skip(1).filter(|l| !l.is_empty()).collect();

    Ok(DriftReport {
        only_local: local_set.difference(&snap_set).count(),
        only_snapshot: snap_set.difference(&local_set).count(),
        common: local_set.intersection(&snap_set).count(),
    })
}

fn resolve_endpoint(
    graph: &CodeGraph,
    cache: &mut HashMap<(String, String, String), i64>,
    identity: &(String, String, String),
) -> Option<i64> {
    if let Some(&id) = cache.get(identity) {
        return Some(id);
    }
    // Endpoint may already exist locally without being part of the snapshot.
    let conn = graph.connection();
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM nodes WHERE kind = ?1 AND file_path = ?2 AND name = ?3",
            params![identity.0, identity.1, identity.2],
            |row| row.get(0),
        )
        .ok();
    if let Some(id) = found {
        cache.insert(identity.clone(), id);
    }
    found
}

fn parse_snapshot(content: &str) -> anyhow::Result<(Vec<SnapNode>, Vec<SnapEdge>)> {
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty snapshot"))?;
    let header: SnapshotHeader = serde_json::from_str(header_line)
        .map_err(|e| anyhow::anyhow!("invalid snapshot header: {e}"))?;
    if header.leanctx_graph_snapshot != SNAPSHOT_VERSION {
        anyhow::bail!(
            "unsupported snapshot version {} (supported: {SNAPSHOT_VERSION})",
            header.leanctx_graph_snapshot
        );
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("invalid snapshot line: {e}"))?;
        if let Some(n) = v.get("n") {
            nodes.push(serde_json::from_value::<SnapNode>(n.clone())?);
        } else if let Some(e) = v.get("e") {
            edges.push(serde_json::from_value::<SnapEdge>(e.clone())?);
        } else {
            anyhow::bail!("unknown snapshot line shape: {line}");
        }
    }
    Ok((nodes, edges))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_with_data() -> CodeGraph {
        let g = CodeGraph::open_in_memory().unwrap();
        let a = g.upsert_node(&Node::file("src/auth.rs")).expect("node a");
        let b = g.upsert_node(&Node::file("src/db.rs")).expect("node b");
        g.upsert_edge(&Edge::new(a, b, EdgeKind::Imports))
            .expect("edge");
        g
    }

    #[test]
    fn export_is_deterministic_and_id_free() {
        let g = graph_with_data();
        let s1 = export_snapshot(&g).unwrap();
        let s2 = export_snapshot(&g).unwrap();
        assert_eq!(s1, s2);
        assert!(!s1.contains("\"id\""), "snapshot must not leak local ids");
        assert!(s1.starts_with("{\"leanctx_graph_snapshot\":1"));
    }

    #[test]
    fn roundtrip_export_import_is_lossless() {
        let g = graph_with_data();
        let snapshot = export_snapshot(&g).unwrap();

        let fresh = CodeGraph::open_in_memory().unwrap();
        let stats = import_snapshot(&fresh, &snapshot).unwrap();
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.edges, 1);
        assert_eq!(stats.skipped_edges, 0);

        let reexported = export_snapshot(&fresh).unwrap();
        assert_eq!(snapshot, reexported, "roundtrip must be lossless");
    }

    #[test]
    fn import_merges_instead_of_replacing() {
        let g = graph_with_data();
        let snapshot = export_snapshot(&g).unwrap();

        let local = CodeGraph::open_in_memory().unwrap();
        local
            .upsert_node(&Node::file("src/local_only.rs"))
            .expect("local node");

        import_snapshot(&local, &snapshot).unwrap();
        let merged = export_snapshot(&local).unwrap();
        assert!(merged.contains("local_only.rs"), "local data must survive");
        assert!(merged.contains("auth.rs"), "snapshot data must be merged");
    }

    #[test]
    fn check_reports_drift_and_sync() {
        let g = graph_with_data();
        let snapshot = export_snapshot(&g).unwrap();

        let synced = check_snapshot(&g, &snapshot).unwrap();
        assert!(synced.in_sync());

        g.upsert_node(&Node::file("src/new_file.rs")).unwrap();
        let drifted = check_snapshot(&g, &snapshot).unwrap();
        assert!(!drifted.in_sync());
        assert_eq!(drifted.only_local, 1);
        assert_eq!(drifted.only_snapshot, 0);
    }

    #[test]
    fn rejects_wrong_version() {
        let g = CodeGraph::open_in_memory().unwrap();
        let err = import_snapshot(
            &g,
            "{\"leanctx_graph_snapshot\":99,\"nodes\":0,\"edges\":0}\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("unsupported snapshot version"));
    }
}
