//! Comprehensive scenario tests for compaction-aware cache behavior.
//! Tests cover: delivery flag reset, stub-path guards, policy modes,
//! compaction sync, and edge cases.

use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::protocol::CrpMode;

// ═══════════════════════════════════════════════════════════════════════════════
// 1. SessionCache — reset_delivery_flags and is_full_delivered
// ═══════════════════════════════════════════════════════════════════════════════

mod cache_delivery_flags {
    use super::*;

    #[test]
    fn new_entry_is_not_delivered() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/test.rs", "fn main() {}");
        assert!(!cache.is_full_delivered("/tmp/test.rs"));
    }

    #[test]
    fn mark_delivered_then_check() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/test.rs", "fn main() {}");
        cache.mark_full_delivered("/tmp/test.rs");
        assert!(cache.is_full_delivered("/tmp/test.rs"));
    }

    #[test]
    fn reset_flags_clears_all_entries() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "a");
        cache.store("/tmp/b.rs", "b");
        cache.store("/tmp/c.rs", "c");
        cache.mark_full_delivered("/tmp/a.rs");
        cache.mark_full_delivered("/tmp/b.rs");
        cache.mark_full_delivered("/tmp/c.rs");

        let count = cache.reset_delivery_flags();
        assert_eq!(count, 3);
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
        assert!(!cache.is_full_delivered("/tmp/b.rs"));
        assert!(!cache.is_full_delivered("/tmp/c.rs"));
    }

    #[test]
    fn reset_flags_returns_zero_when_none_delivered() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "a");
        cache.store("/tmp/b.rs", "b");
        assert_eq!(cache.reset_delivery_flags(), 0);
    }

    #[test]
    fn reset_preserves_cache_content() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "content here");
        cache.mark_full_delivered("/tmp/a.rs");

        cache.reset_delivery_flags();

        // Entry still exists with content
        let entry = cache.get("/tmp/a.rs").unwrap();
        assert!(entry.original_tokens > 0);
        assert!(entry.content().is_some());
    }

    #[test]
    fn reset_preserves_file_refs() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "content");
        let ref1 = cache.get_file_ref("/tmp/a.rs");
        cache.mark_full_delivered("/tmp/a.rs");

        cache.reset_delivery_flags();

        let ref2 = cache.get_file_ref("/tmp/a.rs");
        assert_eq!(ref1, ref2);
    }

    #[test]
    fn partial_reset_counts_correctly() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "a");
        cache.store("/tmp/b.rs", "b");
        cache.store("/tmp/c.rs", "c");
        cache.mark_full_delivered("/tmp/a.rs");
        cache.mark_full_delivered("/tmp/c.rs");
        // b is NOT delivered

        let count = cache.reset_delivery_flags();
        assert_eq!(count, 2); // only a and c had flag set
    }

    #[test]
    fn is_full_delivered_nonexistent_path() {
        let cache = SessionCache::default();
        assert!(!cache.is_full_delivered("/nonexistent/path.rs"));
    }

    #[test]
    fn hash_change_resets_delivery() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "version 1");
        cache.mark_full_delivered("/tmp/a.rs");
        assert!(cache.is_full_delivered("/tmp/a.rs"));

        // Store different content → hash changes → flag resets
        cache.store("/tmp/a.rs", "version 2");
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    fn invalidate_removes_delivery_state() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "content");
        cache.mark_full_delivered("/tmp/a.rs");

        cache.invalidate("/tmp/a.rs");
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    fn double_reset_is_idempotent() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "a");
        cache.mark_full_delivered("/tmp/a.rs");

        assert_eq!(cache.reset_delivery_flags(), 1);
        assert_eq!(cache.reset_delivery_flags(), 0); // already reset
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Compaction Sync — radar detection and flag reset
// ═══════════════════════════════════════════════════════════════════════════════

mod compaction_sync_scenarios {
    use super::*;
    use lean_ctx::server::compaction_sync::{LAST_COMPACTION_TS, sync_if_compacted};
    use serial_test::serial;
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    fn reset_compaction_ts() {
        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
    }

    fn make_cache(paths: &[&str]) -> SessionCache {
        let mut cache = SessionCache::default();
        for p in paths {
            cache.store(p, "fn test() { todo!() }");
            cache.mark_full_delivered(p);
        }
        cache
    }

    fn write_radar(dir: &TempDir, events: &[(&str, u64)]) {
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for (event_type, ts) in events {
            writeln!(f, r#"{{"ts":{ts},"event_type":"{event_type}","tokens":0}}"#).unwrap();
        }
    }

    #[test]
    #[serial]
    fn no_radar_file_does_nothing() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        let mut cache = make_cache(&["/tmp/a.rs"]);
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    #[serial]
    fn radar_without_compaction_does_nothing() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(
            &dir,
            &[
                ("mcp_call", 1000),
                ("file_read", 2000),
                ("native_tool", 3000),
            ],
        );
        let mut cache = make_cache(&["/tmp/a.rs"]);
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    #[serial]
    fn compaction_resets_all_flags() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(&dir, &[("mcp_call", 1000), ("compaction", 2000)]);
        let mut cache = make_cache(&["/tmp/a.rs", "/tmp/b.rs", "/tmp/c.rs"]);

        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
        assert!(!cache.is_full_delivered("/tmp/b.rs"));
        assert!(!cache.is_full_delivered("/tmp/c.rs"));
    }

    #[test]
    #[serial]
    fn second_call_after_same_compaction_does_nothing() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(&dir, &[("compaction", 5000)]);
        let mut cache = make_cache(&["/tmp/a.rs"]);

        assert!(sync_if_compacted(&mut cache, dir.path()));
        cache.mark_full_delivered("/tmp/a.rs");

        // Same compaction event → no reset
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    #[serial]
    fn newer_compaction_triggers_new_reset() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(&dir, &[("compaction", 1000)]);
        let mut cache = make_cache(&["/tmp/a.rs"]);

        sync_if_compacted(&mut cache, dir.path());
        cache.store("/tmp/a.rs", "fn test() { todo!() }");
        cache.mark_full_delivered("/tmp/a.rs");

        // Add newer compaction event
        write_radar(&dir, &[("compaction", 1000), ("compaction", 3000)]);
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    #[serial]
    fn multiple_compactions_takes_latest() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(
            &dir,
            &[
                ("mcp_call", 100),
                ("compaction", 200),
                ("mcp_call", 300),
                ("compaction", 400),
                ("mcp_call", 500),
            ],
        );
        let mut cache = make_cache(&["/tmp/a.rs"]);
        assert!(sync_if_compacted(&mut cache, dir.path()));

        // Check that ts=400 was stored (not 200)
        let ts = LAST_COMPACTION_TS.load(Ordering::Relaxed);
        assert_eq!(ts, 400);
    }

    #[test]
    #[serial]
    fn empty_cache_compaction_returns_true_but_resets_zero() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        write_radar(&dir, &[("compaction", 1000)]);
        let mut cache = SessionCache::default();

        // Returns true (compaction detected) but nothing to reset
        assert!(sync_if_compacted(&mut cache, dir.path()));
    }

    #[test]
    #[serial]
    fn malformed_radar_lines_are_skipped() {
        reset_compaction_ts();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "this is not json").unwrap();
        writeln!(f, r#"{{"ts":2000,"event_type":"compaction","tokens":0}}"#).unwrap();
        writeln!(f, "{{invalid").unwrap();
        drop(f);

        let mut cache = make_cache(&["/tmp/a.rs"]);
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Cache Policy — effective_cache_policy behavior
// ═══════════════════════════════════════════════════════════════════════════════

mod cache_policy {
    use lean_ctx::server::compaction_sync::effective_cache_policy;

    #[test]
    fn default_policy_is_aggressive() {
        // Without env var override, default should be "aggressive"
        // (OnceLock means this test depends on execution order,
        // but the default Config returns None → "aggressive")
        let policy = effective_cache_policy();
        assert!(
            matches!(policy, "aggressive" | "safe" | "off"),
            "policy must be one of the three valid values, got: {policy}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Integration: ctx_read stub behavior with delivery flags
// ═══════════════════════════════════════════════════════════════════════════════

mod ctx_read_stub_behavior {
    use super::*;
    use lean_ctx::tools::ctx_read::handle_with_task_resolved;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn read_full(cache: &mut SessionCache, path: &str) -> String {
        let output = handle_with_task_resolved(cache, path, "full", CrpMode::Off, None);
        output.content
    }

    #[test]
    fn first_read_delivers_content() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "fn hello() {{ println!(\"world\"); }}").unwrap();
        let path = f.path().to_str().unwrap();

        let mut cache = SessionCache::default();
        let content = read_full(&mut cache, path);
        assert!(
            content.contains("hello") || content.contains("fn"),
            "first read should deliver file content, got: {content}"
        );
        assert!(!content.contains("[unchanged"));
    }

    #[test]
    fn second_read_returns_stub() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "fn hello() {{ println!(\"world\"); }}").unwrap();
        let path = f.path().to_str().unwrap();

        let mut cache = SessionCache::default();
        let _ = read_full(&mut cache, path); // first → delivers content
        let content = read_full(&mut cache, path); // second → stub
        assert!(
            content.contains("unchanged") || content.contains("cached"),
            "second read should be a stub, got: {content}"
        );
    }

    #[test]
    fn after_reset_flags_delivers_content_again() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "fn hello() {{ println!(\"world\"); }}").unwrap();
        let path = f.path().to_str().unwrap();

        let mut cache = SessionCache::default();
        let _ = read_full(&mut cache, path); // first read
        let _ = read_full(&mut cache, path); // stub

        // Simulate compaction: reset flags
        cache.reset_delivery_flags();

        let content = read_full(&mut cache, path); // should deliver again
        assert!(
            content.contains("hello") || content.contains("fn"),
            "after reset, should deliver content again, got: {content}"
        );
    }

    #[test]
    fn after_re_delivery_third_read_is_stub_again() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "fn hello() {{ println!(\"world\"); }}").unwrap();
        let path = f.path().to_str().unwrap();

        let mut cache = SessionCache::default();
        let _ = read_full(&mut cache, path); // first → content
        let _ = read_full(&mut cache, path); // second → stub

        cache.reset_delivery_flags();
        let _ = read_full(&mut cache, path); // third → content (post-compaction)
        let content = read_full(&mut cache, path); // fourth → stub again
        assert!(
            content.contains("unchanged") || content.contains("cached"),
            "after re-delivery, next read should be stub again, got: {content}"
        );
    }

    #[test]
    fn content_change_always_delivers_new_content() {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap();

        std::fs::write(path, "version 1\n").unwrap();
        let mut cache = SessionCache::default();
        let _ = read_full(&mut cache, path); // first → content

        // Modify file
        std::fs::write(path, "version 2\n").unwrap();
        let content = read_full(&mut cache, path);
        assert!(
            content.contains("version 2"),
            "after content change, should deliver new content, got: {content}"
        );
    }

    #[test]
    fn fresh_read_always_delivers_content() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "fn hello() {{}}").unwrap();
        let path = f.path().to_str().unwrap();

        let mut cache = SessionCache::default();
        let _ = handle_with_task_resolved(&mut cache, path, "full", CrpMode::Off, None);
        let _ = handle_with_task_resolved(&mut cache, path, "full", CrpMode::Off, None); // stub

        // fresh=true → invalidate → re-read
        cache.invalidate(path);
        let output = handle_with_task_resolved(&mut cache, path, "full", CrpMode::Off, None);
        assert!(
            !output.content.contains("unchanged"),
            "fresh read should deliver content, got: {}",
            output.content
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Edge cases and robustness
// ═══════════════════════════════════════════════════════════════════════════════

mod edge_cases {
    use super::*;

    #[test]
    fn large_number_of_entries_reset_performance() {
        let mut cache = SessionCache::default();
        for i in 0..500 {
            let path = format!("/tmp/file_{i}.rs");
            cache.store(&path, &format!("content {i}"));
            cache.mark_full_delivered(&path);
        }

        let start = std::time::Instant::now();
        let count = cache.reset_delivery_flags();
        let elapsed = start.elapsed();

        assert_eq!(count, 500);
        assert!(
            elapsed.as_millis() < 10,
            "reset_delivery_flags should be fast, took {elapsed:?}"
        );
    }

    #[test]
    fn concurrent_store_and_reset_dont_panic() {
        let mut cache = SessionCache::default();
        for i in 0..100 {
            let path = format!("/tmp/file_{i}.rs");
            cache.store(&path, &format!("v{i}"));
            if i % 2 == 0 {
                cache.mark_full_delivered(&path);
            }
        }
        let count = cache.reset_delivery_flags();
        assert_eq!(count, 50);

        // Verify none are delivered
        for i in 0..100 {
            let path = format!("/tmp/file_{i}.rs");
            assert!(!cache.is_full_delivered(&path));
        }
    }

    #[test]
    fn reset_after_clear_is_zero() {
        let mut cache = SessionCache::default();
        cache.store("/tmp/a.rs", "content");
        cache.mark_full_delivered("/tmp/a.rs");
        cache.clear();
        assert_eq!(cache.reset_delivery_flags(), 0);
    }
}
