//! Cache-safe wire prose squeeze (#895).
//!
//! The proxy rewrites prose in the frozen request region and re-squeezes every
//! tool result on each turn. Those rewrites MUST be byte-identical across turns
//! or the provider prompt-cache prefix is invalidated (#448/#498). Extractive
//! ranking ([`crate::core::extractive`]) depends on the embedding engine, whose
//! availability transitions cold→warm exactly once per process — so the naive
//! "extractive when warm, truncate when cold" would flip an already-emitted
//! frozen rewrite the first time it is recompressed after warmup.
//!
//! This module removes that hazard with a process-global, content-addressed
//! memo: the FIRST squeeze of a given `(content, budget)` is frozen for the
//! process lifetime, so a later recompute (now warm) returns the original bytes.
//! After warmup every compute is the warm, deterministic extractive result, so
//! memo eviction is harmless; the memo only has to bridge the brief cold window.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use crate::core::config::{Config, ProseRanker};
use crate::core::extractive::{self, RankMode};
use crate::core::web::distill::squeeze_prose;

/// Cap on memoized frozen-region squeezes. A conversation's frozen prefix is
/// bounded, so this comfortably bridges the cold window without unbounded growth.
const MEMO_CAP: usize = 8192;

static MEMO: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);

/// Cache-safe prose squeeze for the wire. Uses extractive **centrality** ranking
/// (query-free — never drops a sentence for being "off-topic", so it is safe on
/// system/user instructions) when the engine is available and the configured
/// [`ProseRanker`] allows it, else the deterministic truncating squeeze. The
/// result is memoized per `(content, budget)` so it is byte-stable across turns.
#[must_use]
pub fn squeeze(content: &str, budget: usize) -> String {
    let ranker = Config::load().proxy.resolved_prose_ranker();
    // Truncate mode never touches the engine and is already deterministic, so it
    // needs no memo — keep that path allocation-light.
    if ranker == ProseRanker::Truncate {
        return squeeze_prose(content, budget);
    }

    let key = memo_key(content, budget);
    if let Some(hit) = memo_get(key) {
        return hit;
    }
    let out = compute(content, budget);
    memo_put(key, &out);
    out
}

/// Extractive-or-truncate, without the memo. Centrality mode, no anchor.
fn compute(content: &str, budget: usize) -> String {
    if let Some(ranked) = extractive::rank_and_squeeze(content, budget, RankMode::Centrality, None)
    {
        return ranked;
    }
    squeeze_prose(content, budget)
}

fn memo_key(content: &str, budget: usize) -> u64 {
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    budget.hash(&mut h);
    h.finish()
}

fn memo_get(key: u64) -> Option<String> {
    let guard = MEMO.lock().ok()?;
    guard.as_ref()?.get(&key).cloned()
}

fn memo_put(key: u64, value: &str) {
    if let Ok(mut guard) = MEMO.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        // A full clear (never mid-cold-window in practice) is enough: post-warmup
        // every recompute is the same deterministic extractive output.
        if map.len() >= MEMO_CAP {
            map.clear();
        }
        map.insert(key, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_unique_prose() -> String {
        (0..50)
            .map(|i| format!("Distinct sentence number {i} about handling case {i} carefully."))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn squeeze_is_stable_across_calls() {
        // In `cargo test` the engine is never loaded, so this exercises the
        // memoized truncate fallback — which must be byte-identical across calls.
        let text = long_unique_prose();
        let a = squeeze(&text, 200);
        let b = squeeze(&text, 200);
        assert_eq!(a, b, "wire squeeze must be byte-stable across turns");
        assert!(a.len() <= text.len());
    }

    #[test]
    fn truncate_mode_bypasses_memo_and_matches_squeeze_prose() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PROXY_PROSE_RANKER", "truncate");
        let text = long_unique_prose();
        assert_eq!(squeeze(&text, 200), squeeze_prose(&text, 200));
        crate::test_env::remove_var("LEAN_CTX_PROXY_PROSE_RANKER");
    }
}
