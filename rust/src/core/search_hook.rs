// SPDX-License-Identifier: Apache-2.0
//! Search hot-path bridge for kernel evidence and repeat detection.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::context_kernel::kernel_config;
use crate::core::search_kernel;

static SEARCHES_RECORDED: AtomicUsize = AtomicUsize::new(0);
static REPEATED_WARNED: AtomicUsize = AtomicUsize::new(0);

/// Cumulative search-hook activity for the current session.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SearchHookSummary {
    /// Number of searches forwarded to the search kernel.
    pub searches_recorded: usize,
    /// Number of repeated-query warnings emitted.
    pub repeated_queries_warned: usize,
}

/// Records one search in the kernel when kernel processing is enabled.
pub fn on_search(query: &str, action: &str, result_count: usize, tokens: usize) {
    if !kernel_config::is_enabled() {
        return;
    }
    let _ = action;
    search_kernel::record_search(query, result_count, tokens);
    SEARCHES_RECORDED.fetch_add(1, Ordering::Relaxed);
}

/// Returns a warning when the query has already been recorded.
#[must_use]
pub fn maybe_warn_repeat(query: &str) -> Option<String> {
    if !kernel_config::is_enabled() || !search_kernel::is_repeated_query(query) {
        return None;
    }
    REPEATED_WARNED.fetch_add(1, Ordering::Relaxed);
    Some("⚠ Repeated query detected; consider refining it or reusing prior results.".to_owned())
}

/// Returns cumulative search-hook activity.
#[must_use]
pub fn summary() -> SearchHookSummary {
    SearchHookSummary {
        searches_recorded: SEARCHES_RECORDED.load(Ordering::Relaxed),
        repeated_queries_warned: REPEATED_WARNED.load(Ordering::Relaxed),
    }
}

/// Clears hook counters and delegated search-kernel state.
pub fn reset() {
    SEARCHES_RECORDED.store(0, Ordering::Relaxed);
    REPEATED_WARNED.store(0, Ordering::Relaxed);
    search_kernel::reset();
}

#[cfg(test)]
mod tests {
    use super::{maybe_warn_repeat, on_search, reset, summary};
    use crate::core::context_kernel::kernel_config::{self, KERNEL_TEST_LOCK, KernelFeatures};

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        reset();
        guard
    }

    #[test]
    fn records_search() {
        let _guard = isolated();
        for _ in 0..3 {
            on_search("query", "regex", 2, 10);
        }
        assert_eq!(summary().searches_recorded, 3);
    }

    #[test]
    fn detects_repeat() {
        let _guard = isolated();
        on_search("same", "regex", 1, 10);
        on_search("same", "regex", 1, 10);
        assert!(maybe_warn_repeat("same").is_some());
        assert_eq!(summary().repeated_queries_warned, 1);
    }

    #[test]
    fn disabled_kernel_noop() {
        let _guard = isolated();
        let features = KernelFeatures {
            enabled: false,
            ..KernelFeatures::default()
        };
        kernel_config::update_features(features);
        on_search("same", "regex", 1, 10);
        assert!(maybe_warn_repeat("same").is_none());
        assert_eq!(summary().searches_recorded, 0);
        assert_eq!(summary().repeated_queries_warned, 0);
        kernel_config::reset_features();
    }
}
