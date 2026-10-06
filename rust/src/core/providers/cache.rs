// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const MAX_CACHE_ENTRIES: usize = 2048;
const STALE_SERVE_GRACE: Duration = Duration::from_hours(1);

/// Partition cached responses by the exact request and credential, not authorization state.
/// Credential rotation misses the cache; same-credential revocation still follows cache TTL.
pub(super) fn request_cache_key(
    provider_id: &str,
    request_url: &str,
    credential: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for component in [provider_id.as_bytes(), request_url.as_bytes()] {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    let credential_marker = [u8::from(credential.is_some())];
    hasher.update(&(credential_marker.len() as u64).to_be_bytes());
    hasher.update(&credential_marker);
    if let Some(credential) = credential {
        hasher.update(&(credential.len() as u64).to_be_bytes());
        hasher.update(credential.as_bytes());
    }
    format!("{provider_id}:request:v2:{}", hasher.finalize().to_hex())
}

static PROVIDER_CACHE: std::sync::LazyLock<Mutex<ProviderCache>> =
    std::sync::LazyLock::new(|| Mutex::new(ProviderCache::new()));

struct CacheEntry {
    data: String,
    expires_at: Instant,
    provider_id: String,
}

struct StaleEntry {
    data: String,
    evicted_at: Instant,
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
    pub stale_entries: usize,
    pub max_entries: usize,
    pub provider_stats: Vec<ProviderCacheStats>,
}

impl CacheMetrics {
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
    access_order: Vec<String>,
    stale_entries: HashMap<String, StaleEntry>,
    hits: HashMap<String, u64>,
    misses: HashMap<String, u64>,
    last_fetch: HashMap<String, SystemTime>,
}

impl ProviderCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
            stale_entries: HashMap::new(),
            hits: HashMap::new(),
            misses: HashMap::new(),
            last_fetch: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<&str> {
        self.expire_entries();
        self.purge_stale();
        if let Some(entry) = self.entries.get(key) {
            let provider_id = entry.provider_id.clone();
            self.access_order.retain(|candidate| candidate != key);
            self.access_order.push(key.to_string());
            *self.hits.entry(provider_id).or_insert(0) += 1;
            return self.entries.get(key).map(|entry| entry.data.as_str());
        }
        let provider_id = self.stale_entries.get(key).map_or_else(
            || key.split(':').next().unwrap_or("unknown").to_string(),
            |entry| entry.provider_id.clone(),
        );
        *self.misses.entry(provider_id.clone()).or_insert(0) += 1;
        let stale = self.stale_entries.get(key)?;
        tracing::warn!(key, provider_id, "serving stale provider cache entry");
        Some(stale.data.as_str())
    }

    fn set(&mut self, key: String, data: String, ttl: Duration, provider_id: &str) {
        self.last_fetch
            .insert(provider_id.to_string(), SystemTime::now());
        self.access_order.retain(|candidate| candidate != &key);
        self.access_order.push(key.clone());
        self.stale_entries.remove(&key);
        self.entries.insert(
            key,
            CacheEntry {
                data,
                expires_at: Instant::now() + ttl,
                provider_id: provider_id.to_string(),
            },
        );
        self.enforce_lru_cap();
    }

    fn expire_entries(&mut self) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.move_to_stale(&key, now);
        }
    }

    fn enforce_lru_cap(&mut self) {
        let now = Instant::now();
        while self.entries.len() > MAX_CACHE_ENTRIES {
            let key = self.access_order.remove(0);
            self.move_to_stale(&key, now);
        }
    }

    fn move_to_stale(&mut self, key: &str, evicted_at: Instant) {
        self.access_order.retain(|candidate| candidate != key);
        if let Some(entry) = self.entries.remove(key) {
            self.stale_entries.insert(
                key.to_string(),
                StaleEntry {
                    data: entry.data,
                    evicted_at,
                    provider_id: entry.provider_id,
                },
            );
        }
    }

    fn stale_entry_count(&self) -> usize {
        self.stale_entries.len()
    }

    fn purge_stale(&mut self) {
        let now = Instant::now();
        self.stale_entries
            .retain(|_, entry| entry.evicted_at + STALE_SERVE_GRACE > now);
    }

    fn invalidate_provider(&mut self, provider_id: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, v| v.provider_id != provider_id);
        self.access_order
            .retain(|key| self.entries.contains_key(key));
        self.stale_entries
            .retain(|_, entry| entry.provider_id != provider_id);
        before - self.entries.len()
    }

    fn invalidate_all(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.access_order.clear();
        self.stale_entries.clear();
        count
    }

    fn metrics(&mut self) -> CacheMetrics {
        self.expire_entries();
        self.purge_stale();

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
            stale_entries: self.stale_entries.len(),
            max_entries: MAX_CACHE_ENTRIES,
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
        .map_or_else(CacheMetrics::default, |mut c| c.metrics())
}

pub fn cache_entry_count() -> usize {
    PROVIDER_CACHE.lock().ok().map_or(0, |mut cache| {
        cache.expire_entries();
        cache.entries.len()
    })
}

pub fn cache_stale_count() -> usize {
    PROVIDER_CACHE
        .lock()
        .ok()
        .map_or(0, |cache| cache.stale_entry_count())
}

pub fn cache_purge_stale() -> usize {
    PROVIDER_CACHE.lock().ok().map_or(0, |mut cache| {
        let before = cache.stale_entry_count();
        cache.purge_stale();
        before - cache.stale_entry_count()
    })
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
    fn test_stale_serve_on_expired_entry() {
        let mut cache = ProviderCache::new();
        cache.set(
            "test:key".into(),
            "value".into(),
            Duration::from_millis(10),
            "test",
        );
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(cache.get("test:key"), Some("value"));
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
    fn test_lru_eviction_at_capacity() {
        let mut cache = ProviderCache::new();
        for index in 0..MAX_CACHE_ENTRIES + 10 {
            let key = format!("test:{index}");
            cache.set(key.clone(), key, Duration::from_mins(1), "test");
        }
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
    }

    #[test]
    fn test_stale_serve_expired_grace() {
        let mut cache = ProviderCache::new();
        cache.stale_entries.insert(
            "test:key".into(),
            StaleEntry {
                data: "value".into(),
                evicted_at: Instant::now()
                    .checked_sub(STALE_SERVE_GRACE + Duration::from_millis(1))
                    .unwrap(),
                provider_id: "test".into(),
            },
        );
        assert!(cache.get("test:key").is_none());
    }

    #[test]
    fn test_access_order_update_on_get() {
        let mut cache = ProviderCache::new();
        cache.set("a".into(), "1".into(), Duration::from_mins(1), "test");
        cache.set("b".into(), "2".into(), Duration::from_mins(1), "test");
        cache.get("a");
        assert_eq!(cache.access_order.last().map(String::as_str), Some("a"));
    }

    #[test]
    fn test_metrics_include_stale_count() {
        let mut cache = ProviderCache::new();
        cache.set("a".into(), "1".into(), Duration::from_millis(10), "test");
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(cache.metrics().stale_entries, 1);
    }

    #[test]
    fn test_purge_stale_removes_old() {
        let mut cache = ProviderCache::new();
        cache.stale_entries.insert(
            "old".into(),
            StaleEntry {
                data: "value".into(),
                evicted_at: Instant::now()
                    .checked_sub(STALE_SERVE_GRACE + Duration::from_millis(1))
                    .unwrap(),
                provider_id: "test".into(),
            },
        );
        cache.purge_stale();
        assert_eq!(cache.stale_entry_count(), 0);
    }

    #[test]
    #[serial_test::serial(provider_request_cache)]
    fn test_cache_entry_count_accuracy() {
        invalidate_all();
        set_cached_with_provider("test:count", "value", 60, "test");
        assert_eq!(cache_entry_count(), 1);
        invalidate_all();
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
    fn request_cache_key_is_deterministic_and_principal_request_bound() {
        let request = "https://gitlab.example/api/v4/projects/group%2Fproject/issues?per_page=20&state=opened";
        let key = request_cache_key("gitlab", request, Some("principal-token"));

        assert_eq!(
            key,
            request_cache_key("gitlab", request, Some("principal-token"))
        );
        assert_ne!(
            key,
            request_cache_key("gitlab", request, Some("other-principal-token"))
        );
        assert_ne!(
            key,
            request_cache_key(
                "gitlab",
                "https://gitlab.other/api/v4/projects/group%2Fproject/issues?per_page=20&state=opened",
                Some("principal-token")
            )
        );
        assert_ne!(
            key,
            request_cache_key(
                "gitlab",
                "https://gitlab.example/api/v4/projects/group%2Fproject/issues?per_page=20&state=closed",
                Some("principal-token")
            )
        );
        assert_ne!(
            key,
            request_cache_key("github", request, Some("principal-token"))
        );
        assert_ne!(key, request_cache_key("gitlab", request, None));
        assert_ne!(key, request_cache_key("gitlab", request, Some("")));
        assert_ne!(
            request_cache_key("gitlab", request, None),
            request_cache_key("gitlab", request, Some(""))
        );
        assert!(!key.contains("principal-token"));
        assert!(!key.contains("gitlab.example"));
        assert!(!key.contains("group%2Fproject"));
    }

    #[test]
    fn request_cache_key_does_not_reuse_legacy_raw_key() {
        let key = request_cache_key(
            "gitlab",
            "https://gitlab.example/api/v4/projects/group%2Fproject/issues?per_page=20&state=opened",
            Some("principal-token"),
        );
        let legacy_key = "gitlab:issues:group/project:Some(\"opened\"):None:20";

        assert_ne!(key, legacy_key);
        assert!(key.starts_with("gitlab:request:v2:"));
    }

    #[test]
    fn request_cache_key_isolates_fresh_and_stale_responses() {
        let request = "https://gitlab.example/api/v4/projects/42/issues?state=opened";
        let principal_key = request_cache_key("gitlab", request, Some("principal-token"));
        let foreign_keys = [
            request_cache_key("gitlab", request, Some("other-principal-token")),
            request_cache_key("gitlab", request, None),
            request_cache_key("gitlab", request, Some("")),
            request_cache_key("github", request, Some("principal-token")),
            request_cache_key(
                "gitlab",
                "https://gitlab.other/api/v4/projects/42/issues?state=opened",
                Some("principal-token"),
            ),
            request_cache_key(
                "gitlab",
                "https://gitlab.example/api/v4/projects/42/issues?state=closed",
                Some("principal-token"),
            ),
        ];
        let mut cache = ProviderCache::new();
        cache.set(
            principal_key.clone(),
            "principal response".into(),
            Duration::from_mins(1),
            "gitlab",
        );
        assert_eq!(cache.get(&principal_key), Some("principal response"));
        for key in &foreign_keys {
            assert_eq!(cache.get(key), None);
        }
        cache.move_to_stale(&principal_key, Instant::now());
        assert_eq!(cache.get(&principal_key), Some("principal response"));
        for key in &foreign_keys {
            assert_eq!(cache.get(key), None);
        }
        cache.set(
            "gitlab:issues:42:Some(\"opened\"):None:20".into(),
            "legacy response".into(),
            Duration::from_mins(1),
            "gitlab",
        );
        assert_eq!(cache.get(&foreign_keys[0]), None);
    }
}
