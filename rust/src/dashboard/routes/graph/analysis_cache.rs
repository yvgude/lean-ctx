//! Fingerprint-keyed memoization for expensive dashboard graph analyses.
//!
//! The architecture report recomputes communities, betweenness centrality,
//! import cycles, god-nodes and "surprising connections" on every request —
//! all pure functions of the current graph. We memoize the rendered JSON keyed
//! by a cheap, change-sensitive fingerprint (file count + edge count + last
//! scan). Any rescan bumps `last_scan` and edits change the counts, so the cache
//! invalidates automatically and never serves stale analysis.
//!
//! The store is bounded (`MAX_ENTRIES`, FIFO eviction): keys are `route:project`,
//! so a long-lived dashboard process serving many projects can never grow the
//! cache without limit.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Cap on distinct cached analyses. Each key is `route:project`; 64 covers many
/// projects × routes with a negligible footprint while bounding worst-case
/// growth. Oldest entries are evicted first.
const MAX_ENTRIES: usize = 64;

/// Bounded `key -> (fingerprint, rendered json)` store with FIFO eviction.
struct Store {
    map: HashMap<String, (String, String)>,
    order: VecDeque<String>,
}

impl Store {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Return the cached JSON for `key` iff its stored fingerprint still matches.
    fn get(&self, key: &str, fingerprint: &str) -> Option<String> {
        self.map
            .get(key)
            .filter(|(fp, _)| fp == fingerprint)
            .map(|(_, json)| json.clone())
    }

    /// Insert/refresh `key`, evicting the oldest entries past `MAX_ENTRIES`.
    fn put(&mut self, key: &str, fingerprint: &str, json: String) {
        let is_new = !self.map.contains_key(key);
        self.map
            .insert(key.to_string(), (fingerprint.to_string(), json));
        if is_new {
            self.order.push_back(key.to_string());
            while self.order.len() > MAX_ENTRIES {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

fn store() -> &'static Mutex<Store> {
    static CACHE: OnceLock<Mutex<Store>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Store::new()))
}

/// A cheap, change-sensitive fingerprint of the current graph.
pub(super) fn fingerprint(gp: &crate::core::graph_provider::GraphProvider) -> String {
    format!(
        "{}:{}:{}",
        gp.file_count(),
        gp.edge_count().unwrap_or(0),
        gp.last_scan()
    )
}

/// Return the cached JSON for `key` when its stored fingerprint still matches,
/// otherwise run `compute`, store the result under `(key, fingerprint)`, and
/// return it.
pub(super) fn cached_or_compute(
    key: &str,
    fingerprint: &str,
    compute: impl FnOnce() -> String,
) -> String {
    if let Ok(s) = store().lock()
        && let Some(json) = s.get(key, fingerprint)
    {
        return json;
    }
    let json = compute();
    if let Ok(mut s) = store().lock() {
        s.put(key, fingerprint, json.clone());
    }
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_skips_recompute_until_fingerprint_changes() {
        let key = "test:route";
        let mut calls = 0;
        let mut run = |fp: &str| {
            cached_or_compute(key, fp, || {
                calls += 1;
                format!("payload-{calls}")
            })
        };

        assert_eq!(run("fp-1"), "payload-1"); // miss → compute
        assert_eq!(run("fp-1"), "payload-1"); // hit → cached, no recompute
        assert_eq!(run("fp-2"), "payload-2"); // fingerprint changed → recompute
        assert_eq!(run("fp-2"), "payload-2"); // hit again
    }

    #[test]
    fn store_evicts_oldest_beyond_capacity() {
        // Tested on a local Store so it is deterministic and never races the
        // process-wide cache used by the integration-style test above.
        let mut store = Store::new();
        for i in 0..(MAX_ENTRIES + 5) {
            store.put(&format!("k{i}"), "fp", format!("v{i}"));
        }
        assert!(store.map.len() <= MAX_ENTRIES, "size stays bounded");
        // The five oldest keys were evicted.
        for i in 0..5 {
            assert!(store.get(&format!("k{i}"), "fp").is_none(), "k{i} evicted");
        }
        // The newest key is retained.
        assert_eq!(
            store.get(&format!("k{}", MAX_ENTRIES + 4), "fp").as_deref(),
            Some("v68")
        );
    }

    #[test]
    fn store_refresh_does_not_grow_order() {
        // Re-putting an existing key updates value/fingerprint without adding a
        // second order entry (so it cannot distort eviction).
        let mut store = Store::new();
        store.put("k", "fp-1", "v1".to_string());
        store.put("k", "fp-2", "v2".to_string());
        assert_eq!(store.order.len(), 1);
        assert_eq!(store.get("k", "fp-2").as_deref(), Some("v2"));
        assert!(store.get("k", "fp-1").is_none(), "old fingerprint misses");
    }
}
