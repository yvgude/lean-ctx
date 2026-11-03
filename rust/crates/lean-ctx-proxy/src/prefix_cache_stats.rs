// SPDX-License-Identifier: Apache-2.0
//! Structured prefix-cache statistics for the `/status` endpoint.
//!
//! Aggregates signals from cache modules into a single `prefix_cache` block:
//! hit/miss counts, alignment score, frozen message counts, delta compression
//! ratios, replay metrics, and Headroom-compat counts.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static PREFIX_HITS: AtomicU64 = AtomicU64::new(0);
static PREFIX_MISSES: AtomicU64 = AtomicU64::new(0);
static FROZEN_MSG_SUM: AtomicU64 = AtomicU64::new(0);
static FROZEN_MSG_COUNT: AtomicU64 = AtomicU64::new(0);
static DELTA_BYTES_ORIGINAL: AtomicU64 = AtomicU64::new(0);
static DELTA_BYTES_COMPRESSED: AtomicU64 = AtomicU64::new(0);
static REPLAY_HITS: AtomicU64 = AtomicU64::new(0);
static REPLAY_MISSES: AtomicU64 = AtomicU64::new(0);
static STICKY_INJECTIONS: AtomicU64 = AtomicU64::new(0);
static HEADROOM_COMPAT_REQUESTS: AtomicU64 = AtomicU64::new(0);

pub fn record_hit() {
    PREFIX_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_miss() {
    PREFIX_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_frozen_count(count: u64) {
    FROZEN_MSG_SUM.fetch_add(count, Ordering::Relaxed);
    FROZEN_MSG_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub fn record_delta(original: u64, compressed: u64) {
    DELTA_BYTES_ORIGINAL.fetch_add(original, Ordering::Relaxed);
    DELTA_BYTES_COMPRESSED.fetch_add(compressed, Ordering::Relaxed);
}

pub fn record_replay_hit() {
    REPLAY_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_replay_miss() {
    REPLAY_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_sticky_injection() {
    STICKY_INJECTIONS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_headroom_compat() {
    HEADROOM_COMPAT_REQUESTS.fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrefixCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub alignment_score: u64,
    pub frozen_message_count_avg: f64,
    pub delta_compression_ratio: f64,
    pub replay_hits: u64,
    pub replay_misses: u64,
    pub sticky_tool_injections: u64,
    pub headroom_compat_requests: u64,
}

fn alignment_score(volatile_detected: u64, requests_scanned: u64) -> u64 {
    if requests_scanned == 0 {
        return 100;
    }
    let avg = volatile_detected as f64 / requests_scanned as f64;
    100u64.saturating_sub((avg * 10.0).round() as u64)
}

#[must_use]
pub fn snapshot() -> PrefixCacheStats {
    let hits = PREFIX_HITS.load(Ordering::Relaxed);
    let misses = PREFIX_MISSES.load(Ordering::Relaxed);
    let total = hits + misses;
    let hit_rate = if total > 0 {
        hits as f64 / total as f64
    } else {
        1.0
    };

    let frozen_sum = FROZEN_MSG_SUM.load(Ordering::Relaxed);
    let frozen_count = FROZEN_MSG_COUNT.load(Ordering::Relaxed);
    let frozen_avg = if frozen_count > 0 {
        frozen_sum as f64 / frozen_count as f64
    } else {
        0.0
    };

    let d_orig = DELTA_BYTES_ORIGINAL.load(Ordering::Relaxed);
    let d_comp = DELTA_BYTES_COMPRESSED.load(Ordering::Relaxed);
    let delta_ratio = if d_orig > 0 {
        d_comp as f64 / d_orig as f64
    } else {
        0.0
    };

    let cs = super::cache_safety::snapshot();
    let score = alignment_score(cs.volatile_fields_detected, cs.volatile_system_requests);

    PrefixCacheStats {
        hits,
        misses,
        hit_rate,
        alignment_score: score,
        frozen_message_count_avg: frozen_avg,
        delta_compression_ratio: delta_ratio,
        replay_hits: REPLAY_HITS.load(Ordering::Relaxed),
        replay_misses: REPLAY_MISSES.load(Ordering::Relaxed),
        sticky_tool_injections: STICKY_INJECTIONS.load(Ordering::Relaxed),
        headroom_compat_requests: HEADROOM_COMPAT_REQUESTS.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_score_perfect_when_no_volatiles() {
        assert_eq!(alignment_score(0, 100), 100);
    }

    #[test]
    fn alignment_score_degrades_with_volatiles() {
        assert_eq!(alignment_score(10, 10), 90);
        assert_eq!(alignment_score(50, 10), 50);
    }

    #[test]
    fn alignment_score_clamps_to_zero() {
        assert_eq!(alignment_score(1000, 10), 0);
    }

    #[test]
    fn alignment_score_is_100_when_no_requests() {
        assert_eq!(alignment_score(0, 0), 100);
    }
}
