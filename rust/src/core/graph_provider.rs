use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::graph_index::{self, ProjectIndex};
use super::property_graph::served::GraphGate;
use super::property_graph::{CodeGraph, GraphSource};

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

/// Absolute distance from a symbol's `start` line to a handle's hint `line`.
/// A missing hint contributes zero distance, so handles without an `@Lline`
/// suffix neither help nor hurt the tiebreak (all candidates rank equal here).
fn line_distance(start: usize, target: Option<usize>) -> usize {
    match target {
        Some(t) => start.abs_diff(t),
        None => 0,
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

// One provider lives for one call on the stack; boxing the graph would add an
// allocation per open and touch every construction site for no gain.
#[allow(clippy::large_enum_variant)]
pub enum GraphProvider {
    PropertyGraph(CodeGraph),
    GraphIndex(ProjectIndex),
}

pub struct OpenGraphProvider {
    pub source: GraphProviderSource,
    pub provider: GraphProvider,
}

/// The file an edge end names: a file path, or the file of a
/// `path::symbol` key (a file path never contains `::`).
fn edge_file(end: &str) -> &str {
    end.split_once("::").map_or(end, |(file, _)| file)
}

/// The tool-facing surface. Every file, symbol and edge it returns has passed
/// the serve-time source check (`property_graph::served`, G04): the property
/// graph carries its gate for the request that opened it, the index backend
/// gets one per call. The `*_unchecked` accessors below are the raw store
/// reads and stay private to this facade.
impl GraphProvider {
    fn index_gate(&self) -> Option<GraphGate> {
        match self {
            GraphProvider::PropertyGraph(_) => None,
            GraphProvider::GraphIndex(i) => Some(GraphGate::for_root(&i.project_root)),
        }
    }

    fn file_served(&self, gate: Option<&GraphGate>, path: &str) -> bool {
        match self {
            GraphProvider::PropertyGraph(g) => g.serves_file(path),
            GraphProvider::GraphIndex(_) => gate.is_none_or(|gate| gate.file(path)),
        }
    }

    fn symbol_served(&self, gate: Option<&GraphGate>, path: &str, name: &str) -> bool {
        match self {
            GraphProvider::PropertyGraph(g) => g.serves_symbol(path, name),
            GraphProvider::GraphIndex(_) => gate.is_none_or(|gate| gate.symbol(path, name)),
        }
    }

    /// `paths` found for `anchor`: nothing when the anchor itself may not be
    /// served, otherwise the paths that may.
    fn served_from(&self, anchor: Option<&str>, paths: Vec<String>) -> Vec<String> {
        let gate = self.index_gate();
        if anchor.is_some_and(|anchor| !self.file_served(gate.as_ref(), anchor)) {
            return Vec::new();
        }
        paths
            .into_iter()
            .filter(|path| self.file_served(gate.as_ref(), path))
            .collect()
    }

    fn served_symbols(&self, symbols: Vec<SymbolInfo>) -> Vec<SymbolInfo> {
        let gate = self.index_gate();
        symbols
            .into_iter()
            .filter(|symbol| self.symbol_served(gate.as_ref(), &symbol.file, &symbol.name))
            .collect()
    }

    pub fn dependencies(&self, file_path: &str) -> Vec<String> {
        self.served_from(Some(file_path), self.dependencies_unchecked(file_path))
    }

    pub fn dependents(&self, file_path: &str) -> Vec<String> {
        self.served_from(Some(file_path), self.dependents_unchecked(file_path))
    }

    /// Files reached from `file_path` within `depth`. The walk itself runs
    /// over served files only: nothing is reached through a file that is not
    /// served now (`property_graph::queries::impact_analysis`,
    /// `ProjectIndex::get_related_served`).
    pub fn related(&self, file_path: &str, depth: usize) -> Vec<String> {
        let gate = self.index_gate();
        let served = |path: &str| self.file_served(gate.as_ref(), path);
        if !served(file_path) {
            return Vec::new();
        }
        match self {
            GraphProvider::PropertyGraph(g) => g
                .impact_analysis(file_path, depth)
                .map(|r| r.affected_files)
                .unwrap_or_default(),
            GraphProvider::GraphIndex(i) => i.get_related_served(file_path, depth, &served),
        }
    }

    /// The file graph a computation over the whole graph may use — community
    /// detection, a ranking: the files served now and the edges between two
    /// of them, decided by one gate (`GraphGate::ranking_files`; under
    /// protection a graph too large to decide in full yields nothing).
    pub(crate) fn ranking_graph(&self) -> (Vec<String>, Vec<EdgeInfo>) {
        let mut files = self.file_paths_unchecked();
        files.sort();
        files.dedup();
        let edges = self.edges_unchecked();
        let pairs: Vec<(String, String)> = edges
            .iter()
            .map(|e| (edge_file(&e.from).to_owned(), edge_file(&e.to).to_owned()))
            .collect();
        let served = match self {
            GraphProvider::PropertyGraph(g) => g.rankable_files(files.clone(), &pairs, &[]),
            GraphProvider::GraphIndex(i) => {
                GraphGate::for_root(&i.project_root).ranking_files(files.clone(), &pairs, &[])
            }
        };
        files.retain(|file| served.contains(file));
        let edges = edges
            .into_iter()
            .filter(|e| served.contains(edge_file(&e.from)) && served.contains(edge_file(&e.to)))
            .collect();
        (files, edges)
    }

    pub fn file_paths(&self) -> Vec<String> {
        self.served_from(None, self.file_paths_unchecked())
    }

    pub fn find_symbols(
        &self,
        name: &str,
        file_filter: Option<&str>,
        kind_filter: Option<&str>,
    ) -> Vec<SymbolInfo> {
        self.served_symbols(self.find_symbols_unchecked(name, file_filter, kind_filter))
    }

    /// Every symbol with its file + line span (unfiltered by name).
    pub fn all_symbols(&self) -> Vec<SymbolInfo> {
        self.served_symbols(self.all_symbols_unchecked())
    }

    pub fn get_symbol(&self, key: &str) -> Option<SymbolInfo> {
        let gate = self.index_gate();
        self.get_symbol_unchecked(key)
            .filter(|symbol| self.symbol_served(gate.as_ref(), &symbol.file, &symbol.name))
    }

    pub fn edges(&self) -> Vec<EdgeInfo> {
        let gate = self.index_gate();
        let served = |end: &str| self.file_served(gate.as_ref(), edge_file(end));
        self.edges_unchecked()
            .into_iter()
            .filter(|edge| served(edge.from.as_str()) && served(edge.to.as_str()))
            .collect()
    }

    pub fn get_file_entry(&self, path: &str) -> Option<FileInfo> {
        let gate = self.index_gate();
        self.file_served(gate.as_ref(), path)
            .then(|| self.get_file_entry_unchecked(path))
            .flatten()
    }

    /// Scored related files using multi-edge weights (unscored deps and
    /// dependents for the index backend). Only served neighbours are ranked,
    /// and `limit` is applied after the check: a file that is not served now
    /// takes no slot from one that is.
    pub fn related_files_scored(&self, file_path: &str, limit: usize) -> Vec<(String, f64)> {
        let gate = self.index_gate();
        self.related_files_scored_with(gate.as_ref(), file_path, limit)
    }

    /// [`Self::related_files_scored`] under the caller's request gate: the
    /// opened property graph carries its own; the index backend has none and
    /// is decided by the gate the caller creates once per request, so each
    /// file is decided once and one bound is spent.
    pub(crate) fn related_files_scored_with(
        &self,
        gate: Option<&GraphGate>,
        file_path: &str,
        limit: usize,
    ) -> Vec<(String, f64)> {
        match self {
            GraphProvider::PropertyGraph(g) => {
                g.related_files(file_path, limit).unwrap_or_default()
            }
            GraphProvider::GraphIndex(_) => {
                let served = |path: &str| self.file_served(gate, path);
                let mut result: Vec<(String, f64)> = Vec::new();
                if !served(file_path) {
                    return result;
                }
                let neighbours = self
                    .dependencies_unchecked(file_path)
                    .into_iter()
                    .map(|dep| (dep, 1.0))
                    .chain(
                        self.dependents_unchecked(file_path)
                            .into_iter()
                            .map(|dep| (dep, 0.5)),
                    );
                for (dep, score) in neighbours {
                    if result.len() >= limit {
                        break;
                    }
                    if !result.iter().any(|(path, _)| *path == dep) && served(&dep) {
                        result.push((dep, score));
                    }
                }
                result
            }
        }
    }

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

    fn dependencies_unchecked(&self, file_path: &str) -> Vec<String> {
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

    fn dependents_unchecked(&self, file_path: &str) -> Vec<String> {
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

    fn file_paths_unchecked(&self) -> Vec<String> {
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

    fn find_symbols_unchecked(
        &self,
        name: &str,
        file_filter: Option<&str>,
        kind_filter: Option<&str>,
    ) -> Vec<SymbolInfo> {
        match self {
            GraphProvider::PropertyGraph(g) => g
                // Kind is filtered AFTER mapping, not in SQL: the PG `nodes.kind`
                // column is the coarse `NodeKind` (every code symbol is `"symbol"`),
                // while the precise source kind (`struct`, `fn`, …) lives in metadata
                // and is recovered by `symbol_info_from_node`. Passing `kind_filter`
                // to the SQL query matched it against `"symbol"` and dropped every
                // hit — an exported struct queried with kind="struct" came back empty
                // (#889). ponytail: the SQL LIMIT 100 caps candidates pre-kind-filter,
                // fine until a name substring has >100 cross-kind matches.
                .find_symbols(name, file_filter, None)
                .unwrap_or_default()
                .into_iter()
                .map(symbol_info_from_node)
                .filter(|s| kind_filter.is_none_or(|k| s.kind == k))
                .collect(),
            GraphProvider::GraphIndex(i) => {
                let name_lower = name.to_lowercase();
                // Same order and cap as the property graph's SQL (path, line):
                // a cap over hash order would pick a different 100 per process.
                let mut hits: Vec<_> = i
                    .symbols
                    .values()
                    .filter(|s| s.name.to_lowercase().contains(&name_lower))
                    .filter(|s| file_filter.is_none_or(|f| s.file.contains(f)))
                    .filter(|s| kind_filter.is_none_or(|k| s.kind == k))
                    .collect();
                hits.sort_by(|a, b| {
                    (a.file.as_str(), a.start_line, a.name.as_str()).cmp(&(
                        b.file.as_str(),
                        b.start_line,
                        b.name.as_str(),
                    ))
                });
                hits.into_iter()
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
    fn all_symbols_unchecked(&self) -> Vec<SymbolInfo> {
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

    fn get_symbol_unchecked(&self, key: &str) -> Option<SymbolInfo> {
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

    /// Resolve a stable `SymbolHandle` to
    /// its current [`SymbolInfo`], robust to line drift. Resolution order:
    ///
    /// 1. Exact `(path, name)` — the unique `{file}::{name}` index key, so the
    ///    common case is a single `O(1)` lookup that ignores the `@Lline` hint
    ///    entirely (the symbol may have moved since the handle was emitted).
    /// 2. Otherwise, same-file candidates whose name matches exactly or by its
    ///    unqualified tail (`Config::load` ↔ `load`), ranked by exact-name
    ///    first, then nearest line to the handle hint, then lowest line, then
    ///    name — a total, deterministic order (#498).
    ///
    /// Returns `None` only when no symbol in that file plausibly matches. The
    /// `@Lline` is never a hard requirement, so this is strictly more robust
    /// than a brittle line-only reference.
    pub fn find_symbol_by_handle(
        &self,
        handle: &crate::core::handle::SymbolHandle,
    ) -> Option<SymbolInfo> {
        let key = format!("{}::{}", handle.path, handle.name);
        if let Some(sym) = self.get_symbol(&key) {
            return Some(sym);
        }

        let tail = handle
            .name
            .rsplit("::")
            .next()
            .unwrap_or(handle.name.as_str());
        let mut candidates: Vec<SymbolInfo> = self
            .all_symbols()
            .into_iter()
            .filter(|s| s.file == handle.path)
            .filter(|s| s.name == handle.name || s.name.rsplit("::").next() == Some(tail))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| {
            let exact_a = u8::from(a.name != handle.name);
            let exact_b = u8::from(b.name != handle.name);
            exact_a
                .cmp(&exact_b)
                .then_with(|| {
                    line_distance(a.start_line, handle.line)
                        .cmp(&line_distance(b.start_line, handle.line))
                })
                .then_with(|| a.start_line.cmp(&b.start_line))
                .then_with(|| a.name.cmp(&b.name))
        });
        candidates.into_iter().next()
    }

    fn edges_unchecked(&self) -> Vec<EdgeInfo> {
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
                .filter_map(|p| self.get_file_entry_unchecked(&p))
                .collect(),
            GraphProvider::GraphIndex(i) => {
                let gate = self.index_gate();
                i.files
                    .values()
                    .filter(|e| self.file_served(gate.as_ref(), &e.path))
                    .map(|e| FileInfo {
                        path: e.path.clone(),
                        hash: e.hash.clone(),
                        language: e.language.clone(),
                        line_count: e.line_count,
                        token_count: e.token_count,
                        exports: e.exports.clone(),
                        summary: e.summary.clone(),
                    })
                    .collect()
            }
        }
    }

    fn get_file_entry_unchecked(&self, path: &str) -> Option<FileInfo> {
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
            // The index it already holds, without what may not be served now.
            let gate = self.index_gate();
            let served = |path: &str| self.file_served(gate.as_ref(), path);
            let mut index = i.clone();
            index.files.retain(|path, _| served(path.as_str()));
            index
                .symbols
                .retain(|_, symbol| self.symbol_served(gate.as_ref(), &symbol.file, &symbol.name));
            index
                .edges
                .retain(|edge| served(edge_file(&edge.from)) && served(edge_file(&edge.to)));
            index.rebuild_interner();
            return index;
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
        idx.rebuild_interner();
        idx
    }

    pub fn index_dir(project_root: &str) -> Option<std::path::PathBuf> {
        graph_index::ProjectIndex::index_dir(project_root)
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
    // A graph built by an older engine may hold edges the current engine no
    // longer derives (e.g. guessed `calls`, engine 6): never serve it — fall
    // back to the index extractor and let the flagged rebuild replace it.
    let pg_current = !crate::core::property_graph::engine_outdated(project_root);
    if pg_current && let Ok(pg) = CodeGraph::open(project_root) {
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

    if let Some(idx) = super::index_orchestrator::try_load_graph_index(project_root) {
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

/// Hot-path open for optional enrichment (post-read hints, silent preload):
/// only a fully populated property graph, opened from SQLite. Never parses the
/// JSON index and never builds synchronously — when the graph is not ready it
/// schedules the background build and returns `None`, so the caller simply
/// skips its optional work. A read must not wait seconds (or a 30 s build) for
/// a hint it may not even show.
pub fn open_ready(project_root: &str) -> Option<OpenGraphProvider> {
    let ready = if crate::core::property_graph::engine_outdated(project_root) {
        None
    } else {
        // Nodes and catalog, not edges: a built graph of a project without
        // imports (a single file) legitimately has none.
        CodeGraph::open(project_root).ok().filter(|pg| {
            pg.node_count().unwrap_or(0) > 0 && pg.file_catalog_count().unwrap_or(0) > 0
        })
    };
    if let Some(pg) = ready {
        Some(OpenGraphProvider {
            source: GraphProviderSource::PropertyGraph,
            provider: GraphProvider::PropertyGraph(pg),
        })
    } else {
        trigger_lazy_graph_build(project_root);
        None
    }
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
    // mid-flight and runs concurrently with test bodies that are otherwise
    // serialized on `test_env_lock` — a source of graph-state concurrency, and the
    // root of an intermittent macOS-only flake where a freshly-built index appeared
    // empty to the asserting test. `open_or_build` has a synchronous fallback that
    // fully covers tests, so skip the background build under `cfg!(test)`. Production
    // (and integration tests, which run the lib normally) are unaffected.
    if cfg!(test) {
        return;
    }
    // Preserve the cheap post-trigger fast path, but do not claim the latch
    // until the root has been validated as an actual project.
    if GRAPH_BUILD_TRIGGERED.load(Ordering::SeqCst) {
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
    if GRAPH_BUILD_TRIGGERED.swap(true, Ordering::SeqCst) {
        return;
    }
    // #682.2: build via the same reliable worker that builds the JSON index
    // (which now mirrors into PG, backend-gated), instead of a dedicated
    // fire-and-forget thread that silently died in short-lived processes.
    super::index_orchestrator::ensure_all_background(project_root);
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
    let index = super::index_orchestrator::try_load_graph_index(project_root)
        .filter(|i| !i.files.is_empty())
        .unwrap_or_else(|| graph_index::scan_with_content_cache(project_root).0);
    super::property_graph::mirror_index(project_root, &index)
}

/// Maximum time `open_or_build` will wait for a synchronous graph build before
/// falling back to "graph unavailable" (#1018). Prevents blocking-pool
/// exhaustion on large repos with cold indices, especially on Windows with AV.
const BUILD_TIMEOUT_SECS: u64 = 30;

/// Serialization gate: only one synchronous graph build at a time. Concurrent
/// callers that cannot acquire the gate return immediately with `None` rather
/// than queueing (which would exhaust the blocking pool). The background indexer
/// picks up the build for them asynchronously.
static BUILD_GATE: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

pub fn open_or_build(project_root: &str) -> Option<OpenGraphProvider> {
    if let (Some(p), _) = open_existing(project_root) {
        return Some(p);
    }

    // Tests require the documented byte-stable result even when the parallel
    // suite is building another fixture. Production remains non-blocking.
    let _gate = if cfg!(test) {
        BUILD_GATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    } else {
        let Ok(gate) = BUILD_GATE.try_lock() else {
            tracing::info!(
                "open_or_build: another build in progress for {project_root}; returning None"
            );
            trigger_lazy_graph_build(project_root);
            return None;
        };
        gate
    };

    // Run the potentially-slow `load_or_build` under a timeout thread so we
    // don't block the calling tool handler for unbounded time (#1018).
    let root_owned = project_root.to_string();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let idx = super::graph_index::load_or_build(&root_owned);
        let _ = tx.send(idx);
    });

    let timeout = std::time::Duration::from_secs(BUILD_TIMEOUT_SECS);
    match rx.recv_timeout(timeout) {
        Ok(idx) if !idx.files.is_empty() => Some(OpenGraphProvider {
            source: GraphProviderSource::GraphIndex,
            provider: GraphProvider::GraphIndex(idx),
        }),
        Ok(_) => None,
        Err(_) => {
            tracing::warn!(
                "open_or_build: graph build timed out after {BUILD_TIMEOUT_SECS}s for {project_root}; \
                 triggering background build"
            );
            trigger_lazy_graph_build(project_root);
            None
        }
    }
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
        // The index serves a file only while it exists in the project.
        std::fs::create_dir_all(project_root.join("src")).expect("mkdir src");
        std::fs::write(project_root.join("src/main.rs"), "fn main() {}\n").expect("source");

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

    fn handle_provider() -> (GraphProvider, tempfile::TempDir) {
        let mut idx = ProjectIndex::new("/test");
        for (key, file, name, kind, start, end) in [
            (
                "src/lib.rs::Config",
                "src/lib.rs",
                "Config",
                "struct",
                5usize,
                20usize,
            ),
            (
                "src/lib.rs::Config::load",
                "src/lib.rs",
                "Config::load",
                "method",
                22,
                35,
            ),
            ("src/main.rs::main", "src/main.rs", "main", "fn", 1, 10),
        ] {
            idx.symbols.insert(
                key.to_string(),
                graph_index::SymbolEntry {
                    file: file.to_string(),
                    name: name.to_string(),
                    kind: kind.to_string(),
                    start_line: start,
                    end_line: end,
                    is_exported: true,
                },
            );
        }
        let (idx, files) = idx.on_disk(&[]);
        (GraphProvider::GraphIndex(idx), files)
    }

    #[test]
    fn handle_resolves_exact_file_and_name() {
        let (gp, _files) = handle_provider();
        let h = crate::core::handle::SymbolHandle::new("src/lib.rs", "Config::load", 22);
        let sym = gp.find_symbol_by_handle(&h).expect("resolves");
        assert_eq!(sym.name, "Config::load");
        assert_eq!(sym.start_line, 22);
    }

    #[test]
    fn handle_resolves_after_line_drift() {
        // Same (path, name) but a stale, drifted line — must still resolve to
        // the current symbol at its real line (robust beyond line-only refs).
        let (gp, _files) = handle_provider();
        let h = crate::core::handle::SymbolHandle::new("src/lib.rs", "Config::load", 999);
        let sym = gp
            .find_symbol_by_handle(&h)
            .expect("resolves despite drift");
        assert_eq!(sym.start_line, 22);
    }

    #[test]
    fn handle_resolves_by_unqualified_tail() {
        // Handle carries only the unqualified tail `load`; the exact key misses,
        // so the same-file tail match + line tiebreak recovers `Config::load`.
        let (gp, _files) = handle_provider();
        let h = crate::core::handle::SymbolHandle::new("src/lib.rs", "load", 22);
        let sym = gp.find_symbol_by_handle(&h).expect("resolves by tail");
        assert_eq!(sym.name, "Config::load");
    }

    #[test]
    fn handle_unknown_file_returns_none() {
        let (gp, _files) = handle_provider();
        let h = crate::core::handle::SymbolHandle::new("src/nope.rs", "Config::load", 22);
        assert!(gp.find_symbol_by_handle(&h).is_none());
    }

    #[test]
    fn pg_kind_filter_matches_precise_metadata_kind() {
        // #889: a PG symbol's coarse `nodes.kind` is always `"symbol"`; the
        // source kind (`struct`) lives in metadata. Filtering by kind="struct"
        // must still find it (previously matched the coarse column → empty).
        use super::super::property_graph::{Node, NodeKind};

        let pg = CodeGraph::open_in_memory().unwrap();
        pg.upsert_node(
            &Node::symbol("State", "simplex/simplex.go", NodeKind::Symbol)
                .with_lines(129, 140)
                .with_metadata(r#"{"kind":"struct","exported":true}"#),
        )
        .unwrap();
        let gp = GraphProvider::PropertyGraph(pg);

        let hits = gp.find_symbols("State", None, Some("struct"));
        assert_eq!(
            hits.len(),
            1,
            "kind=struct must resolve the exported struct"
        );
        assert_eq!(hits[0].kind, "struct");
        assert!(hits[0].is_exported);

        // A non-matching kind still filters it out.
        assert!(gp.find_symbols("State", None, Some("fn")).is_empty());
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
        let (idx, _files) = idx.on_disk(&[]);
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
