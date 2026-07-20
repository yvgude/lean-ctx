//! Shared tool lifecycle — ensures CLI and MCP paths have identical side effects.
//!
//! The MCP server dispatcher handles session, ledger, heatmap, intent detection,
//! and knowledge consolidation inline (via in-memory state). When the daemon is
//! unavailable, CLI commands call functions here to achieve the same coverage by
//! loading/saving state from disk.
//!
//! NOTE: When the daemon IS running, CLI routes through `daemon_client` which
//! calls the MCP server — these functions are NOT called in that path.

use crate::core::context_ir::{ContextIrSourceKindV1, ContextIrV1, RecordIrInput};
use crate::core::context_ledger::ContextLedger;
use crate::core::heatmap;
use crate::core::intent_engine::StructuredIntent;
use crate::core::ocla::EfficiencyAnalyzer;
use crate::core::session::SessionState;
use crate::core::stats;

/// How many recently-touched files form the "working set" a new read is
/// associated with for traversal (co-access) edges (#289). Small, so the signal
/// stays local to what the agent is actively juggling.
const TRAVERSAL_WINDOW: usize = 6;

/// Recent distinct file paths (excluding `current`), most-recent first, capped
/// to the traversal window — the working set a new read co-occurs with.
pub(crate) fn recent_working_set(session: &SessionState, current: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in session.files_touched.iter().rev() {
        if f.path == current || out.contains(&f.path) {
            continue;
        }
        out.push(f.path.clone());
        if out.len() >= TRAVERSAL_WINDOW {
            break;
        }
    }
    out
}

/// Whether `root` is a usable project root for repo-relative normalization.
pub(crate) fn usable_root(root: Option<&str>) -> Option<&str> {
    root.filter(|r| !r.trim().is_empty() && *r != ".")
}

/// First 200 chars of `text` on a UTF-8 boundary — the exact excerpt bound the
/// MCP dispatcher applies before handing content to the IR store
/// (`server/call_tool.rs`), kept identical so CLI- and MCP-recorded IR items are
/// byte-compatible. `ContextIrV1::record` redacts and further caps it.
fn ir_excerpt(text: &str) -> &str {
    const MAX: usize = 200;
    if text.len() <= MAX {
        return text;
    }
    let mut end = MAX;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Record a file-read operation with full Context OS side effects.
///
/// `duration` and `output_excerpt` feed the Context IR lineage (#566); the MCP
/// dispatcher records both for every tool call but the shadow-mode `lean-ctx
/// read` subprocess used to drop them, so IR/`ctx_proof` exports were blind to
/// compressed shadow reads.
pub fn record_file_read(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    is_cache_hit: bool,
    duration: std::time::Duration,
    output_excerpt: &str,
) {
    let saved = original_tokens.saturating_sub(output_tokens);
    let tool_key = format!("cli_{mode}");

    stats::record(&tool_key, original_tokens, output_tokens);
    heatmap::record_file_access(path, original_tokens, saved);
    // Verified ledger (#685): recorded explicitly now that the heatmap chokepoint
    // no longer bundles it. This direct-CLI path (daemon off) only has o200k
    // counts; the model-correct re-tokenization happens on the MCP read path,
    // which holds the source text. For the default O200kBase model these are
    // identical anyway.
    crate::core::savings_ledger::record_read_event(original_tokens, saved);

    // Project root the learning sinks below are scoped to. Defaults to "." (the
    // MCP path's `project_root_snapshot` fallback) so a rootless read still
    // trains a global model rather than being dropped.
    let mut learning_root = String::from(".");

    if let Some(mut session) = SessionState::load_latest() {
        session.touch_file(path, None, mode, original_tokens);
        if is_cache_hit {
            session.record_cache_hit();
        }

        if session.active_structured_intent.is_none() && session.files_touched.len() >= 2 {
            let touched: Vec<String> = session
                .files_touched
                .iter()
                .map(|ft| ft.path.clone())
                .collect();
            let inferred = StructuredIntent::from_file_patterns(&touched);
            if inferred.confidence >= 0.4 {
                session.active_structured_intent = Some(inferred);
            }
        }

        let project_root = session.project_root.clone();
        if let Some(root) = usable_root(project_root.as_deref()) {
            learning_root = root.to_string();
        }
        let calls = session.stats.total_tool_calls;

        // Traversal edges: associate this read with the recent working set so the
        // graph learns the files this task actually touches together (#289).
        let working_set = recent_working_set(&session, path);

        let _ = session.save();

        if let Some(root) = usable_root(project_root.as_deref()) {
            crate::core::cooccurrence::record_focus_access(root, path, &working_set);
        }
        maybe_consolidate(project_root.as_deref(), calls);
    }

    // Only real files belong in the context ledger (GL #512): directory
    // overviews and synthetic paths would show up as "files" in the pressure
    // table with eviction/pin semantics that make no sense for them.
    if std::path::Path::new(path).is_file() {
        let mut ledger = ContextLedger::load();
        ledger.record(path, mode, original_tokens, output_tokens);
        ledger.save();
    }

    // Learning sinks the MCP read path runs in a background thread but the CLI
    // path historically skipped — the mode predictor never trained, the
    // compression feedback loop stayed blind and dashboard anomaly signals were
    // missing for every shadow-mode (`view`/`grep` → `lean-ctx read`) hook read
    // (#550). Run inline: a single-shot CLI process must finish them before it
    // flushes and exits, so the off-hot-path thread the daemon uses is moot here.
    record_read_learning(
        path,
        mode,
        original_tokens,
        output_tokens,
        is_cache_hit,
        &learning_root,
    );

    // Context IR lineage (#566): the MCP dispatcher records provenance for every
    // tool call (`server/call_tool.rs`), but the shadow-mode hook's single-shot
    // `lean-ctx read` bypassed it. Disk-backed load→record→save persists the
    // entry before the process exits. `mode` rides the IR `pattern` slot to match
    // the MCP read path (which stores its `mode` arg there).
    let mut ir = ContextIrV1::load();
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Read,
        tool: "ctx_read",
        client_name: None,
        agent_id: None,
        path: Some(path),
        command: None,
        pattern: Some(mode),
        input_tokens: original_tokens,
        output_tokens,
        duration,
        content_excerpt: ir_excerpt(output_excerpt),
    });
    ir.save();
}

/// Replicate the MCP read path's learning side effects (`registered/ctx_read.rs`
/// background thread) for the standalone CLI path (#550): mode-predictor
/// training, the compression feedback outcome and the per-call anomaly metric.
/// All three are disk-backed and therefore work from a single-shot process; the
/// in-memory-only detectors (loop/correction) and the bounce/adaptive signals
/// that require routing through `ctx_read::handle` are tracked separately.
fn record_read_learning(
    path: &str,
    resolved_mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    is_cache_hit: bool,
    project_root: &str,
) {
    let task_completed = crate::core::bounce_tracker::global()
        .lock()
        .ok()
        .and_then(|bt| bt.bounce_rate_for_extension(path))
        .is_none_or(|rate| rate < 0.30);

    // Route the realized read through the OCLA efficiency capability so the
    // production CLI path records the same ETPAO semantics as the contract.
    // The analyzer is local and deterministic; failure keeps the legacy ratio.
    let ocla_density = ocla_read_density(
        path,
        resolved_mode,
        original_tokens,
        output_tokens,
        task_completed,
        project_root,
    );

    // Mode predictor: train auto-mode selection on the realized compression
    // density, exactly as the MCP background thread does.
    let sig = crate::core::mode_predictor::FileSignature::from_path(path, original_tokens);
    let density = ocla_density.unwrap_or_else(|| {
        if output_tokens > 0 {
            original_tokens as f64 / output_tokens as f64
        } else {
            1.0
        }
    });
    let outcome = crate::core::mode_predictor::ModeOutcome {
        mode: resolved_mode.to_string(),
        tokens_in: original_tokens,
        tokens_out: output_tokens,
        density: density.min(1.0),
    };
    let mut predictor = crate::core::mode_predictor::ModePredictor::new();
    predictor.set_project_root(project_root);
    predictor.record(sig, outcome);
    predictor.save();

    // Compression feedback: the per-language outcome the adaptive thresholds and
    // bounce-aware tuning learn from. `total_turns`/`total_reads` are 1 — the
    // accurate count for this single-shot invocation, not a placeholder.
    let saved = original_tokens.saturating_sub(output_tokens);
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    let thresholds = crate::core::adaptive_thresholds::thresholds_for_path(path);
    let feedback_outcome = crate::core::feedback::CompressionOutcome {
        session_id: format!("{}", std::process::id()),
        language: ext,
        entropy_threshold: thresholds.bpe_entropy,
        jaccard_threshold: thresholds.jaccard,
        total_turns: 1,
        tokens_saved: saved as u64,
        tokens_original: original_tokens as u64,
        cache_hits: u32::from(is_cache_hit),
        total_reads: 1,
        // A compressed read only counts as task-completing when this extension
        // is not in a high-bounce state (#593); unknown stays optimistic so the
        // cold start matches the MCP path. 0.30 mirrors BOUNCE_RATE_THRESHOLD.
        task_completed,
        timestamp: chrono::Local::now().to_rfc3339(),
    };
    let mut store = crate::core::feedback::FeedbackStore::load();
    store.project_root = Some(project_root.to_string());
    store.record_outcome(feedback_outcome);

    // Anomaly detector: the same per-call metric the MCP post-dispatch records.
    // `save_debounced` writes on the first call of a fresh process (last-save
    // marker starts at 0), so the single shadow read persists before exit.
    crate::core::anomaly::record_metric("tokens_per_call", output_tokens as f64);
    crate::core::anomaly::save_debounced();
}

/// Compute read density through the production OCLA efficiency capability.
/// Returns `None` when no accepted outcome can produce an ETPAO value.
fn ocla_read_density(
    path: &str,
    resolved_mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    task_completed: bool,
    project_root: &str,
) -> Option<f64> {
    let analyzer = crate::core::ocla::OclaRegistry::global()
        .efficiency_analyzer
        .as_ref();
    read_density_with_analyzer(
        analyzer,
        path,
        resolved_mode,
        original_tokens,
        output_tokens,
        task_completed,
        project_root,
    )
}

fn read_density_with_analyzer(
    analyzer: &dyn EfficiencyAnalyzer,
    path: &str,
    resolved_mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    task_completed: bool,
    project_root: &str,
) -> Option<f64> {
    analyzer
        .analyze_efficiency(crate::core::ocla::EfficiencySample {
            context: crate::core::ocla::OclaRequestContext {
                request_id: format!("read:{path}:{resolved_mode}"),
                session_id: project_root.to_string(),
                agent_id: "lean-ctx".to_string(),
                content_ref: path.to_string(),
                tenant_id: None,
            },
            original_tokens: original_tokens as u64,
            delivered_tokens: output_tokens as u64,
            accepted: Some(task_completed),
        })
        .ok()
        .and_then(|analysis| analysis.etpao_milli)
        .map(|milli| milli as f64 / 1000.0)
}

/// Record a search/grep operation with full Context OS side effects.
///
/// `modeled_baseline` (native-tool estimate, GL #479 D1) feeds the estimated
/// stats series; `observed_tokens` (raw measured match lines, no factor) feeds
/// the verified ledger (GL #479 D2). `pattern`/`path`/`duration`/`output_excerpt`
/// feed the Context IR lineage (#566).
pub fn record_search(
    modeled_baseline: usize,
    observed_tokens: usize,
    output_tokens: usize,
    pattern: &str,
    path: &str,
    duration: std::time::Duration,
    output_excerpt: &str,
) {
    stats::record("cli_grep", modeled_baseline, output_tokens);
    crate::core::savings_ledger::record_tool_event("cli_grep", observed_tokens, output_tokens);

    if let Some(mut session) = SessionState::load_latest() {
        session.record_command();
        let project_root = session.project_root.clone();
        let calls = session.stats.total_tool_calls;
        let _ = session.save();

        maybe_consolidate(project_root.as_deref(), calls);
    }

    // Per-call anomaly metric, mirroring the MCP post-dispatch (#550). Missing it
    // left dashboard signals blind to shadow-mode (`grep` → `lean-ctx grep`) hooks.
    crate::core::anomaly::record_metric("tokens_per_call", output_tokens as f64);
    crate::core::anomaly::save_debounced();

    // Context IR lineage for shadow-mode `grep` → `lean-ctx grep` (#566). The
    // raw matched-line estimate (`observed_tokens`) is the IR input so the stored
    // compression ratio reads matches-in / sent-out.
    let mut ir = ContextIrV1::load();
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Search,
        tool: "ctx_search",
        client_name: None,
        agent_id: None,
        path: Some(path),
        command: None,
        pattern: Some(pattern),
        input_tokens: observed_tokens,
        output_tokens,
        duration,
        content_excerpt: ir_excerpt(output_excerpt),
    });
    ir.save();
}

/// Record a tree/ls operation with full Context OS side effects.
pub fn record_tree(original_tokens: usize, output_tokens: usize) {
    stats::record("cli_ls", original_tokens, output_tokens);

    if let Some(mut session) = SessionState::load_latest() {
        session.record_command();
        let _ = session.save();
    }
}

/// Record a shell command with full Context OS side effects.
/// Always records in stats (even for track-only 0-token calls) so the dashboard
/// command counter stays accurate. Adding 0 tokens does not inflate savings.
pub fn record_shell_command(original_tokens: usize, output_tokens: usize) {
    stats::record("cli_shell", original_tokens, output_tokens);
    // Shell compression is *measured* (raw output vs sent output), so it belongs
    // in the verified ledger too (GL #479 D2). Zero-saving calls are skipped.
    crate::core::savings_ledger::record_tool_event("cli_shell", original_tokens, output_tokens);

    if let Some(mut session) = SessionState::load_latest() {
        session.record_command();
        let project_root = session.project_root.clone();
        let calls = session.stats.total_tool_calls;
        let _ = session.save();

        if original_tokens > 0 {
            maybe_consolidate(project_root.as_deref(), calls);
        }
    }
}

/// Flush every buffered telemetry sink to disk.
///
/// The long-lived MCP daemon flushes these once at shutdown
/// (`cli/dispatch/server.rs`). Single-shot CLI commands — and the shadow-mode
/// hook subprocesses that spawn `lean-ctx read`/`grep` — exit immediately, so
/// without this the buffered heatmap, mode-predictor, feedback and threshold
/// writes are silently lost the moment the process ends: `lean-ctx heatmap`
/// stays empty and `lean-ctx gain` reports nothing for compressed reads (#550).
///
/// Centralized so the daemon shutdown, the parent watchdog and every CLI tool
/// command flush the *exact same* set — the historical per-arm copies had
/// drifted (the `read` arm flushed only `stats`, the `-c` arm four sinks, the
/// daemon nine), which is precisely how the gap went unnoticed.
pub fn flush_all() {
    stats::flush();
    heatmap::flush();
    crate::core::path_mode_memory::flush();
    crate::core::grammar_usage::flush();
    crate::core::auto_mode_resolver::flush_sources();
    crate::core::edit_quality::flush();
    crate::core::edit_metering::flush();
    crate::core::mode_predictor::ModePredictor::flush();
    crate::core::feedback::FeedbackStore::flush();
    crate::core::threshold_learning::flush();
    crate::core::litm_calibration::flush();
}

// TODO(arch): crate::tools::autonomy is still referenced here. Move AutonomyState
// and should_auto_consolidate to core::autonomy_drivers for a clean layer boundary.
fn maybe_consolidate(project_root: Option<&str>, calls: u32) {
    let Some(root) = project_root else { return };
    let autonomy = crate::tools::autonomy::AutonomyState::new();
    if crate::tools::autonomy::should_auto_consolidate(&autonomy, calls) {
        let root = root.to_string();
        let _ = crate::core::consolidation_engine::consolidate_latest(
            &root,
            crate::core::consolidation_engine::ConsolidationBudgets::default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SpyAnalyzer {
        calls: AtomicUsize,
    }

    impl crate::core::ocla::OclaService for SpyAnalyzer {
        fn capability(&self) -> crate::core::ocla::OclaCapability {
            crate::core::ocla::OclaCapability::available(
                crate::core::ocla::OclaCapabilityKind::EfficiencyAnalyzer,
            )
        }
    }

    impl EfficiencyAnalyzer for SpyAnalyzer {
        fn analyze_efficiency(
            &self,
            sample: crate::core::ocla::EfficiencySample,
        ) -> crate::core::ocla::OclaResult<crate::core::ocla::EfficiencyAnalysis> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(sample.original_tokens, 1000);
            assert_eq!(sample.delivered_tokens, 375);
            Ok(crate::core::ocla::EfficiencyAnalysis {
                etpao_milli: sample.accepted.map(|_| 375),
                duplicate_ratio_milli: 625,
                recommendation_refs: Vec::new(),
            })
        }
    }

    // The record_* paths now drive process-global telemetry sinks (mode
    // predictor buffer, anomaly singleton) and read the data-dir env (#550), so
    // every test here takes the shared isolation lock to serialize that state
    // and keep its disk writes inside a throwaway dir.

    #[test]
    fn ocla_read_density_uses_etpao_for_accepted_reads() {
        assert_eq!(
            ocla_read_density("src/main.rs", "aggressive", 1000, 250, true, "."),
            Some(0.25)
        );
        assert_eq!(
            ocla_read_density("src/main.rs", "aggressive", 1000, 250, false, "."),
            None
        );
    }

    #[test]
    fn ocla_read_density_accepts_injected_analyzer() {
        let spy = SpyAnalyzer {
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            read_density_with_analyzer(&spy, "src/main.rs", "full", 1000, 375, true, "."),
            Some(0.375)
        );
        assert_eq!(spy.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn record_file_read_does_not_panic_without_session() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        record_file_read(
            "/tmp/nonexistent.rs",
            "full",
            100,
            50,
            false,
            std::time::Duration::from_millis(1),
            "excerpt",
        );
    }

    #[test]
    fn record_search_does_not_panic_without_session() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        record_search(
            500,
            200,
            150,
            "pattern",
            "/tmp",
            std::time::Duration::from_millis(1),
            "matches",
        );
    }

    #[test]
    fn record_tree_does_not_panic_without_session() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        record_tree(100, 80);
    }

    #[test]
    fn record_shell_does_not_panic_without_session() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        record_shell_command(500, 200);
    }

    #[test]
    fn flush_all_is_idempotent_and_safe_without_state() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        // Empty buffers: flushing must be a harmless no-op, and calling it twice
        // (e.g. a CLI arm followed by an atexit path) must never panic.
        flush_all();
        flush_all();
    }

    #[test]
    fn cli_read_persists_learning_sinks_to_disk() {
        // #550 regression: a single-shot CLI read must leave the mode predictor,
        // compression feedback and heatmap on disk. The daemon used to be the
        // only path that flushed them, so shadow-mode hook reads (`view`/`grep` →
        // `lean-ctx read`) recorded nothing and `lean-ctx heatmap` stayed empty.
        let dir = crate::core::data_dir::isolated_data_dir();
        let file = dir.path().join("sample.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        let path = file.to_string_lossy();

        record_file_read(
            &path,
            "full",
            1000,
            200,
            false,
            std::time::Duration::from_millis(2),
            "sample.rs [3L]\nfn main() {}",
        );
        flush_all();

        let data = crate::core::data_dir::lean_ctx_data_dir().expect("data dir");
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(
            data.join("mode_stats.json").exists(),
            "mode predictor must persist after a CLI read + flush"
        );
        assert!(
            state.join("feedback.json").exists(),
            "compression feedback must persist after a CLI read + flush"
        );
        assert!(
            state.join("heatmap.json").exists(),
            "heatmap must persist after a CLI read + flush"
        );
    }

    #[test]
    fn cli_read_records_context_ir_lineage() {
        // #566: the MCP dispatcher records Context IR for every tool call, but the
        // shadow-mode `lean-ctx read` subprocess used to skip it, so IR/ctx_proof
        // exports were blind to compressed shadow reads. A single-shot CLI read
        // must now persist exactly one IR item (disk-backed load→record→save).
        let dir = crate::core::data_dir::isolated_data_dir();
        let file = dir.path().join("ir_sample.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let path = file.to_string_lossy();

        record_file_read(
            &path,
            "full",
            1000,
            200,
            false,
            std::time::Duration::from_millis(3),
            "ir_sample.rs [1L]\nfn main() {}",
        );

        let ir = ContextIrV1::load();
        assert_eq!(ir.items.len(), 1, "exactly one IR item per CLI read");
        let item = &ir.items[0];
        assert_eq!(item.source.tool, "ctx_read");
        assert!(matches!(item.source.kind, ContextIrSourceKindV1::Read));
        assert!(
            item.source
                .path
                .as_deref()
                .unwrap_or("")
                .ends_with("ir_sample.rs"),
            "IR records the read path, got {:?}",
            item.source.path
        );
        assert_eq!(item.source.pattern.as_deref(), Some("full"));
        assert_eq!(item.input_tokens, 1000);
        assert_eq!(item.output_tokens, 200);
        assert!(item.duration_us > 0, "a real duration must be recorded");
        assert!(!item.content_excerpt.is_empty(), "excerpt must be captured");
    }

    #[test]
    fn cli_search_records_context_ir_lineage() {
        // #566: the shadow-mode `grep` → `lean-ctx grep` path records IR too.
        let _dir = crate::core::data_dir::isolated_data_dir();

        record_search(
            800,
            500,
            120,
            "fn handle",
            "src/",
            std::time::Duration::from_millis(4),
            "src/lib.rs:12: fn handle() {}",
        );

        let ir = ContextIrV1::load();
        assert_eq!(ir.items.len(), 1, "exactly one IR item per CLI search");
        let item = &ir.items[0];
        assert_eq!(item.source.tool, "ctx_search");
        assert!(matches!(item.source.kind, ContextIrSourceKindV1::Search));
        // Input is the raw matched-line estimate, not the modeled baseline.
        assert_eq!(item.input_tokens, 500);
        assert_eq!(item.output_tokens, 120);
        assert!(item.duration_us > 0, "a real duration must be recorded");
    }
}
