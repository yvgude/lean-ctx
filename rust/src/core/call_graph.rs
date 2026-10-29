use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::deep_queries;
use super::graph_provider::GraphProvider;
use super::index_paths::normalize_project_root;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// On-disk format of the persisted call graph. Bump whenever the meaning of a
/// persisted field changes; [`CallGraph::load`] discards any other version, so
/// the next build starts from scratch instead of reusing stale edges.
/// v2: 1-based `caller_line` (v1 was off by one), callee position/receiver/method.
const CALL_GRAPH_FORMAT: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    /// Missing in pre-v2 caches → 0 → rejected by `load`.
    #[serde(default)]
    pub format_version: u32,
    pub project_root: String,
    pub edges: Vec<CallEdge>,
    pub file_hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallEdge {
    pub caller_file: String,
    pub caller_symbol: String,
    /// 1-based line of the call expression.
    pub caller_line: usize,
    pub callee_name: String,
    /// Callee identifier position `(1-based line, 0-based byte column)` — where
    /// a semantic backend resolves the call (see `CallSite::callee_pos`).
    #[serde(default)]
    pub callee_pos: Option<(usize, usize)>,
    #[serde(default)]
    pub receiver: Option<String>,
    #[serde(default)]
    pub is_method: bool,
}

/// Call edges of one analyzed file, each attributed to its enclosing symbol.
/// Call-site and symbol lines are both 1-based.
fn edges_for_file(
    rel_path: &str,
    calls: &[deep_queries::CallSite],
    file_symbols: Option<&Vec<SymbolSpan>>,
) -> Vec<CallEdge> {
    calls
        .iter()
        .map(|call| CallEdge {
            caller_file: rel_path.to_string(),
            caller_symbol: find_enclosing_symbol_owned(file_symbols, call.line),
            caller_line: call.line,
            callee_name: call.callee.clone(),
            callee_pos: call.callee_pos,
            receiver: call.receiver.clone(),
            is_method: call.is_method,
        })
        .collect()
}

/// Minimal symbol span the call-graph builder needs to attribute a call site to
/// its enclosing symbol — backend-agnostic, decoupled from any graph store.
#[derive(Debug, Clone, Default)]
pub struct SymbolSpan {
    pub file: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    /// Signature kind (`fn`, `method`, `trait`, `interface`, …).
    pub kind: String,
}

/// Everything the call-graph builder reads, sourced from the [`GraphProvider`]
/// facade (PropertyGraph). Replaces the former direct `ProjectIndex`
/// dependency (#696): the file inventory, the symbol table (for enclosing-symbol
/// attribution) and the import/reexport adjacency (for scope-aware callee
/// resolution). Source content itself is still read fresh from disk per file.
#[derive(Debug, Clone, Default)]
pub struct CallGraphInputs {
    pub project_root: String,
    pub file_paths: Vec<String>,
    pub symbols: Vec<SymbolSpan>,
    /// `(from, to)` pairs for `import`/`reexport` edges only.
    pub import_edges: Vec<(String, String)>,
}

impl CallGraphInputs {
    /// Open the project graph (PropertyGraph, falling back to legacy) and
    /// materialize call-graph inputs. Returns empty inputs (rooted at
    /// `project_root`) when no graph is available yet — matching the old
    /// behaviour of building from an empty index.
    pub fn open(project_root: &str) -> Self {
        match crate::core::graph_provider::open_or_build(project_root) {
            Some(open) => Self::from_provider(project_root, &open.provider),
            None => Self {
                project_root: normalize_project_root(project_root),
                ..Default::default()
            },
        }
    }

    /// Bridge for callers that already hold a freshly-scanned
    /// [`ProjectIndex`](super::graph_index::ProjectIndex)
    /// (repomap, dashboard coordinator) and want call-graph inputs consistent
    /// with *that* scan rather than a possibly-lagging PropertyGraph. Removed in
    /// #696 Phase D once those callers move to the facade/extractor wholesale.
    pub fn from_project_index(index: &super::graph_index::ProjectIndex) -> Self {
        let symbols = index
            .symbols
            .values()
            .map(|s| SymbolSpan {
                file: s.file.clone(),
                name: s.name.clone(),
                start_line: s.start_line,
                end_line: s.end_line,
                kind: s.kind.clone(),
            })
            .collect();
        let import_edges = index
            .edges
            .iter()
            .filter(|e| e.kind == "import" || e.kind == "reexport")
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        Self {
            project_root: index.project_root.clone(),
            file_paths: index.files.keys().cloned().collect(),
            symbols,
            import_edges,
        }
    }

    /// Materialize the builder inputs from a [`GraphProvider`] facade.
    pub fn from_provider(project_root: &str, provider: &GraphProvider) -> Self {
        let symbols = provider
            .all_symbols()
            .into_iter()
            .map(|s| SymbolSpan {
                file: s.file,
                name: s.name,
                start_line: s.start_line,
                end_line: s.end_line,
                kind: s.kind,
            })
            .collect();
        let import_edges = provider
            .edges()
            .into_iter()
            .filter(|e| e.kind == "import" || e.kind == "reexport")
            .map(|e| (e.from, e.to))
            .collect();
        Self {
            project_root: normalize_project_root(project_root),
            file_paths: provider.file_paths(),
            symbols,
            import_edges,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BfsNode {
    pub symbol: String,
    pub file: String,
    pub line: usize,
    pub depth: usize,
    pub from_symbol: String,
}

#[derive(Debug, Clone)]
pub struct PathHop {
    pub symbol: String,
    pub file: String,
    pub line: usize,
}

#[derive(Clone, Copy)]
enum BfsDirection {
    Callers,
    Callees,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn from_caller_count(count: usize) -> Self {
        match count {
            0..=1 => Self::Low,
            2..=4 => Self::Medium,
            5..=10 => Self::High,
            _ => Self::Critical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }
}

// ---------------------------------------------------------------------------
// Background build state (singleton per process)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct BuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
    pub edges_found: usize,
}

enum BuildState {
    Idle,
    Building {
        files_total: usize,
        files_done: Arc<AtomicUsize>,
        edges_found: Arc<AtomicUsize>,
    },
    Ready(Arc<CallGraph>),
    Failed(String),
}

/// What a resident call graph answers for: the project it was built from
/// and the protection state its inputs were decided under. A graph is never
/// handed to a request of another project, and one filled in Community
/// developer mode is not handed to a protected request (or the reverse).
/// The serve-time gate is not part of this: it stays with the request, which
/// decides the inputs the resident graph is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildKey {
    root: String,
    protected: bool,
}

impl BuildKey {
    fn for_request(project_root: &str) -> Self {
        Self {
            root: normalize_project_root(project_root),
            protected: crate::core::context_admission::stored::source_authority_required(),
        }
    }
}

/// Resident builds kept at once; the least recently requested one that is
/// not building goes first.
const MAX_RESIDENT_BUILDS: usize = 4;

/// The state of a project nobody requested yet.
static IDLE: BuildState = BuildState::Idle;

/// The resident builds, least recently requested first.
#[derive(Default)]
struct Builds {
    entries: Vec<(BuildKey, BuildState)>,
}

impl Builds {
    fn state(&self, key: &BuildKey) -> &BuildState {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&IDLE, |(_, state)| state)
    }

    /// The most recently requested build.
    fn latest(&self) -> &BuildState {
        self.entries.last().map_or(&IDLE, |(_, state)| state)
    }

    fn set(&mut self, key: BuildKey, state: BuildState) {
        self.entries.retain(|(k, _)| *k != key);
        if self.entries.len() >= MAX_RESIDENT_BUILDS {
            let evict = self
                .entries
                .iter()
                .position(|(_, state)| !matches!(state, BuildState::Building { .. }))
                .unwrap_or(0);
            self.entries.remove(evict);
        }
        self.entries.push((key, state));
    }
}

static BUILD: OnceLock<Mutex<Builds>> = OnceLock::new();

fn global_state() -> &'static Mutex<Builds> {
    BUILD.get_or_init(|| Mutex::new(Builds::default()))
}

impl CallGraph {
    pub fn new(project_root: &str) -> Self {
        Self {
            format_version: CALL_GRAPH_FORMAT,
            project_root: normalize_project_root(project_root),
            edges: Vec::new(),
            file_hashes: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Parallel build — processes files via rayon thread pool
    // -----------------------------------------------------------------------

    pub fn build_parallel(
        inputs: &CallGraphInputs,
        progress: Option<(&AtomicUsize, &AtomicUsize)>,
    ) -> Self {
        let project_root = &inputs.project_root;
        let symbols_by_file = group_symbols_by_file_owned(inputs);
        let file_keys: Vec<String> = inputs.file_paths.clone();

        let results: Vec<(String, String, Vec<CallEdge>)> = file_keys
            .par_iter()
            .filter_map(|rel_path| {
                let abs_path = resolve_path(rel_path, project_root);
                let content = std::fs::read_to_string(&abs_path).ok()?;
                let hash = simple_hash(&content);

                let ext = Path::new(rel_path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");

                let analysis = deep_queries::analyze(&content, ext);
                let file_symbols = symbols_by_file.get(rel_path.as_str());

                let edges = edges_for_file(rel_path, &analysis.calls, file_symbols);

                if let Some((done, edge_count)) = progress {
                    done.fetch_add(1, Ordering::Relaxed);
                    edge_count.fetch_add(edges.len(), Ordering::Relaxed);
                }

                Some((rel_path.clone(), hash, edges))
            })
            .collect();

        let mut graph = Self::new(project_root);
        let edge_capacity: usize = results.iter().map(|(_, _, e)| e.len()).sum();
        graph.edges.reserve(edge_capacity);
        graph.file_hashes.reserve(results.len());

        for (path, hash, edges) in results {
            graph.file_hashes.insert(path, hash);
            graph.edges.extend(edges);
        }

        graph
    }

    // -----------------------------------------------------------------------
    // Incremental parallel build — only re-analyzes changed files
    // -----------------------------------------------------------------------

    pub fn build_incremental_parallel(
        inputs: &CallGraphInputs,
        previous: &CallGraph,
        progress: Option<(&AtomicUsize, &AtomicUsize)>,
    ) -> Self {
        let project_root = &inputs.project_root;
        let symbols_by_file = group_symbols_by_file_owned(inputs);
        let file_keys: Vec<String> = inputs.file_paths.clone();

        let prev_edges_by_file = group_edges_by_file(&previous.edges);

        let results: Vec<(String, String, Vec<CallEdge>)> = file_keys
            .par_iter()
            .filter_map(|rel_path| {
                let abs_path = resolve_path(rel_path, project_root);
                let content = std::fs::read_to_string(&abs_path).ok()?;
                let hash = simple_hash(&content);
                let changed = previous.file_hashes.get(rel_path.as_str()) != Some(&hash);

                let edges = if changed {
                    let ext = Path::new(rel_path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");

                    let analysis = deep_queries::analyze(&content, ext);
                    let file_symbols = symbols_by_file.get(rel_path.as_str());

                    edges_for_file(rel_path, &analysis.calls, file_symbols)
                } else {
                    prev_edges_by_file
                        .get(rel_path.as_str())
                        .cloned()
                        .unwrap_or_default()
                };

                if let Some((done, edge_count)) = progress {
                    done.fetch_add(1, Ordering::Relaxed);
                    edge_count.fetch_add(edges.len(), Ordering::Relaxed);
                }

                Some((rel_path.clone(), hash, edges))
            })
            .collect();

        let mut graph = Self::new(project_root);
        let edge_capacity: usize = results.iter().map(|(_, _, e)| e.len()).sum();
        graph.edges.reserve(edge_capacity);
        graph.file_hashes.reserve(results.len());

        for (path, hash, edges) in results {
            graph.file_hashes.insert(path, hash);
            graph.edges.extend(edges);
        }

        graph
    }

    // -----------------------------------------------------------------------
    // Public API: non-blocking access for the dashboard
    // -----------------------------------------------------------------------

    /// Returns the cached graph immediately, or `None` + starts a background build.
    pub fn get_or_start_build(
        project_root: &str,
        inputs: Arc<CallGraphInputs>,
    ) -> Result<Arc<CallGraph>, BuildProgress> {
        let key = BuildKey::for_request(project_root);
        let state = global_state();
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match guard.state(&key) {
            // The resident graph outlives the request that built it. It is
            // found only under this request's project and protection state
            // (`BuildKey`), and handed out only while every file it was built
            // from is still in this request's inputs — those come from the
            // gated graph facade (G04), so a file withdrawn since is missing
            // there. Otherwise the graph is rebuilt over the current inputs.
            BuildState::Ready(graph) if !holds_withdrawn_files(graph, &inputs) => {
                return Ok(Arc::clone(graph));
            }
            BuildState::Ready(_) | BuildState::Idle => {}
            BuildState::Building {
                files_total,
                files_done,
                edges_found,
            } => {
                return Err(BuildProgress {
                    status: "building",
                    files_total: *files_total,
                    files_done: files_done.load(Ordering::Relaxed),
                    edges_found: edges_found.load(Ordering::Relaxed),
                });
            }
            BuildState::Failed(msg) => {
                tracing::warn!("[call_graph: previous build failed: {msg} — retrying]");
            }
        }

        // Try serving from disk cache first (an exact file-set match, so a
        // persisted graph with a withdrawn file is rebuilt, not served)
        if let Some(cached) = Self::load(project_root)
            && !cache_looks_stale(&cached, &inputs)
        {
            let arc = Arc::new(cached);
            guard.set(key, BuildState::Ready(Arc::clone(&arc)));
            return Ok(arc);
        }

        let files_total = inputs.file_paths.len();
        let files_done = Arc::new(AtomicUsize::new(0));
        let edges_found = Arc::new(AtomicUsize::new(0));

        guard.set(
            key.clone(),
            BuildState::Building {
                files_total,
                files_done: Arc::clone(&files_done),
                edges_found: Arc::clone(&edges_found),
            },
        );
        drop(guard);

        let root = key.root.clone();
        let fd = Arc::clone(&files_done);
        let ef = Arc::clone(&edges_found);

        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let previous = CallGraph::load(&root);
                if let Some(prev) = &previous {
                    CallGraph::build_incremental_parallel(&inputs, prev, Some((&fd, &ef)))
                } else {
                    CallGraph::build_parallel(&inputs, Some((&fd, &ef)))
                }
            }));

            match result {
                Ok(graph) => {
                    let _ = graph.save();
                    let arc = Arc::new(graph);
                    if let Ok(mut g) = global_state().lock() {
                        g.set(key, BuildState::Ready(Arc::clone(&arc)));
                    }
                    tracing::info!(
                        "[call_graph: build complete — {} files, {} edges]",
                        arc.file_hashes.len(),
                        arc.edges.len()
                    );
                }
                Err(e) => {
                    let msg = format!("{e:?}");
                    tracing::error!("[call_graph: build panicked: {msg}]");
                    if let Ok(mut g) = global_state().lock() {
                        g.set(key, BuildState::Failed(msg));
                    }
                }
            }
        });

        Err(BuildProgress {
            status: "building",
            files_total,
            files_done: 0,
            edges_found: 0,
        })
    }

    /// Returns the status of the most recently requested build without
    /// starting anything: counts only, no file or symbol of any project.
    pub fn build_status() -> BuildProgress {
        let state = global_state();
        let guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.latest() {
            BuildState::Idle => BuildProgress {
                status: "idle",
                files_total: 0,
                files_done: 0,
                edges_found: 0,
            },
            BuildState::Building {
                files_total,
                files_done,
                edges_found,
            } => BuildProgress {
                status: "building",
                files_total: *files_total,
                files_done: files_done.load(Ordering::Relaxed),
                edges_found: edges_found.load(Ordering::Relaxed),
            },
            BuildState::Ready(graph) => BuildProgress {
                status: "ready",
                files_total: graph.file_hashes.len(),
                files_done: graph.file_hashes.len(),
                edges_found: graph.edges.len(),
            },
            BuildState::Failed(msg) => {
                tracing::debug!("[call_graph: status check — failed: {msg}]");
                BuildProgress {
                    status: "error",
                    files_total: 0,
                    files_done: 0,
                    edges_found: 0,
                }
            }
        }
    }

    /// Force-invalidate every resident result so the next request of any
    /// project triggers a rebuild.
    pub fn invalidate() {
        if let Ok(mut g) = global_state().lock() {
            g.entries.clear();
        }
    }

    // -----------------------------------------------------------------------
    // Legacy synchronous API (kept for non-dashboard callers)
    // -----------------------------------------------------------------------

    pub fn build(inputs: &CallGraphInputs) -> Self {
        Self::build_parallel(inputs, None)
    }

    pub fn build_incremental(inputs: &CallGraphInputs, previous: &CallGraph) -> Self {
        Self::build_incremental_parallel(inputs, previous, None)
    }

    /// Edges calling `symbol`, however the call site spells it: a query for
    /// `write` also finds `crate::config_io::write(..)` (see [`callee_matches`]).
    pub fn callers_of(&self, symbol: &str) -> Vec<&CallEdge> {
        self.edges
            .iter()
            .filter(|e| callee_matches(&e.callee_name, symbol))
            .collect()
    }

    pub fn callees_of(&self, symbol: &str) -> Vec<&CallEdge> {
        let sym_lower = symbol.to_lowercase();
        self.edges
            .iter()
            .filter(|e| e.caller_symbol.to_lowercase() == sym_lower)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Multi-hop BFS traversal
    // -----------------------------------------------------------------------

    /// BFS callers up to `max_depth` hops. Returns (symbol, file, line, depth) per node.
    pub fn bfs_callers(&self, symbol: &str, max_depth: usize) -> Vec<BfsNode> {
        self.bfs_traverse(symbol, max_depth, BfsDirection::Callers)
    }

    /// BFS callees up to `max_depth` hops. Returns (symbol, file, line, depth) per node.
    pub fn bfs_callees(&self, symbol: &str, max_depth: usize) -> Vec<BfsNode> {
        self.bfs_traverse(symbol, max_depth, BfsDirection::Callees)
    }

    fn bfs_traverse(&self, symbol: &str, max_depth: usize, dir: BfsDirection) -> Vec<BfsNode> {
        use std::collections::{HashSet, VecDeque};

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut result: Vec<BfsNode> = Vec::new();

        let start = symbol.to_lowercase();
        visited.insert(start.clone());
        queue.push_back((start, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let neighbors: Vec<&CallEdge> = match dir {
                BfsDirection::Callers => self
                    .edges
                    .iter()
                    .filter(|e| callee_matches(&e.callee_name, &current))
                    .collect(),
                BfsDirection::Callees => self
                    .edges
                    .iter()
                    .filter(|e| e.caller_symbol.to_lowercase() == callee_segment(&current))
                    .collect(),
            };

            for edge in neighbors {
                let next_sym = match dir {
                    BfsDirection::Callers => &edge.caller_symbol,
                    BfsDirection::Callees => &edge.callee_name,
                };
                // Caller symbols are bare names, so a path callee continues the
                // walk (and dedups) under its last segment.
                let next_lower = callee_segment(next_sym).to_lowercase();

                if !visited.insert(next_lower.clone()) {
                    continue;
                }

                result.push(BfsNode {
                    symbol: next_sym.clone(),
                    file: edge.caller_file.clone(),
                    line: edge.caller_line,
                    depth: depth + 1,
                    from_symbol: if depth == 0 {
                        symbol.to_string()
                    } else {
                        current.clone()
                    },
                });

                queue.push_back((next_lower, depth + 1));
            }
        }

        result
    }

    /// Find shortest call path from `from` to `to` using BFS.
    /// Returns None if no path exists (searched up to depth 10).
    /// Find shortest call path from `from` to `to` using BFS.
    /// Returns None if no path exists (searched up to depth 10).
    pub fn find_call_path(&self, from: &str, to: &str) -> Option<Vec<PathHop>> {
        use std::collections::{HashMap as BfsMap, VecDeque};

        // Walk by bare names: caller symbols are bare, path callees continue
        // under their last segment.
        let from_lower = callee_segment(from).to_lowercase();
        let to_lower = callee_segment(to).to_lowercase();

        if from_lower == to_lower {
            return Some(vec![PathHop {
                symbol: from.to_string(),
                file: String::new(),
                line: 0,
            }]);
        }

        const MAX_TRACE_DEPTH: usize = 10;

        // (parent_symbol, file, line, depth)
        let mut visited: BfsMap<String, (String, String, usize, usize)> = BfsMap::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        visited.insert(from_lower.clone(), (String::new(), String::new(), 0, 0));
        queue.push_back(from_lower.clone());

        while let Some(current) = queue.pop_front() {
            let current_depth = visited.get(&current).map_or(0, |e| e.3);
            if current_depth >= MAX_TRACE_DEPTH {
                continue;
            }

            let callees: Vec<&CallEdge> = self
                .edges
                .iter()
                .filter(|e| e.caller_symbol.to_lowercase() == current)
                .collect();

            for edge in callees {
                let next = callee_segment(&edge.callee_name).to_lowercase();
                if visited.contains_key(&next) {
                    continue;
                }

                visited.insert(
                    next.clone(),
                    (
                        current.clone(),
                        edge.caller_file.clone(),
                        edge.caller_line,
                        current_depth + 1,
                    ),
                );

                if next == to_lower {
                    return Some(Self::reconstruct_path(
                        &visited,
                        &from_lower,
                        &to_lower,
                        from,
                        to,
                    ));
                }

                queue.push_back(next);
            }
        }

        None
    }

    fn reconstruct_path(
        visited: &std::collections::HashMap<String, (String, String, usize, usize)>,
        from_lower: &str,
        to_lower: &str,
        from_orig: &str,
        to_orig: &str,
    ) -> Vec<PathHop> {
        let mut path = Vec::new();
        let mut current = to_lower.to_string();

        while current != from_lower {
            let (parent, file, line, _depth) = &visited[&current];
            let sym_name = if current == to_lower {
                to_orig.to_string()
            } else {
                current.clone()
            };
            path.push(PathHop {
                symbol: sym_name,
                file: file.clone(),
                line: *line,
            });
            current = parent.clone();
        }

        path.push(PathHop {
            symbol: from_orig.to_string(),
            file: String::new(),
            line: 0,
        });

        path.reverse();
        path
    }

    /// Count unique transitive callers up to `max_depth`.
    pub fn transitive_caller_count(&self, symbol: &str, max_depth: usize) -> usize {
        let nodes = self.bfs_callers(symbol, max_depth);
        let mut unique: std::collections::HashSet<String> = std::collections::HashSet::new();
        for node in &nodes {
            unique.insert(node.symbol.to_lowercase());
        }
        unique.len()
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = call_graph_dir(&self.project_root)
            .ok_or_else(|| "Cannot determine home directory".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        let compressed = zstd::encode_all(json.as_bytes(), 9).map_err(|e| format!("zstd: {e}"))?;
        let target = dir.join("call_graph.json.zst");
        let tmp = target.with_extension("zst.tmp");
        std::fs::write(&tmp, &compressed).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(dir.join("call_graph.json"));
        Ok(())
    }

    pub fn load(project_root: &str) -> Option<Self> {
        Self::load_any_version(project_root).filter(|g| g.format_version == CALL_GRAPH_FORMAT)
    }

    fn load_any_version(project_root: &str) -> Option<Self> {
        let dir = call_graph_dir(project_root)?;

        let zst_path = dir.join("call_graph.json.zst");
        if zst_path.exists() {
            let compressed = std::fs::read(&zst_path).ok()?;
            let data = zstd::decode_all(compressed.as_slice()).ok()?;
            let content = String::from_utf8(data).ok()?;
            return serde_json::from_str(&content).ok();
        }

        let json_path = dir.join("call_graph.json");
        if json_path.exists() {
            let content = std::fs::read_to_string(&json_path).ok()?;
            let parsed: Self = serde_json::from_str(&content).ok()?;
            // Auto-migrate: compress legacy JSON to zstd
            if let Ok(compressed) = zstd::encode_all(content.as_bytes(), 9) {
                let zst_tmp = zst_path.with_extension("zst.tmp");
                if std::fs::write(&zst_tmp, &compressed).is_ok()
                    && std::fs::rename(&zst_tmp, &zst_path).is_ok()
                {
                    let _ = std::fs::remove_file(&json_path);
                }
            }
            return Some(parsed);
        }

        None
    }

    pub fn load_or_build(project_root: &str, inputs: &CallGraphInputs) -> Self {
        if let Some(previous) = Self::load(project_root) {
            Self::build_incremental(inputs, &previous)
        } else {
            Self::build(inputs)
        }
    }
}

// ---------------------------------------------------------------------------
// Cache staleness check (fast — mtime-based, no content reads)
// ---------------------------------------------------------------------------

fn cache_looks_stale(cached: &CallGraph, inputs: &CallGraphInputs) -> bool {
    if cached.file_hashes.len() != inputs.file_paths.len() {
        return true;
    }
    let cached_files: std::collections::HashSet<&str> =
        cached.file_hashes.keys().map(String::as_str).collect();
    let index_files: std::collections::HashSet<&str> =
        inputs.file_paths.iter().map(String::as_str).collect();
    cached_files != index_files
}

/// Whether `cached` was built from a file that is not among `inputs` — the
/// files the current request may use. Unlike [`cache_looks_stale`] this
/// ignores files the build could not read, so a resident graph is not
/// rebuilt on every request because of one unreadable file.
fn holds_withdrawn_files(cached: &CallGraph, inputs: &CallGraphInputs) -> bool {
    let current: std::collections::HashSet<&str> =
        inputs.file_paths.iter().map(String::as_str).collect();
    cached
        .file_hashes
        .keys()
        .any(|file| !current.contains(file.as_str()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn call_graph_dir(project_root: &str) -> Option<std::path::PathBuf> {
    GraphProvider::index_dir(project_root)
}

fn group_edges_by_file(edges: &[CallEdge]) -> HashMap<&str, Vec<CallEdge>> {
    let mut map: HashMap<&str, Vec<CallEdge>> = HashMap::new();
    for edge in edges {
        map.entry(edge.caller_file.as_str())
            .or_default()
            .push(edge.clone());
    }
    map
}

/// Owned version for safe `Send` across rayon threads.
fn group_symbols_by_file_owned(inputs: &CallGraphInputs) -> HashMap<String, Vec<SymbolSpan>> {
    let mut map: HashMap<String, Vec<SymbolSpan>> = HashMap::new();
    for sym in &inputs.symbols {
        map.entry(sym.file.clone()).or_default().push(sym.clone());
    }
    for syms in map.values_mut() {
        syms.sort_by_key(|s| s.start_line);
    }
    map
}

fn find_enclosing_symbol_owned(file_symbols: Option<&Vec<SymbolSpan>>, line: usize) -> String {
    let Some(syms) = file_symbols else {
        return "<module>".to_string();
    };
    let mut best: Option<&SymbolSpan> = None;
    for sym in syms {
        if line >= sym.start_line && line <= sym.end_line {
            match best {
                None => best = Some(sym),
                Some(prev) => {
                    if (sym.end_line - sym.start_line) < (prev.end_line - prev.start_line) {
                        best = Some(sym);
                    }
                }
            }
        }
    }
    best.map_or_else(|| "<module>".to_string(), |s| s.name.clone())
}

fn resolve_path(relative: &str, project_root: &str) -> String {
    let p = Path::new(relative);
    if p.is_absolute() && p.exists() {
        return relative.to_string();
    }
    let relative = relative.trim_start_matches(['/', '\\']);
    let joined = Path::new(project_root).join(relative);
    joined.to_string_lossy().to_string()
}

fn simple_hash(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Current content hashes of the indexed files — the same values a
/// [`CallGraph`] records in `file_hashes` — without building or loading a
/// call graph. For callers that need only the hashes (semantic cache keys
/// and project revision), which is a fraction of a call-graph build.
pub(crate) fn current_file_hashes(inputs: &CallGraphInputs) -> HashMap<String, String> {
    inputs
        .file_paths
        .par_iter()
        .filter_map(|rel| {
            let content = std::fs::read_to_string(resolve_path(rel, &inputs.project_root)).ok()?;
            Some((rel.clone(), simple_hash(&content)))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Scope-aware callee resolution (#321)
//
// Call edges store callees as bare names, so attributing `Run`/`Get`/`Handle`
// to a file by name alone links every same-named symbol (false positives).
// These helpers resolve a callee to its defining file using the caller's
// lexical scope and refuse to guess when a name stays ambiguous.
// ---------------------------------------------------------------------------

/// Build a `file -> imported files` adjacency from the project index's import
/// and reexport edges, used to scope callee resolution to a caller's imports.
pub fn build_import_adjacency(
    inputs: &CallGraphInputs,
) -> HashMap<String, std::collections::HashSet<String>> {
    let mut adj: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for (from, to) in &inputs.import_edges {
        adj.entry(from.clone()).or_default().insert(to.clone());
    }
    adj
}

/// Pick the defining file for a callee from candidate `def_files`, ranked by the
/// caller's lexical scope (most specific first):
///   1. the caller's own file,
///   2. exactly one file the caller imports,
///   3. exactly one file project-wide.
///
/// Returns `None` when the name stays ambiguous (never guesses).
fn rank_callee_def_file(
    def_files: &[&str],
    caller_file: &str,
    imports: &HashMap<String, std::collections::HashSet<String>>,
) -> Option<(String, ScopeMatch)> {
    if def_files.is_empty() {
        return None;
    }
    if def_files.contains(&caller_file) {
        return Some((caller_file.to_string(), ScopeMatch::SameFile));
    }
    if let Some(imported) = imports.get(caller_file) {
        let mut in_scope = def_files.iter().filter(|f| imported.contains(**f));
        if let Some(first) = in_scope.next()
            && in_scope.next().is_none()
        {
            return Some(((*first).to_string(), ScopeMatch::UniqueImport));
        }
    }
    if def_files.len() == 1 {
        return Some((def_files[0].to_string(), ScopeMatch::UniqueInProject));
    }
    None
}

/// The identifier a callee name ends in: `crate::db::save` → `save`,
/// `pkg.Func` → `Func`, `save` → `save`.
pub fn callee_segment(name: &str) -> &str {
    name.rsplit(|c: char| c == ':' || c == '.')
        .next()
        .unwrap_or(name)
}

/// Lower-cased path segments of a callee or query, without the relative
/// anchors that do not name anything (`crate`, `self`, `super`, `Self`):
/// `crate::config_io::write` → `["config_io", "write"]`.
fn path_segments(name: &str) -> Vec<String> {
    name.split(|c: char| c == ':' || c == '.')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .filter(|s| !matches!(s.as_str(), "crate" | "self" | "super"))
        .collect()
}

/// Whether a call to `callee_name` is a call to the symbol named by `query`.
///
/// Edges keep the callee as written, so `crate::config_io::write(..)` and
/// `config_io::write(..)` must match a query for `write`, and a qualified query
/// (`config_io::write`) must match the bare and the qualified spelling. The
/// shorter path must be a segment suffix of the longer one, so `a::save` never
/// matches `b::save`.
pub fn callee_matches(callee_name: &str, query: &str) -> bool {
    let callee = path_segments(callee_name);
    let query = path_segments(query);
    if callee.is_empty() || query.is_empty() {
        return false;
    }
    let (short, long) = if callee.len() <= query.len() {
        (&callee, &query)
    } else {
        (&query, &callee)
    };
    long.ends_with(short)
}

/// Narrow `defs` (files defining the callee's last segment) to the files the
/// callee's own path names: `config_io::write` → `…/config_io.rs` or
/// `…/config_io/mod.rs`; `CallGraph::build` → the file that also defines
/// `CallGraph`. Returns the single named file, or `None` when the qualifier
/// names no definition file or several (never guesses).
fn module_path_def_file<'a>(
    callee_name: &str,
    defs: &[&'a str],
    type_files: &HashMap<&str, Vec<&str>>,
) -> Option<&'a str> {
    let segments: Vec<&str> = callee_name
        .split(|c: char| c == ':' || c == '.')
        .filter(|s| !s.is_empty() && !matches!(*s, "crate" | "self" | "super" | "Self"))
        .collect();
    let qualifier = *segments
        .len()
        .checked_sub(2)
        .and_then(|i| segments.get(i))?;
    let names_module = |file: &str| -> bool {
        let path = Path::new(file);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == qualifier {
            return true;
        }
        matches!(stem, "mod" | "index" | "__init__" | "lib")
            && path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                == Some(qualifier)
    };
    let type_defs = type_files.get(qualifier);
    let mut named = defs
        .iter()
        .copied()
        .filter(|d| names_module(d) || type_defs.is_some_and(|files| files.contains(d)));
    let first = named.next()?;
    named.next().is_none().then_some(first)
}

/// Which scope step resolved a callee structurally — the strength of the
/// evidence. Same-file and imported definitions are bound by the caller's own
/// scope; a name that is merely unique project-wide may still be an external
/// function of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMatch {
    SameFile,
    UniqueImport,
    UniqueInProject,
    /// The call's own path names the defining module or type
    /// (`config_io::write` → `config_io.rs`), and exactly one such file
    /// defines the callee.
    ModulePath,
}

/// Structural view of one call edge's callee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralTarget {
    Resolved {
        file: String,
        via: ScopeMatch,
    },
    /// Several project definitions remain in scope; structure cannot decide.
    Ambiguous,
    /// No project symbol has this name (std, dependency, dynamic dispatch).
    Unknown,
}

impl StructuralTarget {
    pub fn file(&self) -> Option<&str> {
        match self {
            Self::Resolved { file, .. } => Some(file),
            Self::Ambiguous | Self::Unknown => None,
        }
    }
}

/// Resolve a single callee name to its defining file in the scope of `caller_file`.
pub fn resolve_callee_file(
    callee_name: &str,
    caller_file: &str,
    inputs: &CallGraphInputs,
    imports: &HashMap<String, std::collections::HashSet<String>>,
) -> Option<String> {
    let mut def_files: Vec<&str> = inputs
        .symbols
        .iter()
        .filter(|s| s.name == callee_name)
        .map(|s| s.file.as_str())
        .collect();
    def_files.sort_unstable();
    def_files.dedup();
    rank_callee_def_file(&def_files, caller_file, imports).map(|(file, _)| file)
}

/// Resolve each edge's callee to its defining file in *that edge's own* caller
/// scope (index-aligned with `edges`). `None` = ambiguous or unknown; never
/// guesses. Use this for per-edge consumers (graph edges); the name-keyed
/// [`resolve_callee_files`] additionally drops names that differ across scopes.
pub fn resolve_edge_callee_files(
    inputs: &CallGraphInputs,
    edges: &[CallEdge],
) -> Vec<Option<String>> {
    resolve_edge_callee_targets(inputs, edges)
        .into_iter()
        .map(|t| t.file().map(str::to_string))
        .collect()
}

/// Like [`resolve_edge_callee_files`], but keeps *why* each edge resolved —
/// or that it is ambiguous vs. unknown — for evidence grading and semantic
/// escalation.
pub fn resolve_edge_callee_targets(
    inputs: &CallGraphInputs,
    edges: &[CallEdge],
) -> Vec<StructuralTarget> {
    let callee_names: std::collections::HashSet<&str> = edges
        .iter()
        .flat_map(|e| [e.callee_name.as_str(), callee_segment(&e.callee_name)])
        .collect();
    if callee_names.is_empty() {
        return Vec::new();
    }

    // Definition files per name: callee names, their last path segment, and
    // capitalised names (types a path like `CallGraph::build` may qualify by).
    let mut name_files: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut type_files: HashMap<&str, Vec<&str>> = HashMap::new();
    for sym in &inputs.symbols {
        if callee_names.contains(sym.name.as_str()) {
            name_files
                .entry(sym.name.as_str())
                .or_default()
                .push(sym.file.as_str());
        }
        if sym.name.starts_with(char::is_uppercase) {
            type_files
                .entry(sym.name.as_str())
                .or_default()
                .push(sym.file.as_str());
        }
    }
    for files in name_files.values_mut().chain(type_files.values_mut()) {
        files.sort_unstable();
        files.dedup();
    }

    let imports = build_import_adjacency(inputs);
    edges
        .iter()
        .map(|e| {
            // Only definitions the caller's language can call directly: a Rust
            // `run()` never binds to a shell script's `run`.
            let family = call_family(&e.caller_file);
            let segment = callee_segment(&e.callee_name);
            if segment.len() < e.callee_name.len() {
                // Path callee (`db::save`): bind it only when its own path names
                // exactly one defining file; otherwise it is a semantic
                // candidate, never a structural guess by bare name.
                let Some(defs) = name_files
                    .get(segment)
                    .map(|defs| same_family_defs(defs, family))
                    .filter(|d| !d.is_empty())
                else {
                    return StructuralTarget::Unknown;
                };
                // `Self::new()` / `self::helper()` name the caller's own type or
                // module: bound when the caller's file defines the callee.
                let own_scope = ["Self::", "self::"]
                    .iter()
                    .any(|p| e.callee_name.starts_with(p));
                if own_scope && defs.contains(&e.caller_file.as_str()) {
                    return StructuralTarget::Resolved {
                        file: e.caller_file.clone(),
                        via: ScopeMatch::SameFile,
                    };
                }
                return match module_path_def_file(&e.callee_name, &defs, &type_files) {
                    Some(file) => StructuralTarget::Resolved {
                        file: file.to_string(),
                        via: ScopeMatch::ModulePath,
                    },
                    None => StructuralTarget::Ambiguous,
                };
            }
            let defs = name_files
                .get(e.callee_name.as_str())
                .map(|defs| same_family_defs(defs, family))
                .filter(|d| !d.is_empty());
            match defs {
                None => StructuralTarget::Unknown,
                Some(defs) => match rank_callee_def_file(&defs, &e.caller_file, &imports) {
                    Some((file, via)) => StructuralTarget::Resolved { file, via },
                    None => StructuralTarget::Ambiguous,
                },
            }
        })
        .collect()
}

/// Only definitions the caller's language `family` can call directly: a Rust
/// `run()` never binds to a shell script's `run`.
fn same_family_defs<'a>(defs: &[&'a str], family: Option<&str>) -> Vec<&'a str> {
    defs.iter()
        .copied()
        .filter(|d| family.is_none() || call_family(d).is_none_or(|f| Some(f) == family))
        .collect()
}

/// Languages that call each other's definitions directly, by file; `None`
/// for files whose language is unknown (never filtered out).
fn call_family(path: &str) -> Option<&'static str> {
    let id = crate::core::language_capabilities::language_for_path(path)?.id_str();
    Some(match id {
        "typescript" | "javascript" | "vue" | "svelte" => "js",
        "c" | "cpp" => "c",
        "java" | "kotlin" | "scala" => "jvm",
        "lua" | "luau" => "lua",
        other => other,
    })
}

/// Resolve callee names to a single defining file *when scope makes it
/// unambiguous across all call sites*. Names that resolve to different files
/// from different scopes are omitted, so callers never attribute a call to the
/// wrong file. Keyed by callee name to match the call graph's name-keyed nodes.
pub fn resolve_callee_files(
    inputs: &CallGraphInputs,
    edges: &[CallEdge],
) -> HashMap<String, String> {
    use std::collections::HashSet;

    let mut resolved: HashMap<&str, HashSet<String>> = HashMap::new();
    for (e, file) in edges.iter().zip(resolve_edge_callee_files(inputs, edges)) {
        if let Some(file) = file {
            resolved
                .entry(e.callee_name.as_str())
                .or_default()
                .insert(file);
        }
    }

    resolved
        .into_iter()
        .filter_map(|(name, files)| {
            (files.len() == 1).then(|| {
                (
                    name.to_string(),
                    files
                        .into_iter()
                        .next()
                        .expect("files.len() == 1 guarantees one element"),
                )
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "call_graph_tests.rs"]
pub mod tests;
