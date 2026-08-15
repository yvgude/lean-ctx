use md5::{Digest, Md5};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// The single terminal reuse classification for one eligible cacheable read.
///
/// A request can pass through several cache layers, but it must be counted in
/// exactly one bucket.  In particular, a session-cache rendering that is later
/// collapsed by kernel dedup is an [`UnchangedStub`](Self::UnchangedStub), not
/// two independent hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseOutcome {
    Cold,
    DiskContentHit,
    RenderCacheHit,
    UnchangedStub,
    CrossFileRef,
    FreshBypass,
    Stale,
    VariantEvicted,
    PolicyBypass,
}

impl ReuseOutcome {
    const fn is_render_reuse(self) -> bool {
        matches!(
            self,
            Self::RenderCacheHit | Self::UnchangedStub | Self::CrossFileRef
        )
    }

    const fn is_ctx_read_eligible(self) -> bool {
        !matches!(self, Self::FreshBypass | Self::PolicyBypass)
    }
}

/// Snapshot for the three non-overlapping cache-reuse measurements.
///
/// `eligible_ctx_reads` intentionally excludes explicit fresh reads and
/// policy-disabled cache reads: neither was allowed to reuse a rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReuseSnapshot {
    pub cold: u64,
    pub disk_content_hits: u64,
    pub render_cache_hits: u64,
    pub unchanged_stubs: u64,
    pub cross_file_refs: u64,
    pub fresh_bypasses: u64,
    pub stale: u64,
    pub variant_evicted: u64,
    pub policy_bypasses: u64,
    pub eligible_ctx_reads: u64,
    pub reused_renderings: u64,
    pub eligible_search_reads: u64,
}

impl ReuseSnapshot {
    /// Percentage of eligible `ctx_read`s served by an already-rendered result.
    pub fn read_reuse_rate(&self) -> f64 {
        rate(self.reused_renderings, self.eligible_ctx_reads)
    }

    /// Percentage of eligible search file reads served by `content_cache`.
    pub fn disk_reuse_rate(&self) -> f64 {
        rate(self.disk_content_hits, self.eligible_search_reads)
    }
}

struct ReuseCounters {
    cold: AtomicU64,
    disk_content_hits: AtomicU64,
    render_cache_hits: AtomicU64,
    unchanged_stubs: AtomicU64,
    cross_file_refs: AtomicU64,
    fresh_bypasses: AtomicU64,
    stale: AtomicU64,
    variant_evicted: AtomicU64,
    policy_bypasses: AtomicU64,
    eligible_ctx_reads: AtomicU64,
    reused_renderings: AtomicU64,
    eligible_search_reads: AtomicU64,
}

impl ReuseCounters {
    const fn new() -> Self {
        Self {
            cold: AtomicU64::new(0),
            disk_content_hits: AtomicU64::new(0),
            render_cache_hits: AtomicU64::new(0),
            unchanged_stubs: AtomicU64::new(0),
            cross_file_refs: AtomicU64::new(0),
            fresh_bypasses: AtomicU64::new(0),
            stale: AtomicU64::new(0),
            variant_evicted: AtomicU64::new(0),
            policy_bypasses: AtomicU64::new(0),
            eligible_ctx_reads: AtomicU64::new(0),
            reused_renderings: AtomicU64::new(0),
            eligible_search_reads: AtomicU64::new(0),
        }
    }
}

static REUSE_COUNTERS: ReuseCounters = ReuseCounters::new();

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

fn bump_outcome(counters: &ReuseCounters, outcome: ReuseOutcome) {
    let counter = match outcome {
        ReuseOutcome::Cold => &counters.cold,
        ReuseOutcome::DiskContentHit => &counters.disk_content_hits,
        ReuseOutcome::RenderCacheHit => &counters.render_cache_hits,
        ReuseOutcome::UnchangedStub => &counters.unchanged_stubs,
        ReuseOutcome::CrossFileRef => &counters.cross_file_refs,
        ReuseOutcome::FreshBypass => &counters.fresh_bypasses,
        ReuseOutcome::Stale => &counters.stale,
        ReuseOutcome::VariantEvicted => &counters.variant_evicted,
        ReuseOutcome::PolicyBypass => &counters.policy_bypasses,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn record_ctx_read_outcome_into(counters: &ReuseCounters, outcome: ReuseOutcome) {
    bump_outcome(counters, outcome);
    if outcome.is_ctx_read_eligible() {
        counters.eligible_ctx_reads.fetch_add(1, Ordering::Relaxed);
        if outcome.is_render_reuse() {
            counters.reused_renderings.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Record exactly one terminal outcome for a completed `ctx_read`.
pub fn record_ctx_read_outcome(outcome: ReuseOutcome) {
    record_ctx_read_outcome_into(&REUSE_COUNTERS, outcome);
}

fn record_search_content_read_into(counters: &ReuseCounters, hit: bool) {
    counters
        .eligible_search_reads
        .fetch_add(1, Ordering::Relaxed);
    bump_outcome(
        counters,
        if hit {
            ReuseOutcome::DiskContentHit
        } else {
            ReuseOutcome::Cold
        },
    );
}

/// Record exactly one content-cache outcome for one eligible search file read.
pub fn record_search_content_read(hit: bool) {
    record_search_content_read_into(&REUSE_COUNTERS, hit);
}

/// Return a lock-free point-in-time view of reuse counters.
#[must_use]
pub fn reuse_snapshot() -> ReuseSnapshot {
    snapshot_from(&REUSE_COUNTERS)
}

fn snapshot_from(counters: &ReuseCounters) -> ReuseSnapshot {
    ReuseSnapshot {
        cold: counters.cold.load(Ordering::Relaxed),
        disk_content_hits: counters.disk_content_hits.load(Ordering::Relaxed),
        render_cache_hits: counters.render_cache_hits.load(Ordering::Relaxed),
        unchanged_stubs: counters.unchanged_stubs.load(Ordering::Relaxed),
        cross_file_refs: counters.cross_file_refs.load(Ordering::Relaxed),
        fresh_bypasses: counters.fresh_bypasses.load(Ordering::Relaxed),
        stale: counters.stale.load(Ordering::Relaxed),
        variant_evicted: counters.variant_evicted.load(Ordering::Relaxed),
        policy_bypasses: counters.policy_bypasses.load(Ordering::Relaxed),
        eligible_ctx_reads: counters.eligible_ctx_reads.load(Ordering::Relaxed),
        reused_renderings: counters.reused_renderings.load(Ordering::Relaxed),
        eligible_search_reads: counters.eligible_search_reads.load(Ordering::Relaxed),
    }
}

pub fn file_mtime(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub fn is_cache_entry_stale(path: &str, cached_mtime: Option<SystemTime>) -> bool {
    let current = file_mtime(path);
    match (cached_mtime, current) {
        // Both unavailable (e.g. WSL DrvFS): can't tell → assume fresh (conservative).
        (None, None) => false,
        // One side missing: metadata changed or appeared/disappeared → stale.
        (Some(_), None) | (None, Some(_)) => true,
        // `!=`, not `>`: a *backward* mtime (git checkout, touch -t, snapshot
        // restore) is just as much a content change as a forward one.
        (Some(cached), Some(current)) => current != cached,
    }
}

/// Files larger than this are not content-hashed for stub verification; the
/// mtime check alone decides. Keeps the stub fast-path O(small-file-read).
const VERIFY_HASH_CAP_BYTES: u64 = 8 * 1024 * 1024;

fn cache_verify_enabled() -> bool {
    std::env::var("LEAN_CTX_CACHE_VERIFY").map_or(true, |v| v != "0")
}

/// Staleness with content verification: like [`is_cache_entry_stale`], but when
/// the mtime claims "unchanged", additionally compares the md5 of the on-disk
/// content against the cached hash.
///
/// mtime alone cannot be trusted for *correctness*: same-second writes are
/// invisible on coarse-granularity filesystems (HFS+ 1s, FAT 2s) and mtimes can
/// be restored by tools. Serving an `[unchanged]` stub for changed content
/// would silently mislead the agent — the worst failure mode a context layer
/// can have. The extra disk read costs microseconds for typical source files;
/// the stub's token savings are unaffected. Opt out: `LEAN_CTX_CACHE_VERIFY=0`.
///
/// Note: entries whose stored content differs from disk by design (e.g. secret
/// redaction) hash differently and therefore never serve stubs — conservative
/// and correct.
pub fn is_cache_entry_stale_verified(
    path: &str,
    cached_mtime: Option<SystemTime>,
    cached_hash: &str,
) -> bool {
    if is_cache_entry_stale(path, cached_mtime) {
        return true;
    }
    if cached_hash.is_empty() || !cache_verify_enabled() {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        // Can't stat → never serve a stub on top of it.
        return true;
    };
    if meta.len() > VERIFY_HASH_CAP_BYTES {
        return false;
    }
    match std::fs::read(path) {
        // Hash the same view of the bytes that `store()` hashed (lossy UTF-8).
        Ok(bytes) => compute_md5(&String::from_utf8_lossy(&bytes)) != cached_hash,
        Err(_) => true,
    }
}

pub(super) fn compute_md5(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{
        ReuseCounters, ReuseOutcome, ReuseSnapshot, record_ctx_read_outcome_into,
        record_search_content_read_into, snapshot_from,
    };

    #[test]
    fn every_read_records_one_exclusive_terminal_outcome() {
        let counters = ReuseCounters::new();
        record_ctx_read_outcome_into(&counters, ReuseOutcome::RenderCacheHit);
        record_ctx_read_outcome_into(&counters, ReuseOutcome::UnchangedStub);
        record_ctx_read_outcome_into(&counters, ReuseOutcome::FreshBypass);
        record_ctx_read_outcome_into(&counters, ReuseOutcome::PolicyBypass);
        record_search_content_read_into(&counters, true);
        record_search_content_read_into(&counters, false);

        let snapshot = snapshot_from(&counters);
        assert_eq!(snapshot.render_cache_hits, 1);
        assert_eq!(snapshot.unchanged_stubs, 1);
        assert_eq!(snapshot.fresh_bypasses, 1);
        assert_eq!(snapshot.policy_bypasses, 1);
        assert_eq!(snapshot.disk_content_hits, 1);
        assert_eq!(snapshot.cold, 1);
        assert_eq!(snapshot.eligible_ctx_reads, 2);
        assert_eq!(snapshot.reused_renderings, 2);
        assert_eq!(snapshot.eligible_search_reads, 2);
    }

    #[test]
    fn reuse_rates_keep_read_and_disk_denominators_separate() {
        let metrics = ReuseSnapshot {
            reused_renderings: 3,
            eligible_ctx_reads: 4,
            disk_content_hits: 2,
            eligible_search_reads: 5,
            ..ReuseSnapshot::default()
        };

        assert_eq!(metrics.read_reuse_rate(), 75.0);
        assert_eq!(metrics.disk_reuse_rate(), 40.0);
    }

    #[test]
    fn only_render_reuse_outcomes_count_for_ctx_read_reuse() {
        assert!(ReuseOutcome::RenderCacheHit.is_render_reuse());
        assert!(ReuseOutcome::UnchangedStub.is_render_reuse());
        assert!(ReuseOutcome::CrossFileRef.is_render_reuse());
        assert!(!ReuseOutcome::DiskContentHit.is_render_reuse());
        assert!(!ReuseOutcome::FreshBypass.is_render_reuse());
        assert!(!ReuseOutcome::Stale.is_render_reuse());
    }

    #[test]
    fn bypasses_are_not_eligible_for_read_reuse() {
        assert!(!ReuseOutcome::FreshBypass.is_ctx_read_eligible());
        assert!(!ReuseOutcome::PolicyBypass.is_ctx_read_eligible());
        assert!(ReuseOutcome::Stale.is_ctx_read_eligible());
    }
}
