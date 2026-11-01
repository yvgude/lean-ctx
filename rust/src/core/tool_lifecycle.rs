// SPDX-License-Identifier: Apache-2.0

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
use crate::core::session::SessionState;
use crate::core::stats;
use std::sync::atomic::{AtomicU32, Ordering};

/// How many recently-touched files form the "working set" a new read is
/// associated with for traversal (co-access) edges (#289). Small, so the signal
/// stays local to what the agent is actively juggling.
const PERIODIC_FLUSH_INTERVAL: u32 = 50;
static TOOL_CALL_COUNT: AtomicU32 = AtomicU32::new(0);

/// Whether `root` is a usable project root for repo-relative normalization.
pub(crate) fn usable_root(root: Option<&str>) -> Option<&str> {
    root.filter(|r| !r.trim().is_empty() && *r != ".")
}

/// First 200 chars of `text` on a UTF-8 boundary — the exact excerpt bound the
/// MCP dispatcher applies before handing content to the IR store
/// (`server/call_tool.rs`), kept identical so CLI- and MCP-recorded IR items are
/// byte-compatible. `ContextIrV1::record` redacts and further caps it.
pub(crate) fn ir_excerpt(text: &str) -> &str {
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
pub(crate) fn record_file_read(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    is_cache_hit: bool,
    duration: std::time::Duration,
    output_excerpt: &str,
) {
    record_file_read_accounting(
        path,
        mode,
        original_tokens,
        output_tokens,
        is_cache_hit,
        duration,
    );
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
    record_file_read_projection(path, mode, original_tokens, output_tokens);
    if mode == "aggressive" && original_tokens > output_tokens {
        maybe_periodic_flush();
    }
    maybe_periodic_flush();
}

/// Compatibility accounting without IR persistence or an independent flush.
/// The CLI lifecycle calls this once in its accounting stage.
pub(crate) fn record_file_read_accounting(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    is_cache_hit: bool,
    duration: std::time::Duration,
) {
    let saved = original_tokens.saturating_sub(output_tokens);
    let tool_key = format!("cli_{mode}");

    stats::record(&tool_key, original_tokens, output_tokens);
    crate::core::execution_lifecycle::record_heatmap_access(path, original_tokens, saved);

    // Emit event so the live dashboard feed sees hook-intercepted reads.
    crate::core::events::emit_tool_call(
        &tool_key,
        original_tokens as u64,
        saved as u64,
        Some(mode.to_string()),
        duration.as_millis() as u64,
        Some(path.to_string()),
    );
    if is_cache_hit {
        crate::core::events::emit_cache_hit(path, saved as u64);
    }
    // Verified ledger (#685): recorded explicitly now that the heatmap chokepoint
    // no longer bundles it. This direct-CLI path (daemon off) only has o200k
    // counts; the model-correct re-tokenization happens on the MCP read path,
    // which holds the source text. For the default O200kBase model these are
    // identical anyway.
    crate::core::savings_ledger::record_read_event(original_tokens, saved, None, None);

    if is_cache_hit {
        crate::core::telemetry::global_metrics().record_cache(true);
    }

    // Session accounting goes into the caller's own session only. A CLI or
    // hook process has none and writes none: the project's latest saved
    // session may be another agent's (I07/S03).
    let session_root = crate::core::session::caller::with_mut(|session| {
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
        session.project_root.clone()
    })
    .flatten();

    // Project root the read observation below is scoped to: the caller's
    // project, else the read file's own project. Defaults to "." (the MCP
    // path's `project_root_snapshot` fallback) so a rootless read is still
    // observed rather than dropped.
    let learning_root = usable_root(session_root.as_deref())
        .map(str::to_string)
        .or_else(|| crate::core::protocol::detect_project_root(path))
        .unwrap_or_else(|| String::from("."));

    // Only real files belong in the context ledger (GL #512): directory
    // overviews and synthetic paths would show up as "files" in the pressure
    // table with eviction/pin semantics that make no sense for them.
    if std::path::Path::new(path).is_file() {
        let mut ledger = ContextLedger::load();
        ledger.record(path, mode, original_tokens, output_tokens);
        ledger.save();
    }

    // The read observation the MCP read path records in a background thread but
    // the CLI path historically skipped for shadow-mode (`view`/`grep` →
    // `lean-ctx read`) hook reads (#550). Run inline: a single-shot CLI process
    // must finish it before it flushes and exits.
    record_read_observation(path, mode, &learning_root);
}

pub(crate) fn record_file_read_projection(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
) {
    let saved = original_tokens.saturating_sub(output_tokens);
    // OCLA CompressionProvider runtime projection: for aggressive-mode reads with
    // positive savings, record the compression event through the canonical OCLA
    // capability so the registry tracks real compression evidence.
    if saved > 0 {
        project_ocla_savings(path, original_tokens as u64, output_tokens as u64);
    }
    if mode == "aggressive" && saved > 0 {
        project_ocla_compression(path, original_tokens as u64, output_tokens as u64);
    }
}

/// Record the read as an OCLA observation for the standalone CLI path, as the
/// MCP read path does (#550). A read is observed delivery, never task
/// acceptance: the canonical execution protocol alone admits outcome-based
/// learning.
fn record_read_observation(path: &str, resolved_mode: &str, project_root: &str) {
    let _ = crate::core::ocla::OclaRegistry::global()
        .outcome_tracker
        .record_outcome(read_observation(path, resolved_mode, project_root));
}

fn read_observation(
    path: &str,
    resolved_mode: &str,
    project_root: &str,
) -> crate::core::ocla::Outcome {
    let context = crate::core::ocla::OclaRequestContext {
        request_id: format!("cli-read:{path}:{resolved_mode}"),
        session_id: caller_session_id().unwrap_or_else(|| "cli-read".to_string()),
        agent_id: "lean-ctx".to_string(),
        content_ref: format!("file:{path}"),
        tenant_id: None,
        trace_id: "tr-unit".into(),
        task_id: None,
        parent_task_id: None,
    };
    crate::core::ocla::Outcome {
        context,
        accepted: None,
        quality_score_milli: None,
        outcome_ref: Some(format!("read:{project_root}:{resolved_mode}")),
    }
}

/// Record a search/grep operation with full Context OS side effects.
///
/// `modeled_baseline` (native-tool estimate, GL #479 D1) feeds the estimated
/// stats series; `observed_tokens` (raw measured match lines, no factor) feeds
/// the verified ledger (GL #479 D2). `pattern`/`path`/`duration`/`output_excerpt`
/// feed the Context IR lineage (#566).
#[cfg(test)]
pub(crate) fn record_search(
    modeled_baseline: usize,
    observed_tokens: usize,
    output_tokens: usize,
    pattern: &str,
    path: &str,
    duration: std::time::Duration,
    output_excerpt: &str,
) {
    record_search_accounting(
        modeled_baseline,
        observed_tokens,
        output_tokens,
        path,
        duration,
    );
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

/// Existing search bookkeeping, separated from the IR writer for stage ownership.
pub(crate) fn record_search_accounting(
    modeled_baseline: usize,
    observed_tokens: usize,
    output_tokens: usize,
    path: &str,
    duration: std::time::Duration,
) {
    stats::record("cli_grep", modeled_baseline, output_tokens);

    // Emit event so the live dashboard feed sees shadow-mode search.
    let search_saved = modeled_baseline.saturating_sub(output_tokens);
    if search_saved > 0 {
        crate::core::events::emit_tool_call(
            "cli_grep",
            modeled_baseline as u64,
            search_saved as u64,
            Some("search".to_string()),
            duration.as_millis() as u64,
            Some(path.to_string()),
        );
    }
    crate::core::savings_ledger::record_tool_event(
        "cli_grep",
        observed_tokens,
        output_tokens,
        None,
        None,
    );
    crate::core::savings_tracker::record_compression(
        modeled_baseline as u64,
        output_tokens as u64,
        "cli_grep",
    );

    crate::core::session::caller::with_mut(SessionState::record_command);
}

/// Record a tree/ls operation with full Context OS side effects.
pub(crate) fn record_tree(original_tokens: usize, output_tokens: usize) {
    stats::record("cli_ls", original_tokens, output_tokens);

    // Emit event so the live dashboard feed sees shadow-mode tree/ls.
    let saved = original_tokens.saturating_sub(output_tokens);
    if saved > 0 {
        crate::core::events::emit_tool_call(
            "cli_ls",
            original_tokens as u64,
            saved as u64,
            Some("tree".to_string()),
            0,
            None,
        );
    }

    crate::core::session::caller::with_mut(SessionState::record_command);
}

/// Record a shell command with full Context OS side effects.
/// Always records in stats (even for track-only 0-token calls) so the dashboard
/// command counter stays accurate. Adding 0 tokens does not inflate savings.
pub(crate) fn record_shell_command(original_tokens: usize, output_tokens: usize) {
    record_shell_command_named("(shell)", original_tokens, output_tokens);
}

pub(crate) fn record_shell_command_named(
    command: &str,
    original_tokens: usize,
    output_tokens: usize,
) {
    stats::record("cli_shell", original_tokens, output_tokens);

    // Emit event so the live dashboard feed sees shadow-mode shell compression.
    let saved = original_tokens.saturating_sub(output_tokens);
    if saved > 0 {
        crate::core::events::emit_tool_call(
            "cli_shell",
            original_tokens as u64,
            saved as u64,
            Some("shell".to_string()),
            0,
            None,
        );
    }
    // Shell compression is *measured* (raw output vs sent output), so it belongs
    // in the verified ledger too (GL #479 D2). Zero-saving calls are skipped.
    crate::core::savings_ledger::record_tool_event(
        "cli_shell",
        original_tokens,
        output_tokens,
        None,
        None,
    );
    crate::core::savings_tracker::record_compression(
        original_tokens as u64,
        output_tokens as u64,
        "cli_shell",
    );

    if original_tokens > 0 {
        let label = shell_ledger_label(command);
        let mut ledger = crate::core::context_ledger::ContextLedger::load();
        ledger.record_shell(&label, original_tokens, output_tokens);
        ledger.save();
    }
    crate::core::session::caller::with_mut(SessionState::record_command);
}

/// Increment the tool-call counter and flush buffered telemetry every
/// [`PERIODIC_FLUSH_INTERVAL`] calls. The MCP daemon is long-lived;
/// without periodic flush, counters like `compressed_cache_hit` only
/// appear in `auto_mode_sources.json` after the process exits.
pub(crate) fn maybe_periodic_flush() {
    let count = TOOL_CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_multiple_of(PERIODIC_FLUSH_INTERVAL) {
        let _ = std::thread::spawn(flush_all);
    }
}

/// Flush every buffered telemetry sink to disk.
///
/// The long-lived MCP daemon flushes these once at shutdown
/// (`cli/dispatch/server.rs`). Single-shot CLI commands — and the shadow-mode
/// hook subprocesses that spawn `lean-ctx read`/`grep` — exit immediately, so
/// without this the buffered heatmap and threshold
/// writes are silently lost the moment the process ends: `lean-ctx heatmap`
/// stays empty and `lean-ctx gain` reports nothing for compressed reads (#550).
///
/// Centralized so the daemon shutdown, the parent watchdog and every CLI tool
/// command flush the *exact same* set — the historical per-arm copies had
/// drifted (the `read` arm flushed only `stats`, the `-c` arm four sinks, the
/// daemon nine), which is precisely how the gap went unnoticed.
pub(crate) fn flush_all() {
    stats::flush();
    heatmap::flush();
    crate::core::path_mode_memory::flush();
    crate::core::grammar_usage::flush();
    crate::core::auto_mode_resolver::flush_sources();
    crate::core::edit_quality::flush();
    crate::core::edit_metering::flush();
}

/// [`flush_all`] for a process that is about to exit: first commits the
/// provisional bounces still waiting for their edit window, so a bounce
/// detected just before shutdown reaches the ledger and the path memory that
/// `flush_all` then persists. Not part of `flush_all` itself, which also runs
/// after single tool executions where committing early would book a
/// read-before-edit as a bounce.
pub(crate) fn flush_all_at_exit() {
    crate::core::bounce_tracker::flush_pending();
    flush_all();
}

/// The caller's own session id, if this runs inside an agent's request.
fn caller_session_id() -> Option<String> {
    crate::core::session::caller::with(|session| session.id.clone())
}

/// Project an aggressive-mode compression event into the OCLA CompressionProvider.
/// Best-effort: silently drops if provider is unavailable or source_ref can't be
/// constructed. This is the canonical production callsite for the compression capability.
fn project_ocla_compression(path: &str, source_tokens: u64, output_tokens: u64) {
    use crate::core::ocla::OclaRegistry;
    use crate::core::ocla::types::{CompressionRequest, OclaRequestContext};

    let reg = OclaRegistry::global();
    let source_ref = format!("file:{path}");
    let request = CompressionRequest {
        context: OclaRequestContext {
            request_id: format!("cli-read-{}", path.len()),
            session_id: caller_session_id().unwrap_or_default(),
            agent_id: String::new(),
            content_ref: source_ref.clone(),
            tenant_id: None,
            trace_id: "tr-unit".into(),
            task_id: None,
            parent_task_id: None,
        },
        source_ref,
        source_tokens,
        target_tokens: output_tokens,
        quality_policy_ref: None,
    };
    let _ = reg.compression_provider.compress(request);
}

/// Project a caller-accounted read-savings observation into OCLA summaries.
/// Best-effort: silently drops if the provider is unavailable. Canonical
/// production callsite for the savings-evidence capability.
fn project_ocla_savings(path: &str, original_tokens: u64, output_tokens: u64) {
    use crate::core::ocla::OclaRegistry;
    use crate::core::ocla::types::{OclaRequestContext, SavingsEvidence};

    let context = OclaRequestContext {
        request_id: format!("cli-read-{}", path.len()),
        session_id: caller_session_id().unwrap_or_else(|| "cli-read".to_string()),
        agent_id: "lean-ctx".to_string(),
        content_ref: format!("file:{path}"),
        tenant_id: None,
        trace_id: String::new(),
        task_id: None,
        parent_task_id: None,
    };
    let evidence = SavingsEvidence {
        context,
        original_tokens,
        delivered_tokens: output_tokens,
        quality_ref: None,
        evidence_ref: format!("read:{path}:{original_tokens}:{output_tokens}"),
    };
    let _ = OclaRegistry::global()
        .savings_ledger
        .project_savings(evidence);
}

/// Truncate a shell command to a short label suitable for the context ledger.
fn shell_ledger_label(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or(command);
    let trimmed = first_line.trim();
    if trimmed.len() <= 80 {
        format!("$ {trimmed}")
    } else {
        format!("$ {}…", &trimmed[..trimmed.floor_char_boundary(77)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The record_* paths now drive process-global telemetry sinks (heatmap
    // buffer, anomaly singleton) and read the data-dir env (#550), so
    // every test here takes the shared isolation lock to serialize that state
    // and keep its disk writes inside a throwaway dir.

    #[test]
    fn read_observation_does_not_invent_acceptance_or_quality() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let outcome = read_observation("sample.rs", "full", "/project");
        assert_eq!(outcome.accepted, None);
        assert_eq!(outcome.quality_score_milli, None);
        assert_eq!(outcome.context.content_ref, "file:sample.rs");
        assert_eq!(outcome.outcome_ref.as_deref(), Some("read:/project:full"));
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
    fn periodic_flush_counter_increments() {
        TOOL_CALL_COUNT.store(0, Ordering::Relaxed);
        maybe_periodic_flush();
        assert_eq!(TOOL_CALL_COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cli_read_persists_observation_sinks_to_disk() {
        // #550 regression: a single-shot CLI read must leave the heatmap on
        // disk, and no retired mode-predictor store. Task acceptance cannot be
        // inferred from a read;
        // the real-process read_observation_acceptance test guards historical
        // feedback preservation and absence of fabricated completion records.
        // The daemon used to be the only path that flushed them, so hook reads
        // (`view`/`grep` →
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
            !data.join("mode_stats.json").exists(),
            "a CLI read must not train the retired mode predictor"
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
