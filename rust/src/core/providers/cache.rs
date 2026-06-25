use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

static PROVIDER_CACHE: std::sync::LazyLock<Mutex<ProviderCache>> =
    std::sync::LazyLock::new(|| Mutex::new(ProviderCache::new()));

struct CacheEntry {
    data: String,
    expires_at: Instant,
    #[allow(dead_code)]
    created_at: SystemTime,
    provider_id: String,
}

/// Per-provider cache statistics.
#[derive(Debug, Clone, Default)]
pub struct ProviderCacheStats {
    pub provider_id: String,
    pub hits: u64,
    pub misses: u64,
    pub entry_count: usize,
    pub last_fetch: Option<SystemTime>,
}

impl ProviderCacheStats {
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }
}

/// Global cache statistics across all providers.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    pub total_hits: u64,
    pub total_misses: u64,
    pub total_entries: usize,
    pub provider_stats: Vec<ProviderCacheStats>,
}

impl CacheMetrics {
    #[must_use]
    pub fn total_hit_rate(&self) -> f64 {
        let total = self.total_hits + self.total_misses;
        if total == 0 {
            return 0.0;
        }
        self.total_hits as f64 / total as f64
    }
}

struct ProviderCache {
    entries: HashMap<String, CacheEntry>,
    hits: HashMap<String, u64>,
    misses: HashMap<String, u64>,
    last_fetch: HashMap<String, SystemTime>,
}

impl ProviderCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            hits: HashMap::new(),
            misses: HashMap::new(),
            last_fetch: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<&str> {
        self.entries.retain(|_, v| v.expires_at > Instant::now());
        if let Some(entry) = self.entries.get(key) {
            *self.hits.entry(entry.provider_id.clone()).or_insert(0) += 1;
            Some(entry.data.as_str())
        } else {
            let provider = key.split(':').next().unwrap_or("unknown");
            *self.misses.entry(provider.to_string()).or_insert(0) += 1;
            None
        }
    }

    fn set(&mut self, key: String, data: String, ttl: Duration, provider_id: &str) {
        let now = SystemTime::now();
        self.last_fetch.insert(provider_id.to_string(), now);
        self.entries.insert(
            key,
            CacheEntry {
                data,
                expires_at: Instant::now() + ttl,
                created_at: now,
                provider_id: provider_id.to_string(),
            },
        );
    }

    fn invalidate_provider(&mut self, provider_id: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, v| v.provider_id != provider_id);
        before - self.entries.len()
    }

    fn invalidate_all(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        count
    }

    fn metrics(&mut self) -> CacheMetrics {
        self.entries.retain(|_, v| v.expires_at > Instant::now());

        let mut by_provider: HashMap<String, ProviderCacheStats> = HashMap::new();

        for entry in self.entries.values() {
            let stats = by_provider.entry(entry.provider_id.clone()).or_default();
            stats.provider_id.clone_from(&entry.provider_id);
            stats.entry_count += 1;
        }

        for (pid, &count) in &self.hits {
            let stats = by_provider.entry(pid.clone()).or_default();
            stats.provider_id.clone_from(pid);
            stats.hits = count;
        }
        for (pid, &count) in &self.misses {
            let stats = by_provider.entry(pid.clone()).or_default();
            stats.provider_id.clone_from(pid);
            stats.misses = count;
        }
        for (pid, &ts) in &self.last_fetch {
            let stats = by_provider.entry(pid.clone()).or_default();
            stats.provider_id.clone_from(pid);
            stats.last_fetch = Some(ts);
        }

        let mut provider_stats: Vec<_> = by_provider.into_values().collect();
        provider_stats.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));

        CacheMetrics {
            total_hits: self.hits.values().sum(),
            total_misses: self.misses.values().sum(),
            total_entries: self.entries.len(),
            provider_stats,
        }
    }
}

pub fn get_cached(key: &str) -> Option<String> {
    PROVIDER_CACHE
        .lock()
        .ok()
        .and_then(|mut c| c.get(key).map(std::string::ToString::to_string))
}

pub fn set_cached(key: &str, data: &str, ttl_secs: u64) {
    set_cached_with_provider(
        key,
        data,
        ttl_secs,
        key.split(':').next().unwrap_or("unknown"),
    );
}

pub fn set_cached_with_provider(key: &str, data: &str, ttl_secs: u64, provider_id: &str) {
    if let Ok(mut cache) = PROVIDER_CACHE.lock() {
        cache.set(
            key.to_string(),
            data.to_string(),
            Duration::from_secs(ttl_secs),
            provider_id,
        );
    }
}

pub fn invalidate_provider(provider_id: &str) -> usize {
    PROVIDER_CACHE
        .lock()
        .ok()
        .map_or(0, |mut c| c.invalidate_provider(provider_id))
}

pub fn invalidate_all() -> usize {
    PROVIDER_CACHE
        .lock()
        .ok()
        .map_or(0, |mut c| c.invalidate_all())
}

pub fn cache_metrics() -> CacheMetrics {
    PROVIDER_CACHE
        .lock()
        .ok()
        .map(|mut c| c.metrics())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_set_and_get() {
        let mut cache = ProviderCache::new();
        cache.set(
            "test:key".into(),
            "value".into(),
            Duration::from_mins(1),
            "test",
        );
        assert_eq!(cache.get("test:key"), Some("value"));
    }

    #[test]
    fn cache_expired_entry_returns_none() {
        let mut cache = ProviderCache::new();
        cache.set(
            "test:key".into(),
            "value".into(),
            Duration::from_secs(0),
            "test",
        );
        std::thread::sleep(Duration::from_millis(10));
        assert!(cache.get("test:key").is_none());
    }

    #[test]
    fn cache_tracks_hits_and_misses() {
        let mut cache = ProviderCache::new();
        cache.set(
            "github:key".into(),
            "data".into(),
            Duration::from_mins(1),
            "github",
        );
        cache.get("github:key"); // hit
        cache.get("github:key"); // hit
        cache.get("github:missing"); // miss

        let metrics = cache.metrics();
        assert_eq!(metrics.total_hits, 2);
        assert_eq!(metrics.total_misses, 1);
        assert!((metrics.total_hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn cache_invalidate_provider() {
        let mut cache = ProviderCache::new();
        cache.set(
            "github:a".into(),
            "1".into(),
            Duration::from_mins(1),
            "github",
        );
        cache.set(
            "github:b".into(),
            "2".into(),
            Duration::from_mins(1),
            "github",
        );
        cache.set(
            "gitlab:c".into(),
            "3".into(),
            Duration::from_mins(1),
            "gitlab",
        );

        let removed = cache.invalidate_provider("github");
        assert_eq!(removed, 2);
        assert!(cache.get("github:a").is_none());
        assert_eq!(cache.get("gitlab:c"), Some("3"));
    }

    #[test]
    fn cache_invalidate_all() {
        let mut cache = ProviderCache::new();
        cache.set("a".into(), "1".into(), Duration::from_mins(1), "x");
        cache.set("b".into(), "2".into(), Duration::from_mins(1), "y");

        let removed = cache.invalidate_all();
        assert_eq!(removed, 2);
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn cache_metrics_per_provider() {
        let mut cache = ProviderCache::new();
        cache.set(
            "github:x".into(),
            "a".into(),
            Duration::from_mins(1),
            "github",
        );
        cache.set(
            "gitlab:y".into(),
            "b".into(),
            Duration::from_mins(1),
            "gitlab",
        );
        cache.get("github:x");
        cache.get("gitlab:miss");

        let metrics = cache.metrics();
        assert_eq!(metrics.provider_stats.len(), 2);

        let gh = metrics
            .provider_stats
            .iter()
            .find(|s| s.provider_id == "github")
            .unwrap();
        assert_eq!(gh.entry_count, 1);
        assert_eq!(gh.hits, 1);

        let gl = metrics
            .provider_stats
            .iter()
            .find(|s| s.provider_id == "gitlab")
            .unwrap();
        assert_eq!(gl.entry_count, 1);
        assert!(gl.last_fetch.is_some());
    }

    #[test]
    fn provider_cache_stats_hit_rate() {
        let stats = ProviderCacheStats {
            provider_id: "test".into(),
            hits: 3,
            misses: 1,
            entry_count: 2,
            last_fetch: None,
        };
        assert!((stats.hit_rate() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn provider_cache_stats_hit_rate_zero() {
        let stats = ProviderCacheStats::default();
        assert!((stats.hit_rate() - 0.0).abs() < f64::EPSILON);
    }
}
