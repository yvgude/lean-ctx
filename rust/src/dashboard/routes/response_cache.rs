//! TTL-based in-memory response cache for expensive dashboard API routes.
//!
//! Routes like `/api/signals`, `/api/context-risk`, `/api/context-triage`,
//! and `/api/session` perform heavy disk I/O (git scans, ledger reads,
//! overlay loading). When the dashboard polls every 10s and each panel
//! fires 4+ requests, these can take 4–7s each, causing timeouts.
//!
//! This cache stores computed responses for a configurable TTL (default 5s),
//! so repeated requests within the window return instantly. The hash-based
//! `/api/pulse` mechanism still detects real changes; this just prevents
//! redundant recomputation within a single poll cycle.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_TTL: Duration = Duration::from_secs(5);

struct CacheEntry {
    response: (String, String),
    created: Instant,
    ttl: Duration,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        self.created.elapsed() < self.ttl
    }
}

static CACHE: std::sync::LazyLock<Mutex<HashMap<String, CacheEntry>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Routes eligible for caching (expensive disk I/O, read-only GET).
const CACHED_ROUTES: &[&str] = &[
    "/api/signals",
    "/api/session",
    "/api/context-risk",
    "/api/context-triage",
    "/api/context-bounce",
    "/api/context-radar",
    "/api/context-events",
    "/api/context-model",
    // Re-walks the savings ledger and audit trail from genesis.
    "/api/value",
];

/// Check if a route is cache-eligible.
pub(super) fn is_cacheable(path: &str, method: &str) -> bool {
    method.eq_ignore_ascii_case("GET") && CACHED_ROUTES.contains(&path)
}

/// Try to get a cached response. Returns `Some((status, content_type, body))`
/// if fresh, `None` if stale or absent.
pub(super) fn get(path: &str) -> Option<(&'static str, &'static str, String)> {
    let cache = CACHE.lock().ok()?;
    let entry = cache.get(path)?;
    if entry.is_fresh() {
        let (ref ct, ref body) = entry.response;
        Some(("200 OK", leak_content_type(ct), body.clone()))
    } else {
        None
    }
}

/// Store a response in the cache.
pub(super) fn put(path: &str, content_type: &str, body: &str) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(
            path.to_string(),
            CacheEntry {
                response: (content_type.to_string(), body.to_string()),
                created: Instant::now(),
                ttl: DEFAULT_TTL,
            },
        );
        evict_stale(&mut cache);
    }
}

/// Invalidate a specific route (e.g. after a POST/mutation).
#[allow(dead_code)]
pub(super) fn invalidate(path: &str) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.remove(path);
    }
}

fn evict_stale(cache: &mut HashMap<String, CacheEntry>) {
    if cache.len() > 20 {
        cache.retain(|_, e| e.is_fresh());
    }
}

fn leak_content_type(ct: &str) -> &'static str {
    if ct == "application/json" {
        "application/json"
    } else {
        "text/plain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_stores_and_retrieves() {
        put("/api/signals", "application/json", r#"{"test":true}"#);
        let result = get("/api/signals");
        assert!(result.is_some());
        let (status, ct, body) = result.unwrap();
        assert_eq!(status, "200 OK");
        assert_eq!(ct, "application/json");
        assert_eq!(body, r#"{"test":true}"#);
    }

    #[test]
    fn non_cached_route_returns_none() {
        assert!(get("/api/stats").is_none());
    }

    #[test]
    fn is_cacheable_checks_method_and_path() {
        assert!(is_cacheable("/api/signals", "GET"));
        assert!(!is_cacheable("/api/signals", "POST"));
        assert!(!is_cacheable("/api/stats", "GET"));
    }
}
