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
//!
//! A successful write clears the cache ([`invalidate_all`]), and a read with
//! `_refresh=1` (the Cockpit's forced reload) is computed fresh.

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
    // Verifies the audit trail's chain from genesis for the anchors.
    "/api/gateway/receipts",
];

/// Check if a route is cache-eligible.
pub(super) fn is_cacheable(path: &str, method: &str) -> bool {
    method.eq_ignore_ascii_case("GET") && CACHED_ROUTES.contains(&path)
}

/// The cache key of `path` for the current request: a read bound to a named
/// project (`X-LeanCTX-Project`) never shares an entry with another project.
pub(super) fn key(path: &str) -> String {
    match super::helpers::request_project() {
        Some(project) => format!("{project}\0{path}"),
        None => path.to_string(),
    }
}

/// Try to get a cached response. Returns `Some((status, content_type, body))`
/// if fresh, `None` if stale or absent.
pub(super) fn get(key: &str) -> Option<(&'static str, &'static str, String)> {
    let cache = CACHE.lock().ok()?;
    let entry = cache.get(key)?;
    if entry.is_fresh() {
        let (ref ct, ref body) = entry.response;
        Some(("200 OK", leak_content_type(ct), body.clone()))
    } else {
        None
    }
}

/// Store a response in the cache.
pub(super) fn put(key: &str, content_type: &str, body: &str) {
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(
            key.to_string(),
            CacheEntry {
                response: (content_type.to_string(), body.to_string()),
                created: Instant::now(),
                ttl: DEFAULT_TTL,
            },
        );
        evict_stale(&mut cache);
    }
}

/// Drop every cached response, for every project. Called after each
/// successful write: the cached reads are derived from the ledger, overlays,
/// sessions and config that the writes change (a settings write changes them
/// for every project), and entries live for seconds, so clearing all is
/// cheaper and safer than a per-route map that misses a route added later.
pub(super) fn invalidate_all() {
    if let Ok(mut cache) = CACHE.lock() {
        cache.clear();
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

    // Tests that read back what they stored hold the env lock: a route test
    // that writes (and so clears the cache) holds it too.
    #[test]
    fn cache_stores_and_retrieves() {
        let _lock = crate::core::data_dir::test_env_lock();
        put("/api/signals", "application/json", r#"{"test":true}"#);
        let result = get("/api/signals");
        assert!(result.is_some());
        let (status, ct, body) = result.unwrap();
        assert_eq!(status, "200 OK");
        assert_eq!(ct, "application/json");
        assert_eq!(body, r#"{"test":true}"#);
    }

    #[test]
    fn a_read_for_one_project_never_answers_another_from_cache() {
        use crate::core::policy::runtime::in_project;
        let _lock = crate::core::data_dir::test_env_lock();
        in_project("/work/alpha", || {
            put(
                &key("/api/context-risk"),
                "application/json",
                r#"{"p":"alpha"}"#,
            );
        });
        let beta = in_project("/work/beta", || get(&key("/api/context-risk")));
        let unscoped = get(&key("/api/context-risk"));
        let alpha = in_project("/work/alpha", || get(&key("/api/context-risk")));
        assert!(beta.is_none());
        assert!(unscoped.is_none());
        assert_eq!(alpha.map(|r| r.2).as_deref(), Some(r#"{"p":"alpha"}"#));
    }

    #[test]
    fn invalidate_all_drops_every_project_entry() {
        use crate::core::policy::runtime::in_project;
        let _lock = crate::core::data_dir::test_env_lock();
        put(&key("/api/session"), "application/json", "{}");
        in_project("/work/gamma", || {
            put(&key("/api/session"), "application/json", "{}");
        });
        invalidate_all();
        assert!(get(&key("/api/session")).is_none());
        assert!(in_project("/work/gamma", || get(&key("/api/session"))).is_none());
    }

    /// The router drops the cached reads after a successful write, so a
    /// context read shows a new rule at once; a refused write keeps them.
    #[test]
    fn a_successful_write_drops_the_cached_reads() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project");
        let root = project.path().to_string_lossy().into_owned();
        let post = |body: &str| {
            crate::dashboard::routes::route_response(
                "/api/context-policy",
                "",
                None,
                None,
                true,
                "POST",
                body,
            )
        };
        crate::dashboard::project_binding::in_request_project(Some(&root), || {
            let cached = key("/api/context-risk");
            put(&cached, "application/json", r#"{"cached":true}"#);
            assert_eq!(post("{}").0, "400 Bad Request");
            assert!(get(&cached).is_some(), "a refused write changes nothing");

            let rule =
                r#"{"action":"add","rule":{"name":"docs","match":"*.md","action":"exclude"}}"#;
            let (status, _, body) = post(rule);
            assert_eq!(status, "200 OK", "{body}");
            assert!(get(&cached).is_none(), "the write dropped the cached read");
        });
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
