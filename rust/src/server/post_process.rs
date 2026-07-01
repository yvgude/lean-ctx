//! Composable post-processing stages for `LeanCtxServer::call_tool_guarded`
//! (issue #144).
//!
//! `call_tool_guarded` historically inlined a ~1000-line pre/post-dispatch
//! pipeline. The self-contained, side-effect-isolated *stages* live here as
//! small, individually unit-testable functions so the guarded path stays a thin
//! orchestrator. Each function mirrors exactly one concern of the original
//! pipeline; behaviour is unchanged.

use serde_json::{Map, Value};

use crate::core::config::{CompressionLevel, Config};
use crate::core::context_ir::ContextIrSourceKindV1;

/// Tools that are always allowed to run even when the budget is exhausted —
/// they are how the agent inspects/recovers the budget.
const BUDGET_BYPASS_TOOLS: &[&str] = &["ctx_session", "ctx_cost", "ctx_metrics"];

/// Pre-dispatch budget guard. Returns `Some(message)` when the call must be
/// short-circuited because the budget is exhausted (and emits the matching
/// `budget_exhausted` events), or `None` to proceed.
pub(super) fn budget_exhausted_message(name: &str) -> Option<String> {
    use crate::core::budget_tracker::{BudgetLevel, BudgetTracker};
    let snap = BudgetTracker::global().check();
    if *snap.worst_level() != BudgetLevel::Exhausted || BUDGET_BYPASS_TOOLS.contains(&name) {
        return None;
    }
    for (dim, lvl, used, limit) in [
        (
            "tokens",
            &snap.tokens.level,
            format!("{}", snap.tokens.used),
            format!("{}", snap.tokens.limit),
        ),
        (
            "shell",
            &snap.shell.level,
            format!("{}", snap.shell.used),
            format!("{}", snap.shell.limit),
        ),
        (
            "cost",
            &snap.cost.level,
            format!("${:.2}", snap.cost.used_usd),
            format!("${:.2}", snap.cost.limit_usd),
        ),
    ] {
        if *lvl == BudgetLevel::Exhausted {
            crate::core::events::emit_budget_exhausted(&snap.role, dim, &used, &limit);
        }
    }
    Some(format!(
        "[BUDGET EXHAUSTED] {}\n\
         Use `ctx_session action=role` to check/switch roles, \
         or `ctx_session action=reset` to start fresh.",
        snap.format_compact()
    ))
}

/// Post-dispatch budget guard. Returns `Some(message)` to append a
/// `[BUDGET WARNING]` footer (emitting the matching `budget_warning` events), or
/// `None`. Suppressed when meta output is not visible.
pub(super) fn budget_warning_message() -> Option<String> {
    use crate::core::budget_tracker::{BudgetLevel, BudgetTracker};
    let snap = BudgetTracker::global().check();
    if *snap.worst_level() != BudgetLevel::Warning {
        return None;
    }
    for (dim, lvl, used, limit, pct) in [
        (
            "tokens",
            &snap.tokens.level,
            format!("{}", snap.tokens.used),
            format!("{}", snap.tokens.limit),
            snap.tokens.percent,
        ),
        (
            "shell",
            &snap.shell.level,
            format!("{}", snap.shell.used),
            format!("{}", snap.shell.limit),
            snap.shell.percent,
        ),
        (
            "cost",
            &snap.cost.level,
            format!("${:.2}", snap.cost.used_usd),
            format!("${:.2}", snap.cost.limit_usd),
            snap.cost.percent,
        ),
    ] {
        if *lvl == BudgetLevel::Warning {
            crate::core::events::emit_budget_warning(&snap.role, dim, &used, &limit, pct);
        }
    }
    if crate::core::protocol::meta_visible() {
        Some(format!("[BUDGET WARNING] {}", snap.format_compact()))
    } else {
        None
    }
}

/// Map a tool name to the Context-IR source kind recorded for lineage.
pub(super) fn context_ir_source_kind(name: &str) -> ContextIrSourceKindV1 {
    match name {
        n if n.contains("read") || n.contains("multi_read") || n.contains("smart_read") => {
            ContextIrSourceKindV1::Read
        }
        "ctx_shell" => ContextIrSourceKindV1::Shell,
        "ctx_search" | "ctx_semantic_search" => ContextIrSourceKindV1::Search,
        "ctx_provider" => ContextIrSourceKindV1::Provider,
        _ => ContextIrSourceKindV1::Other,
    }
}

/// Whether the terse compression stage must be skipped for this call (raw shell,
/// any read-family output, edit evidence, or structural shell output).
///
/// Read-family tools return file content the agent reads and *edits against*. The
/// prose terse pipeline (dictionary `return`→`ret`, `string`→`str`, … plus
/// line-score filtering) corrupts source and drops repeated lines, turning a
/// `mode="full"` read — whose contract is "guaranteed complete content" — into a
/// lossy, un-editable digest. The read tools already apply their own mode-aware,
/// structure-preserving compression (map/signatures/aggressive), so the generic
/// terse layer must never post-process their output. Previously this only skipped
/// when the read had *already saved* tokens, so verbatim `full`/`lines:` reads
/// (0 savings) were silently dictionary-mangled and de-duplicated.
///
/// `ctx_edit` is exempt for the same reason (#382): its `evidence (diff)` block
/// embeds verbatim source lines that must stay byte-accurate — agents treat the
/// evidence as proof of what was written. The rest of its output (status line,
/// pre/postimage hashes) is already minimal, so terse has nothing to gain.
fn skip_terse(name: &str, args: Option<&Map<String, Value>>, is_raw_shell: bool) -> bool {
    let mode = crate::server::helpers::get_str(args, "mode");
    is_raw_shell
        || crate::core::terse::is_verbatim_read(name, mode.as_deref())
        || name == "ctx_edit"
        || (name == "ctx_shell"
            && crate::server::helpers::get_str(args, "command")
                .is_some_and(|c| crate::shell::compress::has_structural_output(&c)))
}

/// Apply the session terse-compression stage. Returns the (possibly) compressed
/// text; the original is returned untouched when compression is inactive, must
/// be skipped, or fails the quality/savings gate.
pub(super) fn compress_terse(
    text: String,
    name: &str,
    args: Option<&Map<String, Value>>,
    config: &Config,
    is_raw_shell: bool,
) -> String {
    if skip_terse(name, args, is_raw_shell) {
        return text;
    }
    let compression = CompressionLevel::effective(config);
    if !compression.is_active() {
        return text;
    }
    let terse_result = crate::core::terse::pipeline::compress(&text, &compression, None);
    if terse_result.quality_passed && terse_result.savings_pct >= 3.0 {
        terse_result.output
    } else {
        text
    }
}

/// Final output token count plus persistent-stats correction (OPT-4): the
/// dispatcher records savings before terse/hints run, so once post-processing
/// changes the text we recompute the real sent-token count and adjust the saved
/// total to reflect what the model actually receives. Returns the final count.
pub(super) fn finalize_token_count_and_adjust(
    name: &str,
    result_text: &str,
    pre_terse_len: usize,
    output_tokens: u64,
    tool_saved_tokens: usize,
) -> usize {
    #[allow(clippy::cast_possible_truncation)]
    let output_token_count = if result_text.len() == pre_terse_len {
        output_tokens as usize
    } else {
        crate::core::tokens::count_tokens(result_text)
    };

    if result_text.len() != pre_terse_len && output_tokens > 0 {
        let delta = terse_savings_delta(
            output_tokens as usize,
            output_token_count,
            tool_saved_tokens,
        );
        if delta != 0 {
            crate::core::stats::adjust_savings(name, delta);
        }
    }
    output_token_count
}

/// Signed OPT-4 correction added to persistent saved-tokens once terse
/// post-processing shrinks the payload. `original` is reconstructed from
/// `pre_sent` — the *pre-terse* token count — so the delta captures terse's
/// actual reduction. Reconstructing from the post-terse count instead collapses
/// `actual_savings` to `pre_savings`, making the delta always 0, so terse
/// savings are never recorded. Returns 0 when there is nothing to adjust.
fn terse_savings_delta(pre_sent: usize, actual_sent: usize, pre_savings: usize) -> i64 {
    let original = pre_sent + pre_savings;
    let actual_savings = original.saturating_sub(actual_sent);
    pre_savings as i64 - actual_savings as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_ir_source_kind_maps_tools() {
        assert!(matches!(
            context_ir_source_kind("ctx_read"),
            ContextIrSourceKindV1::Read
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_smart_read"),
            ContextIrSourceKindV1::Read
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_shell"),
            ContextIrSourceKindV1::Shell
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_search"),
            ContextIrSourceKindV1::Search
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_semantic_search"),
            ContextIrSourceKindV1::Search
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_provider"),
            ContextIrSourceKindV1::Provider
        ));
        assert!(matches!(
            context_ir_source_kind("ctx_knowledge"),
            ContextIrSourceKindV1::Other
        ));
    }

    #[test]
    fn budget_bypass_tools_never_short_circuit() {
        // Regardless of budget state, the recovery tools must be allowed: the
        // function returns None for them (they bypass the exhaustion gate).
        for t in BUDGET_BYPASS_TOOLS {
            assert!(
                budget_exhausted_message(t).is_none(),
                "{t} must bypass the budget gate"
            );
        }
    }

    #[test]
    fn skip_terse_for_raw_shell_and_reads() {
        assert!(skip_terse("ctx_shell", None, true), "raw shell skips terse");
        // Reads always skip terse — even a verbatim `full` read that saved 0 tokens —
        // so file content stays byte-faithful for editing (no `return`→`ret` mangling,
        // no de-dup of repeated lines).
        assert!(
            skip_terse("ctx_read", None, false),
            "full/verbatim read (0 savings) must still skip terse"
        );
        assert!(
            skip_terse("ctx_multi_read", None, false),
            "multi_read must skip terse"
        );
        assert!(
            skip_terse("ctx_smart_read", None, false),
            "smart_read must skip terse"
        );
        assert!(
            !skip_terse("ctx_grep", None, false),
            "ordinary tool output is eligible for terse"
        );
        // #382: ctx_edit embeds verbatim source in its evidence diff — the
        // terse dictionary (`return`→`ret`) and line-score filtering corrupted
        // it into a false "the edit went wrong" signal for agents.
        assert!(
            skip_terse("ctx_edit", None, false),
            "edit evidence must stay byte-accurate"
        );
    }

    #[test]
    fn skip_terse_mode_based_guard_protects_verbatim() {
        use serde_json::json;
        let full_args = json!({"mode": "full"}).as_object().unwrap().clone();
        let lines_args = json!({"mode": "lines:1-20"}).as_object().unwrap().clone();
        let map_args = json!({"mode": "map"}).as_object().unwrap().clone();
        // Defense in depth: a non-read tool requesting a verbatim mode is still
        // protected, so a future read path is byte-exact before it is named (#404).
        assert!(skip_terse("ctx_future_reader", Some(&full_args), false));
        assert!(skip_terse("ctx_x", Some(&lines_args), false));
        // A non-read tool with a lossy/structured mode stays terse-eligible.
        assert!(!skip_terse("ctx_search", Some(&map_args), false));
    }

    #[test]
    fn compress_terse_keeps_reads_byte_identical() {
        // Content laced with terse triggers (dictionary words, blank lines and a
        // UTF-8 BOM) that the prose pipeline would otherwise mangle. A read must
        // come back byte-for-byte even with the densest compression level (#404).
        let content = "\u{feff}fn run() {\n\n    // command execution pipeline\n    let command = execution();\n    return command;\n}\n";
        let cfg = Config {
            compression_level: CompressionLevel::Max,
            ..Config::default()
        };
        let out = compress_terse(content.to_string(), "ctx_read", None, &cfg, false);
        assert_eq!(
            out, content,
            "ctx_read output must be byte-identical (#404)"
        );
    }

    #[test]
    fn finalize_token_count_uses_cached_count_when_unchanged() {
        // When the text length is unchanged from pre-terse, the cached token
        // count is returned verbatim (no recount, no stats adjustment).
        let text = "hello world";
        let n = finalize_token_count_and_adjust("ctx_shell", text, text.len(), 42, 0);
        assert_eq!(n, 42);
    }

    #[test]
    fn terse_delta_reconstructs_from_pre_terse_count() {
        // Terse shrank 100 pre-terse tokens to 60 sent; the handler had already
        // reported 20 saved. The true original is 120, so real savings are 60 and
        // we must add the extra 40 (delta = 20 - 60 = -40). Reconstructing from the
        // post-terse count (60) — the original bug — would give original = 80,
        // actual_savings = 20, delta = 0: the terse reduction is silently lost.
        assert_eq!(terse_savings_delta(100, 60, 20), -40);
        // Regression guard: feeding the post-terse count as `pre_sent` yields the
        // tautological no-op the fix removed.
        assert_eq!(terse_savings_delta(60, 60, 20), 0);
        // No handler savings (ctx_compose/ctx_search/…) still yields a correction
        // when terse trims output — the case the old `tool_saved_tokens > 0` gate
        // dropped entirely.
        assert_eq!(terse_savings_delta(100, 70, 0), -30);
        // Nothing to correct when terse did not change the count.
        assert_eq!(terse_savings_delta(80, 80, 0), 0);
    }
}
