//! Tests verifying that `ctx_read` never hangs indefinitely due to session lock contention.
//! Covers the Windows deadlock scenario: shell-hook holds session write-lock while
//! `ctx_read` waits on session read-lock, causing circular dependency.

use lean_ctx::core::pathutil::safe_canonicalize_bounded;
use std::path::Path;
use std::time::{Duration, Instant};

// ═══════════════════════════════════════════════════════════════════════════════
// 1. safe_canonicalize_bounded — timeout behavior
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn canonicalize_existing_path_succeeds_quickly() {
    let start = Instant::now();
    let result = safe_canonicalize_bounded(Path::new("/tmp"), 2000);
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_secs(1));
    // On macOS /tmp -> /private/tmp
    assert!(
        result.to_str().unwrap().contains("tmp"),
        "should resolve to a path containing 'tmp', got: {}",
        result.display()
    );
}

#[test]
fn canonicalize_nonexistent_path_returns_original() {
    let fake = Path::new("/nonexistent/path/that/does/not/exist/xyzzy123");
    let start = Instant::now();
    let result = safe_canonicalize_bounded(fake, 2000);
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_secs(1));
    assert_eq!(result, fake.to_path_buf());
}

#[test]
fn canonicalize_bounded_respects_timeout() {
    // We can't easily simulate a hanging canonicalize on non-Windows,
    // but we verify the function doesn't panic and completes quickly
    // for various path types
    let paths = &[
        "/tmp",
        "/var",
        "/usr/bin",
        "/this/does/not/exist",
        "relative/path/here",
    ];

    for p in paths {
        let start = Instant::now();
        let _ = safe_canonicalize_bounded(Path::new(p), 500);
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "canonicalize took too long for {p}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Session lock timeout — simulated contention
// ═══════════════════════════════════════════════════════════════════════════════

mod session_lock_timeout {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::RwLock;

    #[test]
    fn read_lock_succeeds_when_uncontested() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let lock = Arc::new(RwLock::new(42u32));

        rt.block_on(async {
            let result = tokio::time::timeout(Duration::from_secs(5), lock.read()).await;
            assert!(result.is_ok());
            assert_eq!(*result.unwrap(), 42);
        });
    }

    #[test]
    fn read_lock_times_out_when_write_held() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let lock = Arc::new(RwLock::new(42u32));
        let lock2 = lock.clone();

        rt.block_on(async {
            // Hold write lock in background for 3 seconds
            let _guard = lock2.write().await;

            let start = Instant::now();
            let result = tokio::time::timeout(Duration::from_millis(200), lock.read()).await;

            assert!(result.is_err(), "should have timed out");
            assert!(start.elapsed() < Duration::from_millis(400));
        });
    }

    #[test]
    fn write_lock_times_out_when_read_held() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let lock = Arc::new(RwLock::new(42u32));
        let lock2 = lock.clone();

        rt.block_on(async {
            let _guard = lock2.read().await;

            let start = Instant::now();
            let result = tokio::time::timeout(Duration::from_millis(200), lock.write()).await;

            assert!(result.is_err(), "should have timed out");
            assert!(start.elapsed() < Duration::from_millis(400));
        });
    }

    #[test]
    fn block_in_place_timeout_pattern_works() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let lock = Arc::new(RwLock::new(String::from("hello")));
        let lock2 = lock.clone();

        rt.block_on(async {
            // Simulate a held write lock (like shell-hook holding session)
            let _guard = lock2.write().await;

            // This is the exact pattern used in ctx_read.rs
            let result = tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(tokio::time::timeout(
                    Duration::from_millis(300),
                    lock.read(),
                ))
            });

            assert!(result.is_err(), "should timeout, not deadlock");
        });
    }

    #[test]
    fn multiple_readers_succeed_concurrently() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let lock = Arc::new(RwLock::new(String::from("data")));

        rt.block_on(async {
            let mut handles = Vec::new();
            for _ in 0..10 {
                let l = lock.clone();
                handles.push(tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(1), l.read()).await;
                    assert!(result.is_ok());
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        });
    }

    #[test]
    fn graceful_degradation_on_write_timeout() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let lock = Arc::new(RwLock::new(String::from("session_data")));
        let lock2 = lock.clone();

        rt.block_on(async {
            let _guard = lock2.read().await;

            // Simulate the post-read session update (write lock)
            let start = Instant::now();
            let session_guard = tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(tokio::time::timeout(
                    Duration::from_millis(200),
                    lock.write(),
                ))
            });

            // Should timeout gracefully, not hang
            assert!(session_guard.is_err());
            assert!(start.elapsed() < Duration::from_millis(400));
            // In production, we'd fall back to project_root from ctx
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Integration: read_file_lossy doesn't hang on nonexistent/weird paths
// ═══════════════════════════════════════════════════════════════════════════════

mod read_file_safety {
    use lean_ctx::tools::ctx_read::read_file_lossy;
    use std::time::{Duration, Instant};

    #[test]
    fn nonexistent_file_errors_quickly() {
        let start = Instant::now();
        let result = read_file_lossy("/this/path/does/not/exist/file.rs");
        assert!(result.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "should fail fast, not hang"
        );
    }

    #[test]
    fn existing_file_reads_within_timeout() {
        let start = Instant::now();
        let result = read_file_lossy("/tmp/.lean-ctx-test-read");
        // May error (file doesn't exist) or succeed, but must not hang
        let _ = result;
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "should complete within timeout"
        );
    }

    #[test]
    fn deeply_nested_nonexistent_path_errors_quickly() {
        let deep = "/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/file.txt";
        let start = Instant::now();
        let result = read_file_lossy(deep);
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[cfg(windows)]
    #[test]
    fn windows_gemini_path_doesnt_hang() {
        // Simulates the exact path pattern from the bug report
        let path = r"C:\Users\USER\.gemini\some_config.json";
        let start = Instant::now();
        let _ = read_file_lossy(path);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Windows .gemini path must not hang"
        );
    }
}
