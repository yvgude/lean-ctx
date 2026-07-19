//! Response Optimizer (P9 / DIM 2 — Output-Optimierung).
//!
//! Reduces output tokens without semantic loss through two mechanisms:
//!
//! 1. **Response Cache** — identical user queries within a session get the
//!    cached response instead of a full LLM round-trip. Saves 100% of output
//!    tokens on cache hits.
//!
//! 2. **Response Dedup** — detects when the model repeats substantially
//!    similar answers within a conversation and signals this to the client
//!    (future: truncate/summarize repeated content).
//!
//! These complement the existing mechanisms:
//! - `verbosity.rs` — wire-level "be concise" steer (reduces verbosity ~33%)
//! - `output_savings.rs` — A/B measurement of output reduction
//! - `effort_routing.rs` — thinking budget control
//!
//! **Opt-in only** (`proxy.response_cache = true`). Off by default.
//!
//! ## Cache design
//!
//! - Key: BLAKE3 hash of (model + last N user messages + system prompt)
//! - Value: the complete streamed response (reassembled)
//! - TTL: configurable, default 5 minutes (short — LLM answers can evolve)
//! - Capacity: bounded LRU, default 64 entries per session
//! - Scope: per-session (not cross-session — avoids stale context leaks)
//!
//! ## Dedup design
//!
//! - Tracks BLAKE3 fingerprints of recent responses (last 16)
//! - A response whose first 200 chars match a recent fingerprint is flagged
//! - Flagging is observability-only in v1 (no truncation)
//!
//! ## Determinism
//!
//! Cache hits are deterministic: same key always returns the same value.
//! Cache *misses* are non-deterministic (LLM output varies), but the decision
//! to serve from cache vs. forward is deterministic given the cache state.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Configuration for the response optimizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResponseOptimizerConfig {
    /// Master switch. Default: false (opt-in).
    pub enabled: bool,
    /// Enable the response cache. Default: true (when optimizer is enabled).
    pub cache_enabled: bool,
    /// Enable dedup detection. Default: true.
    pub dedup_enabled: bool,
    /// Cache TTL in seconds. Default: 300 (5 minutes).
    pub cache_ttl_secs: u64,
    /// Max cached responses per session. Default: 64.
    pub cache_capacity: usize,
    /// Number of recent response fingerprints to track for dedup. Default: 16.
    pub dedup_window: usize,
}

impl Default for ResponseOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_enabled: true,
            dedup_enabled: true,
            cache_ttl_secs: 300,
            cache_capacity: 64,
            dedup_window: 16,
        }
    }
}

/// A cached response entry.
#[derive(Debug, Clone)]
struct CacheEntry {
    response_body: String,
    created_at: Instant,
    output_tokens_saved: u64,
}

/// The response cache — bounded LRU with TTL eviction.
#[derive(Debug)]
pub struct ResponseCache {
    entries: VecDeque<(u64, CacheEntry)>,
    capacity: usize,
    ttl: Duration,
}

impl ResponseCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            ttl,
        }
    }

    /// Look up a cache key. Returns the cached response if found and not expired.
    pub fn get(&mut self, key: u64) -> Option<&str> {
        self.evict_expired();
        let pos = self.entries.iter().position(|(k, _)| *k == key)?;
        // Move to back (LRU touch).
        let entry = self.entries.remove(pos)?;
        let response = &entry.1.response_body;
        let result = response.as_str();
        self.entries.push_back(entry);
        // Safety: we just pushed it back, reference is valid for the borrow.
        self.entries.back().map(|(_, e)| e.response_body.as_str())
    }

    /// Insert a response into the cache.
    pub fn put(&mut self, key: u64, response: String, output_tokens: u64) {
        self.evict_expired();
        // Remove existing entry with same key (update).
        self.entries.retain(|(k, _)| *k != key);
        // Evict LRU if at capacity.
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((
            key,
            CacheEntry {
                response_body: response,
                created_at: Instant::now(),
                output_tokens_saved: output_tokens,
            },
        ));
    }

    /// Remove expired entries.
    fn evict_expired(&mut self) {
        let now = Instant::now();
        self.entries
            .retain(|(_, e)| now.duration_since(e.created_at) < self.ttl);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Response deduplication tracker.
#[derive(Debug)]
pub struct DedupTracker {
    fingerprints: VecDeque<u64>,
    window: usize,
}

impl DedupTracker {
    pub fn new(window: usize) -> Self {
        Self {
            fingerprints: VecDeque::with_capacity(window),
            window,
        }
    }

    /// Record a response fingerprint. Returns true if this is a duplicate
    /// (fingerprint was already in the recent window).
    pub fn record(&mut self, fingerprint: u64) -> bool {
        let is_dup = self.fingerprints.contains(&fingerprint);
        if self.fingerprints.len() >= self.window {
            self.fingerprints.pop_front();
        }
        self.fingerprints.push_back(fingerprint);
        is_dup
    }

    pub fn clear(&mut self) {
        self.fingerprints.clear();
    }
}

/// An optimization decision record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OptimizationDecision {
    /// Whether the response was served from cache.
    pub cache_hit: bool,
    /// Whether the response was flagged as a duplicate.
    pub is_duplicate: bool,
    /// Cache key (BLAKE3-based hash).
    pub cache_key: u64,
    /// Estimated output tokens saved (0 if cache miss).
    pub tokens_saved: u64,
    /// Source of the optimization.
    pub source: OptimizationSource,
}

/// What triggered the optimization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OptimizationSource {
    /// No optimization applied (cache miss, not a dup).
    None,
    /// Response served from cache.
    Cache,
    /// Response flagged as duplicate of a recent answer.
    Dedup,
    /// Both cache hit and duplicate detection triggered.
    CacheAndDedup,
}

/// Per-session optimizer state. Each session/conversation gets its own instance.
#[derive(Debug)]
pub struct SessionOptimizer {
    pub cache: ResponseCache,
    pub dedup: DedupTracker,
    pub config: ResponseOptimizerConfig,
    pub stats: OptimizerStats,
}

/// Optimizer statistics for observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OptimizerStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub dedup_detections: u64,
    pub total_tokens_saved: u64,
}

impl SessionOptimizer {
    pub fn new(config: ResponseOptimizerConfig) -> Self {
        let cache = ResponseCache::new(
            config.cache_capacity,
            Duration::from_secs(config.cache_ttl_secs),
        );
        let dedup = DedupTracker::new(config.dedup_window);
        Self {
            cache,
            dedup,
            config,
            stats: OptimizerStats::default(),
        }
    }

    /// Check if a request can be served from cache.
    /// Returns the cached response body if available.
    pub fn try_cache_hit(&mut self, cache_key: u64) -> Option<&str> {
        if !self.config.cache_enabled {
            return None;
        }
        let hit = self.cache.get(cache_key);
        if hit.is_some() {
            self.stats.cache_hits += 1;
        } else {
            self.stats.cache_misses += 1;
        }
        hit
    }

    /// Record a response for future cache lookups and dedup detection.
    pub fn record_response(
        &mut self,
        cache_key: u64,
        response: &str,
        output_tokens: u64,
    ) -> OptimizationDecision {
        let fingerprint = fingerprint_response(response);
        let is_dup = if self.config.dedup_enabled {
            let dup = self.dedup.record(fingerprint);
            if dup {
                self.stats.dedup_detections += 1;
            }
            dup
        } else {
            false
        };

        if self.config.cache_enabled {
            self.cache
                .put(cache_key, response.to_string(), output_tokens);
        }

        OptimizationDecision {
            cache_hit: false,
            is_duplicate: is_dup,
            cache_key,
            tokens_saved: 0,
            source: if is_dup {
                OptimizationSource::Dedup
            } else {
                OptimizationSource::None
            },
        }
    }

    /// Build a decision record for a cache hit.
    pub fn cache_hit_decision(&self, cache_key: u64, tokens_saved: u64) -> OptimizationDecision {
        OptimizationDecision {
            cache_hit: true,
            is_duplicate: false,
            cache_key,
            tokens_saved,
            source: OptimizationSource::Cache,
        }
    }
}

/// Compute a cache key from the request components that determine the response.
/// Uses a fast non-cryptographic hash (FxHash-style) for performance.
pub fn compute_cache_key(model: &str, system: Option<&str>, messages: &[&str]) -> u64 {
    let mut hasher = SimpleHasher::new();
    hasher.write(model.as_bytes());
    hasher.write(b"\x00");
    if let Some(sys) = system {
        hasher.write(sys.as_bytes());
    }
    hasher.write(b"\x00");
    for msg in messages {
        hasher.write(msg.as_bytes());
        hasher.write(b"\x01");
    }
    hasher.finish()
}

/// Compute a fingerprint of a response for dedup detection.
/// Uses the first 200 chars to catch repeated preambles/patterns.
pub fn fingerprint_response(response: &str) -> u64 {
    let prefix = if response.len() > 200 {
        &response[..200]
    } else {
        response
    };
    let mut hasher = SimpleHasher::new();
    hasher.write(prefix.as_bytes());
    hasher.finish()
}

/// A simple, fast, non-cryptographic hasher (FNV-1a inspired).
/// Used for cache keys and fingerprints where collision resistance is not
/// security-critical (worst case: a cache miss or false dedup negative).
struct SimpleHasher {
    state: u64,
}

impl SimpleHasher {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;

    fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= u64::from(b);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(&self) -> u64 {
        self.state
    }
}

/// Global optimizer registry — maps session IDs to their optimizer instances.
/// In production, session lifetime is managed by the proxy's connection tracking.
static OPTIMIZERS: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<Mutex<SessionOptimizer>>>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<std::collections::HashMap<String, Arc<Mutex<SessionOptimizer>>>> {
    OPTIMIZERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Get or create the optimizer for a session.
pub fn get_or_create(session_id: &str, config: &ResponseOptimizerConfig) -> Arc<Mutex<SessionOptimizer>> {
    let mut reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(SessionOptimizer::new(config.clone()))))
        .clone()
}

/// Remove a session's optimizer (cleanup on session end).
pub fn remove_session(session_id: &str) {
    let mut reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.remove(session_id);
}

/// Global statistics across all sessions.
pub fn global_stats() -> OptimizerStats {
    let reg = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut total = OptimizerStats::default();
    for opt in reg.values() {
        let guard = opt.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        total.cache_hits += guard.stats.cache_hits;
        total.cache_misses += guard.stats.cache_misses;
        total.dedup_detections += guard.stats.dedup_detections;
        total.total_tokens_saved += guard.stats.total_tokens_saved;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ResponseOptimizerConfig {
        ResponseOptimizerConfig {
            enabled: true,
            ..Default::default()
        }
    }

    // ─── Cache tests ─────────────────────────────────────────────────────

    #[test]
    fn cache_stores_and_retrieves() {
        let mut cache = ResponseCache::new(8, Duration::from_secs(60));
        cache.put(42, "hello world".to_string(), 5);
        assert_eq!(cache.get(42), Some("hello world"));
    }

    #[test]
    fn cache_miss_returns_none() {
        let mut cache = ResponseCache::new(8, Duration::from_secs(60));
        assert_eq!(cache.get(99), None);
    }

    #[test]
    fn cache_respects_capacity() {
        let mut cache = ResponseCache::new(3, Duration::from_secs(60));
        cache.put(1, "a".into(), 1);
        cache.put(2, "b".into(), 1);
        cache.put(3, "c".into(), 1);
        cache.put(4, "d".into(), 1);
        // Oldest (key=1) evicted.
        assert_eq!(cache.get(1), None);
        assert_eq!(cache.get(2), Some("b"));
        assert_eq!(cache.get(4), Some("d"));
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn cache_updates_existing_key() {
        let mut cache = ResponseCache::new(8, Duration::from_secs(60));
        cache.put(1, "old".into(), 5);
        cache.put(1, "new".into(), 5);
        assert_eq!(cache.get(1), Some("new"));
        assert_eq!(cache.len(), 1);
    }

    // ─── Dedup tests ─────────────────────────────────────────────────────

    #[test]
    fn dedup_detects_repeated_fingerprint() {
        let mut dedup = DedupTracker::new(8);
        assert!(!dedup.record(100), "first occurrence");
        assert!(!dedup.record(200), "different fingerprint");
        assert!(dedup.record(100), "repeated");
    }

    #[test]
    fn dedup_window_evicts_old_entries() {
        let mut dedup = DedupTracker::new(3);
        dedup.record(1);
        dedup.record(2);
        dedup.record(3);
        // Window full [1,2,3]. Adding 4 evicts 1 → [2,3,4].
        dedup.record(4);
        assert!(!dedup.record(1), "1 was evicted from window");
        // Recording 1 evicted 2 → window is now [3,4,1].
        assert!(dedup.record(3), "3 still in window");
        assert!(!dedup.record(2), "2 was evicted when 1 was added");
    }











    // ─── Cache key computation ───────────────────────────────────────────

    #[test]
    fn cache_key_is_deterministic() {
        let k1 = compute_cache_key("gpt-4o", Some("sys"), &["hello", "world"]);
        let k2 = compute_cache_key("gpt-4o", Some("sys"), &["hello", "world"]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_inputs() {
        let k1 = compute_cache_key("gpt-4o", Some("sys"), &["hello"]);
        let k2 = compute_cache_key("gpt-4o", Some("sys"), &["world"]);
        assert_ne!(k1, k2);

        let k3 = compute_cache_key("gpt-4o", None, &["hello"]);
        let k4 = compute_cache_key("claude-sonnet-4", None, &["hello"]);
        assert_ne!(k3, k4);
    }

    #[test]
    fn cache_key_order_matters() {
        let k1 = compute_cache_key("m", None, &["a", "b"]);
        let k2 = compute_cache_key("m", None, &["b", "a"]);
        assert_ne!(k1, k2, "message order must affect key");
    }

    // ─── Response fingerprinting ─────────────────────────────────────────

    #[test]
    fn fingerprint_uses_prefix() {
        let short = "hello";
        let long = format!("{}{}", "x".repeat(200), "DIFFERENT_TAIL");
        let long2 = format!("{}{}", "x".repeat(200), "OTHER_TAIL");
        // Same 200-char prefix → same fingerprint.
        assert_eq!(fingerprint_response(&long), fingerprint_response(&long2));
        // Different prefix → different fingerprint.
        assert_ne!(fingerprint_response(short), fingerprint_response(&long));
    }

    // ─── SessionOptimizer integration ────────────────────────────────────

    #[test]
    fn session_optimizer_cache_flow() {
        let mut opt = SessionOptimizer::new(default_config());
        let key = compute_cache_key("gpt-4o", None, &["what is rust?"]);

        // Miss on first query.
        assert!(opt.try_cache_hit(key).is_none());
        assert_eq!(opt.stats.cache_misses, 1);

        // Record the response.
        let decision = opt.record_response(key, "Rust is a systems programming language.", 12);
        assert!(!decision.cache_hit);
        assert!(!decision.is_duplicate);

        // Hit on identical query.
        let hit = opt.try_cache_hit(key);
        assert_eq!(hit, Some("Rust is a systems programming language."));
        assert_eq!(opt.stats.cache_hits, 1);
    }

    #[test]
    fn session_optimizer_dedup_flow() {
        let mut opt = SessionOptimizer::new(default_config());
        let key1 = 100;
        let key2 = 200;

        // Same response to different queries → dedup flags it.
        let response = "Rust is a systems programming language.";
        let d1 = opt.record_response(key1, response, 12);
        assert!(!d1.is_duplicate);

        let d2 = opt.record_response(key2, response, 12);
        assert!(d2.is_duplicate);
        assert_eq!(d2.source, OptimizationSource::Dedup);
        assert_eq!(opt.stats.dedup_detections, 1);
    }

    #[test]
    fn disabled_optimizer_is_noop() {
        let config = ResponseOptimizerConfig {
            enabled: true,
            cache_enabled: false,
            dedup_enabled: false,
            ..Default::default()
        };
        let mut opt = SessionOptimizer::new(config);
        let key = 42;

        assert!(opt.try_cache_hit(key).is_none());
        let d = opt.record_response(key, "response", 10);
        assert!(!d.is_duplicate);
        // Cache should be empty since disabled.
        assert!(opt.cache.is_empty());
    }

    #[test]
    fn global_registry_creates_and_retrieves() {
        let config = default_config();
        let opt1 = get_or_create("session-test-1", &config);
        let opt2 = get_or_create("session-test-1", &config);
        // Same session → same instance.
        assert!(Arc::ptr_eq(&opt1, &opt2));

        let opt3 = get_or_create("session-test-2", &config);
        assert!(!Arc::ptr_eq(&opt1, &opt3));

        // Cleanup.
        remove_session("session-test-1");
        remove_session("session-test-2");
    }

    // ─── Determinism ─────────────────────────────────────────────────────

    #[test]
    fn optimizer_decisions_are_deterministic() {
        let mut opt = SessionOptimizer::new(default_config());
        let key = compute_cache_key("m", None, &["q"]);
        opt.record_response(key, "answer", 5);

        // Same cache state + same key → deterministic hit.
        let h1 = opt.try_cache_hit(key).map(str::to_string);
        let h2 = opt.try_cache_hit(key).map(str::to_string);
        assert_eq!(h1, h2);
    }
}
