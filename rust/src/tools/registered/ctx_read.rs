use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, require_resolved_path,
};
use crate::tool_defs::tool_def;

/// Per-file lock that serializes concurrent reads of the same path.
///
/// When multiple subagents read sequentially through a shared set of files,
/// they tend to hit the same path at the same time. Without per-file locking
/// they all contend on the global cache write lock while doing redundant I/O.
/// This lock ensures only one thread reads a given file from disk; the others
/// wait cheaply on the per-file mutex, then hit the warm cache.
///
/// Backed by the shared `core::path_locks` registry so reads and edits of the
/// same path coordinate through a single mutex (see issue #320).
fn per_file_lock(path: &str) -> Arc<Mutex<()>> {
    crate::core::path_locks::per_file_lock(path)
}

pub struct CtxReadTool;

impl McpTool for CtxReadTool {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_read",
            "Read a file. Prefer over native Read/cat/head/tail (cached, compressed).\n\
             Omit mode to auto-select (recommended); use full only right before editing. \
             Re-reads ~13 tokens. fresh=true forces a disk re-read.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute file path" },
                    "mode": {
                        "type": "string",
                        "default": "auto",
                        "description": "Omit to auto-select (recommended). auto(default)|full (for editing)|raw|map (large files)|signatures|diff|task|reference|aggressive|entropy|lines:N-M|density:0.X"
                    },
                    "start_line": { "type": "integer", "description": "First line, 1-based (alias: offset)" },
                    "offset": { "type": "integer", "description": "Alias for start_line" },
                    "limit": { "type": "integer", "description": "Max lines to read" },
                    "fresh": { "type": "boolean", "description": "Bypass cache, force disk re-read" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_inner(args, ctx, &path)
        })) {
            Ok(result) => result,
            Err(_) => Err(ErrorData::internal_error(
                format!(
                    "ctx_read panicked while processing '{path}'. This is a bug — please report it."
                ),
                None,
            )),
        }
    }
}

impl CtxReadTool {
    #[allow(clippy::unused_self)]
    fn handle_inner(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
        path: &str,
    ) -> Result<ToolOutput, ErrorData> {
        let session_lock = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

        let current_task = {
            let rt = tokio::runtime::Handle::current();
            let mut attempt = 0u32;
            loop {
                if let Ok(session) = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    session_lock.read(),
                )) {
                    break session.task.as_ref().map(|t| t.description.clone());
                }
                attempt += 1;
                if attempt >= 3 {
                    tracing::warn!(
                        "session read-lock timeout after {attempt} attempts in ctx_read for {path}"
                    );
                    return Err(ErrorData::internal_error(
                        "session lock timeout — another tool may be holding it. Retry in a moment.",
                        None,
                    ));
                }
                tracing::debug!(
                    "session read-lock attempt {attempt}/3 timed out for {path}, retrying"
                );
                std::thread::sleep(std::time::Duration::from_millis(100 * u64::from(attempt)));
            }
        };
        let task_ref = current_task.as_deref();

        let profile = crate::core::profiles::active_profile();
        let explicit_mode_arg = get_str(args, "mode");
        let explicit_mode = explicit_mode_arg.is_some();
        let mut mode = if let Some(m) = explicit_mode_arg {
            m
        } else if profile.read.default_mode_effective() == "auto" {
            if let Ok(cache) = cache_lock.try_read() {
                crate::tools::ctx_smart_read::select_mode_with_task(&cache, path, task_ref)
            } else {
                tracing::debug!(
                    "cache lock contested during auto-mode selection for {path}; \
                 falling back to full"
                );
                "full".to_string()
            }
        } else {
            profile.read.default_mode_effective().to_string()
        };
        let mut fresh = get_bool(args, "fresh").unwrap_or(false);
        let cache_policy = crate::server::compaction_sync::effective_cache_policy();
        if cache_policy == "off" {
            fresh = true;
        }
        // `start_line` (and its `offset`/`limit` aliases) can pin a line window.
        // The resolution lives in `apply_line_window`/`resolve_line_window` so
        // the runtime path and the unit tests share one implementation and can
        // never drift (GitHub #432 aliases, #259 explicit-mode, #253 line-1).
        apply_line_window(
            &mut mode,
            &mut fresh,
            explicit_mode,
            get_int(args, "start_line"),
            get_int(args, "offset"),
            get_int(args, "limit"),
        );

        let pressure_action = ctx.pressure_snapshot.as_ref().map(|p| &p.recommendation);
        let resolved_agent_id = ctx.agent_id.as_ref().and_then(|a| match a.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        });
        let gate_result = crate::server::context_gate::pre_dispatch_read_for_agent(
            path,
            &mode,
            task_ref,
            Some(&ctx.project_root),
            pressure_action,
            resolved_agent_id.as_deref(),
        );
        if gate_result.budget_blocked {
            let msg = gate_result
                .budget_warning
                .unwrap_or_else(|| "Agent token budget exceeded".to_string());
            return Err(ErrorData::invalid_params(msg, None));
        }
        let budget_warning = gate_result.budget_warning.clone();
        if let Some(overridden) = gate_result.overridden_mode {
            mode = overridden;
        }

        let (mode, degrade_warning) = if crate::tools::ctx_read::is_instruction_file(path) {
            ("full".to_string(), None)
        } else {
            auto_degrade_read_mode(&mode)
        };

        if mode.starts_with("lines:") {
            fresh = true;
        }

        if crate::core::binary_detect::is_binary_file(path) {
            let msg = crate::core::binary_detect::binary_file_message(path);
            return Err(ErrorData::invalid_params(msg, None));
        }
        {
            let cap = crate::core::limits::max_read_bytes() as u64;
            if let Ok(meta) = std::fs::metadata(path)
                && meta.len() > cap
            {
                let msg = format!(
                    "File too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
                         Use mode=\"lines:1-100\" for partial reads or increase the limit.",
                    meta.len(),
                    cap
                );
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        // Compaction-aware: if host compacted since last check, reset delivery flags
        // so post-compaction reads deliver full content instead of stubs.
        if !fresh
            && let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir()
            && let Ok(mut cache) = cache_lock.try_write()
        {
            crate::server::compaction_sync::sync_if_compacted(&mut cache, &data_dir);
        }

        // Fast path: if both per-file lock and cache write-lock are immediately
        // available, execute inline without spawning a thread. This avoids thread +
        // channel overhead for the ~90% of calls that are cache hits.
        let read_timeout = std::time::Duration::from_secs(30);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (output, resolved_mode, original, is_cache_hit, file_ref, cache_stats) = {
            let crp_mode = ctx.crp_mode;
            let task_ref = current_task.as_deref();

            let fast_result = 'fast: {
                let file_lock = per_file_lock(path);
                let Some(_file_guard) = file_lock.try_lock().ok() else {
                    break 'fast None;
                };

                // Phase 1 (shared lock): the dominant case is re-reading an
                // unchanged file in full mode. Serve that stub under a *read*
                // lock so parallel reads of distinct files run concurrently
                // instead of serializing on the global write lock.
                if !fresh
                    && mode == "full"
                    && let Ok(cache) = cache_lock.try_read()
                    && let Some(read_output) =
                        crate::tools::ctx_read::try_stub_hit_readonly(&cache, path)
                {
                    let content = read_output.content;
                    let rmode = read_output.resolved_mode;
                    let orig = cache.get(path).map_or(0, |e| e.original_tokens);
                    let hit = content.contains(" cached ")
                        || content.contains("[unchanged")
                        || content.contains("[delta:");
                    let fref = cache.file_ref_map().get(path).cloned();
                    let stats = cache.get_stats();
                    let stats_snapshot = (stats.total_reads(), stats.cache_hits());
                    break 'fast Some((content, rmode, orig, hit, fref, stats_snapshot));
                }

                // Phase 2 (write lock): cache miss, changed file, or non-stub
                // modes (map/signatures/diff/lines) that mutate cache state.
                let Some(mut cache) = cache_lock.try_write().ok() else {
                    break 'fast None;
                };
                let read_output = if fresh {
                    crate::tools::ctx_read::handle_fresh_with_task_resolved(
                        &mut cache, path, &mode, crp_mode, task_ref,
                    )
                } else {
                    crate::tools::ctx_read::handle_with_task_resolved(
                        &mut cache, path, &mode, crp_mode, task_ref,
                    )
                };
                let content = read_output.content;
                let rmode = read_output.resolved_mode;
                let orig = cache.get(path).map_or(0, |e| e.original_tokens);
                let hit = content.contains(" cached ")
                    || content.contains("[unchanged")
                    || content.contains("[delta:");
                let fref = cache.file_ref_map().get(path).cloned();
                let stats = cache.get_stats();
                let stats_snapshot = (stats.total_reads(), stats.cache_hits());
                Some((content, rmode, orig, hit, fref, stats_snapshot))
            };

            if let Some(result) = fast_result {
                result
            } else {
                // Slow path: spawn thread with bounded timeout for contended locks.
                let cache_lock = cache_lock.clone();
                let mode = mode.clone();
                let task_owned = current_task.clone();
                let path_owned = path.to_string();
                let cancel_flag = cancelled.clone();
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let file_lock = per_file_lock(&path_owned);

                    // Bounded per-file lock: if a zombie thread still holds it, don't
                    // wait forever. 25s keeps us inside the 30s recv_timeout.
                    let _file_guard = {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(25);
                        loop {
                            if cancel_flag.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Ok(guard) = file_lock.try_lock() {
                                break guard;
                            }
                            if std::time::Instant::now() >= deadline {
                                tracing::error!(
                                    "ctx_read: per-file lock timeout after 25s for {path_owned}"
                                );
                                let _ = tx.send((
                                    format!("per-file lock contention for {path_owned} — retry in a moment"),
                                    "error".to_string(), 0, false, None, (0, 0),
                                ));
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    };

                    if cancel_flag.load(Ordering::Relaxed) {
                        return;
                    }

                    // Bounded cache write-lock: avoids indefinite block when a zombie
                    // thread from a previous timed-out call still holds the lock.
                    let mut cache = {
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(25);
                        loop {
                            if cancel_flag.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Ok(guard) = cache_lock.try_write() {
                                break guard;
                            }
                            if std::time::Instant::now() >= deadline {
                                tracing::error!(
                                    "ctx_read: cache write-lock timeout after 25s for {path_owned}"
                                );
                                let _ = tx.send((
                                    format!(
                                        "cache lock contention for {path_owned} — retry in a moment"
                                    ),
                                    "error".to_string(),
                                    0,
                                    false,
                                    None,
                                    (0, 0),
                                ));
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                    };

                    let task_ref = task_owned.as_deref();
                    let read_output = if fresh {
                        crate::tools::ctx_read::handle_fresh_with_task_resolved(
                            &mut cache,
                            &path_owned,
                            &mode,
                            crp_mode,
                            task_ref,
                        )
                    } else {
                        crate::tools::ctx_read::handle_with_task_resolved(
                            &mut cache,
                            &path_owned,
                            &mode,
                            crp_mode,
                            task_ref,
                        )
                    };
                    let content = read_output.content;
                    let rmode = read_output.resolved_mode;
                    let orig = cache.get(&path_owned).map_or(0, |e| e.original_tokens);
                    let hit = content.contains(" cached ");
                    let fref = cache.file_ref_map().get(path_owned.as_str()).cloned();
                    let stats = cache.get_stats();
                    let stats_snapshot = (stats.total_reads(), stats.cache_hits());
                    let _ = tx.send((content, rmode, orig, hit, fref, stats_snapshot));
                });
                if let Ok(result) = rx.recv_timeout(read_timeout) {
                    result
                } else {
                    cancelled.store(true, Ordering::Relaxed);
                    tracing::error!("ctx_read timed out after {read_timeout:?} for {path}");
                    let msg = format!(
                        "ERROR: ctx_read timed out after {}s reading {path}. \
                     The file may be very large or a blocking I/O issue occurred. \
                     Try mode=\"lines:1-100\" for a partial read.",
                        read_timeout.as_secs()
                    );
                    return Err(ErrorData::internal_error(msg, None));
                }
            } // end else (slow path)
        };

        // Convert error results to proper MCP ErrorData instead of success body
        if resolved_mode == "error" {
            return Err(ErrorData::invalid_params(output, None));
        }

        let output_tokens = crate::core::tokens::count_tokens(&output);
        let saved = original.saturating_sub(output_tokens);

        // Session updates (bounded lock — 10s timeout, read already succeeded)
        let mut ensured_root: Option<String> = None;
        let mut traversal_working_set: Vec<String> = Vec::new();
        let project_root_snapshot;
        {
            let rt = tokio::runtime::Handle::current();
            let session_guard = rt.block_on(tokio::time::timeout(
                std::time::Duration::from_secs(10),
                session_lock.write(),
            ));
            if let Ok(mut session) = session_guard {
                session.touch_file(path, file_ref.as_deref(), &resolved_mode, original);
                // Capture the recent working set (under the lock) so the
                // background thread can record a traversal/co-access edge (#289).
                traversal_working_set =
                    crate::core::tool_lifecycle::recent_working_set(&session, path);
                // Auto-generate file summary from output content
                let file_summary = extract_file_summary(&output, path);
                if !file_summary.is_empty() {
                    session.set_file_summary(path, &file_summary);
                }
                if is_cache_hit {
                    session.record_cache_hit();
                }
                if session.active_structured_intent.is_none() && session.files_touched.len() >= 2 {
                    let touched: Vec<String> = session
                        .files_touched
                        .iter()
                        .map(|f| f.path.clone())
                        .collect();
                    let inferred =
                        crate::core::intent_engine::StructuredIntent::from_file_patterns(&touched);
                    if inferred.confidence >= 0.4 {
                        session.active_structured_intent = Some(inferred);
                    }
                }
                // Auto-infer task every 5th file read if not explicitly set
                if session.task.is_none() && session.stats.files_read % 5 == 0 {
                    session.auto_infer_task();
                }
                let root_missing = session
                    .project_root
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty());
                if root_missing && let Some(root) = crate::core::protocol::detect_project_root(path)
                {
                    session.project_root = Some(root.clone());
                    ensured_root = Some(root);
                }
                project_root_snapshot = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ".".to_string());
            } else {
                tracing::warn!(
                    "session write-lock timeout (5s) in ctx_read post-update for {path}"
                );
                project_root_snapshot = ctx.project_root.clone();
            }
        }

        if let Some(root) = ensured_root.as_deref() {
            crate::core::index_orchestrator::ensure_all_background(root);
        }

        // Telemetry + learning are pure side-effects that never influence this
        // response, yet they did synchronous disk I/O on every read (heatmap
        // append, ModePredictor load+save, FeedbackStore load). Push them off
        // the hot path so reads — especially cache-hit stubs — return without
        // waiting on disk (#149).
        {
            let path_bg = path.to_string();
            let resolved_mode_bg = resolved_mode.clone();
            let project_root_bg = project_root_snapshot.clone();
            let (turns, hits) = cache_stats;
            std::thread::spawn(move || {
                // A panic in telemetry must not poison locks or leave a zombie thread;
                // it never affects the already-returned read response.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    crate::core::heatmap::record_file_access(&path_bg, original, saved);

                    // Traversal/co-access edge: this read fired together with the
                    // recent working set captured under the session lock (#289).
                    if let Some(root) =
                        crate::core::tool_lifecycle::usable_root(Some(project_root_bg.as_str()))
                    {
                        crate::core::cooccurrence::record_focus_access(
                            root,
                            &path_bg,
                            &traversal_working_set,
                        );
                    }

                    let sig =
                        crate::core::mode_predictor::FileSignature::from_path(&path_bg, original);
                    let density = if output_tokens > 0 {
                        original as f64 / output_tokens as f64
                    } else {
                        1.0
                    };
                    let outcome = crate::core::mode_predictor::ModeOutcome {
                        mode: resolved_mode_bg,
                        tokens_in: original,
                        tokens_out: output_tokens,
                        density: density.min(1.0),
                    };
                    let mut predictor = crate::core::mode_predictor::ModePredictor::new();
                    predictor.set_project_root(&project_root_bg);
                    predictor.record(sig, outcome);
                    predictor.save();

                    let ext = std::path::Path::new(&path_bg)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_string();
                    let thresholds =
                        crate::core::adaptive_thresholds::thresholds_for_path(&path_bg);
                    let feedback_outcome = crate::core::feedback::CompressionOutcome {
                        session_id: format!("{}", std::process::id()),
                        language: ext,
                        entropy_threshold: thresholds.bpe_entropy,
                        jaccard_threshold: thresholds.jaccard,
                        total_turns: turns as u32,
                        tokens_saved: saved as u64,
                        tokens_original: original as u64,
                        cache_hits: hits as u32,
                        total_reads: turns as u32,
                        // Real behavioral signal instead of a hardcoded success
                        // (#593): a compressed read only counts as task-completing
                        // when this extension is not in a high-bounce state —
                        // compression that keeps forcing full re-reads is not
                        // "completing" anything. Unknown (too few reads) stays
                        // optimistic so the cold start is unchanged. 0.30 mirrors
                        // bounce_tracker::BOUNCE_RATE_THRESHOLD.
                        task_completed: crate::core::bounce_tracker::global()
                            .lock()
                            .ok()
                            .and_then(|bt| bt.bounce_rate_for_extension(&path_bg))
                            .is_none_or(|rate| rate < 0.30),
                        timestamp: chrono::Local::now().to_rfc3339(),
                    };
                    let mut store = crate::core::feedback::FeedbackStore::load();
                    store.project_root = Some(project_root_bg);
                    store.record_outcome(feedback_outcome);
                }));
            });
        }

        if let Some(aid) = resolved_agent_id.as_deref() {
            crate::core::agent_budget::record_consumption(aid, output_tokens);
        }

        // Cross-source hints: if a graph index exists and has cross-source edges
        // pointing to this file, append compact hints so the agent knows about
        // related issues/PRs/schemas without a separate tool call.
        let hints_suffix = {
            if let Some(index) = crate::core::graph_index::ProjectIndex::load(&ctx.project_root) {
                let hints = crate::core::cross_source_hints::hints_for_file(
                    path,
                    &index.edges,
                    &ctx.project_root,
                );
                if hints.is_empty() {
                    String::new()
                } else {
                    crate::core::cross_source_hints::format_hints(&hints)
                }
            } else {
                String::new()
            }
        };

        let mut warnings = Vec::new();
        if let Some(ref w) = budget_warning {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = degrade_warning {
            warnings.push(w.as_str());
        }
        let final_output = if !warnings.is_empty() {
            format!("{output}{hints_suffix}\n\n{}", warnings.join("\n"))
        } else if hints_suffix.is_empty() {
            output
        } else {
            format!("{output}{hints_suffix}")
        };

        Ok(ToolOutput {
            text: final_output,
            original_tokens: original,
            saved_tokens: saved,
            mode: Some(resolved_mode),
            path: Some(path.to_string()),
            changed: false,
            shell_outcome: None,
        })
    }
}

/// Resolve the `start_line`/`offset`/`limit` arguments into `(start, limit)`.
///
/// `offset` is an alias for `start_line` (1-based first line); `start_line`
/// wins if a caller passes both. `limit` (when > 0) bounds the number of lines;
/// a bare `limit` reads from line 1. Returns `None` when no windowing argument
/// is present, so the caller leaves the mode untouched (GitHub #432).
fn resolve_line_window(
    start_line: Option<i64>,
    offset: Option<i64>,
    limit: Option<i64>,
) -> Option<(i64, Option<i64>)> {
    let start = start_line.or(offset).map(|v| v.max(1));
    let limit = limit.filter(|&l| l > 0);
    match (start, limit) {
        (Some(s), l) => Some((s, l)),
        (None, Some(_)) => Some((1, limit)),
        (None, None) => None,
    }
}

/// Build the `lines:N-M` mode string for a resolved window. An unbounded window
/// (no `limit`) reads to EOF via the historical `999999` sentinel.
fn lines_mode(start: i64, limit: Option<i64>) -> String {
    match limit {
        Some(l) => format!("lines:{start}-{}", start + l - 1),
        None => format!("lines:{start}-999999"),
    }
}

/// Apply a resolved line window to `mode`/`fresh`. An explicit non-lines mode
/// (map/signatures/…) is never clobbered (#259), and `start_line=1` with no
/// limit is a no-op so it cannot disturb an auto/explicit read (#253).
fn apply_line_window(
    mode: &mut String,
    fresh: &mut bool,
    explicit_mode: bool,
    start_line: Option<i64>,
    offset: Option<i64>,
    limit: Option<i64>,
) {
    let Some((start, limit)) = resolve_line_window(start_line, offset, limit) else {
        return;
    };
    if start <= 1 && limit.is_none() {
        return;
    }
    *fresh = true;
    if !explicit_mode || mode.starts_with("lines") {
        *mode = lines_mode(start, limit);
    }
}

fn apply_verdict(
    mode: &str,
    verdict: crate::core::degradation_policy::DegradationVerdictV1,
) -> (String, bool) {
    use crate::core::degradation_policy::DegradationVerdictV1;
    match verdict {
        DegradationVerdictV1::Ok => (mode.to_string(), false),
        DegradationVerdictV1::Warn => match mode {
            "full" => ("map".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Throttle => match mode {
            "full" | "map" => ("signatures".to_string(), true),
            other => (other.to_string(), false),
        },
        DegradationVerdictV1::Block => {
            if mode == "signatures" {
                ("signatures".to_string(), false)
            } else {
                ("signatures".to_string(), true)
            }
        }
    }
}

fn auto_degrade_read_mode(mode: &str) -> (String, Option<String>) {
    if crate::core::config::Config::load().no_degrade_effective() {
        return (mode.to_string(), None);
    }
    let profile = crate::core::profiles::active_profile();
    if !profile.degradation.enforce_effective() {
        return (mode.to_string(), None);
    }
    let policy = crate::core::degradation_policy::evaluate_v1_for_tool("ctx_read", None);
    let (new_mode, degraded) = apply_verdict(mode, policy.decision.verdict);
    let warning = if degraded {
        Some(format!(
            "⚠ Context pressure: mode={mode} was downgraded to mode={new_mode} \
             (verdict: {:?}). Use start_line=1 to bypass, or run ctx_compress to free budget.",
            policy.decision.verdict
        ))
    } else {
        None
    };
    (new_mode, warning)
}

fn extract_file_summary(output: &str, path: &str) -> String {
    let hint = crate::core::auto_findings::extract_content_hint(output);
    if !hint.is_empty() {
        return hint;
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_count = output.lines().count();
    if line_count > 5 {
        format!("{ext} file, {line_count} lines")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn per_file_lock_same_path_returns_same_mutex() {
        let lock_a1 = per_file_lock("/tmp/test_same_path.txt");
        let lock_a2 = per_file_lock("/tmp/test_same_path.txt");
        assert!(Arc::ptr_eq(&lock_a1, &lock_a2));
    }

    #[test]
    fn per_file_lock_different_paths_return_different_mutexes() {
        let lock_a = per_file_lock("/tmp/test_path_a.txt");
        let lock_b = per_file_lock("/tmp/test_path_b.txt");
        assert!(!Arc::ptr_eq(&lock_a, &lock_b));
    }

    #[test]
    fn per_file_lock_serializes_concurrent_access() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let path = "/tmp/test_concurrent_serialization.txt";
        let mut handles = Vec::new();

        for _ in 0..5 {
            let counter = counter.clone();
            let max_concurrent = max_concurrent.clone();
            let path = path.to_string();
            handles.push(std::thread::spawn(move || {
                let lock = per_file_lock(&path);
                let _guard = lock.lock().unwrap();
                let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn per_file_lock_allows_parallel_different_paths() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0..4 {
            let counter = counter.clone();
            let max_concurrent = max_concurrent.clone();
            let path = format!("/tmp/test_parallel_{i}.txt");
            handles.push(std::thread::spawn(move || {
                let lock = per_file_lock(&path);
                let _guard = lock.lock().unwrap();
                let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(50));
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert!(max_concurrent.load(Ordering::SeqCst) > 1);
    }

    /// Regression test for Issue #229: a zombie thread holding the cache write-lock
    /// must not block subsequent reads indefinitely. The try_write() loop inside
    /// the spawned thread should respect its 25s deadline and the cancellation flag.
    #[test]
    fn zombie_thread_does_not_block_subsequent_cache_access() {
        let cache: Arc<tokio::sync::RwLock<u32>> = Arc::new(tokio::sync::RwLock::new(0));

        // Simulate a zombie: hold the write-lock on a background thread for 2s.
        let zombie_lock = cache.clone();
        let _zombie = std::thread::spawn(move || {
            let _guard = zombie_lock.blocking_write();
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        std::thread::sleep(std::time::Duration::from_millis(50));

        // A try_read() must fail immediately (zombie holds write-lock).
        assert!(cache.try_read().is_err());

        // A try_write() loop with cancellation must exit promptly.
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = cancel.clone();
        let lock2 = cache.clone();
        let waiter = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            loop {
                if cancel2.load(Ordering::Relaxed) {
                    return (false, start.elapsed());
                }
                if let Ok(_guard) = lock2.try_write() {
                    return (true, start.elapsed());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });

        // Set cancellation after 200ms — the loop should exit quickly.
        std::thread::sleep(std::time::Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);

        let (acquired, elapsed) = waiter.join().unwrap();
        assert!(
            !acquired,
            "should not have acquired lock while zombie holds it"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "cancellation should have stopped the loop promptly"
        );
    }

    // -- Regression: GitHub Issue #253 + #259 --
    // Delegates to the real runtime helper so this test can never drift from
    // production behaviour.
    fn apply_start_line(
        mode: &mut String,
        fresh: &mut bool,
        explicit_mode: bool,
        start_line: Option<i64>,
    ) {
        super::apply_line_window(mode, fresh, explicit_mode, start_line, None, None);
    }

    #[test]
    fn start_line_1_does_not_override_mode() {
        let mut mode = "auto".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, false, Some(1));
        assert_eq!(mode, "auto", "start_line=1 should not change mode");
        assert!(!fresh, "start_line=1 should not force fresh=true");
    }

    #[test]
    fn start_line_gt1_overrides_implicit_mode() {
        let mut mode = "auto".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, false, Some(50));
        assert_eq!(mode, "lines:50-999999");
        assert!(fresh);
    }

    #[test]
    fn start_line_gt1_does_not_override_explicit_map() {
        // GitHub #259: mode=map + start_line=50 → mode stays map
        let mut mode = "map".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, true, Some(50));
        assert_eq!(
            mode, "map",
            "explicit mode=map must not be clobbered by start_line"
        );
        assert!(fresh, "start_line>1 should still force fresh");
    }

    #[test]
    fn start_line_gt1_does_not_override_explicit_signatures() {
        let mut mode = "signatures".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, true, Some(100));
        assert_eq!(mode, "signatures");
        assert!(fresh);
    }

    #[test]
    fn start_line_gt1_honors_explicit_lines_mode() {
        let mut mode = "lines:1-50".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, true, Some(30));
        assert_eq!(
            mode, "lines:30-999999",
            "explicit lines mode should accept start_line override"
        );
        assert!(fresh);
    }

    #[test]
    fn start_line_none_does_nothing() {
        let mut mode = "map".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, true, None);
        assert_eq!(mode, "map");
        assert!(!fresh);
    }

    #[test]
    fn start_line_1_with_explicit_mode_preserves_it() {
        // OpenCode sends start_line=1 + mode=map — both should be preserved
        let mut mode = "map".to_string();
        let mut fresh = false;
        apply_start_line(&mut mode, &mut fresh, true, Some(1));
        assert_eq!(mode, "map");
        assert!(!fresh);
    }

    // -- Regression: GitHub Issue #432 — `offset`/`limit` aliases --

    #[test]
    fn offset_is_alias_for_start_line() {
        let mut mode = "auto".to_string();
        let mut fresh = false;
        super::apply_line_window(&mut mode, &mut fresh, false, None, Some(40), None);
        assert_eq!(mode, "lines:40-999999");
        assert!(fresh);
    }

    #[test]
    fn offset_and_limit_make_bounded_window() {
        let mut mode = "auto".to_string();
        let mut fresh = false;
        super::apply_line_window(&mut mode, &mut fresh, false, None, Some(40), Some(20));
        assert_eq!(mode, "lines:40-59", "20 inclusive lines starting at 40");
        assert!(fresh);
    }

    #[test]
    fn limit_alone_reads_from_first_line() {
        let mut mode = "auto".to_string();
        let mut fresh = false;
        super::apply_line_window(&mut mode, &mut fresh, false, None, None, Some(25));
        assert_eq!(mode, "lines:1-25");
        assert!(fresh);
    }

    #[test]
    fn start_line_wins_over_offset_when_both_present() {
        assert_eq!(
            super::resolve_line_window(Some(10), Some(99), None),
            Some((10, None))
        );
    }

    #[test]
    fn resolve_clamps_start_and_drops_nonpositive_limit() {
        // Negative/zero start clamps to 1; non-positive limit is ignored.
        assert_eq!(
            super::resolve_line_window(Some(-5), None, Some(0)),
            Some((1, None))
        );
        // A bare non-positive limit yields no window at all.
        assert_eq!(super::resolve_line_window(None, None, Some(-3)), None);
        assert_eq!(super::resolve_line_window(None, None, None), None);
    }

    #[test]
    fn lines_mode_bounds_are_inclusive() {
        assert_eq!(super::lines_mode(40, Some(20)), "lines:40-59");
        assert_eq!(super::lines_mode(5, None), "lines:5-999999");
    }

    #[test]
    fn explicit_map_not_clobbered_by_offset_limit() {
        // #259 must also hold for the new aliases.
        let mut mode = "map".to_string();
        let mut fresh = false;
        super::apply_line_window(&mut mode, &mut fresh, true, None, Some(40), Some(20));
        assert_eq!(mode, "map", "explicit mode wins over offset/limit");
        assert!(fresh);
    }

    /// Schema/handler consistency (GitHub #432): the handler reads
    /// start_line/offset/limit, so the advertised schema must document them —
    /// otherwise agents (and the generated docs/manifest) can't discover the
    /// aliases and the divergence that caused this bug returns.
    #[test]
    fn schema_advertises_line_window_aliases() {
        let tool = CtxReadTool.tool_def();
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("ctx_read schema has a properties object");
        for key in ["path", "mode", "start_line", "offset", "limit", "fresh"] {
            assert!(props.contains_key(key), "ctx_read schema missing '{key}'");
        }
    }

    // -- Regression: GitHub Issue #262 --
    // auto_degrade_read_mode must produce a warning when mode is downgraded.

    use crate::core::degradation_policy::DegradationVerdictV1;

    #[test]
    fn verdict_ok_does_not_degrade() {
        let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Ok);
        assert_eq!(mode, "full");
        assert!(!degraded);
    }

    #[test]
    fn verdict_warn_degrades_full_to_map() {
        let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Warn);
        assert_eq!(mode, "map");
        assert!(degraded, "full→map must be flagged as degraded");
    }

    #[test]
    fn verdict_warn_keeps_map() {
        let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Warn);
        assert_eq!(mode, "map");
        assert!(!degraded, "map is not degraded under Warn");
    }

    #[test]
    fn verdict_warn_keeps_signatures() {
        let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Warn);
        assert_eq!(mode, "signatures");
        assert!(!degraded);
    }

    #[test]
    fn verdict_throttle_degrades_full_to_signatures() {
        let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Throttle);
        assert_eq!(mode, "signatures");
        assert!(degraded);
    }

    #[test]
    fn verdict_throttle_degrades_map_to_signatures() {
        let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Throttle);
        assert_eq!(mode, "signatures");
        assert!(degraded);
    }

    #[test]
    fn verdict_throttle_keeps_lines() {
        let (mode, degraded) = super::apply_verdict("lines:1-50", DegradationVerdictV1::Throttle);
        assert_eq!(mode, "lines:1-50");
        assert!(!degraded, "lines mode bypasses degradation");
    }

    #[test]
    fn verdict_block_degrades_full_to_signatures() {
        let (mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Block);
        assert_eq!(mode, "signatures");
        assert!(degraded);
    }

    #[test]
    fn verdict_block_does_not_degrade_signatures() {
        let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Block);
        assert_eq!(mode, "signatures");
        assert!(!degraded, "already at signatures — no degradation needed");
    }

    #[test]
    fn degrade_warning_message_contains_mode_info() {
        let (new_mode, degraded) = super::apply_verdict("full", DegradationVerdictV1::Warn);
        assert!(degraded);
        let warning = format!(
            "⚠ Context pressure: mode=full was downgraded to mode={new_mode} (verdict: {:?}).",
            DegradationVerdictV1::Warn
        );
        assert!(warning.contains("mode=full"));
        assert!(warning.contains("mode=map"));
        assert!(warning.contains("Warn"));
    }

    // --- auto_degrade_read_mode: no_degrade integration ---
    // With default config (no LCTX_NO_DEGRADE), the profile's degradation.enforce
    // is also off by default, so auto_degrade_read_mode returns mode unchanged.

    #[test]
    fn auto_degrade_preserves_full_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("full");
        assert_eq!(mode, "full");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_map_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("map");
        assert_eq!(mode, "map");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_signatures_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("signatures");
        assert_eq!(mode, "signatures");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_diff_always() {
        let (mode, warning) = super::auto_degrade_read_mode("diff");
        assert_eq!(mode, "diff");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_lines_mode_always() {
        let (mode, warning) = super::auto_degrade_read_mode("lines:10-50");
        assert_eq!(mode, "lines:10-50");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_aggressive_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("aggressive");
        assert_eq!(mode, "aggressive");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_entropy_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("entropy");
        assert_eq!(mode, "entropy");
        assert!(warning.is_none());
    }

    #[test]
    fn auto_degrade_preserves_auto_when_default_config() {
        if std::env::var("LCTX_NO_DEGRADE").is_ok() {
            return;
        }
        let (mode, warning) = super::auto_degrade_read_mode("auto");
        assert_eq!(mode, "auto");
        assert!(warning.is_none());
    }

    // --- apply_verdict: exhaustive mode × verdict matrix ---

    #[test]
    fn verdict_warn_does_not_degrade_diff() {
        let (mode, degraded) = super::apply_verdict("diff", DegradationVerdictV1::Warn);
        assert_eq!(mode, "diff");
        assert!(!degraded);
    }

    #[test]
    fn verdict_throttle_does_not_degrade_signatures() {
        let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Throttle);
        assert_eq!(mode, "signatures");
        assert!(!degraded);
    }

    #[test]
    fn verdict_ok_preserves_map() {
        let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Ok);
        assert_eq!(mode, "map");
        assert!(!degraded);
    }

    #[test]
    fn verdict_ok_preserves_signatures() {
        let (mode, degraded) = super::apply_verdict("signatures", DegradationVerdictV1::Ok);
        assert_eq!(mode, "signatures");
        assert!(!degraded);
    }

    #[test]
    fn verdict_ok_preserves_lines() {
        let (mode, degraded) = super::apply_verdict("lines:1-100", DegradationVerdictV1::Ok);
        assert_eq!(mode, "lines:1-100");
        assert!(!degraded);
    }

    #[test]
    fn verdict_block_degrades_map_to_signatures() {
        let (mode, degraded) = super::apply_verdict("map", DegradationVerdictV1::Block);
        assert_eq!(mode, "signatures");
        assert!(degraded);
    }
}
