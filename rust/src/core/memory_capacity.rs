//! Single memory capacity manager (#995 Phase 2).
//!
//! One reclaim formula and one generic, archive-backed, hysteresis reclaim used
//! by every store. Replaces the duplicated `reclaim_target_capacity` and the
//! per-store hard drops (history drain, procedure/pattern truncate): eviction is
//! now lossless everywhere because the dropped tail is archived (and restorable)
//! before removal.

use serde::Serialize;
use std::cmp::Ordering;

use super::memory_archive::{ArchiveConfig, MemoryStore, archive_items};

/// Live count to settle at after a reclaim: drop `ceil(max * headroom_pct)`
/// items so a busy store keeps real headroom instead of churning right at its
/// cap. `headroom_pct = 0.25` reproduces the prior `max - ceil(max/4)` target
/// byte-for-byte.
pub fn reclaim_target(max: usize, headroom_pct: f32) -> usize {
    if max == 0 {
        return 0;
    }
    let pct = headroom_pct.clamp(0.0, 0.95);
    let drop = ((max as f32) * pct).ceil() as usize;
    max.saturating_sub(drop)
}

/// Whether a store at `len` should reclaim now. Hysteresis: trigger only at/above
/// the cap rather than continuously keeping N% free, so a store does not reclaim
/// on every write once it nears capacity.
pub fn should_reclaim(len: usize, max: usize, enabled: bool) -> bool {
    enabled && max > 0 && len >= max
}

/// How many items a [`reclaim_store`] would archive for a store at `len`, without
/// touching the store or the archive. Powers dry-run previews (#995 Phase 6).
pub fn reclaim_preview(len: usize, max: usize, headroom_pct: f32, enabled: bool) -> usize {
    if !should_reclaim(len, max, enabled) {
        return 0;
    }
    len.saturating_sub(reclaim_target(max, headroom_pct))
}

/// Generic, archive-backed, hysteresis reclaim.
///
/// When `items.len() >= max`, sort by `retention_cmp` (best-kept first) and
/// archive + drop the tail down to [`reclaim_target`]. The dropped items are
/// archived under `store`/`scope` *before* removal, so the reclaim is lossless
/// and restorable. Returns the archived items. No-op when disabled, under cap,
/// or `max == 0`.
pub fn reclaim_store<T, F>(
    store: MemoryStore,
    scope: Option<&str>,
    items: &mut Vec<T>,
    max: usize,
    headroom_pct: f32,
    enabled: bool,
    mut retention_cmp: F,
) -> Vec<T>
where
    T: Serialize,
    F: FnMut(&T, &T) -> Ordering,
{
    if !should_reclaim(items.len(), max, enabled) {
        return Vec::new();
    }
    let target = reclaim_target(max, headroom_pct);
    let drop_count = items.len().saturating_sub(target);
    if drop_count == 0 {
        return Vec::new();
    }

    // Rank a copy of the indices by retention (best-kept first); the worst
    // `drop_count` are evicted. Order-preserving: only the chosen indices are
    // removed, so the kept items keep their original relative order and a reclaim
    // never reshuffles the live store as a side effect. `sort_by` is stable, so
    // ties resolve to original order for deterministic eviction.
    let mut ranked: Vec<usize> = (0..items.len()).collect();
    ranked.sort_by(|&a, &b| retention_cmp(&items[a], &items[b]));
    let mut evict: Vec<usize> = ranked[target..].to_vec();
    evict.sort_unstable();

    let mut archived: Vec<T> = Vec::with_capacity(evict.len());
    for &idx in evict.iter().rev() {
        archived.push(items.remove(idx));
    }
    archived.reverse(); // restore original order for the archived payload

    if !archived.is_empty() {
        let _ = archive_items(store, scope, &archived, &ArchiveConfig::from_env());
    }
    archived
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde::Deserialize;

    fn with_temp_data_dir<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "lctx-capacity-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let out = f();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Item {
        rank: u32,
    }

    #[test]
    fn reclaim_target_matches_legacy_quarter_reclaim() {
        // The pre-#995 target was `max - ceil(max/4)`. headroom 0.25 must match
        // it exactly across a range of caps, including the awkward small ones.
        let legacy = |max: usize| max.saturating_sub(max.div_ceil(4));
        for max in [1usize, 2, 3, 4, 5, 6, 7, 8, 10, 100, 200, 1000] {
            assert_eq!(
                reclaim_target(max, 0.25),
                legacy(max),
                "mismatch at max={max}"
            );
        }
    }

    #[test]
    fn reclaim_target_zero_headroom_keeps_all() {
        assert_eq!(reclaim_target(200, 0.0), 200);
    }

    #[test]
    fn reclaim_target_is_clamped() {
        // Absurd headroom never drops the store below ~5%.
        assert!(reclaim_target(100, 9.9) >= 5);
    }

    #[test]
    fn should_reclaim_hysteresis() {
        assert!(!should_reclaim(99, 100, true), "under cap: no reclaim");
        assert!(should_reclaim(100, 100, true), "at cap: reclaim");
        assert!(should_reclaim(150, 100, true), "over cap: reclaim");
        assert!(!should_reclaim(150, 100, false), "disabled: no reclaim");
        assert!(!should_reclaim(150, 0, true), "max 0: no reclaim");
    }

    #[test]
    fn reclaim_store_is_lossless_and_keeps_best() {
        with_temp_data_dir(|| {
            // rank 0 = best kept (retention_cmp ascending by rank).
            let mut items: Vec<Item> = (0..8).map(|rank| Item { rank }).collect();
            let archived = reclaim_store(
                MemoryStore::Patterns,
                Some("p"),
                &mut items,
                8,
                0.25,
                true,
                |a, b| a.rank.cmp(&b.rank),
            );
            // 8 -> keep 6, archive 2.
            assert_eq!(items.len(), 6);
            assert_eq!(archived.len(), 2);
            // Lossless: union of kept + archived == original set.
            let mut all: Vec<u32> = items.iter().chain(&archived).map(|i| i.rank).collect();
            all.sort_unstable();
            assert_eq!(all, (0..8).collect::<Vec<_>>());
            // Worst (highest rank) were archived.
            assert_eq!(
                archived.iter().map(|i| i.rank).collect::<Vec<_>>(),
                vec![6, 7]
            );
        });
    }

    #[test]
    fn reclaim_store_noop_under_cap() {
        with_temp_data_dir(|| {
            let mut items: Vec<Item> = (0..3).map(|rank| Item { rank }).collect();
            let archived = reclaim_store(
                MemoryStore::History,
                Some("p"),
                &mut items,
                10,
                0.25,
                true,
                |a, b| a.rank.cmp(&b.rank),
            );
            assert!(archived.is_empty());
            assert_eq!(items.len(), 3);
        });
    }

    #[test]
    fn reclaim_store_respects_disabled() {
        with_temp_data_dir(|| {
            let mut items: Vec<Item> = (0..20).map(|rank| Item { rank }).collect();
            let archived = reclaim_store(
                MemoryStore::Procedures,
                Some("p"),
                &mut items,
                10,
                0.25,
                false,
                |a, b| a.rank.cmp(&b.rank),
            );
            assert!(archived.is_empty());
            assert_eq!(
                items.len(),
                20,
                "disabled reclaim leaves the store untouched"
            );
        });
    }
}
