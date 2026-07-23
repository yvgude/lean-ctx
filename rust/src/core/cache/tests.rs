use super::*;
use std::time::Duration;

#[test]
fn cache_stores_and_retrieves() {
    let mut cache = SessionCache::new();
    let result = cache.store("/test/file.rs", "fn main() {}");
    assert!(!result.was_hit);
    assert_eq!(result.line_count, 1);
    assert!(cache.get("/test/file.rs").is_some());
}

#[test]
fn cache_hit_on_same_content() {
    let mut cache = SessionCache::new();
    cache.store("/test/file.rs", "content");
    let result = cache.store("/test/file.rs", "content");
    assert!(result.was_hit, "same content should be a cache hit");
}

#[test]
fn cache_miss_on_changed_content() {
    let mut cache = SessionCache::new();
    cache.store("/test/file.rs", "old content");
    let result = cache.store("/test/file.rs", "new content");
    assert!(!result.was_hit, "changed content should not be a cache hit");
}

#[test]
fn file_refs_are_sequential() {
    let mut cache = SessionCache::new();
    assert_eq!(cache.get_file_ref("/a.rs"), "F1");
    assert_eq!(cache.get_file_ref("/b.rs"), "F2");
    assert_eq!(cache.get_file_ref("/a.rs"), "F1"); // stable
}

#[test]
fn cache_clear_resets_everything() {
    let mut cache = SessionCache::new();
    cache.store("/a.rs", "a");
    cache.store("/b.rs", "b");
    let count = cache.clear();
    assert_eq!(count, 2);
    assert!(cache.get("/a.rs").is_none());
    assert_eq!(cache.get_file_ref("/c.rs"), "F1"); // refs reset
}

#[test]
fn cache_invalidate_removes_entry() {
    let mut cache = SessionCache::new();
    cache.store("/test.rs", "test");
    assert!(cache.invalidate("/test.rs"));
    assert!(!cache.invalidate("/nonexistent.rs"));
}

#[test]
fn cache_stats_track_correctly() {
    let mut cache = SessionCache::new();
    cache.store("/a.rs", "hello");
    cache.store("/a.rs", "hello"); // hit
    let stats = cache.get_stats();
    assert_eq!(stats.total_reads(), 2);
    assert_eq!(stats.cache_hits(), 1);
    assert!(stats.hit_rate() > 0.0);
}

#[test]
fn current_full_content_serves_cached_when_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("handover.md");
    std::fs::write(&file, "HANDOVER V1\n").unwrap();
    let path = file.to_str().unwrap();

    let mut cache = SessionCache::new();
    cache.store(path, "HANDOVER V1\n");

    let (content, tokens) = cache.current_full_content(path).unwrap();
    assert_eq!(content, "HANDOVER V1\n");
    assert!(tokens > 0);
}

#[test]
fn current_full_content_rereads_when_file_changed() {
    // Handover staleness: a file cached by agent A and then edited must not
    // be served from the stale cache to agent B (ctx_retrieve / ctx_share).
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("handover.md");
    std::fs::write(&file, "HANDOVER V1\n").unwrap();
    let path = file.to_str().unwrap();

    let mut cache = SessionCache::new();
    cache.store(path, "HANDOVER V1\n");

    // Simulate an edit between agents (new mtime + new content).
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(&file, "HANDOVER V2 CHANGED\n").unwrap();

    let (content, _) = cache.current_full_content(path).unwrap();
    assert_eq!(
        content, "HANDOVER V2 CHANGED\n",
        "stale cached copy must be re-read from disk, not served as-is"
    );
}

#[test]
fn current_full_content_none_without_entry() {
    let cache = SessionCache::new();
    assert!(cache.current_full_content("/no/such/file.rs").is_none());
}

#[test]
fn current_full_content_falls_back_to_cache_when_file_unreadable() {
    // Stale + now-unreadable (deleted/moved): there is no current content to
    // serve, so the last-known cached copy is returned rather than nothing.
    // Canonicalize the temp dir up front so the cache key is stable after the
    // file is removed (macOS /var -> /private/var symlink).
    let dir = tempfile::tempdir().unwrap();
    let canon = dir.path().canonicalize().unwrap();
    let file = canon.join("gone.md");
    std::fs::write(&file, "ORIGINAL\n").unwrap();
    let path = file.to_str().unwrap().to_string();

    let mut cache = SessionCache::new();
    cache.store(&path, "ORIGINAL\n");
    std::fs::remove_file(&file).unwrap();

    let (content, _) = cache.current_full_content(&path).unwrap();
    assert_eq!(
        content, "ORIGINAL\n",
        "unreadable file must fall back to last-known cached content"
    );
}

#[test]
fn record_cache_hit_works_through_shared_ref() {
    let mut cache = SessionCache::new();
    cache.store("/x.rs", "hello world");
    // &self path: a cache hit can be recorded without a write lock.
    let shared: &SessionCache = &cache;
    assert!(shared.record_cache_hit("/x.rs").is_some());
    assert!(shared.record_cache_hit("/x.rs").is_some());
    // store=1 + two hits => read_count 3, cache_hits 2.
    assert_eq!(cache.get("/x.rs").unwrap().read_count(), 3);
    assert_eq!(cache.get_stats().cache_hits(), 2);
}

#[test]
fn concurrent_cache_hits_are_lossless() {
    use std::sync::Arc;
    let mut cache = SessionCache::new();
    cache.store("/a.rs", "a");
    cache.store("/b.rs", "b");
    // Shared (no RwLock): proves SessionCache is Sync and hit recording is
    // lock-free and atomic — the whole point of the read-mostly refactor.
    let cache = Arc::new(cache);
    let threads = 8;
    let iters = 1_000;
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let c = Arc::clone(&cache);
            std::thread::spawn(move || {
                for _ in 0..iters {
                    c.record_cache_hit("/a.rs");
                    c.record_cache_hit("/b.rs");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let total = (threads * iters) as u64;
    assert_eq!(cache.get_stats().cache_hits(), total * 2);
    assert_eq!(cache.get("/a.rs").unwrap().read_count(), 1 + total as u32);
    assert_eq!(cache.get("/b.rs").unwrap().read_count(), 1 + total as u32);
}

#[test]
fn hebbian_eviction_bonus_is_wired() {
    // #3: files read together build a Hebbian association via store()'s
    // recording, and that association must feed the eviction bonus.
    //
    // Widen the burst window so the two store() calls below always land in
    // one burst — independent of real time. Previously this test warmed up
    // tiktoken and hoped the calls stayed inside the real 500ms window; that
    // still flaked under parallel test execution when scheduling jitter
    // delayed one store() past the window. An injected window removes the
    // wall-clock dependency entirely.
    let mut cache = SessionCache::new();
    cache.set_co_access_burst_window(std::time::Duration::from_secs(3600));
    cache.store("/a.rs", "fn a() {}");
    cache.store("/b.rs", "fn b() {}");
    cache.flush_co_access(); // commit the burst → association (a,b) forms
    let bonus = cache.hebbian_eviction_bonus();
    assert!(
        !bonus.is_empty(),
        "co-accessed reads must yield a Hebbian eviction bonus (#3 wired)"
    );
}

#[test]
fn md5_is_deterministic() {
    let h1 = compute_md5("test content");
    let h2 = compute_md5("test content");
    assert_eq!(h1, h2);
    assert_ne!(h1, compute_md5("different"));
}

#[test]
fn rrf_eviction_prefers_recent() {
    let key_a = "a.rs".to_string();
    let key_b = "b.rs".to_string();
    // Construct entries first so the global instant base is initialized,
    // then assign access times relative to a post-init reference.
    let recent = CacheEntry::new("a", "h1".to_string(), 1, 10, "/a.rs".to_string(), None);
    let old = CacheEntry::new("b", "h2".to_string(), 1, 10, "/b.rs".to_string(), None);
    let t_old = Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t_recent = Instant::now();
    old.set_last_access(t_old);
    recent.set_last_access(t_recent);
    let now = Instant::now();
    let entries: Vec<(&String, &CacheEntry)> = vec![(&key_a, &recent), (&key_b, &old)];
    let scores = eviction_scores_rrf(&entries, now);
    let score_a = scores.iter().find(|(p, _)| p == "a.rs").unwrap().1;
    let score_b = scores.iter().find(|(p, _)| p == "b.rs").unwrap().1;
    assert!(
        score_a > score_b,
        "recently accessed entries should score higher via RRF"
    );
}

#[test]
fn rrf_eviction_prefers_frequent() {
    let now = Instant::now();
    let key_a = "a.rs".to_string();
    let key_b = "b.rs".to_string();
    let frequent = {
        let e = CacheEntry::new("a", "h1".to_string(), 1, 10, "/a.rs".to_string(), None);
        e.set_read_count(20);
        e
    };
    let rare = CacheEntry::new("b", "h2".to_string(), 1, 10, "/b.rs".to_string(), None);
    let entries: Vec<(&String, &CacheEntry)> = vec![(&key_a, &frequent), (&key_b, &rare)];
    let scores = eviction_scores_rrf(&entries, now);
    let score_a = scores.iter().find(|(p, _)| p == "a.rs").unwrap().1;
    let score_b = scores.iter().find(|(p, _)| p == "b.rs").unwrap().1;
    assert!(
        score_a > score_b,
        "frequently accessed entries should score higher via RRF"
    );
}

#[test]
fn cache_budget_resolver_precedence() {
    // env wins when positive
    assert_eq!(resolve_cache_max_tokens(Some("250000"), 999), 250_000);
    assert_eq!(resolve_cache_max_tokens(Some(" 80000 "), 0), 80_000);
    // env 0 / blank / garbage falls through to config
    assert_eq!(resolve_cache_max_tokens(Some("0"), 123_456), 123_456);
    assert_eq!(resolve_cache_max_tokens(Some(""), 123_456), 123_456);
    assert_eq!(resolve_cache_max_tokens(Some("lots"), 123_456), 123_456);
    // no env → config field
    assert_eq!(resolve_cache_max_tokens(None, 42_000), 42_000);
    // nothing set anywhere → built-in default
    assert_eq!(resolve_cache_max_tokens(None, 0), DEFAULT_CACHE_MAX_TOKENS);
    assert_eq!(
        resolve_cache_max_tokens(Some("0"), 0),
        DEFAULT_CACHE_MAX_TOKENS
    );
}

#[test]
fn evict_if_needed_removes_lowest_score() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_CACHE_MAX_TOKENS", "50");
    let mut cache = SessionCache::new();
    let big_content = "a]".repeat(30); // ~30 tokens
    cache.store("/old.rs", &big_content);
    // /old.rs now in cache with ~30 tokens

    let new_content = "b ".repeat(30); // ~30 tokens incoming
    cache.store("/new.rs", &new_content);
    // should have evicted /old.rs to make room
    // (total would be ~60 which exceeds 50)

    // At least one should remain, total should be <= 50
    assert!(
        cache.total_cached_tokens() <= 60,
        "eviction should have kicked in"
    );
    crate::test_env::remove_var("LEAN_CTX_CACHE_MAX_TOKENS");
}

#[test]
fn stale_detection_flags_newer_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.txt");
    let p = path.to_string_lossy().to_string();

    std::fs::write(&path, "one").unwrap();
    let mut cache = SessionCache::new();
    cache.store(&p, "one");

    let entry = cache.get(&p).unwrap();
    assert!(!is_cache_entry_stale(&p, entry.stored_mtime));

    // Ensure mtime granularity differences don't make this flaky.
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "two").unwrap();

    let entry = cache.get(&p).unwrap();
    assert!(is_cache_entry_stale(&p, entry.stored_mtime));
}

// P0-7 (#419): a *backward* mtime (git checkout, touch -t) is a change.
#[test]
fn stale_detection_flags_backward_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backward.txt");
    let p = path.to_string_lossy().to_string();

    std::fs::write(&path, "one").unwrap();
    let mut cache = SessionCache::new();
    cache.store(&p, "one");
    let entry_mtime = cache.get(&p).unwrap().stored_mtime;
    assert!(!is_cache_entry_stale(&p, entry_mtime));

    // Simulate `git checkout` of an older version: content + older mtime.
    std::fs::write(&path, "zero").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_hours(1))
        .unwrap();
    drop(f);

    assert!(
        is_cache_entry_stale(&p, entry_mtime),
        "older mtime must read as stale"
    );
}

// P0-7 (#419): identical mtime with different content (same-second write,
// restored timestamps) is caught by the content-hash verification.
#[test]
fn verified_staleness_catches_same_mtime_content_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sneaky.txt");
    let p = path.to_string_lossy().to_string();

    std::fs::write(&path, "one").unwrap();
    let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut cache = SessionCache::new();
    cache.store(&p, "one");
    let (mtime, hash) = {
        let e = cache.get(&p).unwrap();
        (e.stored_mtime, e.hash.clone())
    };

    // Unchanged file: both checks agree it is fresh.
    assert!(!is_cache_entry_stale_verified(&p, mtime, &hash));

    // Change the content but restore the exact original mtime.
    std::fs::write(&path, "two").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(original_mtime).unwrap();
    drop(f);

    assert!(
        !is_cache_entry_stale(&p, mtime),
        "test premise: the mtime check alone is fooled"
    );
    assert!(
        is_cache_entry_stale_verified(&p, mtime, &hash),
        "hash verification must catch the change"
    );
}

#[test]
fn verified_staleness_flags_unreadable_file() {
    let mut cache = SessionCache::new();
    cache.store("/nonexistent/file.rs", "content");
    let (mtime, hash) = {
        let e = cache.get("/nonexistent/file.rs").unwrap();
        (e.stored_mtime, e.hash.clone())
    };
    assert!(is_cache_entry_stale_verified(
        "/nonexistent/file.rs",
        mtime,
        &hash
    ));
}

#[test]
fn compressed_outputs_cached_and_retrieved() {
    let mut cache = SessionCache::new();
    cache.store("/test.rs", "fn main() {}");
    cache.set_compressed("/test.rs", "map", "compressed map output".to_string());
    assert_eq!(
        cache.get_compressed("/test.rs", "map"),
        Some(&"compressed map output".to_string())
    );
    assert_eq!(cache.get_compressed("/test.rs", "signatures"), None);
}

#[test]
fn compressed_outputs_cleared_on_content_change() {
    let mut cache = SessionCache::new();
    cache.store("/test.rs", "old content");
    cache.set_compressed("/test.rs", "map", "old map".to_string());
    assert!(cache.get_compressed("/test.rs", "map").is_some());

    cache.store("/test.rs", "new content");
    assert_eq!(cache.get_compressed("/test.rs", "map"), None);
}

#[test]
fn compressed_outputs_survive_same_content_store() {
    let mut cache = SessionCache::new();
    cache.store("/test.rs", "content");
    cache.set_compressed("/test.rs", "map", "cached map".to_string());

    let result = cache.store("/test.rs", "content");
    assert!(result.was_hit);
    assert_eq!(
        cache.get_compressed("/test.rs", "map"),
        Some(&"cached map".to_string())
    );
}

#[test]
fn compressed_outputs_cleared_on_invalidate() {
    let mut cache = SessionCache::new();
    cache.store("/test.rs", "content");
    cache.set_compressed("/test.rs", "signatures", "cached sigs".to_string());
    cache.invalidate("/test.rs");
    assert_eq!(cache.get_compressed("/test.rs", "signatures"), None);
}

#[test]
fn compressed_outputs_cleared_on_clear() {
    let mut cache = SessionCache::new();
    cache.store("/a.rs", "a");
    cache.set_compressed("/a.rs", "map", "map_a".to_string());
    cache.clear();
    assert_eq!(cache.get_compressed("/a.rs", "map"), None);
}
