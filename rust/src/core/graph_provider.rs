use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::graph_index::{self, ProjectIndex};
use super::property_graph::CodeGraph;

static GRAPH_BUILD_TRIGGERED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub file: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub is_exported: bool,
}

#[derive(Debug, Clone)]
pub struct EdgeInfo {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub weight: f64,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: String,
    pub hash: String,
    pub language: String,
    pub line_count: usize,
    pub token_count: usize,
    pub exports: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphProviderSource {
    PropertyGraph,
    GraphIndex,
}

pub enum GraphProvider {
    PropertyGraph(CodeGraph),
    GraphIndex(ProjectIndex),
}

pub struct OpenGraphProvider {
    pub source: GraphProviderSource,
    pub provider: GraphProvider,
}

impl GraphProvider {
    pub fn node_count(&self) -> Option<usize> {
        match self {
            GraphProvider::PropertyGraph(g) => g.node_count().ok(),
            GraphProvider::GraphIndex(i) => Some(i.file_count()),
        }
    }

    pub fn edge_count(&self) -> Option<usize> {
        match self {
            GraphProvider::PropertyGraph(g) => g.edge_count().ok(),
            GraphProvider::GraphIndex(i) => Some(i.edge_count()),
        }
    }

    /// The underlying [`ProjectIndex`] when this provider is index-backed; `None`
    /// for the property-graph backend. Lets callers compute index-derived
    /// analyses (e.g. realized per-language coverage) without re-opening it.
    pub fn as_graph_index(&self) -> Option<&ProjectIndex> {
        match self {
            GraphProvider::GraphIndex(i) => Some(i),
            GraphProvider::PropertyGraph(_) => None,
        }
    }

    pub fn dependencies(&self, file_path: &str) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g.dependencies(file_path).unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i
                .edges
                .iter()
                .filter(|e| e.kind == "import" && e.from == file_path)
                .map(|e| e.to.clone())
                .collect(),
        }
    }

    pub fn dependents(&self, file_path: &str) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g.dependents(file_path).unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i
                .edges
                .iter()
                .filter(|e| e.kind == "import" && e.to == file_path)
                .map(|e| e.from.clone())
                .collect(),
        }
    }

    pub fn related(&self, file_path: &str, depth: usize) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                .impact_analysis(file_path, depth)
                .map(|r| r.affected_files)
                .unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i.get_related(file_path, depth),
        }
    }

    pub fn file_paths(&self) -> Vec<String> {
        match self {
            GraphProvider::PropertyGraph(g) => g.file_catalog_paths().unwrap_or_default(),
            GraphProvider::GraphIndex(i) => {
                let mut paths: Vec<String> = i.files.keys().cloned().collect();
                paths.sort();
                paths
            }
        }
    }

    pub fn file_count(&self) -> usize {
        match self {
            GraphProvider::PropertyGraph(g) => g.file_catalog_count().unwrap_or(0),
            GraphProvider::GraphIndex(i) => i.files.len(),
        }
    }

    pub fn symbol_count(&self) -> usize {
        match self {
            GraphProvider::PropertyGraph(g) => g.symbol_count().unwrap_or(0),
            GraphProvider::GraphIndex(i) => i.symbols.len(),
        }
    }

    pub fn find_symbols(
        &self,
        name: &str,
        file_filter: Option<&str>,
        kind_filter: Option<&str>,
    ) -> Vec<SymbolInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                .find_symbols(name, file_filter, kind_filter)
                .unwrap_or_default()
                .into_iter()
                .map(|n| SymbolInfo {
                    name: n.name,
                    file: n.file_path,
                    kind: n.kind.as_str().to_string(),
                    start_line: n.line_start.unwrap_or(0),
                    end_line: n.line_end.unwrap_or(0),
                    is_exported: true,
                })
                .collect(),
            GraphProvider::GraphIndex(i) => {
                let name_lower = name.to_lowercase();
                i.symbols
                    .values()
                    .filter(|s| s.name.to_lowercase().contains(&name_lower))
                    .filter(|s| file_filter.is_none_or(|f| s.file.contains(f)))
                    .filter(|s| kind_filter.is_none_or(|k| s.kind == k))
                    .take(100)
                    .map(|s| SymbolInfo {
                        name: s.name.clone(),
                        file: s.file.clone(),
                        kind: s.kind.clone(),
                        start_line: s.start_line,
                        end_line: s.end_line,
                        is_exported: s.is_exported,
                    })
                    .collect()
            }
        }
    }

    pub fn get_symbol(&self, key: &str) -> Option<SymbolInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => {
                let parts: Vec<&str> = key.rsplitn(2, "::").collect();
                if parts.len() != 2 {
                    return None;
                }
                let (sym_name, file_path) = (parts[0], parts[1]);
                g.get_node_by_symbol(sym_name, file_path)
                    .ok()
                    .flatten()
                    .map(|n| SymbolInfo {
                        name: n.name,
                        file: n.file_path,
                        kind: n.kind.as_str().to_string(),
                        start_line: n.line_start.unwrap_or(0),
                        end_line: n.line_end.unwrap_or(0),
                        is_exported: true,
                    })
            }
            GraphProvider::GraphIndex(i) => i.get_symbol(key).map(|s| SymbolInfo {
                name: s.name.clone(),
                file: s.file.clone(),
                kind: s.kind.clone(),
                start_line: s.start_line,
                end_line: s.end_line,
                is_exported: s.is_exported,
            }),
        }
    }

    pub fn edges(&self) -> Vec<EdgeInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                .all_edges_flat()
                .unwrap_or_default()
                .into_iter()
                .map(|(from, to, kind, weight)| EdgeInfo {
                    from,
                    to,
                    kind,
                    weight,
                })
                .collect(),
            GraphProvider::GraphIndex(i) => i
                .edges
                .iter()
                .map(|e| EdgeInfo {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    kind: e.kind.clone(),
                    weight: e.weight as f64,
                })
                .collect(),
        }
    }

    pub fn edges_by_kind(&self, kind: &str) -> Vec<EdgeInfo> {
        self.edges()
            .into_iter()
            .filter(|e| e.kind == kind)
            .collect()
    }

    pub fn get_file_entry(&self, path: &str) -> Option<FileInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => {
                g.get_file_catalog(path).ok().flatten().map(|e| FileInfo {
                    path: e.path,
                    hash: e.hash,
                    language: e.language,
                    line_count: e.line_count,
                    token_count: e.token_count,
                    exports: e.exports,
                    summary: e.summary,
                })
            }
            GraphProvider::GraphIndex(i) => i.files.get(path).map(|e| FileInfo {
                path: e.path.clone(),
                hash: e.hash.clone(),
                language: e.language.clone(),
                line_count: e.line_count,
                token_count: e.token_count,
                exports: e.exports.clone(),
                summary: e.summary.clone(),
            }),
        }
    }

    pub fn last_scan(&self) -> String {
        match self {
            GraphProvider::PropertyGraph(_) => String::new(),
            GraphProvider::GraphIndex(i) => i.last_scan.clone(),
        }
    }

    pub fn index_dir(project_root: &str) -> Option<std::path::PathBuf> {
        graph_index::ProjectIndex::index_dir(project_root)
    }

    /// Scored related files using multi-edge weights.
    /// Falls back to unscored deps/dependents for GraphIndex backend.
    pub fn related_files_scored(&self, file_path: &str, limit: usize) -> Vec<(String, f64)> {
        match self {
            GraphProvider::PropertyGraph(g) => {
                g.related_files(file_path, limit).unwrap_or_default()
            }
            GraphProvider::GraphIndex(_) => {
                let mut result: Vec<(String, f64)> = Vec::new();
                for dep in self.dependencies(file_path) {
                    result.push((dep, 1.0));
                }
                for dep in self.dependents(file_path) {
                    if !result.iter().any(|(p, _)| *p == dep) {
                        result.push((dep, 0.5));
                    }
                }
                result.truncate(limit);
                result
            }
        }
    }
}

pub fn open_best_effort(project_root: &str) -> Option<OpenGraphProvider> {
    let t0 = std::time::Instant::now();
    let mut pg_provider = None;
    let mut pg_populated = false;
    if let Ok(pg) = CodeGraph::open(project_root) {
        let nodes = pg.node_count().unwrap_or(0);
        let edges = pg.edge_count().unwrap_or(0);
        let file_cat = pg.file_catalog_count().unwrap_or(0);
        pg_populated = nodes > 0 && edges > 0 && file_cat > 0;
        if pg_populated {
            log_source_selection(GraphProviderSource::PropertyGraph, nodes, edges, t0);
            return Some(OpenGraphProvider {
                source: GraphProviderSource::PropertyGraph,
                provider: GraphProvider::PropertyGraph(pg),
            });
        }
        if nodes > 0 && file_cat > 0 {
            pg_provider = Some(pg);
        }
    }

    if !pg_populated {
        trigger_lazy_graph_build(project_root);
    }

    if let Some(idx) = super::index_orchestrator::try_load_graph_index(project_root) {
        let files = idx.files.len();
        let edges = idx.edges.len();
        if !idx.edges.is_empty() || !idx.files.is_empty() {
            log_source_selection(GraphProviderSource::GraphIndex, files, edges, t0);
            return Some(OpenGraphProvider {
                source: GraphProviderSource::GraphIndex,
                provider: GraphProvider::GraphIndex(idx),
            });
        }
    }

    if let Some(pg) = pg_provider {
        let nodes = pg.node_count().unwrap_or(0);
        log_source_selection(GraphProviderSource::PropertyGraph, nodes, 0, t0);
        return Some(OpenGraphProvider {
            source: GraphProviderSource::PropertyGraph,
            provider: GraphProvider::PropertyGraph(pg),
        });
    }

    None
}

fn log_source_selection(
    source: GraphProviderSource,
    nodes: usize,
    edges: usize,
    start: std::time::Instant,
) {
    let elapsed_ms = start.elapsed().as_millis();
    if std::env::var("LCTX_DEBUG").is_ok() {
        eprintln!(
            "[graph_provider] source={source:?} nodes={nodes} edges={edges} resolve_ms={elapsed_ms}"
        );
    }
    let _ = (source, nodes, edges, elapsed_ms);
}

/// Triggers a background graph build once per process when the graph is empty.
fn trigger_lazy_graph_build(project_root: &str) {
    // Unit tests rewrite the process-global `LEAN_CTX_DATA_DIR` per test (each uses
    // its own tempdir). A detached, fire-and-forget build thread reads that global
    // mid-flight and runs concurrently with the otherwise-serial (`--test-threads=1`)
    // test bodies — the one source of graph-state concurrency in the suite, and the
    // root of an intermittent macOS-only flake where a freshly-built index appeared
    // empty to the asserting test. `open_or_build` has a synchronous fallback that
    // fully covers tests, so skip the background build under `cfg!(test)`. Production
    // (and integration tests, which run the lib normally) are unaffected.
    if cfg!(test) {
        return;
    }
    if GRAPH_BUILD_TRIGGERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let root = Path::new(project_root);
    // Both probes are TCC-guarded (#356): a non-existent/non-dir path has no
    // markers, and a launchd-standalone process never stats under ~/Documents.
    let is_project = crate::core::pathutil::has_project_marker(root)
        || crate::core::pathutil::has_multi_repo_children(root);
    if !is_project {
        return;
    }
    let root_owned = project_root.to_string();
    std::thread::spawn(move || {
        // TODO(arch): calls into tools::ctx_impact -- should use a trait/callback
        // to decouple core from tools layer.
        let _ = crate::tools::ctx_impact::handle("build", None, &root_owned, None, None);
    });
}

pub fn open_or_build(project_root: &str) -> Option<OpenGraphProvider> {
    if let Some(p) = open_best_effort(project_root) {
        return Some(p);
    }
    let idx = super::graph_index::load_or_build(project_root);
    if idx.files.is_empty() {
        return None;
    }
    Some(OpenGraphProvider {
        source: GraphProviderSource::GraphIndex,
        provider: GraphProvider::GraphIndex(idx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_effort_prefers_graph_index_when_property_graph_empty() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string()) };

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir proj");
        let root = project_root.to_string_lossy().to_string();

        let mut idx = ProjectIndex::new(&root);
        idx.files.insert(
            "src/main.rs".to_string(),
            super::super::graph_index::FileEntry {
                path: "src/main.rs".to_string(),
                hash: "h".to_string(),
                language: "rs".to_string(),
                line_count: 1,
                token_count: 1,
                exports: vec![],
                summary: String::new(),
            },
        );
        idx.save().expect("save index");

        let open = open_best_effort(&root).expect("open");
        assert_eq!(open.source, GraphProviderSource::GraphIndex);

        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
    }

    #[test]
    fn best_effort_none_when_no_graphs() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string()) };

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir proj");
        let root = project_root.to_string_lossy().to_string();

        let open = open_best_effort(&root);
        assert!(open.is_none());

        // SAFETY: single-threaded context (test/startup); no concurrent env access.
        unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
    }

    #[test]
    fn parity_dependencies_both_stores_agree() {
        use super::super::graph_index::{FileEntry, IndexEdge};
        use super::super::property_graph::{Edge, EdgeKind, Node};

        let pg = CodeGraph::open_in_memory().unwrap();
        let a_id = pg.upsert_node(&Node::file("src/a.rs")).unwrap();
        let b_id = pg.upsert_node(&Node::file("src/b.rs")).unwrap();
        let c_id = pg.upsert_node(&Node::file("src/c.rs")).unwrap();
        pg.upsert_edge(&Edge::new(a_id, b_id, EdgeKind::Imports))
            .unwrap();
        pg.upsert_edge(&Edge::new(a_id, c_id, EdgeKind::Imports))
            .unwrap();

        let mut idx = ProjectIndex::new("/test");
        for name in &["src/a.rs", "src/b.rs", "src/c.rs"] {
            idx.files.insert(
                name.to_string(),
                FileEntry {
                    path: name.to_string(),
                    hash: "h".into(),
                    language: "rs".into(),
                    line_count: 1,
                    token_count: 1,
                    exports: vec![],
                    summary: String::new(),
                },
            );
        }
        idx.edges.push(IndexEdge {
            from: "src/a.rs".into(),
            to: "src/b.rs".into(),
            kind: "import".into(),
            weight: 1.0,
        });
        idx.edges.push(IndexEdge {
            from: "src/a.rs".into(),
            to: "src/c.rs".into(),
            kind: "import".into(),
            weight: 1.0,
        });

        let pg_deps = GraphProvider::PropertyGraph(pg);
        let gi_deps = GraphProvider::GraphIndex(idx);

        let mut pg_result = pg_deps.dependencies("src/a.rs");
        let mut gi_result = gi_deps.dependencies("src/a.rs");
        pg_result.sort();
        gi_result.sort();

        assert_eq!(
            pg_result, gi_result,
            "Import edges must match between PG and GraphIndex"
        );

        let mut pg_dependents = pg_deps.dependents("src/b.rs");
        let mut gi_dependents = gi_deps.dependents("src/b.rs");
        pg_dependents.sort();
        gi_dependents.sort();
        assert_eq!(
            pg_dependents, gi_dependents,
            "Dependents must match between PG and GraphIndex"
        );
    }
}
