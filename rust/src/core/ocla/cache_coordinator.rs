//! Coordinator for the L1-to-L3 generalized delivery cache lookup chain.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::cache_tiers::{L1ProcessCache, L2DaemonCache, L3DiskCache};
use super::cache_types::{CacheKey, CacheValidator, DeliveryEntryV2, DeliveryStatsV2};

static GLOBAL_CACHE: OnceLock<BuiltinCacheCoordinator> = OnceLock::new();

/// Returns the process-global cache coordinator, lazily initialized from
/// `[cache]`.
pub fn materialized_cache() -> &'static BuiltinCacheCoordinator {
    GLOBAL_CACHE.get_or_init(|| {
        let config = crate::core::config::Config::load();
        let cache_cfg = &config.cache;
        let l3 = L3DiskCache::open(
            crate::core::data_dir::lean_ctx_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/lean-ctx-cache")),
        )
        .unwrap_or_else(|_| {
            L3DiskCache::open("/tmp/lean-ctx-cache-fallback")
                .expect("fallback L3 cache must initialize")
        });
        l3.startup_validate(
            std::time::Duration::from_secs(cache_cfg.l1_ttl_secs.saturating_mul(6)),
            cache_cfg.l3_max_bytes,
        );
        BuiltinCacheCoordinator::new(
            L1ProcessCache::new(std::time::Duration::from_secs(cache_cfg.l1_ttl_secs)),
            L2DaemonCache::new(
                cache_cfg.l2_max_entries,
                std::time::Duration::from_secs(cache_cfg.l2_ttl_secs),
            ),
            l3,
        )
    })
}

/// Coordinates lookups and writes across the generalized delivery cache tiers.
pub trait CacheCoordinator {
    /// Checks every tier for a fresh entry matching `key` and `validator`.
    fn check(&self, key: &CacheKey, validator: &CacheValidator) -> Option<DeliveryEntryV2>;

    /// Records a newly materialized entry in every cache tier.
    fn record(&self, entry: DeliveryEntryV2);

    /// Returns a snapshot of cumulative cache activity.
    fn stats(&self) -> DeliveryStatsV2;

    /// Checks multiple keys in input order.
    fn batch_check(&self, requests: &[(CacheKey, CacheValidator)]) -> Vec<Option<DeliveryEntryV2>> {
        requests
            .iter()
            .map(|(key, validator)| self.check(key, validator))
            .collect()
    }
}

#[derive(Debug, Default)]
struct CacheCounters {
    l1_hits: AtomicU64,
    l2_hits: AtomicU64,
    l3_hits: AtomicU64,
    misses: AtomicU64,
    materializations: AtomicU64,
    references_served: AtomicU64,
    tokens_saved: AtomicU64,
    evictions: AtomicU64,
    expired: AtomicU64,
}

impl CacheCounters {
    fn snapshot(&self) -> DeliveryStatsV2 {
        DeliveryStatsV2 {
            l1_hits: self.l1_hits.load(Ordering::Relaxed),
            l2_hits: self.l2_hits.load(Ordering::Relaxed),
            l3_hits: self.l3_hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            materializations: self.materializations.load(Ordering::Relaxed),
            references_served: self.references_served.load(Ordering::Relaxed),
            tokens_saved: self.tokens_saved.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            expired: self.expired.load(Ordering::Relaxed),
        }
    }
}

/// Built-in coordinator that promotes L3 hits to L2 and L2 hits to L1.
#[derive(Debug)]
pub struct BuiltinCacheCoordinator {
    l1: L1ProcessCache,
    l2: L2DaemonCache,
    l3: L3DiskCache,
    counters: CacheCounters,
}

impl BuiltinCacheCoordinator {
    /// Creates a coordinator from independently configured cache tiers.
    pub fn new(l1: L1ProcessCache, l2: L2DaemonCache, l3: L3DiskCache) -> Self {
        Self {
            l1,
            l2,
            l3,
            counters: CacheCounters::default(),
        }
    }

    /// Returns the process-local cache tier.
    pub fn l1(&self) -> &L1ProcessCache {
        &self.l1
    }

    /// Returns the daemon-shared cache tier.
    pub fn l2(&self) -> &L2DaemonCache {
        &self.l2
    }

    /// Returns the disk-backed cache tier.
    pub fn l3(&self) -> &L3DiskCache {
        &self.l3
    }

    fn serve(&self, entry: DeliveryEntryV2) -> DeliveryEntryV2 {
        self.counters
            .references_served
            .fetch_add(1, Ordering::Relaxed);
        self.counters
            .tokens_saved
            .fetch_add(entry.token_count, Ordering::Relaxed);
        entry
    }

    fn valid(
        &self,
        entry: DeliveryEntryV2,
        key: &CacheKey,
        validator: &CacheValidator,
    ) -> Option<DeliveryEntryV2> {
        if entry.is_fresh_for(key, validator, epoch_ms()) {
            Some(entry)
        } else {
            self.counters.expired.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

impl CacheCoordinator for BuiltinCacheCoordinator {
    fn check(&self, key: &CacheKey, validator: &CacheValidator) -> Option<DeliveryEntryV2> {
        if let Some(entry) = self.l1.get(key) {
            if let Some(entry) = self.valid(entry, key, validator) {
                self.counters.l1_hits.fetch_add(1, Ordering::Relaxed);
                return Some(self.serve(entry));
            }
            self.l1.remove(key);
        }
        if let Some(entry) = self.l2.get(key) {
            if let Some(entry) = self.valid(entry, key, validator) {
                self.counters.l2_hits.fetch_add(1, Ordering::Relaxed);
                self.l1.insert(entry.clone());
                return Some(self.serve(entry));
            }
            self.l2.remove(key);
        }
        if let Some(entry) = self.l3.get(key) {
            if let Some(entry) = self.valid(entry, key, validator) {
                self.counters.l3_hits.fetch_add(1, Ordering::Relaxed);
                let _ = self.l2.insert(entry.clone());
                self.l1.insert(entry.clone());
                return Some(self.serve(entry));
            }
            let _ = self.l3.remove(key);
        }
        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn record(&self, entry: DeliveryEntryV2) {
        self.counters
            .materializations
            .fetch_add(1, Ordering::Relaxed);
        self.l1.insert(entry.clone());
        if self.l2.insert(entry.clone()).is_some() {
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }
        if self.l3.insert(entry).is_err() {
            self.counters.evictions.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> DeliveryStatsV2 {
        self.counters.snapshot()
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::ocla::cache_types::{
        AgentHost, CacheIdentity, CacheValidator, ContentHandleRef, DeliveryKind,
    };

    fn entry(name: &str) -> DeliveryEntryV2 {
        DeliveryEntryV2 {
            schema_version: 2,
            key: CacheKey(format!("cache:v1:file_read:{name}")),
            kind: DeliveryKind::FileRead,
            validator: CacheValidator::Immutable,
            handle: ContentHandleRef {
                algorithm: "blake3".into(),
                digest: "e".repeat(64),
                byte_len: 1,
                media_type: "text/plain".into(),
            },
            display_path: None,
            line_count: None,
            token_count: 5,
            producer: CacheIdentity {
                agent_id: "agent".into(),
                conversation_id: "conversation".into(),
                host: AgentHost::Codex,
            },
            created_at_epoch_ms: 0,
            expires_at_epoch_ms: u64::MAX,
        }
    }

    fn coordinator() -> (BuiltinCacheCoordinator, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let coord = BuiltinCacheCoordinator::new(
            L1ProcessCache::new(Duration::from_mins(1)),
            L2DaemonCache::new(8, Duration::from_mins(1)),
            L3DiskCache::open(dir.path()).expect("L3 open"),
        );
        (coord, dir)
    }

    #[test]
    fn coordinator_records_and_reads_from_l1() {
        let (coordinator, _dir) = coordinator();
        let entry = entry("l1_check");
        coordinator.record(entry.clone());
        assert_eq!(
            coordinator.check(&entry.key, &entry.validator),
            Some(entry.clone())
        );
        let stats = coordinator.stats();
        assert_eq!(stats.l1_hits, 1);
    }

    #[test]
    fn l2_hit_promotes_entry_to_l1() {
        let (coordinator, _dir) = coordinator();
        let entry = entry("l2_promote");
        coordinator.l2().insert(entry.clone());
        assert_eq!(
            coordinator.check(&entry.key, &entry.validator),
            Some(entry.clone())
        );
        assert!(coordinator.l1().get(&entry.key).is_some());
        assert_eq!(coordinator.stats().l2_hits, 1);
    }
    #[test]
    fn l3_hit_promotes_entry_to_both_memory_tiers() {
        let (coordinator, _dir) = coordinator();
        let entry = entry("l3");
        coordinator.l3().insert(entry.clone()).unwrap();
        assert_eq!(
            coordinator.check(&entry.key, &entry.validator),
            Some(entry.clone())
        );
        assert!(coordinator.l1().get(&entry.key).is_some());
        assert!(coordinator.l2().get(&entry.key).is_some());
        assert_eq!(coordinator.stats().l3_hits, 1);
    }

    #[test]
    fn batch_check_keeps_request_order() {
        let (coordinator, _dir) = coordinator();
        let hit = entry("hit");
        let miss = entry("miss");
        coordinator.record(hit.clone());
        let results = coordinator.batch_check(&[
            (hit.key.clone(), hit.validator.clone()),
            (miss.key.clone(), miss.validator.clone()),
        ]);
        assert_eq!(results, vec![Some(hit), None]);
    }
}
