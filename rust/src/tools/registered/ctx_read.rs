use std::sync::{Arc, RwLock};
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, require_resolved_path};
use crate::tool_defs::tool_def;
use crate::tools::ctx_read::{LineRange, ReadMode, ReadOutput};

/// Per-file lock that serializes concurrent reads of the same path.
///
/// Multiple readers proceed in parallel (`RwLock` read-lock). Edits acquire a
/// write-lock for exclusive access. Backed by the shared `core::path_locks`
/// registry so reads and edits of the same path coordinate (see issue #320).
fn per_file_lock(path: &str) -> Arc<RwLock<()>> {
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
            "Read source files. mode defaults to \"signatures\".\n\
             WORKFLOW: after ctx_compose identified relevant files.\n\
             ANTIPATTERN: not for understanding code — use ctx_compose FIRST (saves tokens).\n\
             full=verbatim(edit-ready), signatures=API(default), map=structure, diff=git-delta.\n\
             Use range.offset/range.limit for partial reads in full mode only.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path" },
                    "mode": {
                        "type": "string",
                        "description": "full=verbatim(edit-ready) signatures=API(default) map=structure diff=git-delta",
                        "default": "signatures"
                    },
                    "range": {
                        "type": "object",
                        "description": "Line range (only for full mode).",
                        "properties": {
                            "offset": { "type": "integer", "description": "1-based first line" },
                            "limit": { "type": "integer", "description": "Max lines" }
                        }
                    }
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

        // ── 1. Read current task from session ──
        let current_task = {
            let rt = tokio::runtime::Handle::current();
            let mut attempt = 0u32;
            loop {
                if let Ok(session) = rt.block_on(tokio::time::timeout(
                    Duration::from_secs(5),
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
                std::thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
            }
        };
        let task_ref = current_task.as_deref();

        // ── 2. Parse mode string from args ──
        let mode_str = get_str(args, "mode").unwrap_or_else(|| "signatures".to_string());

        // ── 3. Context gate (pre-dispatch, no cache dependency) ──
        let pressure_action = ctx.pressure_snapshot.as_ref().map(|p| &p.recommendation);
        let resolved_agent_id = ctx.agent_id.as_ref().and_then(|a| match a.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        });
        let gate_result = crate::server::context_gate::pre_dispatch_read_for_agent(
            path,
            &mode_str,
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

        let mut effective = gate_result.overridden_mode.unwrap_or(mode_str);

        // Instruction files always return full content
        if crate::tools::ctx_read::is_instruction_file(path) {
            effective = "full".to_string();
        }

        // ── 4. Auto-degrade based on context pressure ──
        let (final_mode, degrade_warning) = if crate::tools::ctx_read::is_instruction_file(path) {
            ("full".to_string(), None)
        } else {
            auto_degrade_read_mode(&effective)
        };

        // ── 5. Pre-read validation (binary, size) ──
        if crate::core::binary_detect::is_binary_file(path) {
            let msg = crate::core::binary_detect::binary_file_message(path);
            return Err(ErrorData::invalid_params(msg, None));
        }
        {
            let cap = crate::core::limits::max_read_bytes();
            if let Ok(meta) = std::fs::metadata(path)
                && meta.len() > cap as u64
            {
                let msg = format!(
                    "File too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
                     Use offset=1, limit=100 for partial reads or increase the limit.",
                    meta.len(),
                    cap
                );
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

        // ── 6. Parse into ReadMode (validate once at the boundary) ──
        let (offset, limit) = extract_range(args);
        let read_mode = parse_read_mode(&final_mode, offset, limit);

        // ── 7. Read file via pure ctx_read::read() with per_file_lock ──
        let crp_mode = ctx.crp_mode;
        let read_timeout = Duration::from_secs(30);
        let path_owned = path.to_string();
        let task_for_thread = current_task.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);

        std::thread::spawn(move || {
            let lock = per_file_lock(&path_owned);
            let _guard = lock.read().unwrap();
            let task_ref = task_for_thread.as_deref();
            let result = crate::tools::ctx_read::read(&path_owned, &read_mode, crp_mode, task_ref);
            let _ = tx.send(result);
        });

        let read_output: ReadOutput = match rx.recv_timeout(read_timeout) {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(ErrorData::invalid_params(e.to_string(), None)),
            Err(_) => {
                tracing::error!("ctx_read timed out after {read_timeout:?} for {path}");
                return Err(ErrorData::internal_error(
                    format!(
                        "ERROR: ctx_read timed out after {}s reading {path}. \
                     The file may be very large or a blocking I/O issue occurred. \
                     Try offset=1, limit=100 for a partial read.",
                        read_timeout.as_secs()
                    ),
                    None,
                ));
            }
        };

        let ReadOutput {
            content,
            mode: resolved_mode,
            original_tokens: original,
            output_tokens,
        } = read_output;

        let resolved_mode_label = resolved_mode.label().to_string();
        let saved = original.saturating_sub(output_tokens);

        // ── 8. Session updates (bounded write-lock) ──
        let mut ensured_root: Option<String> = None;
        let mut traversal_working_set: Vec<String> = Vec::new();
        let project_root_snapshot;
        {
            let rt = tokio::runtime::Handle::current();
            let session_guard = rt.block_on(tokio::time::timeout(
                Duration::from_secs(10),
                session_lock.write(),
            ));
            if let Ok(mut session) = session_guard {
                session.touch_file(path, None, &resolved_mode_label, original);
                traversal_working_set =
                    crate::core::tool_lifecycle::recent_working_set(&session, path);
                let file_summary = extract_file_summary(&content, path);
                if !file_summary.is_empty() {
                    session.set_file_summary(path, &file_summary);
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

        // ── 9. Telemetry + learning (background, no cache stats) ──
        {
            let path_bg = path.to_string();
            let resolved_mode_bg = resolved_mode_label.clone();
            let project_root_bg = project_root_snapshot.clone();
            std::thread::spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    crate::core::heatmap::record_file_access(&path_bg, original, saved);

                    {
                        use crate::core::savings_ledger as ledger;
                        ledger::record_read_event(original, saved);
                    }

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
                        total_turns: 0u32,
                        tokens_saved: saved as u64,
                        tokens_original: original as u64,
                        cache_hits: 0u32,
                        total_reads: 0u32,
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

        // ── 10. Cross-source hints ──
        let hints_suffix = {
            let graph_db =
                crate::core::property_graph::graph_dir(&ctx.project_root).join("graph.db");
            let edges = if graph_db.exists() {
                crate::core::property_graph::CodeGraph::open(&ctx.project_root)
                    .map(|g| g.all_cross_source_edges())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if edges.is_empty() {
                String::new()
            } else {
                let hints = crate::core::cross_source_hints::hints_for_file(
                    path,
                    &edges,
                    &ctx.project_root,
                );
                crate::core::cross_source_hints::format_hints(&hints)
            }
        };

        // ── 11. Build output with warnings ──
        let mut warnings = Vec::new();
        if let Some(ref w) = budget_warning {
            warnings.push(w.as_str());
        }
        if let Some(ref w) = degrade_warning {
            warnings.push(w.as_str());
        }
        let final_output = if !warnings.is_empty() {
            format!("{content}{hints_suffix}\n\n{}", warnings.join("\n"))
        } else if hints_suffix.is_empty() {
            content
        } else {
            format!("{content}{hints_suffix}")
        };

        Ok(ToolOutput {
            text: final_output,
            original_tokens: original,
            saved_tokens: saved,
            mode: Some(resolved_mode_label),
            path: Some(path.to_string()),
            changed: false,
            shell_outcome: None,
        })
    }
}

// ── Mode parsing helpers ──

/// Extract line range from `range` object in args.
fn extract_range(args: &Map<String, Value>) -> (Option<i64>, Option<i64>) {
    let range_val = args.get("range").and_then(|v| v.as_object());
    let offset = range_val.and_then(|r| r.get("offset").and_then(serde_json::Value::as_i64));
    let limit = range_val.and_then(|r| r.get("limit").and_then(serde_json::Value::as_i64));
    (offset, limit)
}

/// Parse a mode string + optional offset/limit into a [`ReadMode`].
///
/// Validated once at the MCP boundary; the returned [`ReadMode`] is guaranteed
/// valid and needs no re-checking downstream.
fn parse_read_mode(mode: &str, offset: Option<i64>, limit: Option<i64>) -> ReadMode {
    match mode {
        "full" => ReadMode::Full(parse_range(offset, limit)),
        "signatures" => ReadMode::Signatures,
        "map" => ReadMode::Map,
        "diff" => ReadMode::Diff,
        other => {
            tracing::debug!("unknown read mode '{other}', defaulting to signatures");
            ReadMode::Signatures
        }
    }
}

/// Convert optional offset/limit to an optional 1-based [`LineRange`].
fn parse_range(offset: Option<i64>, limit: Option<i64>) -> Option<LineRange> {
    match offset {
        Some(off) if off > 0 => {
            let start = off as usize;
            let end = limit.map_or(usize::MAX, |l| {
                start.saturating_add(l as usize).saturating_sub(1)
            });
            Some(LineRange::new(start, end))
        }
        _ => limit
            .filter(|&l| l > 0)
            .map(|l| LineRange::new(1, l as usize)),
    }
}

// ── Existing helpers (unchanged) ──

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
             (verdict: {:?}). Use fresh=true to bypass, or run ctx_compress to free budget.",
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
    use std::sync::atomic::Ordering;

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
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let path = "/tmp/test_concurrent_serialization.txt";
        let mut handles = Vec::new();

        for _ in 0..5 {
            let counter = counter.clone();
            let max_concurrent = max_concurrent.clone();
            let path = path.to_string();
            handles.push(std::thread::spawn(move || {
                let lock = per_file_lock(&path);
                let _guard = lock.write().unwrap();
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
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0..4 {
            let counter = counter.clone();
            let max_concurrent = max_concurrent.clone();
            let path = format!("/tmp/test_parallel_{i}.txt");
            handles.push(std::thread::spawn(move || {
                let lock = per_file_lock(&path);
                let _guard = lock.write().unwrap();
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
