//! Prompt-cache miss attribution (#986, cache-economics telemetry).
//!
//! The proxy already keeps the client-cached prefix byte-stable (#448) and can
//! re-seed a leaner one on a cold resume (#480). What it could not yet *measure*
//! is **why** a turn fails to hit the provider prompt-cache — the single most
//! actionable cache signal. There are only two causes, and they want opposite
//! fixes:
//!
//! - **TTL lapse** — the cacheable prefix is byte-identical to last turn, but the
//!   idle gap exceeded the provider's cache TTL, so the entry expired. The fix is
//!   the cold-prefix repack (#480) / longer TTL, never a prefix change.
//! - **Prefix change** — the cacheable prefix is *different* from last turn, so
//!   the provider re-writes from the first changed byte regardless of timing. The
//!   fix is to stop mutating the prefix (a moving system prompt, an edited earlier
//!   turn, volatile fields — see the cache-aligner #940/#974).
//!
//! This module classifies every anchored turn (`cached > 0`) into one of four
//! outcomes by comparing the `cached_prefix_hash` and idle time against the
//! conversation's previous turn, and exposes cumulative gauges on `/status`. It
//! is **measurement-only** — the request body is never touched — and gated behind
//! the opt-in `proxy.cache_policy`, so a default proxy pays nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cold_prefix;

/// Hard cap on tracked conversations so a long-lived proxy can't grow the
/// last-prefix map without bound; the oldest entry is evicted past this.
const MAX_TRACKED: usize = 4096;

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

/// Previous-turn record for one conversation: the cacheable-prefix hash and the
/// Unix-seconds timestamp it was last seen.
#[derive(Debug, Clone, Copy)]
struct PrefixState {
    prefix_hash: u64,
    last_touch: u64,
}

static COLD_STARTS: AtomicU64 = AtomicU64::new(0);
static WARM_REUSES: AtomicU64 = AtomicU64::new(0);
static TTL_LAPSES: AtomicU64 = AtomicU64::new(0);
static PREFIX_CHANGES: AtomicU64 = AtomicU64::new(0);

fn store() -> &'static Mutex<HashMap<u64, PrefixState>> {
    static STORE: OnceLock<Mutex<HashMap<u64, PrefixState>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Pure classification of a turn's cache outcome. `prev` is the conversation's
/// previous `(prefix_hash, last_touch)`, `curr_hash` this turn's cacheable-prefix
/// hash, `now`/`ttl_secs` the idle clock. Pure (no globals, no I/O) so the
/// TTL-vs-prefix decision is unit-tested independently of the live store.
#[must_use]
pub fn classify(prev: Option<(u64, u64)>, curr_hash: u64, now: u64, ttl_secs: u64) -> CacheOutcome {
    match prev {
        None => CacheOutcome::ColdStart,
        Some((prev_hash, last_touch)) => {
            if prev_hash != curr_hash {
                CacheOutcome::PrefixChange
            } else if now.saturating_sub(last_touch) > ttl_secs {
                CacheOutcome::TtlLapse
            } else {
                CacheOutcome::WarmReuse
            }
        }
    }
}

/// Estimated reuse count for a conversation. Returns the global warm_reuse
/// count as a rough proxy — individual per-conversation tracking would
/// require extending PrefixState. Global warm_reuses / total_anchored gives
/// the average reuse rate across all conversations.
#[must_use]
pub fn estimated_reuse_rate() -> u32 {
    let warm = WARM_REUSES.load(Ordering::Relaxed);
    let total = warm
        + COLD_STARTS.load(Ordering::Relaxed)
        + TTL_LAPSES.load(Ordering::Relaxed)
        + PREFIX_CHANGES.load(Ordering::Relaxed);
    if total == 0 {
        return 5; // conservative default: assume 5 reuses
    }
    // Average reuse: warm / (total - warm) capped at u32
    let non_warm = total.saturating_sub(warm).max(1);
    (warm / non_warm).min(u32::MAX as u64) as u32
}

fn bump(outcome: CacheOutcome) {
    let counter = match outcome {
        CacheOutcome::ColdStart => &COLD_STARTS,
        CacheOutcome::WarmReuse => &WARM_REUSES,
        CacheOutcome::TtlLapse => &TTL_LAPSES,
        CacheOutcome::PrefixChange => &PREFIX_CHANGES,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn evict_oldest(map: &mut HashMap<u64, PrefixState>) {
    if let Some(oldest) = map
        .iter()
        .min_by_key(|(_, s)| s.last_touch)
        .map(|(k, _)| *k)
    {
        map.remove(&oldest);
    }
}

/// Attribute this request's cache outcome and record it, updating the
/// conversation's last-seen prefix baseline for the next turn. Only anchored
/// turns (`cached > 0`) are attributable; an unanchored turn returns `None`
/// (the cache-aligner telemetry #940 covers "client never anchors"). The caller
/// owns the opt-in gate — this only runs when `proxy.cache_policy` is enabled.
pub fn record_request(messages: &[Value], cached: usize) -> Option<CacheOutcome> {
    let conv_key = cold_prefix::conversation_key(messages)?;
    let curr_hash = cold_prefix::cached_prefix_hash(messages, cached)?;
    let ttl = cold_prefix::resolved_ttl_secs(messages, cached).unwrap_or(0);
    let now = now_secs();

    let outcome = {
        let mut map = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = map.get(&conv_key).map(|s| (s.prefix_hash, s.last_touch));
        let outcome = classify(prev, curr_hash, now, ttl);
        map.insert(
            conv_key,
            PrefixState {
                prefix_hash: curr_hash,
                last_touch: now,
            },
        );
        if map.len() > MAX_TRACKED {
            evict_oldest(&mut map);
        }
        outcome
    };
    bump(outcome);
    Some(outcome)
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
    use serde_json::json;

    #[test]
    fn classify_distinguishes_ttl_lapse_from_prefix_change() {
        // No prior turn → cold start.
        assert_eq!(classify(None, 1, 100, 300), CacheOutcome::ColdStart);
        // Same prefix, within TTL → warm reuse (should hit).
        assert_eq!(
            classify(Some((1, 100)), 1, 350, 300),
            CacheOutcome::WarmReuse
        );
        // Same prefix, idle past TTL → ttl lapse.
        assert_eq!(
            classify(Some((1, 100)), 1, 500, 300),
            CacheOutcome::TtlLapse
        );
        // Different prefix → prefix change, regardless of timing.
        assert_eq!(
            classify(Some((1, 100)), 2, 110, 300),
            CacheOutcome::PrefixChange
        );
        // Different prefix wins even past TTL (the mutation is the root cause).
        assert_eq!(
            classify(Some((1, 100)), 2, 9999, 300),
            CacheOutcome::PrefixChange
        );
    }

    fn anchored(first_text: &str) -> Vec<Value> {
        vec![
            json!({"role": "user", "content": [
                {"type": "text", "text": first_text, "cache_control": {"type": "ephemeral"}}
            ]}),
            json!({"role": "assistant", "content": "ok"}),
        ]
    }

    #[test]
    fn unanchored_turn_is_not_attributed() {
        let msgs = anchored("unanchored-attribution-test");
        // cached == 0: nothing anchored to attribute.
        assert_eq!(record_request(&msgs, 0), None);
    }

    #[test]
    fn first_anchored_turn_is_cold_start_then_warm() {
        let msgs = anchored("cold-then-warm-attribution-test");
        assert_eq!(record_request(&msgs, 1), Some(CacheOutcome::ColdStart));
        // Immediately again (idle ~0, prefix identical) → warm reuse.
        assert_eq!(record_request(&msgs, 1), Some(CacheOutcome::WarmReuse));
    }

    #[test]
    fn prefix_change_detected_with_stable_head() {
        // Stable head (conversation key), but the cached prefix spans two messages
        // and the second one changes — a true mid-prefix mutation.
        let head = json!({"role": "user", "content": [
            {"type": "text", "text": "stable-head-attribution", "cache_control": {"type": "ephemeral"}}
        ]});
        let v1 = vec![
            head.clone(),
            json!({"role": "assistant", "content": "answer one"}),
        ];
        let v2 = vec![
            head,
            json!({"role": "assistant", "content": "answer two CHANGED"}),
        ];

        assert_eq!(record_request(&v1, 2), Some(CacheOutcome::ColdStart));
        assert_eq!(record_request(&v2, 2), Some(CacheOutcome::PrefixChange));
    }

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
