// SPDX-License-Identifier: Apache-2.0
//! Thin content-deduplication hook for the ctx_read hot path.

use lean_ctx::core::context_kernel::ctx_read_dedup::{self, ReadDedupSummary};

/// Returns an unchanged-content stub when deduplication applies and finds a match.
///
/// #2172: a stub claims the content is "already in context", so it is only
/// sound when the earlier delivery reached the model whole. A read larger than
/// the turn budget is cut by the response pipeline (`core::budget`) after this
/// point, so the model holds a prefix, not the content: recording it made the
/// next explicit `full` / `lines:N-M` read of the same range come back as
/// `→ path unchanged (ref:…)` while the requested lines had never been shown.
/// Such a read is neither recorded nor stubbed, and any earlier fingerprint for
/// the path is dropped because the model's latest view of it is incomplete.
#[must_use]
pub fn maybe_dedup(path: &str, content: &str, mode: &str, fresh: bool) -> Option<String> {
    if !ctx_read_dedup::should_dedup(mode) {
        return None;
    }
    if exceeds_turn_budget(content) {
        ctx_read_dedup::on_file_write(path);
        return None;
    }
    let stub = ctx_read_dedup::try_dedup(path, content, fresh)?;
    let original_tokens = lean_ctx::core::tokens::count_tokens(content);
    let stub_tokens = lean_ctx::core::tokens::count_tokens(&stub);
    lean_ctx::core::stats::record_reread(original_tokens.saturating_sub(stub_tokens));
    Some(stub)
}

/// Whether the response pipeline may truncate this read at the ordinary turn
/// budget (`raw` reads, the only verbatim-budget mode, never reach dedup).
///
/// `content` is the rendered read body, but the pipeline appends headers and
/// hints before `core::budget` measures the reply, so a body just under the
/// limit can still be cut. The check keeps a fifth of the limit as margin: a
/// read near the limit is delivered again rather than stubbed. The session
/// cache asks the same question before it marks a read as fully delivered.
pub(crate) fn exceeds_turn_budget(content: &str) -> bool {
    let limit = lean_ctx::core::config::Config::load_arc().turn_fresh_limit_effective();
    limit > 0 && lean_ctx::core::tokens::count_tokens(content).saturating_mul(6) / 5 > limit
}

/// Invalidates cached content for `path` after a file write.
pub fn on_write(path: &str) {
    ctx_read_dedup::on_file_write(path);
}

/// Returns current ctx_read deduplication statistics.
#[must_use]
pub fn summary() -> ReadDedupSummary {
    ctx_read_dedup::dedup_summary()
}

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::{maybe_dedup, on_write};

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = lean_ctx::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lean_ctx::core::context_kernel::kernel_config::reset_features();
        lean_ctx::core::context_kernel::dedup_wiring::reset_dedup();
        guard
    }

    #[test]
    fn dedup_returns_none_for_new_content() {
        let _guard = isolated();
        assert_eq!(maybe_dedup("new.rs", "content", "full", false), None);
    }

    #[test]
    fn dedup_returns_stub_for_repeated() {
        let _guard = isolated();
        assert_eq!(maybe_dedup("repeat.rs", "content", "full", false), None);
        assert!(maybe_dedup("repeat.rs", "content", "full", false).is_some());
    }

    #[test]
    fn raw_mode_skips_dedup() {
        let _guard = isolated();
        assert_eq!(maybe_dedup("raw.rs", "content", "full", false), None);
        assert_eq!(maybe_dedup("raw.rs", "content", "raw", false), None);
    }

    /// #2172: a read the turn budget cuts never reached the model whole, so
    /// repeating the explicit request must deliver content, not a stub.
    #[test]
    fn a_read_cut_by_the_turn_budget_is_never_stubbed() {
        let _guard = isolated();
        let _limit = lean_ctx::test_env::EnvVarGuard::set("LEAN_CTX_TURN_FRESH_LIMIT", "50");
        let controller = (1..=400)
            .map(|i| format!("{i:>6}|  register_route_{i}(request, response);"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(maybe_dedup("ctrl.ts", &controller, "full", false), None);
        assert_eq!(
            maybe_dedup("ctrl.ts", &controller, "full", false),
            None,
            "a truncated delivery must not be answered with `already in context`"
        );
        let window = "  10|  register_route_10(request, response);";
        assert_eq!(maybe_dedup("ctrl.ts", window, "lines:10-10", false), None);
        assert!(
            maybe_dedup("ctrl.ts", window, "lines:10-10", false).is_some(),
            "a true re-read of the same delivered range may still be a reference"
        );
    }

    #[test]
    fn write_invalidates_cache() {
        let _guard = isolated();
        assert_eq!(maybe_dedup("written.rs", "content", "full", false), None);
        assert!(maybe_dedup("written.rs", "content", "full", false).is_some());
        on_write("written.rs");
        assert_eq!(maybe_dedup("written.rs", "content", "full", false), None);
    }
}
