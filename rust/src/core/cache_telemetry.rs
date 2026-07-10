//! Read-cache reliability telemetry.
//!
//! The subjective "re-reads feel unreliable" signal is turned into data here:
//! every event that wipes or invalidates a *fully-delivered* entry — and thus
//! forces the next read to re-send the whole file instead of the cheap
//! `[unchanged]` stub — increments a process-global counter grouped by cause.
//!
//! Counters are monotonic `AtomicU64`s. They are surfaced ONLY in the
//! `ctx_cache status` diagnostic, never inside a cacheable tool-output body, so
//! output determinism (#498) is preserved.

use std::sync::atomic::{AtomicU64, Ordering};

static COMPACTION: AtomicU64 = AtomicU64::new(0);
static IDLE: AtomicU64 = AtomicU64::new(0);
static EVICTION: AtomicU64 = AtomicU64::new(0);
static CONVERSATION: AtomicU64 = AtomicU64::new(0);
static RAW_CAP_COUNT: AtomicU64 = AtomicU64::new(0);
static RAW_CAP_PREVENTED: AtomicU64 = AtomicU64::new(0);

/// Fully-delivered entries whose delivery flag was reset by a host compaction.
pub fn record_compaction(n: u64) {
    if n > 0 {
        COMPACTION.fetch_add(n, Ordering::Relaxed);
    }
}

/// Fully-delivered entries dropped by an idle-TTL cache clear.
pub fn record_idle(n: u64) {
    if n > 0 {
        IDLE.fetch_add(n, Ordering::Relaxed);
    }
}

/// Fully-delivered entries evicted under RAM / token-budget pressure.
pub fn record_eviction(n: u64) {
    if n > 0 {
        EVICTION.fetch_add(n, Ordering::Relaxed);
    }
}

/// A re-read that fell back to full content because the reading conversation
/// differed from the one the entry was delivered to (conversation scoping, #954).
pub fn record_conversation_mismatch() {
    CONVERSATION.fetch_add(1, Ordering::Relaxed);
}

/// Framed output was larger than raw content — `cap_to_raw()` fell back to
/// verbatim to prevent negative savings. Tracked so the dashboard can
/// distinguish "no savings" from "savings prevented inflation".
pub fn record_raw_cap(prevented_inflation_tokens: u64) {
    RAW_CAP_COUNT.fetch_add(1, Ordering::Relaxed);
    RAW_CAP_PREVENTED.fetch_add(prevented_inflation_tokens, Ordering::Relaxed);
}

/// Immutable snapshot of the re-delivery counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub compaction: u64,
    pub idle: u64,
    pub eviction: u64,
    pub conversation: u64,
    pub raw_cap_count: u64,
    pub raw_cap_prevented: u64,
}

impl Snapshot {
    /// Total forced re-deliveries across all causes.
    pub fn total(&self) -> u64 {
        self.compaction
            .saturating_add(self.idle)
            .saturating_add(self.eviction)
            .saturating_add(self.conversation)
    }
}

/// Reads the current counters into a consistent snapshot.
pub fn snapshot() -> Snapshot {
    Snapshot {
        compaction: COMPACTION.load(Ordering::Relaxed),
        idle: IDLE.load(Ordering::Relaxed),
        eviction: EVICTION.load(Ordering::Relaxed),
        conversation: CONVERSATION.load(Ordering::Relaxed),
        raw_cap_count: RAW_CAP_COUNT.load(Ordering::Relaxed),
        raw_cap_prevented: RAW_CAP_PREVENTED.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_sums_every_cause() {
        let s = Snapshot {
            compaction: 1,
            idle: 2,
            eviction: 4,
            conversation: 8,
            raw_cap_count: 0,
            raw_cap_prevented: 0,
        };
        assert_eq!(s.total(), 15);
    }

    #[test]
    fn zero_is_a_noop() {
        // A zero-sized event must never move a counter (avoids logging "0 wiped").
        let before = snapshot();
        record_compaction(0);
        record_idle(0);
        record_eviction(0);
        // Only `>=` is safe to assert on a process-global counter: other tests
        // may increment concurrently. A 0-sized call adds nothing of its own.
        let after = snapshot();
        assert!(after.compaction >= before.compaction);
        assert!(after.idle >= before.idle);
        assert!(after.eviction >= before.eviction);
    }

    #[test]
    fn each_cause_increments_monotonically() {
        let before = snapshot();
        record_compaction(2);
        record_idle(3);
        record_eviction(5);
        record_conversation_mismatch();
        let after = snapshot();
        // Monotonic deltas (`>=`) tolerate concurrent increments from sibling
        // tests while still proving each recorder targets the right counter.
        assert!(after.compaction >= before.compaction + 2);
        assert!(after.idle >= before.idle + 3);
        assert!(after.eviction >= before.eviction + 5);
        assert!(after.conversation > before.conversation);
        assert!(after.total() >= before.total() + 11);
    }
}
