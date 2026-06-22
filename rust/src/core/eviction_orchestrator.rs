//! Connects the RSS-based memory_guard to real cache eviction via HomeostasisController.
//!
//! The orchestrator bridges two systems:
//! - `memory_guard`: monitors process RSS and reports PressureLevel (Normal..Critical)
//! - `homeostasis`: decides which eviction action to take based on token utilization
//!
//! On each pressure callback, the orchestrator queries current cache utilization,
//! feeds it to HomeostasisController, and executes the recommended action.

use std::sync::{Arc, Mutex};

use super::bm25_cache::SharedBm25Cache;
use super::cache::SessionCache;
use super::homeostasis::{HomeostasisAction, HomeostasisController};
use super::memory_guard;

type SharedCache = Arc<tokio::sync::RwLock<SessionCache>>;

pub struct EvictionOrchestrator {
    cache: SharedCache,
    bm25_cache: SharedBm25Cache,
    controller: Mutex<HomeostasisController>,
    token_budget: usize,
}

impl EvictionOrchestrator {
    pub fn new(cache: SharedCache, bm25_cache: SharedBm25Cache) -> Self {
        let token_budget = super::cache::max_cache_tokens();
        Self {
            cache,
            bm25_cache,
            controller: Mutex::new(HomeostasisController::new(token_budget)),
            token_budget,
        }
    }

    /// Called by the memory_guard thread when pressure is detected.
    /// Runs on the guardian thread — must not block on async locks for too long.
    pub fn on_pressure(&self, level: memory_guard::PressureLevel) {
        if level == memory_guard::PressureLevel::Normal {
            return;
        }

        let current_tokens = self.try_read_cache_tokens();
        let bm25_bytes = super::bm25_cache::memory_usage(&self.bm25_cache);

        let effective_tokens = if bm25_bytes > 0 {
            current_tokens + bm25_bytes / 4
        } else {
            current_tokens
        };

        let action = {
            let mut ctrl = self
                .controller
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ctrl.evaluate(effective_tokens)
        };

        if action == HomeostasisAction::None {
            return;
        }

        tracing::info!(
            "[eviction] pressure={level:?} tokens={current_tokens}/{} bm25={:.1}MB action={action:?}",
            self.token_budget,
            bm25_bytes as f64 / 1_048_576.0,
        );

        let pressure_reduced = self.execute_action(&action, bm25_bytes);

        let mut ctrl = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ctrl.report_outcome(pressure_reduced);
    }

    fn execute_action(&self, action: &HomeostasisAction, bm25_bytes: usize) -> bool {
        match action {
            HomeostasisAction::None => true,

            HomeostasisAction::TrimOutputs => {
                let trimmed = self.try_write_cache(SessionCache::trim_compressed_outputs);
                tracing::info!("[eviction] trimmed compressed outputs from {trimmed} entries");
                trimmed > 0
            }

            HomeostasisAction::EvictProbationary { .. } => {
                let evicted = self.try_write_cache(|cache| {
                    let n = cache.evict_probationary();
                    cache.trim_shared_blocks();
                    n
                });
                tracing::info!("[eviction] evicted {evicted} probationary entries");
                evicted > 0
            }

            HomeostasisAction::UnloadIndices => {
                if bm25_bytes > 0 {
                    super::bm25_cache::unload(&self.bm25_cache);
                }
                let content_freed = super::content_cache::memory_usage_bytes();
                super::content_cache::clear();
                let trimmed = self.try_write_cache(SessionCache::trim_compressed_outputs);
                memory_guard::jemalloc_purge();
                tracing::info!(
                    "[eviction] unloaded indices (bm25={:.1}MB + content={:.1}MB freed, {trimmed} outputs trimmed)",
                    bm25_bytes as f64 / 1_048_576.0,
                    content_freed as f64 / 1_048_576.0,
                );
                bm25_bytes > 0 || content_freed > 0 || trimmed > 0
            }

            HomeostasisAction::EvictProtected { target_tokens } => {
                self.try_write_cache(|cache| cache.evict_to_budget(*target_tokens));
                memory_guard::jemalloc_purge();
                tracing::info!(
                    "[eviction] evicted protected entries to budget {target_tokens} tokens"
                );
                true
            }

            HomeostasisAction::EmergencyDrop => {
                let cleared = self.try_write_cache(SessionCache::clear);
                super::bm25_cache::unload(&self.bm25_cache);
                super::content_cache::clear();
                memory_guard::jemalloc_purge();
                tracing::warn!(
                    "[eviction] EMERGENCY: cleared {cleared} cache entries + unloaded all indices"
                );
                true
            }
        }
    }

    /// Try to read current token count without blocking.
    /// Falls back to budget (assumes full) if the lock is contended.
    fn try_read_cache_tokens(&self) -> usize {
        match self.cache.try_read() {
            Ok(guard) => guard.total_cached_tokens(),
            Err(_) => self.token_budget,
        }
    }

    /// Try to write to the cache. Returns default value if lock is contended.
    fn try_write_cache<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut SessionCache) -> R,
        R: Default,
    {
        if let Ok(mut guard) = self.cache.try_write() {
            f(&mut guard)
        } else {
            tracing::debug!("[eviction] cache write lock contended, skipping");
            R::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_orchestrator() -> EvictionOrchestrator {
        let cache = Arc::new(tokio::sync::RwLock::new(SessionCache::new()));
        let bm25_cache: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        EvictionOrchestrator::new(cache, bm25_cache)
    }

    #[test]
    fn normal_pressure_is_noop() {
        let orch = make_orchestrator();
        orch.on_pressure(memory_guard::PressureLevel::Normal);
    }

    #[test]
    fn soft_pressure_with_empty_cache_is_noop() {
        let orch = make_orchestrator();
        orch.on_pressure(memory_guard::PressureLevel::Soft);
    }

    #[test]
    fn emergency_clears_cache() {
        let cache = Arc::new(tokio::sync::RwLock::new(SessionCache::new()));
        {
            let mut c = cache.blocking_write();
            c.store("/a.rs", "fn a() {}");
            c.store("/b.rs", "fn b() {}");
        }
        let bm25: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let orch = EvictionOrchestrator {
            cache: cache.clone(),
            bm25_cache: bm25,
            controller: Mutex::new(HomeostasisController::new(100)),
            token_budget: 100,
        };

        orch.execute_action(&HomeostasisAction::EmergencyDrop, 0);
        let c = cache.blocking_read();
        assert_eq!(c.total_cached_tokens(), 0);
    }

    #[test]
    fn trim_outputs_clears_compressed() {
        let cache = Arc::new(tokio::sync::RwLock::new(SessionCache::new()));
        {
            let mut c = cache.blocking_write();
            c.store("/a.rs", "fn main() {}");
            c.set_compressed("/a.rs", "map", "compressed map".to_string());
        }
        let bm25: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let orch = EvictionOrchestrator {
            cache: cache.clone(),
            bm25_cache: bm25,
            controller: Mutex::new(HomeostasisController::new(100_000)),
            token_budget: 100_000,
        };

        let result = orch.execute_action(&HomeostasisAction::TrimOutputs, 0);
        assert!(result);
        let c = cache.blocking_read();
        assert!(c.get_compressed("/a.rs", "map").is_none());
    }

    #[test]
    fn evict_probationary_removes_single_reads() {
        let cache = Arc::new(tokio::sync::RwLock::new(SessionCache::new()));
        {
            let mut c = cache.blocking_write();
            c.store("/once.rs", "fn once() {}");
            c.store("/twice.rs", "fn twice() {}");
            c.store("/twice.rs", "fn twice() {}"); // second read → read_count=2
        }
        let bm25: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let orch = EvictionOrchestrator {
            cache: cache.clone(),
            bm25_cache: bm25,
            controller: Mutex::new(HomeostasisController::new(100_000)),
            token_budget: 100_000,
        };

        let result = orch.execute_action(
            &HomeostasisAction::EvictProbationary { target_tokens: 0 },
            0,
        );
        assert!(result);
        let c = cache.blocking_read();
        assert!(c.get("/once.rs").is_none());
        assert!(c.get("/twice.rs").is_some());
    }
}
