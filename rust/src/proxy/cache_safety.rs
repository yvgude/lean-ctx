//! Cache-preservation telemetry for the proxy's frozen-region prose rewrites
//! (#710).
//!
//! The proxy only ever rewrites prose inside the cache-safe frozen window
//! `[cached_prefix_len, boundary)` — never inside the client-cached prefix and
//! never in the live tail. This module turns that invariant into a *measurable*
//! production signal: every request that performs a frozen-region prose rewrite
//! reports whether the rewrite stayed cache-safe, and `/status` surfaces the
//! resulting ratio (`1.0` = every rewrite was provably cache-safe, the
//! healthy steady state). A value below `1.0` is a regression signal.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Total prose segments (text fields) compressed across all requests.
static PROSE_SEGMENTS: AtomicU64 = AtomicU64::new(0);
/// Requests that performed at least one frozen-region prose rewrite.
static PROSE_REQUESTS: AtomicU64 = AtomicU64::new(0);
/// Of those, the requests whose every rewrite was cache-safe.
static CACHE_SAFE_REQUESTS: AtomicU64 = AtomicU64::new(0);
/// Deliberate cold-prefix repacks (#480): requests where the proxy predicted the
/// client-cached prefix was already cold and rewrote it on purpose. Tracked
/// separately so an *intentional* prefix rewrite never dilutes the
/// `cache_safe_ratio`, whose job is to catch *accidental* #448 regressions.
static COLD_PREFIX_REPACKS: AtomicU64 = AtomicU64::new(0);

/// Record one request's frozen-region prose activity.
///
/// `segments` is how many prose fields were compressed this request; `all_safe`
/// is `true` when *every* rewrite landed strictly inside the cache-safe frozen
/// window. A no-op request (`segments == 0`) is not counted, so the ratio
/// reflects only requests that actually mutated prose.
pub fn record(segments: u64, all_safe: bool) {
    if segments == 0 {
        return;
    }
    PROSE_SEGMENTS.fetch_add(segments, Ordering::Relaxed);
    PROSE_REQUESTS.fetch_add(1, Ordering::Relaxed);
    if all_safe {
        CACHE_SAFE_REQUESTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record one deliberate cold-prefix repack (#480). Counted on its own gauge,
/// never against [`record`]'s cache-safe ratio.
pub fn record_cold_repack() {
    COLD_PREFIX_REPACKS.fetch_add(1, Ordering::Relaxed);
}

/// Cache-preservation ratio: `safe / total`, or `1.0` when nothing has been
/// rewritten yet (the trivially-safe empty state). Pure, so it is unit-tested
/// independently of the global counters.
#[must_use]
pub fn ratio(safe: u64, total: u64) -> f64 {
    if total == 0 {
        return 1.0;
    }
    safe as f64 / total as f64
}

/// Point-in-time view of the cache-safety counters for `/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheSafety {
    /// Prose segments compressed in the frozen region (cumulative).
    pub prose_segments_compressed: u64,
    /// Requests that performed at least one frozen-region prose rewrite.
    pub prose_requests: u64,
    /// Fraction of those requests whose every rewrite was cache-safe (`1.0` is
    /// the healthy steady state; the proxy only rewrites inside the cache-safe
    /// window by construction).
    pub cache_safe_ratio: f64,
    /// Deliberate cold-prefix repacks (#480), cumulative. Non-zero only when the
    /// opt-in mode fired on a predicted-cold session resume — expected, not a
    /// regression.
    #[serde(default)]
    pub cold_prefix_repacks: u64,
}

#[must_use]
pub fn snapshot() -> CacheSafety {
    let prose_requests = PROSE_REQUESTS.load(Ordering::Relaxed);
    let safe = CACHE_SAFE_REQUESTS.load(Ordering::Relaxed);
    CacheSafety {
        prose_segments_compressed: PROSE_SEGMENTS.load(Ordering::Relaxed),
        prose_requests,
        cache_safe_ratio: ratio(safe, prose_requests),
        cold_prefix_repacks: COLD_PREFIX_REPACKS.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_is_one_when_empty() {
        assert_eq!(ratio(0, 0), 1.0);
    }

    #[test]
    fn ratio_reflects_unsafe_rewrites() {
        assert_eq!(ratio(3, 3), 1.0);
        assert_eq!(ratio(2, 4), 0.5);
        assert_eq!(ratio(0, 2), 0.0);
    }
}
