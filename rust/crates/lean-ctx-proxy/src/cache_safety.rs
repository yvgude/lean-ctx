// SPDX-License-Identifier: Apache-2.0
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
/// Prompt-cache breakpoints the proxy actively injected (#939): requests where a
/// client set no `cache_control` and the proxy added one on `system` so an
/// otherwise-uncached prefix bills at the cached rate. Pure win signal.
static BREAKPOINTS_INJECTED: AtomicU64 = AtomicU64::new(0);
/// Requests whose unanchored system prompt carried at least one volatile,
/// cache-busting field (#940, cache-aligner telemetry). A measurement-only
/// signal — the body is never mutated — that quantifies how much cache the
/// client's system prompt leaks before any opt-in relocate.
static VOLATILE_SYSTEM_REQUESTS: AtomicU64 = AtomicU64::new(0);
/// Cumulative volatile fields detected across those requests (#940).
static VOLATILE_FIELDS_DETECTED: AtomicU64 = AtomicU64::new(0);
/// Requests where the opt-in relocate (#974) actively moved volatile fields out
/// of the cacheable prefix into the uncached tail. A pure cache-win signal.
static VOLATILE_RELOCATE_REQUESTS: AtomicU64 = AtomicU64::new(0);
/// Cumulative volatile fields relocated across those requests (#974).
static VOLATILE_FIELDS_RELOCATED: AtomicU64 = AtomicU64::new(0);

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

/// Record one actively-injected prompt-cache breakpoint (#939).
pub fn record_breakpoint_injected() {
    BREAKPOINTS_INJECTED.fetch_add(1, Ordering::Relaxed);
}

/// Record one unanchored-system scan that found `fields` volatile fields (#940).
/// A no-op when none were found, so the gauges count only cache-leaking requests.
pub fn record_volatile_system(fields: u64) {
    if fields == 0 {
        return;
    }
    VOLATILE_SYSTEM_REQUESTS.fetch_add(1, Ordering::Relaxed);
    VOLATILE_FIELDS_DETECTED.fetch_add(fields, Ordering::Relaxed);
}

/// Record one request whose system prompt had `fields` volatile values relocated
/// to the uncached tail (#974). A no-op when none moved, so the gauges count only
/// requests the relocate actually rewrote.
pub fn record_volatile_relocated(fields: u64) {
    if fields == 0 {
        return;
    }
    VOLATILE_RELOCATE_REQUESTS.fetch_add(1, Ordering::Relaxed);
    VOLATILE_FIELDS_RELOCATED.fetch_add(fields, Ordering::Relaxed);
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
    /// Prompt-cache breakpoints the proxy actively injected (#939), cumulative.
    /// Non-zero only when the opt-in `cache_breakpoint` mode added a `system`
    /// breakpoint for a client that set none — a pure cache win, not a regression.
    #[serde(default)]
    pub breakpoints_injected: u64,
    /// Requests whose unanchored system prompt leaked at least one volatile field
    /// (#940), cumulative. Measurement-only (the body is never mutated); non-zero
    /// only when the opt-in `cache_aligner` telemetry is enabled.
    #[serde(default)]
    pub volatile_system_requests: u64,
    /// Volatile fields detected across those requests (#940), cumulative.
    #[serde(default)]
    pub volatile_fields_detected: u64,
    /// Requests where the opt-in relocate (#974) moved volatile fields out of the
    /// cacheable prefix into the uncached tail, cumulative. Non-zero only with
    /// `cache_align_relocate` enabled — a pure cache win, not a regression.
    #[serde(default)]
    pub volatile_relocate_requests: u64,
    /// Volatile fields relocated across those requests (#974), cumulative.
    #[serde(default)]
    pub volatile_fields_relocated: u64,
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
        breakpoints_injected: BREAKPOINTS_INJECTED.load(Ordering::Relaxed),
        volatile_system_requests: VOLATILE_SYSTEM_REQUESTS.load(Ordering::Relaxed),
        volatile_fields_detected: VOLATILE_FIELDS_DETECTED.load(Ordering::Relaxed),
        volatile_relocate_requests: VOLATILE_RELOCATE_REQUESTS.load(Ordering::Relaxed),
        volatile_fields_relocated: VOLATILE_FIELDS_RELOCATED.load(Ordering::Relaxed),
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
