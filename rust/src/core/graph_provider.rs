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

/// Normalize a property-graph edge-kind string back to the graph_index spelling
/// when materializing a [`ProjectIndex`]. The mirror maps graph_index `import` →
/// `EdgeKind::Imports`, which serializes as the plural `imports`; the legacy
/// dependency consumers (`graph_index` BFS, the facade's index-backed
/// `dependencies`/`dependents`, `ctx_graph`) filter on the singular `import`, so
/// reverse exactly that one rename. All other kinds already share their spelling
/// across both stores (#696 C1).
fn index_edge_kind(pg_kind: &str) -> String {
    match pg_kind {
        "imports" => "import".to_string(),
        other => other.to_string(),
    }
}

/// Convert a property-graph symbol [`Node`] into a backend-agnostic
/// [`SymbolInfo`], recovering the precise source `kind` and export flag from the
/// node metadata (the `Node` itself only carries a coarse `NodeKind`). Single
/// source of truth for the three facade methods that surface PG symbols, so a
/// materialized `ProjectIndex` is lossless (#696 C1).
fn symbol_info_from_node(n: super::property_graph::Node) -> SymbolInfo {
    let (meta_kind, meta_exported) =
        super::property_graph::parse_symbol_metadata(n.metadata.as_deref());
    SymbolInfo {
        kind: meta_kind.unwrap_or_else(|| n.kind.as_str().to_string()),
        is_exported: meta_exported.unwrap_or(true),
        name: n.name,
        file: n.file_path,
        start_line: n.line_start.unwrap_or(0),
        end_line: n.line_end.unwrap_or(0),
    }
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
                .map(|r| r.affected_files.into_iter().map(|e| e.file_path).collect())
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
                .map(symbol_info_from_node)
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

    /// Every symbol with its file + line span (unfiltered). Backend-agnostic
    /// equivalent of iterating `ProjectIndex::symbols` — used by the call-graph
    /// builder to attribute call sites to their enclosing symbol.
    pub fn all_symbols(&self) -> Vec<SymbolInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                .all_symbols()
                .unwrap_or_default()
                .into_iter()
                .map(symbol_info_from_node)
                .collect(),
            GraphProvider::GraphIndex(i) => i
                .symbols
                .values()
                .map(|s| SymbolInfo {
                    name: s.name.clone(),
                    file: s.file.clone(),
                    kind: s.kind.clone(),
                    start_line: s.start_line,
                    end_line: s.end_line,
                    is_exported: s.is_exported,
                })
                .collect(),
        }
    }

    pub fn get_symbol(&self, key: &str) -> Option<SymbolInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => {
                // Keys are `rel_path::sym_name` (graph_index, see `mod.rs`). A
                // file path never contains `::`, but a symbol name does for trait
                // impls (`std::fmt::Display for T`). Split on the FIRST `::` so
                // those names round-trip — `rsplitn` mangled them (#682.3).
                let parts: Vec<&str> = key.splitn(2, "::").collect();
                if parts.len() != 2 {
                    return None;
                }
                let (file_path, sym_name) = (parts[0], parts[1]);
                g.get_node_by_symbol(sym_name, file_path)
                    .ok()
                    .flatten()
                    .map(symbol_info_from_node)
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
            // Normalize the raw property-graph edge-kind vocabulary back to the
            // graph_index vocabulary every consumer speaks (notably `imports` →
            // `import`, queried by impact/overview via `edges_by_kind("import")`).
            // Without this the facade leaks `EdgeKind::as_str()` plurals and PG
            // vs legacy backends would answer the same query differently (#696).
            GraphProvider::PropertyGraph(g) => g
                .all_edges_flat()
                .unwrap_or_default()
                .into_iter()
                .map(|(from, to, kind, weight)| EdgeInfo {
                    from,
                    to,
                    kind: index_edge_kind(&kind),
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

    /// Every catalogued file as [`FileInfo`]. Backend-agnostic equivalent of
    /// iterating `ProjectIndex::files` — used by stats/bootstrap consumers that
    /// need per-file language + token counts, not just paths.
    pub fn file_entries(&self) -> Vec<FileInfo> {
        match self {
            GraphProvider::PropertyGraph(_) => self
                .file_paths()
                .into_iter()
                .filter_map(|p| self.get_file_entry(&p))
                .collect(),
            GraphProvider::GraphIndex(i) => i
                .files
                .values()
                .map(|e| FileInfo {
                    path: e.path.clone(),
                    hash: e.hash.clone(),
                    language: e.language.clone(),
                    line_count: e.line_count,
                    token_count: e.token_count,
                    exports: e.exports.clone(),
                    summary: e.summary.clone(),
                })
                .collect(),
        }
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

    /// Reconstruct a full [`ProjectIndex`] from this provider — the inverse of
    /// the graph_index→PG mirror
    /// ([`populate_from_project_index`](super::property_graph::populate_from_project_index)).
    /// Lets the
    /// remaining legacy `ProjectIndex` consumers be sourced from the
    /// PropertyGraph (parity-proven lossless, #682.3) so the redundant JSON
    /// store can be retired (#696 phase C). For the GraphIndex backend it clones
    /// the index it already holds.
    pub fn materialize_project_index(&self, project_root: &str) -> ProjectIndex {
        if let GraphProvider::GraphIndex(i) = self {
            return i.clone();
        }
        let mut idx = ProjectIndex::new(project_root);
        // Stamp `last_scan` from the graph's build time (graph.meta.json) so the
        // TTL staleness check reflects the real build age, not this
        // materialization moment (#696 C4). Content-based staleness still keys
        // off the meta file's mtime independently.
        if let Some(meta) = super::property_graph::load_meta(project_root)
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&meta.built_at)
        {
            idx.last_scan = dt
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
        }
        for f in self.file_entries() {
            idx.files.insert(
                f.path.clone(),
                graph_index::FileEntry {
                    path: f.path,
                    hash: f.hash,
                    language: f.language,
                    line_count: f.line_count,
                    token_count: f.token_count,
                    exports: f.exports,
                    summary: f.summary,
                },
            );
        }
        for s in self.all_symbols() {
            let key = format!("{}::{}", s.file, s.name);
            idx.symbols.insert(
                key,
                graph_index::SymbolEntry {
                    file: s.file,
                    name: s.name,
                    kind: s.kind,
                    start_line: s.start_line,
                    end_line: s.end_line,
                    is_exported: s.is_exported,
                    minhash: Vec::new(),
                },
            );
        }
        for e in self.edges() {
            // `edges()` already normalizes PG kinds to the graph_index
            // vocabulary, so `e.kind` is ready to store verbatim.
            idx.edges.push(graph_index::IndexEdge {
                from: e.from,
                to: e.to,
                kind: e.kind,
                weight: e.weight as f32,
            });
        }
        idx
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

/// Open whichever graph is already populated on disk, **without** triggering any
/// build, plus a flag telling the caller the property graph still wants a
/// (re)build. Prefers a fully-populated PropertyGraph and falls back to the
/// in-memory graph_index extractor while the PG is not yet populated (first run
/// or just after a rebuild), flagging `needs_build` so the caller can warm the
/// PG for the next call (#696 phase D: the `legacy` backend escape hatch was
/// retired once PG-only persistence proved lossless in #682.3).
fn open_existing(project_root: &str) -> (Option<OpenGraphProvider>, bool) {
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
            return (
                Some(OpenGraphProvider {
                    source: GraphProviderSource::PropertyGraph,
                    provider: GraphProvider::PropertyGraph(pg),
                }),
                false,
            );
        }
        if nodes > 0 && file_cat > 0 {
            pg_provider = Some(pg);
        }
    }

    // PG is not fully populated: a (re)build would help the next call.
    let needs_build = !pg_populated;

    if let Some(idx) = crate::core::graph_index::ProjectIndex::load(project_root) {
        let files = idx.files.len();
        let edges = idx.edges.len();
        if !idx.edges.is_empty() || !idx.files.is_empty() {
            log_source_selection(GraphProviderSource::GraphIndex, files, edges, t0);
            return (
                Some(OpenGraphProvider {
                    source: GraphProviderSource::GraphIndex,
                    provider: GraphProvider::GraphIndex(idx),
                }),
                needs_build,
            );
        }
    }

    if let Some(pg) = pg_provider {
        let nodes = pg.node_count().unwrap_or(0);
        log_source_selection(GraphProviderSource::PropertyGraph, nodes, 0, t0);
        return (
            Some(OpenGraphProvider {
                source: GraphProviderSource::PropertyGraph,
                provider: GraphProvider::PropertyGraph(pg),
            }),
            needs_build,
        );
    }

    (None, needs_build)
}

/// Open an already-built graph, kicking off a one-shot background build when the
/// property graph is not fully populated so the *next* call is fast. Returns
/// `None` on this call when nothing is ready yet. Best-effort callers
/// (dashboards, context gate, stats, `ctx_graph`) use this; callers that need a
/// graph *right now* use [`open_or_build`], which builds synchronously instead.
pub fn open_best_effort(project_root: &str) -> Option<OpenGraphProvider> {
    let (existing, needs_build) = open_existing(project_root);
    if needs_build {
        trigger_lazy_graph_build(project_root);
    }
    existing
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
    // Background build no longer needed — indexes are SQLite-backed.
}

/// Build the property graph from the proven graph_index extractor (#682.1).
///
/// Loads the current [`ProjectIndex`] (or scans if absent) and mirrors it into
/// the SQLite store — files + `file_catalog`, symbols, and structural edges —
/// then stamps `graph.meta.json`. Sourcing PG from the mature extractor
/// guarantees PG ⊇ graph_index (so a later backend flip cannot lose data) and
/// populates the `file_catalog` that the `pg_populated` gate requires.
///
/// Synchronous and self-contained in `core` (no `tools` dependency), so callers
/// can build reliably without the fire-and-forget caveat of the lazy trigger.
pub fn build_property_graph(project_root: &str) -> anyhow::Result<()> {
    let index = crate::core::graph_index::ProjectIndex::load(project_root)
        .filter(|i| !i.files.is_empty())
        .unwrap_or_else(|| {
            let handle = super::index_pipeline::pipeline::IndexPipeline::new(
                std::path::PathBuf::from(project_root),
            )
            .build()
            .expect("pipeline build failed");
            handle.run_and_load().expect("pipeline run failed").0
        });
    #[allow(deprecated)]
    super::property_graph::mirror_index(project_root, &index)
}

pub fn open_or_build(project_root: &str) -> Option<OpenGraphProvider> {
    // Open via `open_existing` (not `open_best_effort`): we build synchronously
    // below, so we must NOT spawn the background indexer. Its worker holds the
    // graph-index file lock, which would starve the synchronous scan into
    // returning an empty index — the "No graph available" first-call regression
    // after the PropertyGraph default flip (#695/#682.2).
    if let (Some(p), _) = open_existing(project_root) {
        return Some(p);
    }
    // Graceful fallback for non-existent roots (matching try_build_pipeline).
    let root = std::path::PathBuf::from(project_root);
    if !root.exists() || !root.is_dir() {
        return None;
    }
    let handle = super::index_pipeline::pipeline::IndexPipeline::new(root)
        .build()
        .ok()?;
    let (idx, _) = handle.run_and_load().ok()?;
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
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

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
        #[allow(deprecated)]
        idx.save().expect("save index");

        let open = open_best_effort(&root).expect("open");
        assert_eq!(open.source, GraphProviderSource::GraphIndex);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn best_effort_none_when_no_graphs() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir proj");
        let root = project_root.to_string_lossy().to_string();

        let open = open_best_effort(&root);
        assert!(open.is_none());

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
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

    /// Round-trip guard for #696 C1: graph_index → PG (mirror) → graph_index
    /// (materialize) must preserve files (full catalog), symbols (incl. the
    /// precise `kind` + `is_exported`, which live in node metadata) and import
    /// edges. Proves the PropertyGraph can be the sole source for the remaining
    /// legacy `ProjectIndex` consumers — the prerequisite for retiring the JSON
    /// store.
    #[test]
    fn materialize_project_index_round_trips_losslessly() {
        use super::super::graph_index::{FileEntry, IndexEdge, SymbolEntry};
        use super::super::property_graph::populate_from_project_index;

        let mut a = ProjectIndex::new("/test");
        a.files.insert(
            "src/a.rs".to_string(),
            FileEntry {
                path: "src/a.rs".to_string(),
                hash: "hash-a".to_string(),
                language: "rs".to_string(),
                line_count: 42,
                token_count: 137,
                exports: vec!["Foo".to_string()],
                summary: "module a".to_string(),
            },
        );
        a.files.insert(
            "src/b.rs".to_string(),
            FileEntry {
                path: "src/b.rs".to_string(),
                hash: "hash-b".to_string(),
                language: "rs".to_string(),
                line_count: 7,
                token_count: 19,
                exports: vec![],
                summary: String::new(),
            },
        );
        // Two symbols with DIFFERENT kinds and export flags — the fields most at
        // risk of being flattened by the coarse property-graph `NodeKind`.
        a.symbols.insert(
            "src/a.rs::Foo".to_string(),
            SymbolEntry {
                file: "src/a.rs".to_string(),
                name: "Foo".to_string(),
                kind: "struct".to_string(),
                start_line: 1,
                end_line: 9,
                is_exported: true,
                minhash: Vec::new(),
            },
        );
        a.symbols.insert(
            "src/b.rs::helper".to_string(),
            SymbolEntry {
                file: "src/b.rs".to_string(),
                name: "helper".to_string(),
                kind: "function".to_string(),
                start_line: 3,
                end_line: 6,
                is_exported: false,
                minhash: Vec::new(),
            },
        );
        a.edges.push(IndexEdge {
            from: "src/b.rs".to_string(),
            to: "src/a.rs".to_string(),
            kind: "import".to_string(),
            weight: 1.0,
        });

        let pg = CodeGraph::open_in_memory().unwrap();
        populate_from_project_index(&pg, &a).unwrap();
        let provider = GraphProvider::PropertyGraph(pg);
        let b = provider.materialize_project_index("/test");

        // Files: inventory + every catalog field survive.
        let mut a_files: Vec<&String> = a.files.keys().collect();
        let mut b_files: Vec<&String> = b.files.keys().collect();
        a_files.sort();
        b_files.sort();
        assert_eq!(a_files, b_files, "file inventory must round-trip");
        for (path, fa) in &a.files {
            let fb = b.files.get(path).expect("file present after round trip");
            assert_eq!(fa.hash, fb.hash, "hash {path}");
            assert_eq!(fa.language, fb.language, "language {path}");
            assert_eq!(fa.line_count, fb.line_count, "line_count {path}");
            assert_eq!(fa.token_count, fb.token_count, "token_count {path}");
            assert_eq!(fa.exports, fb.exports, "exports {path}");
            assert_eq!(fa.summary, fb.summary, "summary {path}");
        }

        // Symbols: keys + spans + the metadata-carried kind/export flag survive.
        let mut a_syms: Vec<&String> = a.symbols.keys().collect();
        let mut b_syms: Vec<&String> = b.symbols.keys().collect();
        a_syms.sort();
        b_syms.sort();
        assert_eq!(a_syms, b_syms, "symbol table must round-trip");
        for (key, sa) in &a.symbols {
            let sb = b.symbols.get(key).expect("symbol present after round trip");
            assert_eq!(sa.name, sb.name, "name {key}");
            assert_eq!(sa.file, sb.file, "file {key}");
            assert_eq!(sa.kind, sb.kind, "kind {key}");
            assert_eq!(sa.start_line, sb.start_line, "start_line {key}");
            assert_eq!(sa.end_line, sb.end_line, "end_line {key}");
            assert_eq!(sa.is_exported, sb.is_exported, "is_exported {key}");
        }

        // Structural edges: the import edge survives (PG may enrich, never lose).
        assert!(
            b.edges
                .iter()
                .any(|e| e.from == "src/b.rs" && e.to == "src/a.rs" && e.kind == "import"),
            "import edge must round-trip; got {:?}",
            b.edges
        );
    }
}
