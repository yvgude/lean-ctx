use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::core::graph_index;
use crate::core::vector_index::BM25Index;

#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    pub include_deps_graph: bool,
    pub max_nodes: usize,
    pub max_edges: usize,
}

#[derive(Debug, Serialize)]
pub struct ContextArtifacts {
    pub generated_at_ms: u64,
    pub project_root: String,
    pub git: GitInfo,
    pub index: IndexSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deps_graph: Option<DepsGraph>,
}

#[derive(Debug, Serialize)]
pub struct GitInfo {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Serialize)]
pub struct IndexSummary {
    pub graph_index: GraphIndexSummary,
    pub bm25_index: Bm25IndexSummary,
    pub property_graph: PropertyGraphSummary,
}

#[derive(Debug, Serialize)]
pub struct GraphIndexSummary {
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    pub last_scan: String,
    pub index_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Bm25IndexSummary {
    pub files: usize,
    pub chunks: usize,
    pub index_file: String,
}

#[derive(Debug, Serialize)]
pub struct PropertyGraphSummary {
    pub exists: bool,
    pub db_path: String,
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct DepsGraph {
    pub nodes: Vec<String>,
    pub edges: Vec<DepsEdge>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct DepsEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

pub fn export_json(project_root: &Path, opts: &ExportOptions) -> Result<String, String> {
    let artifacts = build(project_root, opts)?;
    serde_json::to_string_pretty(&artifacts).map_err(|e| e.to_string())
}

pub fn build(project_root: &Path, opts: &ExportOptions) -> Result<ContextArtifacts, String> {
    let root_s = project_root.to_string_lossy().to_string();

    let git = git_info(project_root);

    let graph = graph_index::load_or_build(&root_s);
    let graph_summary = GraphIndexSummary {
        files: graph.file_count(),
        symbols: graph.symbol_count(),
        edges: graph.edge_count(),
        last_scan: graph.last_scan.clone(),
        index_dir: graph_index::ProjectIndex::index_dir(&root_s)
            .map(|p| p.to_string_lossy().to_string()),
    };

    let bm25 = BM25Index::load_or_build(project_root);
    let bm25_summary = Bm25IndexSummary {
        files: bm25.files.len(),
        chunks: bm25.doc_count,
        index_file: BM25Index::index_file_path(project_root)
            .to_string_lossy()
            .to_string(),
    };

    let pg = property_graph_summary(project_root);

    let deps_graph = if opts.include_deps_graph {
        Some(build_deps_graph(&graph, opts.max_nodes, opts.max_edges))
    } else {
        None
    };

    Ok(ContextArtifacts {
        generated_at_ms: now_ms(),
        project_root: root_s,
        git,
        index: IndexSummary {
            graph_index: graph_summary,
            bm25_index: bm25_summary,
            property_graph: pg,
        },
        deps_graph,
    })
}

fn build_deps_graph(
    idx: &graph_index::ProjectIndex,
    max_nodes: usize,
    max_edges: usize,
) -> DepsGraph {
    let max_nodes = max_nodes.max(1);
    let max_edges = max_edges.max(1);

    let mut nodes: Vec<String> = idx.files.keys().cloned().collect();
    nodes.sort();

    let truncated_nodes = nodes.len() > max_nodes;
    if truncated_nodes {
        nodes.truncate(max_nodes);
    }
    let node_set: std::collections::HashSet<&str> = nodes.iter().map(String::as_str).collect();

    let mut edges: Vec<DepsEdge> = Vec::new();
    for e in &idx.edges {
        if edges.len() >= max_edges {
            break;
        }
        if !node_set.contains(e.from.as_str()) || !node_set.contains(e.to.as_str()) {
            continue;
        }
        edges.push(DepsEdge {
            from: e.from.clone(),
            to: e.to.clone(),
            kind: e.kind.clone(),
        });
    }

    let truncated_edges = idx.edges.len() > edges.len() && edges.len() >= max_edges;
    DepsGraph {
        nodes,
        edges,
        truncated: truncated_nodes || truncated_edges,
    }
}

fn property_graph_summary(project_root: &Path) -> PropertyGraphSummary {
    let db_path = project_root.join(".lean-ctx").join("graph.db");
    let db_path_s = db_path.to_string_lossy().to_string();
    if !db_path.exists() {
        return PropertyGraphSummary {
            exists: false,
            db_path: db_path_s,
            nodes: None,
            edges: None,
        };
    }

    match crate::core::property_graph::CodeGraph::open(project_root) {
        Ok(g) => PropertyGraphSummary {
            exists: true,
            db_path: g.db_path().to_string_lossy().to_string(),
            nodes: g.node_count().ok(),
            edges: g.edge_count().ok(),
        },
        Err(_) => PropertyGraphSummary {
            exists: true,
            db_path: db_path_s,
            nodes: None,
            edges: None,
        },
    }
}

fn git_info(project_root: &Path) -> GitInfo {
    let head = git_out(project_root, &["rev-parse", "--short", "HEAD"]);
    let branch = git_out(project_root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let dirty = git_dirty(project_root);
    GitInfo {
        head,
        branch,
        dirty,
    }
}

fn git_dirty(project_root: &Path) -> bool {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

fn git_out(project_root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
