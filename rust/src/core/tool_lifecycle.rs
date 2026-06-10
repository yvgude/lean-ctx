//! Shared tool lifecycle — ensures CLI and MCP paths have identical side effects.
//!
//! The MCP server dispatcher handles session, ledger, heatmap, intent detection,
//! and knowledge consolidation inline (via in-memory state). When the daemon is
//! unavailable, CLI commands call functions here to achieve the same coverage by
//! loading/saving state from disk.
//!
//! NOTE: When the daemon IS running, CLI routes through `daemon_client` which
//! calls the MCP server — these functions are NOT called in that path.

use crate::core::context_ledger::ContextLedger;
use crate::core::heatmap;
use crate::core::intent_engine::StructuredIntent;
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

/// Record a file-read operation with full Context OS side effects.
pub fn record_file_read(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    is_cache_hit: bool,
) {
    let saved = original_tokens.saturating_sub(output_tokens);
    let tool_key = format!("cli_{mode}");

    stats::record(&tool_key, original_tokens, output_tokens);
    heatmap::record_file_access(path, original_tokens, saved);

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
}

/// Record a search/grep operation with full Context OS side effects.
///
/// `modeled_baseline` (native-tool estimate, GL #479 D1) feeds the estimated
/// stats series; `observed_tokens` (raw measured match lines, no factor) feeds
/// the verified ledger (GL #479 D2).
pub fn record_search(modeled_baseline: usize, observed_tokens: usize, output_tokens: usize) {
    stats::record("cli_grep", modeled_baseline, output_tokens);
    crate::core::savings_ledger::record_tool_event("cli_grep", observed_tokens, output_tokens);

    if let Some(mut session) = SessionState::load_latest() {
        session.record_command();
        let project_root = session.project_root.clone();
        let calls = session.stats.total_tool_calls;
        let _ = session.save();

        maybe_consolidate(project_root.as_deref(), calls);
    }
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

    #[test]
    fn record_file_read_does_not_panic_without_session() {
        record_file_read("/tmp/nonexistent.rs", "full", 100, 50, false);
    }

    #[test]
    fn record_search_does_not_panic_without_session() {
        record_search(500, 200, 150);
    }

    #[test]
    fn record_tree_does_not_panic_without_session() {
        record_tree(100, 80);
    }

    #[test]
    fn record_shell_does_not_panic_without_session() {
        record_shell_command(500, 200);
    }
}
