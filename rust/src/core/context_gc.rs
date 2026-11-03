use crate::core::cache::SessionCache;

const PRESSURE_THRESHOLD_TOKENS: usize = 50_000;
const TARGET_AFTER_GC_TOKENS: usize = 35_000;

#[allow(dead_code)]
pub struct GcResult {
    pub tokens_freed: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

pub(crate) fn should_run_gc(cache: &SessionCache) -> bool {
    cache.total_cached_tokens() > PRESSURE_THRESHOLD_TOKENS
}

pub(crate) fn run_gc(cache: &mut SessionCache) -> GcResult {
    let tokens_before = cache.total_cached_tokens();
    if tokens_before <= PRESSURE_THRESHOLD_TOKENS {
        return GcResult {
            tokens_freed: 0,
            tokens_before,
            tokens_after: tokens_before,
        };
    }

    cache.evict_to_budget(TARGET_AFTER_GC_TOKENS);
    let tokens_after = cache.total_cached_tokens();

    GcResult {
        tokens_freed: tokens_before.saturating_sub(tokens_after),
        tokens_before,
        tokens_after,
    }
}

pub fn maybe_gc(cache: &mut SessionCache) -> Option<GcResult> {
    if should_run_gc(cache) {
        Some(run_gc(cache))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_does_not_run_below_threshold() {
        let cache = SessionCache::new();
        assert!(!should_run_gc(&cache));
    }
}
