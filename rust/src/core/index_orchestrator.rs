use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::core::bm25_index::BM25Index;
use crate::core::embedding_index::EmbeddingBuildOutcome;
use crate::core::graph_index::{self, ProjectIndex};
use crate::core::index_pipeline::dump_engine::DumpEngine;
use crate::core::index_pipeline::pipeline::{IndexPipeline, PipelineReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Building,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
struct Component {
    state: State,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    duration_ms: Option<u64>,
    last_error: Option<String>,
    /// Human-readable outcome detail surfaced to operators (e.g. doc count +
    /// persisted size, or the "not persisted: too large …" remedy). Independent
    /// of `last_error` so a *successful* build can still carry a warning note.
    note: Option<String>,
}

impl Component {
    fn new() -> Self {
        Self {
            state: State::Idle,
            started_ms: None,
            finished_ms: None,
            duration_ms: None,
            last_error: None,
            note: None,
        }
    }
}

#[derive(Debug)]
struct ProjectBuild {
    worker_running: bool,
    /// Set the first time a heavy-index tool lazily pre-warms this root (#152).
    /// Prevents re-triggering a full rebuild on every subsequent dispatch — the
    /// tools' own `load_or_build` paths handle staleness from then on.
    warm_triggered: bool,
    graph: Component,
    bm25: Component,
    /// Dense embedding index (semantic search). Built after BM25 as Phase 3.
    /// Tracked separately so the orchestrator does not block on a missing ONNX
    /// model — the status lets users see why semantic stays cold (#249).
    semantic: Component,
    /// Most recent pipeline run report — carries mode, incremental flag,
    /// elapsed time and per-file/per-node stats.
    pipeline_report: Option<PipelineReport>,
}

impl ProjectBuild {
    fn new() -> Self {
        Self {
            worker_running: false,
            warm_triggered: false,
            graph: Component::new(),
            bm25: Component::new(),
            semantic: Component::new(),
            pipeline_report: None,
        }
    }
}

// Lock ordering (see rust/LOCK_ORDERING.md):
//   L1 = REGISTRY outer Mutex  (the HashMap guard)
//   L2 = per-project Arc<Mutex<ProjectBuild>>  (inner guard)
//
// Invariant: L1 must NEVER be held while locking L2.
// `entry_for()` enforces this by cloning the Arc and dropping L1 before
// the caller acquires L2.
static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn entry_for(project_root: &str) -> Arc<Mutex<ProjectBuild>> {
    let mut map = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(project_root.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(ProjectBuild::new())))
        .clone()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Per-repo lock name for serializing the BM25 build across processes, mirroring
/// the `graph-idx-<hash>` lock (see LOCK_ORDERING.md). The distinct `bm25-` vs
fn start_component(c: &mut Component) {
    c.state = State::Building;
    c.started_ms = Some(now_ms());
    c.finished_ms = None;
    c.duration_ms = None;
    c.last_error = None;
    c.note = None;
}

fn finish_ok(c: &mut Component) {
    c.state = State::Ready;
    let end = now_ms();
    c.finished_ms = Some(end);
    c.duration_ms = c.started_ms.map(|s| end.saturating_sub(s));
}

fn finish_err(c: &mut Component, e: String) {
    c.state = State::Failed;
    let end = now_ms();
    c.finished_ms = Some(end);
    c.duration_ms = c.started_ms.map(|s| end.saturating_sub(s));
    c.last_error = Some(e);
}

/// The index warmth a tool benefits from. Drives lazy, demand-driven warming
/// (issue #152) so the server no longer scans the whole project eagerly on every
/// `initialize` — a session that only uses `ctx_read`/`ctx_shell`/`ctx_tree`
/// pays zero indexing cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmNeed {
    /// No prebuilt index needed.
    None,
    /// Only the resident line-search (trigram) index — cheap, used by `ctx_search`.
    Search,
    /// Full project indices (graph + BM25; this also warms the search index).
    Heavy,
}

/// Classify a tool by the index warmth it benefits from. Unknown tools default
/// to [`WarmNeed::None`]; a heavy tool mis-classified as `None` still works — it
/// just builds its index synchronously on first use instead of being pre-warmed.
#[must_use]
pub fn warm_need_for_tool(tool: &str) -> WarmNeed {
    match tool {
        // Tools that build/consume the graph, call-graph, BM25 or artifact index.
        // `ctx_search` is included for lazy indexing: the first search
        // triggers a full pipeline build so indices are available on demand.
        "ctx_search"
        | "ctx_graph"
        | "ctx_callgraph"
        | "ctx_routes"
        | "ctx_repomap"
        | "ctx_impact"
        | "ctx_artifacts"
        | "ctx_semantic_search"
        | "ctx_provider"
        | "ctx_compose"
        | "ctx_explore"
        | "ctx_review" => WarmNeed::Heavy,
        _ => WarmNeed::None,
    }
}

/// Lazily warm the indices a tool needs, deduped per root. Never blocks (all
/// work is spawned in the background) and is safe to call on every dispatch.
///
/// Returns `true` only when this call is the *first* heavy pre-warm for `root`
/// in this process — the caller can use that signal to warm secondary roots once
/// without re-reading session state on every dispatch.
pub fn ensure_warm_for_tool(project_root: &str, tool: &str) -> bool {
    if project_root.is_empty() {
        return false;
    }
    match warm_need_for_tool(tool) {
        WarmNeed::None | WarmNeed::Search => false,
        WarmNeed::Heavy => {
            let entry = entry_for(project_root);
            let first_warm = {
                let mut s = entry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if s.warm_triggered {
                    false
                } else {
                    s.warm_triggered = true;
                    true
                }
            };
            if first_warm {
                ensure_all_background(project_root);
            }
            first_warm
        }
    }
}

/// Stack size for background index workers. Large enough that deep ASTs and
/// graph traversals cannot overflow it (the #378 SIGABRT class). The AST walks
/// are iterative now too, so this is defense-in-depth.
const INDEXER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Fire-and-forget: ask a running daemon to own the index build for `root`
/// (#460, shared-indexer-daemon / thin clients).
///
/// The daemon is the single long-lived, machine-wide indexer. Once it holds the
/// per-repo `graph-idx`/`bm25-idx` build locks, every session load-shares its
/// on-disk result instead of each running a full scan during a cold boot wave —
/// turning N simultaneous index passes into ~one. Strictly additive and
/// best-effort: it runs on its own thread (no ambient runtime to nest into), is
/// skipped when we *are* the daemon or when no daemon is reachable, and never
/// blocks the caller. The local build started right after remains the fallback,
/// so indexing always works with no daemon present.
fn nudge_daemon_index(project_root: &str) {
    // The daemon must never delegate the build to itself.
    if crate::daemon::is_foreground_daemon() {
        return;
    }
    let root = project_root.to_string();
    let _ = std::thread::Builder::new()
        .name("leanctx-index-nudge".to_string())
        .spawn(move || {
            if !crate::daemon::is_daemon_running() {
                return;
            }
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                return;
            };
            let body = serde_json::json!({ "root": root }).to_string();
            rt.block_on(async {
                let _ = crate::daemon_client::try_daemon_request("POST", "/v1/index/ensure", &body)
                    .await;
            });
        });
}

pub fn ensure_all_background(project_root: &str) {
    let state = entry_for(project_root);
    let should_spawn = {
        let mut s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.worker_running {
            false
        } else {
            s.worker_running = true;
            true
        }
    };

    if !should_spawn {
        return;
    }

    // #460: hand the build to the daemon (the single machine-wide indexer) when
    // one is running and we aren't it. Deduped naturally — we only reach here
    // when this process is actually about to start a build. Purely additive: the
    // local build below still runs and load-shares via the per-repo locks.
    nudge_daemon_index(project_root);

    let root = project_root.to_string();
    let indexer = move || {
        // ---- Single IndexPipeline run for Graph + BM25 ----
        let state = entry_for(&root);

        // Mark both components as building.
        {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            start_component(&mut s.graph);
            start_component(&mut s.bm25);
        }

        let pipeline_result = std::panic::catch_unwind(|| -> Result<_, String> {
            let handle = IndexPipeline::new(std::path::PathBuf::from(&root))
                .build()
                .map_err(|e| format!("pipeline build failed: {e}"))?;
            let report = handle
                .run()
                .map_err(|e| format!("pipeline run failed: {e}"))?;

            // Load the resulting indices from disk (dumped by the pipeline).
            let (graph_opt, bm25_opt, _metadata_store) =
                DumpEngine::load_with_integrity_check(std::path::Path::new(&root))
                    .map_err(|e| format!("loading dumped indices failed: {e}"))?;

            Ok((report, graph_opt, bm25_opt))
        });

        {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            match pipeline_result {
                Ok(Ok((report, graph_opt, bm25_opt))) => {
                    // #696 C4: persist graph index to the property graph
                    // (stamping `graph.meta.json`) so PG mirrors the freshly
                    // scanned index.
                    if let Some(ref idx) = graph_opt
                        && let Err(e) = idx.save()
                    {
                        tracing::warn!("[index_orchestrator: graph save failed: {e}]");
                    }

                    let graph_note = graph_opt.as_ref().map(|idx| {
                        format!("{} files, {} edges", idx.file_count(), idx.edge_count())
                    });
                    let bm25_note = bm25_opt
                        .as_ref()
                        .map(|idx| format!("{} chunks", idx.chunks.len()));

                    s.graph.note = graph_note;
                    s.bm25.note = bm25_note;
                    finish_ok(&mut s.graph);
                    finish_ok(&mut s.bm25);

                    // Update semantic component from the pipeline report.
                    // The pipeline's Step 8c already ran build_or_update, so we
                    // simply reflect its outcome in the orchestrator's state.
                    // Match BEFORE moving report into pipeline_report.
                    let embedding_outcome = &report.embedding_ready;
                    match embedding_outcome {
                        EmbeddingBuildOutcome::Ready => {
                            finish_ok(&mut s.semantic);
                        }
                        EmbeddingBuildOutcome::Skipped => {
                            s.semantic.state = State::Idle;
                            s.semantic.note =
                                Some("embeddings disabled by feature flag or config".to_string());
                        }
                        EmbeddingBuildOutcome::ModelNotAvailable(reason) => {
                            s.semantic.state = State::Idle;
                            s.semantic.note =
                                Some(format!("embedding model not available: {reason}"));
                        }
                        EmbeddingBuildOutcome::Failed => {
                            finish_err(
                                &mut s.semantic,
                                "embedding build failed (see logs)".to_string(),
                            );
                        }
                    }

                    // Store the report after all field reads are done.
                    s.pipeline_report = Some(report);
                }
                Ok(Err(e)) => {
                    finish_err(&mut s.graph, e.clone());
                    finish_err(&mut s.bm25, e);
                }
                Err(panic) => {
                    let msg = format!("index build panicked: {panic:?}");
                    finish_err(&mut s.graph, msg.clone());
                    finish_err(&mut s.bm25, msg);
                }
            }

            s.worker_running = false;
        }
    };

    // Indexing parses large ASTs and traverses graphs; give the worker a
    // generous stack as defense-in-depth against deep-recursion overflow (the
    // #378 SIGABRT class) and a name so it is identifiable in crash dumps.
    let spawned = std::thread::Builder::new()
        .name("leanctx-index".to_string())
        .stack_size(INDEXER_STACK_BYTES)
        .spawn(indexer);
    if spawned.is_err() {
        // The OS refused a new thread (rare). Clear the in-flight flag so a
        // later trigger retries instead of assuming a build runs forever.
        let mut s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.worker_running = false;
    }
}

/// Ensure background indexing for all extra roots (in addition to the primary).
/// Each extra root that is not a subdirectory of `primary_root` gets its own
/// graph + BM25 index. Capped at `MAX_EXTRA_ROOT_BUILDS` to prevent runaway.
const MAX_EXTRA_ROOT_BUILDS: usize = 8;

pub fn ensure_extra_roots_background(primary_root: &str, extra_roots: &[String]) {
    let primary = Path::new(primary_root);
    let mut built = 0;
    for root in extra_roots {
        if built >= MAX_EXTRA_ROOT_BUILDS {
            break;
        }
        let rp = Path::new(root);
        if !rp.is_dir() {
            continue;
        }
        // Skip if extra_root is inside primary (already indexed by the primary scan)
        if rp.starts_with(primary) {
            continue;
        }
        // Skip if primary is inside this extra_root (avoid double-indexing the parent)
        if primary.starts_with(rp) {
            continue;
        }
        ensure_all_background(root);
        built += 1;
    }
}

/// Lightweight, allocation-frugal snapshot of the BM25 component for the
/// in-call composer/search messaging. Avoids the heavier [`disk_status`] walk.
#[derive(Debug, Clone)]
pub struct Bm25Summary {
    pub state: &'static str,
    /// While building: elapsed so far. Otherwise: last build duration.
    pub elapsed_ms: Option<u64>,
    pub note: Option<String>,
    pub last_error: Option<String>,
}

/// Lightweight snapshot of the semantic (dense embedding) component.
#[derive(Debug, Clone)]
pub struct SemanticSummary {
    pub state: &'static str,
    pub elapsed_ms: Option<u64>,
    pub note: Option<String>,
    pub last_error: Option<String>,
}

/// Shared helper: compute (state_str, elapsed_ms) for a component.
/// Deduplicates the elapsed-while-building logic and state-to-string mapping
/// between bm25_summary and semantic_summary.
fn component_elapsed_and_state(c: &Component) -> (&'static str, Option<u64>) {
    let elapsed_ms = if matches!(c.state, State::Building) {
        c.started_ms.map(|start| now_ms().saturating_sub(start))
    } else {
        c.duration_ms
    };
    let state = match c.state {
        State::Idle => "idle",
        State::Building => "building",
        State::Ready => "ready",
        State::Failed => "failed",
    };
    (state, elapsed_ms)
}

pub fn semantic_summary(project_root: &str) -> SemanticSummary {
    let entry = entry_for(project_root);
    let s = entry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let c = &s.semantic;
    let (state, elapsed_ms) = component_elapsed_and_state(c);
    SemanticSummary {
        state,
        elapsed_ms,
        note: c.note.clone(),
        last_error: c.last_error.clone(),
    }
}

pub fn bm25_summary(project_root: &str) -> Bm25Summary {
    let entry = entry_for(project_root);
    let s = entry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let c = &s.bm25;
    let (state, elapsed_ms) = component_elapsed_and_state(c);
    Bm25Summary {
        state,
        elapsed_ms,
        note: c.note.clone(),
        last_error: c.last_error.clone(),
    }
}

pub fn try_load_graph_index(project_root: &str) -> Option<ProjectIndex> {
    // Resident cache: avoids re-materializing the index from the property graph
    // (SQLite query) on every graph-touching query. Returns an in-memory clone.
    crate::core::graph_cache::get_cached(project_root).map(|arc| (*arc).clone())
}

pub fn try_load_bm25_index(project_root: &str) -> Option<BM25Index> {
    BM25Index::load(Path::new(project_root))
}

/// Result of an IndexPipeline run: both graph and BM25 indexes.
pub struct IndexBundle {
    pub graph: ProjectIndex,
    pub bm25: BM25Index,
}

/// Load both graph and BM25 indexes from disk, building them via IndexPipeline
/// if not found. Replacement for the deprecated [`BM25Index::build_from_directory`]
/// and [`BM25Index::build_from_directory`] — routes all callers through the
/// pipeline so there is a single code path for index construction.
///
/// Returns empty defaults when the pipeline cannot be built (e.g. non-existent
/// root) so callers never panic on index access.
pub fn load_indexes(project_root: &Path) -> IndexBundle {
    let root_str = project_root.to_string_lossy();
    if let (Some(graph), Some(bm25)) = (
        ProjectIndex::load(root_str.as_ref()),
        BM25Index::load(project_root),
    ) {
        return IndexBundle { graph, bm25 };
    }
    // Root guard: non-existent/non-dir roots get empty indexes, matching
    // the graceful fallback in try_build_pipeline.
    if !project_root.exists() || !project_root.is_dir() {
        return IndexBundle {
            graph: ProjectIndex::new(root_str.as_ref()),
            bm25: BM25Index::default(),
        };
    }
    match IndexPipeline::new(project_root.to_path_buf()).build() {
        Ok(pipeline) => match pipeline.run_and_load() {
            Ok((graph, bm25)) => IndexBundle { graph, bm25 },
            Err(_) => IndexBundle {
                graph: ProjectIndex::new(root_str.as_ref()),
                bm25: BM25Index::default(),
            },
        },
        Err(_) => IndexBundle {
            graph: ProjectIndex::new(root_str.as_ref()),
            bm25: BM25Index::default(),
        },
    }
}

/// Returns true if any project is currently building its indices.
pub fn is_building() -> bool {
    let map = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.values().any(|entry| {
        let st = entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(st.bm25.state, State::Building)
            || matches!(st.graph.state, State::Building)
            || matches!(st.semantic.state, State::Building)
    })
}

#[derive(Debug, Serialize)]
struct ComponentStatus<'a> {
    state: &'a str,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    duration_ms: Option<u64>,
    last_error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

fn component_status(c: &Component) -> ComponentStatus<'_> {
    ComponentStatus {
        state: match c.state {
            State::Idle => "idle",
            State::Building => "building",
            State::Ready => "ready",
            State::Failed => "failed",
        },
        started_ms: c.started_ms,
        finished_ms: c.finished_ms,
        duration_ms: c.duration_ms,
        last_error: c.last_error.as_deref(),
        note: c.note.as_deref(),
    }
}

#[derive(Debug, Serialize)]
struct StatusResponse<'a> {
    project_root: &'a str,
    graph_index: ComponentStatus<'a>,
    bm25_index: ComponentStatus<'a>,
    /// Dense embedding index built after BM25.  "idle" means the ONNX model
    /// has not been downloaded yet or the embeddings feature was not compiled
    /// in; "ready" means embeddings are persisted and search will use them.
    semantic_index: ComponentStatus<'a>,
    /// Indexing mode used by the most recent pipeline run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_mode: Option<&'a str>,
    /// Whether the most recent pipeline run was incremental.
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_incremental: Option<bool>,
    /// Elapsed wall-clock time of the most recent pipeline run, in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_elapsed_ms: Option<u64>,
    disk: DiskStatusAll,
}

#[derive(Debug, Serialize, Default)]
pub struct DiskStatus {
    pub exists: bool,
    pub size_bytes: Option<u64>,
    pub file_count: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct DiskStatusAll {
    pub graph_index: DiskStatus,
    pub bm25_index: DiskStatus,
    pub code_graph: DiskStatus,
    /// On-disk embedding index (`embeddings.bin`).  Present when dense search
    /// has been built at least once; absent when the model is not downloaded
    /// yet or embeddings are disabled by config.
    pub semantic_index: DiskStatus,
}

fn disk_status_for_graph(project_root: &str) -> DiskStatus {
    // #696 C4: the property graph is the sole store. The logical graph-index
    // view (file count) is sized/timed by `graph.meta.json`, which the mirror
    // stamps on every build; `disk_status_for_code_graph` reports the raw
    // SQLite store (nodes, graph.db) as a distinct facet.
    let Some(dir) = graph_index::ProjectIndex::index_dir(project_root) else {
        return DiskStatus::default();
    };
    let meta_file = dir.join("graph.meta.json");
    if !meta_file.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&meta_file).ok();
    let file_count =
        graph_index::ProjectIndex::load(project_root).map(|idx| idx.files.len() as u64);
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn disk_status_for_bm25(project_root: &str) -> DiskStatus {
    let root = Path::new(project_root);
    let path = BM25Index::index_file_path(root);
    if !path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&path).ok();
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: None,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn disk_status_for_code_graph(project_root: &str) -> DiskStatus {
    let dir = crate::core::property_graph::graph_dir(project_root);
    let db_path = dir.join("graph.db");
    if !db_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&db_path).ok();
    let node_count = crate::core::property_graph::CodeGraph::open(project_root)
        .ok()
        .and_then(|g| {
            g.connection()
                .query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .ok()
                .map(|c| c as u64)
        });
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: node_count,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn format_time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let dt = chrono::DateTime::from_timestamp(secs as i64, 0);
    dt.map_or_else(
        || format!("{secs}"),
        |d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

pub fn disk_status_for_semantic(project_root: &str) -> DiskStatus {
    let root = Path::new(project_root);
    let dir = crate::core::index_namespace::vectors_dir(root);
    let bin_path = dir.join("embeddings.bin");
    if !bin_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&bin_path).ok();
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: None,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

pub fn disk_status(project_root: &str) -> DiskStatusAll {
    DiskStatusAll {
        graph_index: disk_status_for_graph(project_root),
        bm25_index: disk_status_for_bm25(project_root),
        code_graph: disk_status_for_code_graph(project_root),
        semantic_index: disk_status_for_semantic(project_root),
    }
}

pub fn status_json(project_root: &str) -> String {
    // Compute disk status first — may do SQLite I/O and must NOT hold L2
    // (per-project Mutex) while doing so, or the background index worker
    // cannot call finish_ok / set worker_running = false (#deadlock).
    let disk = disk_status(project_root);
    let state = entry_for(project_root);
    let s = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Extract pipeline fields from the stored report into local variables
    // so the &str borrow lives long enough for serialization.
    let pipeline_mode_label: Option<String> = s
        .pipeline_report
        .as_ref()
        .map(|r| r.mode.label().to_string());
    let pipeline_incremental = s.pipeline_report.as_ref().map(|r| r.is_incremental);
    let pipeline_elapsed_ms = s.pipeline_report.as_ref().map(|r| r.elapsed_ms);

    let res = StatusResponse {
        project_root,
        graph_index: component_status(&s.graph),
        bm25_index: component_status(&s.bm25),
        semantic_index: component_status(&s.semantic),
        pipeline_mode: pipeline_mode_label.as_deref(),
        pipeline_incremental,
        pipeline_elapsed_ms,
        disk,
    };
    serde_json::to_string(&res).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_is_valid_json() {
        let s = status_json("/tmp");
        let _: serde_json::Value = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn warm_need_classifies_tools() {
        // Lightweight tools must never trigger a project scan (#152).
        for light in [
            "ctx_read",
            "ctx_shell",
            "ctx_tree",
            "ctx_knowledge",
            "unknown_tool",
        ] {
            assert_eq!(warm_need_for_tool(light), WarmNeed::None, "{light}");
        }
        // ctx_search triggers the full pipeline for lazy indexing.
        assert_eq!(warm_need_for_tool("ctx_search"), WarmNeed::Heavy);
        for heavy in [
            "ctx_graph",
            "ctx_callgraph",
            "ctx_routes",
            "ctx_repomap",
            "ctx_impact",
            "ctx_artifacts",
            "ctx_semantic_search",
            "ctx_provider",
            "ctx_compose",
            "ctx_explore",
            "ctx_review",
        ] {
            assert_eq!(warm_need_for_tool(heavy), WarmNeed::Heavy, "{heavy}");
        }
    }

    #[test]
    fn ensure_warm_lightweight_never_signals_and_search_now_signals() {
        assert!(!ensure_warm_for_tool("", "ctx_graph"));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        assert!(!ensure_warm_for_tool(&root, "ctx_read"));
        // ctx_search is now Heavy (lazy indexing) — first call signals true.
        assert!(ensure_warm_for_tool(&root, "ctx_search"));
    }

    #[test]
    fn ensure_warm_heavy_is_once_per_root() {
        // The first heavy pre-warm signals `true` (so the caller warms extra
        // roots once); every subsequent call is a no-op `false`, preventing a
        // rebuild-on-every-dispatch storm.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        assert!(
            ensure_warm_for_tool(&root, "ctx_callgraph"),
            "first heavy warm must signal true"
        );
        assert!(
            !ensure_warm_for_tool(&root, "ctx_callgraph"),
            "second heavy warm must be deduped to false"
        );
        assert!(
            !ensure_warm_for_tool(&root, "ctx_semantic_search"),
            "any later heavy tool on the same root is also deduped"
        );
    }

    #[test]
    fn bm25_summary_unknown_project_is_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let summary = bm25_summary(tmp.path().to_string_lossy().as_ref());
        assert_eq!(summary.state, "idle");
        assert!(summary.note.is_none());
        assert!(summary.last_error.is_none());
    }

    #[test]
    fn extra_roots_skips_subdirs_of_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();
        let sub = primary.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let external = tmp.path().join("external");
        std::fs::create_dir_all(&external).unwrap();

        let primary_str = primary.to_string_lossy().to_string();
        let extra = vec![
            sub.to_string_lossy().to_string(),
            external.to_string_lossy().to_string(),
        ];

        // Should not panic; subdirs are skipped, external is attempted
        ensure_extra_roots_background(&primary_str, &extra);
    }

    #[test]
    fn extra_roots_caps_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("primary");
        std::fs::create_dir_all(&primary).unwrap();

        let mut extra = Vec::new();
        for i in 0..20 {
            let d = tmp.path().join(format!("ext-{i}"));
            std::fs::create_dir_all(&d).unwrap();
            extra.push(d.to_string_lossy().to_string());
        }

        let primary_str = primary.to_string_lossy().to_string();
        // Should not spawn more than MAX_EXTRA_ROOT_BUILDS threads
        ensure_extra_roots_background(&primary_str, &extra);
    }
}
