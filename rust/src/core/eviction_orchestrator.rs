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

#[derive(Default)]
struct EvictionRegistry {
    targets: Vec<std::sync::Weak<EvictionOrchestrator>>,
}

impl EvictionRegistry {
    fn register(&mut self, target: &Arc<EvictionOrchestrator>) {
        self.targets.retain(|target| target.strong_count() > 0);
        self.targets.push(Arc::downgrade(target));
    }

    fn live_targets(&mut self) -> Vec<Arc<EvictionOrchestrator>> {
        let live = self
            .targets
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        self.targets.retain(|target| target.strong_count() > 0);
        live
    }
}

static TARGETS: std::sync::LazyLock<Mutex<EvictionRegistry>> =
    std::sync::LazyLock::new(|| Mutex::new(EvictionRegistry::default()));

/// Register a server-local eviction target without extending its lifetime.
pub fn register(target: &Arc<EvictionOrchestrator>) {
    TARGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .register(target);
}

/// Fan process-wide RSS pressure out to every live server-local cache.
/// Returns whether any eviction target actually reclaimed resident memory.
pub fn on_memory_pressure(level: memory_guard::PressureLevel) -> bool {
    let live = TARGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live_targets();

    let mut made_progress = false;
    for target in live {
        made_progress |= target.on_pressure(level);
    }

    // These caches are process-global rather than owned by a server target.
    if level >= memory_guard::PressureLevel::Hard {
        let content_bytes = super::content_cache::memory_usage_bytes();
        let ann_bytes = super::ann_cache::memory_usage_bytes();
        super::content_cache::clear();
        super::ann_cache::clear();
        let search_dropped = super::search_index::clear_resident();
        super::graph_cache::invalidate(None);
        made_progress |= content_bytes > 0 || ann_bytes > 0 || search_dropped;
    }

    made_progress
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
    pub(crate) fn on_pressure(&self, level: memory_guard::PressureLevel) -> bool {
        if level == memory_guard::PressureLevel::Normal {
            return false;
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

        // #685: the homeostasis controller reasons over *cache-token*
        // utilization, but RSS pressure can come from structures the token
        // heuristic cannot see (ANN/HNSW graph, resident trigram + graph
        // indices, transient build state). At Hard/Critical RSS the guard's
        // signal must win: enforce a floor action so eviction always reaches
        // the index caches instead of returning `None` because the session
        // cache happens to be small.
        let action = floor_action_for_rss(level, action);

        if action == HomeostasisAction::None {
            return false;
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
        pressure_reduced
    }

    fn execute_action(&self, action: &HomeostasisAction, bm25_bytes: usize) -> bool {
        match action {
            HomeostasisAction::None => false,

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
                // #685: the ANN/HNSW graph, resident trigram indices and the
                // materialized graph indexes were invisible to eviction — on
                // embedding-heavy sessions they dominated RSS while the
                // guardian could only trim the (small) session cache.
                let ann_freed = super::ann_cache::memory_usage_bytes();
                super::ann_cache::clear();
                super::search_index::clear_resident();
                super::graph_cache::invalidate(None);
                let trimmed = self.try_write_cache(SessionCache::trim_compressed_outputs);
                memory_guard::jemalloc_purge();
                tracing::info!(
                    "[eviction] unloaded indices (bm25={:.1}MB + content={:.1}MB + ann={:.1}MB freed, \
                     search/graph residents dropped, {trimmed} outputs trimmed)",
                    bm25_bytes as f64 / 1_048_576.0,
                    content_freed as f64 / 1_048_576.0,
                    ann_freed as f64 / 1_048_576.0,
                );
                bm25_bytes > 0 || content_freed > 0 || ann_freed > 0 || trimmed > 0
            }

            HomeostasisAction::EvictProtected { target_tokens } => {
                let before = self.try_read_cache_tokens();
                self.try_write_cache(|cache| cache.evict_to_budget(*target_tokens));
                let after = self.try_read_cache_tokens();
                memory_guard::jemalloc_purge();
                tracing::info!(
                    "[eviction] evicted protected entries to budget {target_tokens} tokens"
                );
                after < before
            }

            HomeostasisAction::EmergencyDrop => {
                let cleared = self.try_write_cache(SessionCache::clear);
                let content_freed = super::content_cache::memory_usage_bytes();
                let ann_freed = super::ann_cache::memory_usage_bytes();
                super::bm25_cache::unload(&self.bm25_cache);
                super::content_cache::clear();
                // #685: emergency must reach every resident structure.
                super::ann_cache::clear();
                super::search_index::clear_resident();
                super::graph_cache::invalidate(None);
                memory_guard::jemalloc_purge();
                tracing::warn!(
                    "[eviction] EMERGENCY: cleared {cleared} cache entries + unloaded all indices \
                     (bm25, content, ann, search, graph)"
                );
                cleared > 0 || bm25_bytes > 0 || content_freed > 0 || ann_freed > 0
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

/// RSS floor (#685): guarantee a minimum eviction strength for a given guard
/// pressure level, regardless of the token-based homeostasis verdict. Token
/// utilization measures only the session cache; the structures that actually
/// blew up in #685 (HNSW graph, resident indices, build transients) are
/// invisible to it — Hard/Critical RSS must always at least unload indices.
fn floor_action_for_rss(
    level: memory_guard::PressureLevel,
    action: HomeostasisAction,
) -> HomeostasisAction {
    let rank = |a: &HomeostasisAction| match a {
        HomeostasisAction::None => 0u8,
        HomeostasisAction::TrimOutputs => 1,
        HomeostasisAction::EvictProbationary { .. } => 2,
        HomeostasisAction::UnloadIndices => 3,
        HomeostasisAction::EvictProtected { .. } => 4,
        HomeostasisAction::EmergencyDrop => 5,
    };
    let floor = match level {
        memory_guard::PressureLevel::Critical => HomeostasisAction::EmergencyDrop,
        memory_guard::PressureLevel::Hard => HomeostasisAction::UnloadIndices,
        // Soft/Medium RSS: trust the graduated token-based controller, but do
        // not let real pressure end in a no-op.
        memory_guard::PressureLevel::Soft | memory_guard::PressureLevel::Medium => {
            HomeostasisAction::TrimOutputs
        }
        memory_guard::PressureLevel::Normal => HomeostasisAction::None,
    };
    if rank(&action) >= rank(&floor) {
        action
    } else {
        floor
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
    fn registry_fans_out_and_prunes_dropped_targets() {
        // #685/#975-class: on_pressure(Critical) floors to EmergencyDrop, which
        // clears the process-wide ann_cache — hold its cross-module test lock
        // so a concurrent ann_cache test can't observe a wiped cache mid-assertion.
        let _ann_lock = crate::core::ann_cache::test_lock();
        let first = Arc::new(make_orchestrator());
        let second = Arc::new(make_orchestrator());
        let first_cache = first.cache.clone();
        let second_cache = second.cache.clone();
        first_cache
            .blocking_write()
            .store("/first.rs", "fn first() {}");
        second_cache
            .blocking_write()
            .store("/second.rs", "fn second() {}");

        let mut registry = EvictionRegistry::default();
        registry.register(&first);
        registry.register(&second);
        let live = registry.live_targets();
        assert_eq!(live.len(), 2);
        for target in live {
            target.on_pressure(memory_guard::PressureLevel::Critical);
        }
        assert_eq!(first_cache.blocking_read().total_cached_tokens(), 0);
        assert_eq!(second_cache.blocking_read().total_cached_tokens(), 0);

        drop(second);
        assert_eq!(registry.live_targets().len(), 1);
        assert_eq!(registry.targets.len(), 1);
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
        // See `registry_fans_out_and_prunes_dropped_targets` above: EmergencyDrop
        // clears the shared ann_cache, so hold its cross-module test lock.
        let _ann_lock = crate::core::ann_cache::test_lock();
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
    fn rss_floor_escalates_weak_actions_under_hard_pressure() {
        // Session cache nearly empty → controller says None; Hard RSS must
        // still unload indices (#685: token heuristic blind to index RAM).
        let floored =
            floor_action_for_rss(memory_guard::PressureLevel::Hard, HomeostasisAction::None);
        assert_eq!(floored, HomeostasisAction::UnloadIndices);

        let floored = floor_action_for_rss(
            memory_guard::PressureLevel::Critical,
            HomeostasisAction::TrimOutputs,
        );
        assert_eq!(floored, HomeostasisAction::EmergencyDrop);

        let floored =
            floor_action_for_rss(memory_guard::PressureLevel::Soft, HomeostasisAction::None);
        assert_eq!(floored, HomeostasisAction::TrimOutputs);
    }

    #[test]
    fn rss_floor_keeps_stronger_controller_actions() {
        // The controller may already demand more than the floor — keep it.
        let strong = HomeostasisAction::EvictProtected {
            target_tokens: 1_000,
        };
        let kept = floor_action_for_rss(memory_guard::PressureLevel::Hard, strong.clone());
        assert_eq!(kept, strong);

        let normal =
            floor_action_for_rss(memory_guard::PressureLevel::Normal, HomeostasisAction::None);
        assert_eq!(normal, HomeostasisAction::None);
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
