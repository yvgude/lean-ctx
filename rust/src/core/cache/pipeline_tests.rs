use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use filetime::{FileTime, set_file_mtime};
use serial_test::serial;

use super::SessionCache;
use crate::core::content_cache::{self, FileState};
use crate::core::ocla::response_cache::{CachedResponse, ResponseCache, ResponseCacheKey};
use crate::core::telemetry::global_metrics;

fn telemetry_counts() -> (u64, u64) {
    let metrics = global_metrics();
    (
        metrics.cache_hits.load(Ordering::Relaxed),
        metrics.cache_misses.load(Ordering::Relaxed),
    )
}

fn response() -> CachedResponse {
    CachedResponse {
        body: b"cached response".to_vec(),
        status: 200,
        tokens: 3,
        created_at: Instant::now(),
        ttl: Duration::from_mins(1),
    }
}

fn insert_content(path: &std::path::Path, body: &str) -> FileState {
    std::fs::write(path, body).expect("write cached file");
    let state = FileState::from_path(path).expect("read file state");
    let admission = crate::core::context_admission::stores::StoreAdmission::current();
    content_cache::insert(path, state, Arc::from(body), &admission);
    state
}

#[test]
#[serial(cache_telemetry)]
fn test_session_cache_hit_updates_telemetry() {
    let mut cache = SessionCache::new();
    cache.store("/pipeline/session.rs", "fn cached() {}");

    let entry = cache.record_cache_hit("/pipeline/session.rs");
    assert!(entry.is_some(), "SessionCache must return entry on hit");
    assert_eq!(entry.unwrap().read_count(), 2, "read_count must bump");
}

#[test]
#[serial(cache_telemetry)]
fn test_content_cache_hit_updates_telemetry() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("content-hit.rs");
    let state = insert_content(&path, "fn content_hit() {}\n");
    let before = telemetry_counts();
    let before_snapshot = global_metrics().snapshot();

    assert!(content_cache::get(&path, state).is_some());

    let after = telemetry_counts();
    let after_snapshot = global_metrics().snapshot();
    assert!(after.0 > before.0, "content hit was not recorded");
    assert!(
        after_snapshot.cache_hit_rate >= before_snapshot.cache_hit_rate,
        "a cache hit must not reduce the telemetry hit rate"
    );
}

#[test]
#[serial(cache_telemetry)]
fn test_cache_miss_updates_telemetry() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("content-miss.rs");
    std::fs::write(&path, "fn missing() {}\n").expect("write queried file");
    let state = FileState::from_path(&path).expect("read file state");
    let before = telemetry_counts();
    let before_snapshot = global_metrics().snapshot();

    assert!(content_cache::get(&path, state).is_none());

    let after = telemetry_counts();
    let after_snapshot = global_metrics().snapshot();
    assert!(after.1 > before.1, "content miss was not recorded");
    assert!(
        after_snapshot.cache_hit_rate <= before_snapshot.cache_hit_rate,
        "a cache miss must not increase the telemetry hit rate"
    );
}

#[test]
#[serial(cache_telemetry)]
fn test_response_cache_key_determinism() {
    let first = ResponseCacheKey::new("gpt-cache", 0x5eed, 0.25, 4096);
    let second = ResponseCacheKey::new("gpt-cache", 0x5eed, 0.25, 4096);

    assert_eq!(first, second);
}

#[test]
#[serial(cache_telemetry)]
fn test_cache_stats_aggregate_correctly() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let content_path = dir.path().join("aggregate-content.rs");
    let content_state = insert_content(&content_path, "fn content() {}\n");
    let missing_path = dir.path().join("aggregate-missing.rs");
    std::fs::write(&missing_path, "fn miss() {}\n").expect("write missing file");
    let missing_state = FileState::from_path(&missing_path).expect("read missing state");

    let responses = ResponseCache::new(4, Duration::from_mins(1));
    let response_key = ResponseCacheKey::new("gpt-cache", 17, 0.0, 128);
    responses.put(response_key.clone(), response());
    let before = telemetry_counts();

    assert!(content_cache::get(&content_path, content_state).is_some());
    assert!(responses.get(&response_key).is_some());
    assert!(content_cache::get(&missing_path, missing_state).is_none());

    let after = telemetry_counts();
    assert!(
        after.0 > before.0,
        "content-cache hit must be recorded in telemetry"
    );
    assert!(after.1 > before.1, "the miss must be recorded");
}

#[test]
#[serial(cache_telemetry)]
fn test_stale_entry_counts_as_miss() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("stale.rs");
    let stored_state = insert_content(&path, "fn stale() {}\n");
    let changed_mtime = SystemTime::now() + Duration::from_secs(2);
    set_file_mtime(&path, FileTime::from_system_time(changed_mtime)).expect("change file mtime");
    let current_state = FileState::from_path(&path).expect("read changed file state");
    assert_ne!(stored_state.mtime_ms, current_state.mtime_ms);
    let before = telemetry_counts();

    assert!(content_cache::get(&path, current_state).is_none());

    let after = telemetry_counts();
    assert!(after.1 > before.1, "stale lookup was not recorded");
}
