// SPDX-License-Identifier: Apache-2.0
//! Prompt-cache miss attribution counters (#986): the proxy classifies each
//! anchored turn (`proxy::cache_attribution`) and counts the outcome here; the
//! proxy's `/status` and the MCP server's metrics read the cumulative gauges.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// The cache outcome attributed to one anchored (`cached > 0`) request, by
/// comparing its cacheable prefix + idle time against the previous turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// First time this conversation's anchored prefix was seen — no prior turn to
    /// compare, so nothing is attributable (counts only as a baseline).
    ColdStart,
    /// Prefix byte-identical to last turn and within the TTL — the provider cache
    /// should hit. The healthy steady state.
    WarmReuse,
    /// Prefix byte-identical to last turn but idle past the TTL — the entry
    /// expired; a miss caused by time, not a prefix change.
    TtlLapse,
    /// Prefix differs from last turn — the provider re-writes from the first
    /// changed byte; a miss caused by a mutated prefix, regardless of timing.
    PrefixChange,
}

static COLD_STARTS: AtomicU64 = AtomicU64::new(0);
static WARM_REUSES: AtomicU64 = AtomicU64::new(0);
static TTL_LAPSES: AtomicU64 = AtomicU64::new(0);
static PREFIX_CHANGES: AtomicU64 = AtomicU64::new(0);

/// Count one attributed outcome.
pub(crate) fn record_outcome(outcome: CacheOutcome) {
    let counter = match outcome {
        CacheOutcome::ColdStart => &COLD_STARTS,
        CacheOutcome::WarmReuse => &WARM_REUSES,
        CacheOutcome::TtlLapse => &TTL_LAPSES,
        CacheOutcome::PrefixChange => &PREFIX_CHANGES,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Point-in-time view of the miss-attribution counters for `/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheAttribution {
    /// First-sighting anchored turns (baseline only, not a hit or a miss).
    pub cold_starts: u64,
    /// Turns whose prefix was stable and within TTL — the cache should hit.
    pub warm_reuses: u64,
    /// Misses caused by an expired entry on an otherwise-stable prefix.
    pub ttl_lapses: u64,
    /// Misses caused by a changed cacheable prefix (a mutated/edited prefix).
    pub prefix_changes: u64,
}

impl CacheAttribution {
    /// Provider prompt-cache reuse among turns that have a prior anchored
    /// prefix to compare. Cold starts establish that baseline, so they are not
    /// a hit or miss in this rate.
    #[must_use]
    pub fn provider_reuse_rate(&self) -> f64 {
        let eligible = self
            .warm_reuses
            .saturating_add(self.ttl_lapses)
            .saturating_add(self.prefix_changes);
        if eligible == 0 {
            0.0
        } else {
            self.warm_reuses as f64 * 100.0 / eligible as f64
        }
    }

    #[must_use]
    pub fn eligible_provider_requests(&self) -> u64 {
        self.warm_reuses
            .saturating_add(self.ttl_lapses)
            .saturating_add(self.prefix_changes)
    }
}

#[must_use]
pub fn snapshot() -> CacheAttribution {
    CacheAttribution {
        cold_starts: COLD_STARTS.load(Ordering::Relaxed),
        warm_reuses: WARM_REUSES.load(Ordering::Relaxed),
        ttl_lapses: TTL_LAPSES.load(Ordering::Relaxed),
        prefix_changes: PREFIX_CHANGES.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_reuse_excludes_cold_starts_from_denominator() {
        let attribution = CacheAttribution {
            cold_starts: 99,
            warm_reuses: 6,
            ttl_lapses: 2,
            prefix_changes: 4,
        };

        assert_eq!(attribution.eligible_provider_requests(), 12);
        assert_eq!(attribution.provider_reuse_rate(), 50.0);
    }
}
