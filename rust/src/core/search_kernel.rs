// SPDX-License-Identifier: Apache-2.0
//! Evidence and repeated-query tracking for the search tool.

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::core::context_kernel::evidence_hook;

static QUERY_TOKENS: LazyLock<Mutex<HashMap<u64, usize>>> = LazyLock::new(Default::default);
static TOTAL_SEARCHES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_TOKENS: AtomicUsize = AtomicUsize::new(0);

/// Cumulative search activity for the current session.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SearchSummary {
    /// Number of recorded searches.
    pub total_searches: usize,
    /// Number of distinct query hashes.
    pub unique_queries: usize,
    /// Number of searches whose query was previously recorded.
    pub repeated_queries: usize,
    /// Number of result tokens across all recorded searches.
    pub total_tokens: usize,
}

fn query_hash(query: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    query.hash(&mut hasher);
    hasher.finish()
}

/// Records search evidence and remembers the query for repetition detection.
pub fn record_search(query: &str, result_count: usize, tokens: usize) {
    if !crate::core::context_kernel::kernel_config::is_enabled() {
        return;
    }
    evidence_hook::record_tool_call("ctx_search", result_count, tokens);
    QUERY_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(query_hash(query))
        .and_modify(|c| *c += 1)
        .or_insert(1);
    TOTAL_SEARCHES.fetch_add(1, Ordering::Relaxed);
    TOTAL_TOKENS.fetch_add(tokens, Ordering::Relaxed);
}

/// Returns whether the query has already been recorded in this session.
#[must_use]
pub fn is_repeated_query(query: &str) -> bool {
    QUERY_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&query_hash(query))
        .is_some_and(|&c| c > 1)
}

/// Returns cumulative search activity for the current session.
#[must_use]
pub fn search_summary() -> SearchSummary {
    let total_searches = TOTAL_SEARCHES.load(Ordering::Relaxed);
    let unique_queries = QUERY_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    SearchSummary {
        total_searches,
        unique_queries,
        repeated_queries: total_searches.saturating_sub(unique_queries),
        total_tokens: TOTAL_TOKENS.load(Ordering::Relaxed),
    }
}

/// Clears all recorded search activity.
pub fn reset() {
    QUERY_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    TOTAL_SEARCHES.store(0, Ordering::Relaxed);
    TOTAL_TOKENS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::{is_repeated_query, record_search, reset, search_summary};
    use crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK;

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        guard
    }

    #[test]
    fn records_search_evidence() {
        let _guard = isolated();
        record_search("one", 1, 10);
        record_search("two", 2, 20);
        record_search("three", 3, 30);
        assert_eq!(search_summary().total_searches, 3);
    }

    #[test]
    fn detects_repeated_query() {
        let _guard = isolated();
        record_search("same", 1, 10);
        record_search("same", 1, 10);
        assert!(is_repeated_query("same"));
        assert_eq!(search_summary().repeated_queries, 1);
    }

    #[test]
    fn unique_queries_tracked() {
        let _guard = isolated();
        record_search("one", 1, 10);
        record_search("two", 1, 20);
        record_search("three", 1, 30);
        let summary = search_summary();
        assert_eq!(summary.unique_queries, 3);
        assert_eq!(summary.repeated_queries, 0);
    }
}
