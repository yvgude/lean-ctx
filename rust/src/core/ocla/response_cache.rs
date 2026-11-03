//! Bounded in-memory cache for identical model responses.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Maximum number of responses retained by a response cache.
pub const MAX_ENTRIES: usize = 512;
/// Default lifetime for a cached response.
pub const DEFAULT_TTL: Duration = Duration::from_mins(5);

/// Selects how cached responses determine their lifetime.
#[derive(Clone, Debug)]
pub enum CachePolicy {
    /// Global TTL for all models.
    Uniform,
    /// Per-model TTL overrides selected by model-prefix match.
    ModelAware {
        overrides: HashMap<String, Duration>,
    },
}

static GLOBAL_RESPONSE_CACHE: OnceLock<ResponseCache> = OnceLock::new();

pub fn global_response_cache() -> &'static ResponseCache {
    GLOBAL_RESPONSE_CACHE.get_or_init(ResponseCache::default)
}

/// Stable cache key derived from response-defining request fields.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResponseCacheKey {
    /// First 64 bits of the digest of the key components.
    pub hash: u64,
    /// Model name used for model-aware cache expiry.
    pub model: String,
}

impl ResponseCacheKey {
    /// Hashes model, prompt hash, temperature, and maximum output tokens.
    pub fn new(
        model: impl AsRef<str>,
        prompt_hash: u64,
        temperature: f32,
        max_tokens: u64,
    ) -> Self {
        let mut hasher = blake3::Hasher::new();
        let model = model.as_ref();
        hasher.update(&(model.len() as u64).to_be_bytes());
        hasher.update(model.as_bytes());
        hasher.update(&prompt_hash.to_be_bytes());
        hasher.update(&temperature.to_bits().to_be_bytes());
        hasher.update(&max_tokens.to_be_bytes());

        let digest = hasher.finalize();
        let mut bytes = [0; 8];
        bytes.copy_from_slice(&digest.as_bytes()[..8]);
        Self {
            hash: u64::from_be_bytes(bytes),
            model: model.to_owned(),
        }
    }
}

/// A response stored in the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedResponse {
    pub body: Vec<u8>,
    pub status: u16,
    pub tokens: u64,
    pub created_at: Instant,
    pub ttl: Duration,
}

#[derive(Debug, Default)]
struct CacheState {
    entries: VecDeque<(ResponseCacheKey, CachedResponse)>,
    hits: u64,
    misses: u64,
    evictions: u64,
    evictions_by_reason: HashMap<String, u64>,
}

/// Thread-safe bounded LRU response cache.
#[derive(Debug)]
pub struct ResponseCache {
    capacity: usize,
    ttl: Duration,
    policy: CachePolicy,
    state: Mutex<CacheState>,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(MAX_ENTRIES, DEFAULT_TTL)
    }
}

impl ResponseCache {
    /// Creates a cache with the requested capacity and defaulting zero TTLs.
    ///
    /// Capacity is clamped to the inclusive range 1..=512.
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity: capacity.clamp(1, MAX_ENTRIES),
            ttl,
            policy: CachePolicy::Uniform,
            state: Mutex::new(CacheState::default()),
        }
    }

    /// Creates a cache with the requested TTL and the maximum capacity.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self::new(MAX_ENTRIES, ttl)
    }

    /// Replaces the cache expiry policy for existing and future entries.
    pub fn set_policy(&mut self, policy: CachePolicy) {
        self.policy = policy;
    }

    /// Looks up a response, refreshing its LRU position on a live hit.
    pub fn get(&self, key: &ResponseCacheKey) -> Option<CachedResponse> {
        let mut state = self.lock_state();
        let now = Instant::now();
        let position = state
            .entries
            .iter()
            .position(|(entry_key, _)| entry_key == key);

        let Some(position) = position else {
            state.misses += 1;
            return None;
        };

        let expiration_reason = {
            let (entry_key, response) = &state.entries[position];
            self.expiration_reason(entry_key, response, now)
        };
        if let Some(reason) = expiration_reason {
            state.entries.remove(position);
            record_eviction(&mut state, reason);
            state.misses += 1;
            return None;
        }

        let entry = state.entries.remove(position)?;
        state.entries.push_back(entry.clone());
        state.hits += 1;
        Some(entry.1)
    }

    /// Inserts or replaces a response, evicting the least recently used entry
    /// when the bounded capacity is reached.
    pub fn put(&self, key: ResponseCacheKey, mut response: CachedResponse) {
        let mut state = self.lock_state();

        if response.ttl.is_zero() {
            response.ttl = self.ttl;
        }
        self.remove_expired(&mut state, Instant::now());

        if let Some(position) = state
            .entries
            .iter()
            .position(|(entry_key, _)| entry_key == &key)
        {
            state.entries.remove(position);
        } else if state.entries.len() >= self.capacity {
            state.entries.pop_front();
            record_eviction(&mut state, "lru_capacity");
        }

        state.entries.push_back((key, response));
    }

    /// Returns cumulative hit, miss, and eviction counters.
    pub fn stats(&self) -> CacheStats {
        let state = self.lock_state();
        let total = state.hits + state.misses;
        CacheStats {
            entries: state.entries.len(),
            hits: state.hits,
            misses: state.misses,
            evictions: state.evictions,
            evictions_by_reason: state.evictions_by_reason.clone(),
            hit_rate: if total == 0 {
                0.0
            } else {
                state.hits as f64 / total as f64
            },
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn effective_ttl(&self, model: &str) -> Duration {
        match &self.policy {
            CachePolicy::Uniform => self.ttl,
            CachePolicy::ModelAware { overrides } => overrides
                .iter()
                .filter(|(prefix, _)| model.starts_with(prefix.as_str()))
                .max_by_key(|(prefix, _)| prefix.len())
                .map_or(self.ttl, |(_, ttl)| *ttl),
        }
    }

    fn expiration_reason(
        &self,
        key: &ResponseCacheKey,
        response: &CachedResponse,
        now: Instant,
    ) -> Option<&'static str> {
        let ttl = self.effective_ttl(&key.model);
        let expired = now
            .checked_duration_since(response.created_at)
            .unwrap_or_default()
            >= ttl;
        expired.then_some(match &self.policy {
            CachePolicy::Uniform => "ttl_expired",
            CachePolicy::ModelAware { .. } => "model_ttl_expired",
        })
    }

    fn remove_expired(&self, state: &mut CacheState, now: Instant) {
        let mut retained = VecDeque::with_capacity(state.entries.len());
        while let Some((key, response)) = state.entries.pop_front() {
            if let Some(reason) = self.expiration_reason(&key, &response, now) {
                record_eviction(state, reason);
            } else {
                retained.push_back((key, response));
            }
        }
        state.entries = retained;
    }
}

fn record_eviction(state: &mut CacheState, reason: &str) {
    state.evictions += 1;
    *state
        .evictions_by_reason
        .entry(reason.to_owned())
        .or_default() += 1;
}

/// Snapshot of cache activity.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub evictions_by_reason: HashMap<String, u64>,
    pub hit_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_key(model: &str) -> ResponseCacheKey {
        ResponseCacheKey::new(model, 42, 0.2, 128)
    }

    fn response(body: &[u8], created_at: Instant, ttl: Duration) -> CachedResponse {
        CachedResponse {
            body: body.to_vec(),
            status: 200,
            tokens: body.len() as u64,
            created_at,
            ttl,
        }
    }

    #[test]
    fn key_hash_changes_when_request_fields_change() {
        let base = cache_key("model-a");
        assert_ne!(base, cache_key("model-b"));
        assert_ne!(base, ResponseCacheKey::new("model-a", 43, 0.2, 128));
        assert_ne!(base, ResponseCacheKey::new("model-a", 42, 0.3, 128));
        assert_ne!(base, ResponseCacheKey::new("model-a", 42, 0.2, 129));
    }

    #[test]
    fn cache_hit_and_miss_update_stats() {
        let cache = ResponseCache::new(4, Duration::from_mins(1));
        let key = cache_key("model-a");
        cache.put(
            key.clone(),
            response(b"answer", Instant::now(), Duration::ZERO),
        );

        assert_eq!(cache.get(&key).unwrap().body, b"answer");
        assert!(cache.get(&cache_key("model-b")).is_none());

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.evictions, 0);
        assert!(stats.evictions_by_reason.is_empty());
        assert!((stats.hit_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn expired_entries_count_as_misses() {
        let cache = ResponseCache::with_ttl(Duration::from_mins(1));
        let key = cache_key("model-a");
        cache.put(
            key.clone(),
            response(
                b"old",
                Instant::now().checked_sub(Duration::from_secs(61)).unwrap(),
                Duration::from_mins(1),
            ),
        );

        assert!(cache.get(&key).is_none());
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.evictions, 1);
        assert_eq!(stats.evictions_by_reason["ttl_expired"], 1);
    }

    #[test]
    fn lru_eviction_removes_least_recently_used_entry() {
        let cache = ResponseCache::new(2, Duration::from_mins(1));
        let first = cache_key("first");
        let second = cache_key("second");
        let third = cache_key("third");

        cache.put(
            first.clone(),
            response(b"1", Instant::now(), Duration::ZERO),
        );
        cache.put(
            second.clone(),
            response(b"2", Instant::now(), Duration::ZERO),
        );
        assert!(cache.get(&first).is_some());
        cache.put(
            third.clone(),
            response(b"3", Instant::now(), Duration::ZERO),
        );

        assert!(cache.get(&second).is_none());
        assert!(cache.get(&first).is_some());
        assert!(cache.get(&third).is_some());

        let stats = cache.stats();
        assert_eq!(stats.evictions, 1);
        assert_eq!(stats.evictions_by_reason["lru_capacity"], 1);
        assert_eq!(stats.hits, 3);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn capacity_is_hard_capped() {
        let cache = ResponseCache::new(MAX_ENTRIES + 1, Duration::from_mins(1));
        for index in 0..=MAX_ENTRIES {
            cache.put(
                ResponseCacheKey {
                    hash: index as u64,
                    model: String::new(),
                },
                response(b"x", Instant::now(), Duration::ZERO),
            );
        }

        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn default_policy_uses_five_minute_ttl() {
        let cache = ResponseCache::default();
        let key = cache_key("default");
        cache.put(
            key.clone(),
            response(
                b"answer",
                Instant::now()
                    .checked_sub(Duration::from_secs(301))
                    .unwrap(),
                Duration::ZERO,
            ),
        );

        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn model_aware_ttl_overrides_default() {
        let mut cache = ResponseCache::new(4, Duration::from_mins(1));
        let gpt = cache_key("gpt-4o");
        let other = cache_key("other-model");
        let created_at = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();

        cache.put(gpt.clone(), response(b"gpt", created_at, Duration::ZERO));
        cache.put(
            other.clone(),
            response(b"other", created_at, Duration::ZERO),
        );
        cache.set_policy(CachePolicy::ModelAware {
            overrides: HashMap::from([(String::from("gpt"), Duration::from_secs(1))]),
        });

        assert!(cache.get(&gpt).is_none());
        assert_eq!(cache.get(&other).unwrap().body, b"other");
        assert_eq!(cache.stats().evictions_by_reason["model_ttl_expired"], 1);
    }

    #[test]
    fn uniform_policy_uses_global_ttl() {
        let cache = ResponseCache::new(4, Duration::from_secs(1));
        let created_at = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        let first = cache_key("gpt-4o");
        let second = cache_key("other-model");

        cache.put(
            first.clone(),
            response(b"first", created_at, Duration::from_mins(1)),
        );
        cache.put(
            second.clone(),
            response(b"second", created_at, Duration::from_mins(1)),
        );

        assert!(cache.get(&first).is_none());
        assert!(cache.get(&second).is_none());
        assert_eq!(cache.stats().evictions_by_reason["ttl_expired"], 2);
    }

    #[test]
    fn set_policy_changes_behavior() {
        let mut cache = ResponseCache::new(4, Duration::from_mins(1));
        let gpt = cache_key("gpt-4o");
        let created_at = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();

        cache.put(gpt.clone(), response(b"answer", created_at, Duration::ZERO));
        assert!(cache.get(&gpt).is_some());

        cache.put(gpt.clone(), response(b"answer", created_at, Duration::ZERO));
        cache.set_policy(CachePolicy::ModelAware {
            overrides: HashMap::from([(String::from("gpt"), Duration::from_secs(1))]),
        });
        assert!(cache.get(&gpt).is_none());
    }

    #[test]
    fn eviction_stats_track_reason() {
        let cache = ResponseCache::new(1, Duration::from_secs(1));
        let expired = cache_key("expired");
        let first = cache_key("first");
        let second = cache_key("second");

        cache.put(
            expired,
            response(
                b"expired",
                Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
                Duration::ZERO,
            ),
        );
        cache.put(first, response(b"first", Instant::now(), Duration::ZERO));
        cache.put(second, response(b"second", Instant::now(), Duration::ZERO));

        let stats = cache.stats();
        assert_eq!(stats.evictions, 2);
        assert_eq!(stats.evictions_by_reason["ttl_expired"], 1);
        assert_eq!(stats.evictions_by_reason["lru_capacity"], 1);
    }
}
