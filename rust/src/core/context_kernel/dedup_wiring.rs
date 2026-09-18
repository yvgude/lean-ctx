//! Global content-deduplication wiring for context delivery hot paths.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::context_dedup::{ContextDedup, DedupResult, format_unchanged_stub};

static DEDUP: OnceLock<Mutex<ContextDedup>> = OnceLock::new();
static SEEN_PATHS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static STATS: OnceLock<Mutex<DedupStats>> = OnceLock::new();
/// Action to take based on a content deduplication check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupAction {
    /// Content is new and should be delivered in full.
    DeliverFull,
    /// Content is unchanged and can be replaced by a compact reference.
    DeliverStub {
        /// Compact reference to the content already present in context.
        stub: String,
    },
    /// Content changed and should be delivered in full.
    DeliverModified,
}

/// Cumulative content deduplication statistics for this process.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DedupStats {
    /// Number of enabled deduplication checks.
    pub total_checks: usize,
    /// Number of checks that found unchanged content.
    pub cache_hits: usize,
    /// Number of checks that required full delivery.
    pub cache_misses: usize,
    /// Estimated tokens avoided by unchanged-content stubs.
    pub tokens_saved: usize,
    /// Fraction of enabled checks that found unchanged content.
    pub hit_rate: f64,
}

/// Checks whether `content` changed since its last delivery at `path`.
#[must_use]
pub fn check_content(path: &str, content: &str, fresh: bool) -> DedupAction {
    if fresh {
        invalidate(path);
        return DedupAction::DeliverFull;
    }
    check_content_enabled(
        super::kernel_config::features().content_dedup,
        crate::core::conversation::scope_cannot_identify_caller(),
        path,
        content,
    )
}

/// Returns a snapshot of cumulative content deduplication statistics.
#[must_use]
pub fn dedup_stats() -> DedupStats {
    let mut snapshot = *lock(stats());
    snapshot.hit_rate = if snapshot.total_checks == 0 {
        0.0
    } else {
        snapshot.cache_hits as f64 / snapshot.total_checks as f64
    };
    snapshot
}

/// Applies content deduplication, returning either full content or a stub.
#[must_use]
pub fn apply_dedup(path: &str, content: &str) -> String {
    apply_dedup_enabled(
        super::kernel_config::features().content_dedup,
        path,
        content,
    )
}

fn apply_dedup_enabled(enabled: bool, path: &str, content: &str) -> String {
    match check_content_enabled(enabled, false, path, content) {
        DedupAction::DeliverStub { stub } => stub,
        DedupAction::DeliverFull | DedupAction::DeliverModified => content.to_owned(),
    }
}

/// Invalidates cached content for `path` after a write or external change.
pub fn invalidate(path: &str) {
    lock(dedup()).invalidate(path);
    lock(seen_paths()).remove(path);
}

/// Clears cached content and all cumulative deduplication statistics.
pub fn reset_dedup() {
    lock(dedup()).clear();
    lock(seen_paths()).clear();
    *lock(stats()) = DedupStats::default();
}

/// `callers_indistinguishable` makes the ledger fail closed (#1804).
///
/// The ledger is process-global and keyed on path alone — it carries no
/// session, conversation or agent argument, so it cannot tell two callers
/// apart. That is sound only while one process serves one context window. In
/// Claude Code a single lean-ctx process serves the parent session *and* every
/// sub-agent, so a sub-agent's **first** read of a file the parent already read
/// matched the parent's fingerprint and got `already in context` — a stub for
/// content that agent never received, with nothing in it to signal the loss.
///
/// Dedup's whole premise is that the content sits in the requesting model's
/// context window. When that cannot be established, the only safe answer is the
/// content itself: re-delivering costs tokens, delivering a dangling reference
/// costs the caller its data.
fn check_content_enabled(
    enabled: bool,
    callers_indistinguishable: bool,
    path: &str,
    content: &str,
) -> DedupAction {
    if !enabled {
        return DedupAction::DeliverFull;
    }

    let result = lock(dedup()).check_and_record(path, content);
    match result {
        DedupResult::Unchanged { hash, saved_tokens } if !callers_indistinguishable => {
            record_check(true, saved_tokens);
            DedupAction::DeliverStub {
                stub: format_unchanged_stub(path, &hash),
            }
        }
        // Recorded above, so a later read by the same caller still dedups once
        // the scope can identify it; this delivery just cannot be a stub.
        DedupResult::Unchanged { .. } => {
            record_check(false, 0);
            DedupAction::DeliverFull
        }
        DedupResult::Fresh => {
            let modified = !lock(seen_paths()).insert(path.to_owned());
            record_check(false, 0);
            if modified {
                DedupAction::DeliverModified
            } else {
                DedupAction::DeliverFull
            }
        }
    }
}
fn record_check(hit: bool, saved_tokens: usize) {
    let mut current = lock(stats());
    current.total_checks += 1;
    if hit {
        current.cache_hits += 1;
        current.tokens_saved += saved_tokens;
    } else {
        current.cache_misses += 1;
    }
}

fn dedup() -> &'static Mutex<ContextDedup> {
    DEDUP.get_or_init(|| {
        Mutex::new(ContextDedup::new(
            super::kernel_config::features().dedup_capacity,
        ))
    })
}

fn seen_paths() -> &'static Mutex<HashSet<String>> {
    SEEN_PATHS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn stats() -> &'static Mutex<DedupStats> {
    STATS.get_or_init(|| Mutex::new(DedupStats::default()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::{
        DedupAction, apply_dedup_enabled, check_content_enabled, dedup_stats, invalidate,
        reset_dedup,
    };

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_dedup();
        guard
    }

    #[test]
    fn indistinguishable_callers_never_receive_a_stub() {
        // #1804: no test covered two distinct callers reading the same unchanged
        // path, because every test resets the ledger first. In Claude Code one
        // process serves the parent and all its sub-agents, so the second read
        // here models a *different* agent's first read — it must get content.
        let _guard = isolated();
        check_content_enabled(true, true, "src/lib.rs", "content");
        assert_eq!(
            check_content_enabled(true, true, "src/lib.rs", "content"),
            DedupAction::DeliverFull,
            "a ledger that cannot tell callers apart must not substitute a stub"
        );
    }

    #[test]
    fn indistinguishable_callers_do_not_count_as_cache_hits() {
        // A withheld stub saved nothing; recording it as a hit would overstate
        // savings and hide the regression in `tools health`.
        let _guard = isolated();
        check_content_enabled(true, true, "a", "one");
        check_content_enabled(true, true, "a", "one");
        let stats = dedup_stats();
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.tokens_saved, 0);
    }

    #[test]
    fn new_content_delivers_full() {
        let _guard = isolated();
        assert_eq!(
            check_content_enabled(true, false, "src/lib.rs", "content"),
            DedupAction::DeliverFull
        );
    }

    #[test]
    fn repeated_content_delivers_stub() {
        let _guard = isolated();
        check_content_enabled(true, false, "src/lib.rs", "content");
        assert!(matches!(
            check_content_enabled(true, false, "src/lib.rs", "content"),
            DedupAction::DeliverStub { .. }
        ));
    }

    #[test]
    fn modified_content_delivers_modified() {
        let _guard = isolated();
        check_content_enabled(true, false, "src/lib.rs", "before");
        assert_eq!(
            check_content_enabled(true, false, "src/lib.rs", "after"),
            DedupAction::DeliverModified
        );
    }

    #[test]
    fn disabled_always_full() {
        let _guard = isolated();
        assert_eq!(
            check_content_enabled(false, false, "src/lib.rs", "content"),
            DedupAction::DeliverFull
        );
        assert_eq!(
            check_content_enabled(false, false, "src/lib.rs", "content"),
            DedupAction::DeliverFull
        );
        assert_eq!(dedup_stats().total_checks, 0);
    }

    #[test]
    fn apply_dedup_returns_stub() {
        let _guard = isolated();
        assert_eq!(
            apply_dedup_enabled(true, "src/lib.rs", "content"),
            "content"
        );
        let stub = apply_dedup_enabled(true, "src/lib.rs", "content");
        assert!(stub.contains("src/lib.rs unchanged"));
    }

    #[test]
    fn invalidate_forces_full() {
        let _guard = isolated();
        check_content_enabled(true, false, "src/lib.rs", "content");
        invalidate("src/lib.rs");
        assert_eq!(
            check_content_enabled(true, false, "src/lib.rs", "content"),
            DedupAction::DeliverFull
        );
    }

    #[test]
    fn stats_track_hits() {
        let _guard = isolated();
        check_content_enabled(true, false, "a", "one");
        check_content_enabled(true, false, "a", "one");
        check_content_enabled(true, false, "a", "one");
        check_content_enabled(true, false, "b", "two");
        check_content_enabled(true, false, "b", "two");

        let stats = dedup_stats();
        assert_eq!(stats.total_checks, 5);
        assert_eq!(stats.cache_hits, 3);
        assert_eq!(stats.cache_misses, 2);
        assert!((stats.hit_rate - 0.6).abs() < f64::EPSILON);
    }
}
