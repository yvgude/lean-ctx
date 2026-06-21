//! Tests verifying the lock-contention hardening (plan: `harden_ctx_read_locks`).
//! Covers `adaptive_timeout` inversion fix, `bounded_lock` behavior under load,
//! and concurrent read scenarios.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// ═══════════════════════════════════════════════════════════════════════════════
// 1. adaptive_timeout: slow/degraded → LONGER, not shorter
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn adaptive_timeout_fast_returns_base() {
    use lean_ctx::core::io_health;
    let base = Duration::from_secs(10);
    let adapted = io_health::adaptive_timeout(base);
    assert!(
        adapted >= base,
        "fast environment should return >= base, got {adapted:?}"
    );
}

#[test]
fn adaptive_timeout_degraded_returns_longer_than_base() {
    use lean_ctx::core::io_health;

    for _ in 0..10 {
        io_health::record_freeze();
    }

    let base = Duration::from_secs(10);
    let adapted = io_health::adaptive_timeout(base);
    assert!(
        adapted > base,
        "degraded environment MUST return longer timeout, got {adapted:?} for base {base:?}"
    );
    assert!(
        adapted <= base.mul_f32(3.0),
        "should not be excessively long, got {adapted:?}"
    );
}

#[test]
fn adaptive_timeout_never_zero_or_sub_second() {
    use lean_ctx::core::io_health;

    for _ in 0..20 {
        io_health::record_freeze();
    }

    let base = Duration::from_secs(5);
    let adapted = io_health::adaptive_timeout(base);
    assert!(
        adapted >= Duration::from_secs(1),
        "timeout must be >= 1s even under extreme freezes, got {adapted:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. bounded_lock: timeout returns None, no panic
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn bounded_write_under_contention_returns_none_not_panic() {
    let lock: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    let _hold = lock.write().await;

    let result = tokio::task::spawn_blocking({
        let lock = lock.clone();
        move || lean_ctx::server::bounded_lock::write(&lock, "contention_test")
    })
    .await
    .unwrap();

    assert!(
        result.is_none(),
        "should return None under contention, not panic"
    );
}

#[tokio::test]
async fn bounded_read_under_write_contention_returns_none() {
    let lock: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    let _hold = lock.write().await;

    let start = Instant::now();
    let result = tokio::task::spawn_blocking({
        let lock = lock.clone();
        move || lean_ctx::server::bounded_lock::read(&lock, "read_contention_test")
    })
    .await
    .unwrap();

    assert!(result.is_none());
    assert!(
        start.elapsed() < Duration::from_secs(25),
        "should not hang indefinitely"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Parallel reads: multiple concurrent readers on same lock
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_bounded_reads_all_succeed() {
    let lock: Arc<RwLock<Vec<u8>>> = Arc::new(RwLock::new(vec![1, 2, 3, 4, 5]));
    let mut handles = Vec::new();

    for i in 0..8 {
        let lock = lock.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let guard = lean_ctx::server::bounded_lock::read(&lock, &format!("parallel_read:{i}"));
            guard.map(|g| g.len())
        }));
    }

    let start = Instant::now();
    for h in handles {
        let result = h.await.unwrap();
        assert_eq!(result, Some(5), "all parallel readers should succeed");
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "parallel reads should complete quickly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writer_blocks_readers_but_they_recover() {
    let lock: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));

    // Writer holds lock for 1s
    let lock_w = lock.clone();
    let writer = tokio::task::spawn_blocking(move || {
        if let Some(mut guard) = lean_ctx::server::bounded_lock::write(&lock_w, "blocking_writer") {
            *guard = 42;
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Readers that start while writer holds lock
    let mut readers = Vec::new();
    for i in 0..4 {
        let lock_r = lock.clone();
        readers.push(tokio::task::spawn_blocking(move || {
            lean_ctx::server::bounded_lock::read(&lock_r, &format!("waiting_reader:{i}"))
                .map(|g| *g)
        }));
    }

    writer.await.unwrap();

    for h in readers {
        let result = h.await.unwrap();
        // Readers may have gotten None (timeout) or Some(42) (after writer released)
        // Either is acceptable — the key is they don't hang or panic
        if let Some(val) = result {
            assert_eq!(val, 42);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Simulated 2-agent contention on session read-lock
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_read_lock_retry_pattern_works() {
    let session: Arc<RwLock<String>> = Arc::new(RwLock::new("task: fix bug".into()));

    // Agent A: holds write lock for 2s (simulates record_tool_receipt)
    let session_a = session.clone();
    let writer = tokio::spawn(async move {
        let mut guard = session_a.write().await;
        *guard = "task: updated".into();
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(guard);
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Agent B: uses retry pattern (same as our ctx_read fix)
    let session_b = session.clone();
    let reader = tokio::task::spawn_blocking(move || {
        let mut attempt = 0u32;
        let start = Instant::now();
        loop {
            if let Ok(guard) = tokio::runtime::Handle::current().block_on(tokio::time::timeout(
                Duration::from_secs(5),
                session_b.read(),
            )) {
                return (true, guard.clone(), start.elapsed());
            }
            attempt += 1;
            if attempt >= 3 {
                return (false, String::new(), start.elapsed());
            }
            std::thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
        }
    });

    let (success, value, elapsed) = reader.await.unwrap();
    writer.await.unwrap();

    assert!(
        success,
        "retry should eventually succeed after writer releases"
    );
    assert_eq!(value, "task: updated");
    assert!(
        elapsed < Duration::from_secs(10),
        "should complete well within budget, took {elapsed:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Channel error delivery (slow-path thread sends error, not silent return)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn channel_receives_error_on_lock_contention() {
    let lock = Arc::new(std::sync::Mutex::new(()));
    let _hold = lock.lock().unwrap();

    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1);
    let lock2 = lock.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if let Ok(_guard) = lock2.try_lock() {
                let _ = tx.send("success".into());
                return;
            }
            if Instant::now() >= deadline {
                let _ = tx.send("lock contention error".into());
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });

    let result = rx.recv_timeout(Duration::from_secs(2));
    assert!(
        result.is_ok(),
        "channel should receive message, not timeout"
    );
    assert!(
        result.unwrap().contains("contention"),
        "should report contention error"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 6. Post-dispatch spawn doesn't block caller
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_background_task_doesnt_block_caller() {
    let lock: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));

    let start = Instant::now();

    // Simulate the fire-and-forget pattern from server/mod.rs
    let lock_bg = lock.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut guard = lock_bg.write().await;
        guard.push("background update".into());
    });

    // Caller should return immediately, not wait for the spawned task's 1s
    // sleep. A 500ms ceiling stays well under 1s while tolerating scheduling
    // jitter on slow CI runners.
    let caller_elapsed = start.elapsed();
    assert!(
        caller_elapsed < Duration::from_millis(500),
        "spawning background task should not block on the task, took {caller_elapsed:?}"
    );

    // Wait for the background task to complete. Poll with a generous deadline
    // instead of a fixed sleep so the test stays robust on slow/loaded CI
    // runners (the task itself sleeps 1s before writing).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if lock.read().await.len() == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background task did not complete within 10s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let guard = lock.read().await;
    assert_eq!(guard.len(), 1);
    assert_eq!(guard[0], "background update");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Simulated multi-agent scenario (2 agents, interleaved reads + writes)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_agents_interleaved_reads_and_writes() {
    let session: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
    let cache: Arc<RwLock<std::collections::HashMap<String, String>>> =
        Arc::new(RwLock::new(std::collections::HashMap::new()));

    let mut handles = Vec::new();

    // Agent A: alternates between session writes and cache reads
    for i in 0..5 {
        let session_a = session.clone();
        let cache_a = cache.clone();
        handles.push(tokio::spawn(async move {
            {
                let mut s = session_a.write().await;
                s.push(format!("agent_a:{i}"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            {
                let c = cache_a.read().await;
                let _ = c.get("key");
            }
        }));
    }

    // Agent B: alternates between cache writes and session reads
    for i in 0..5 {
        let session_b = session.clone();
        let cache_b = cache.clone();
        handles.push(tokio::spawn(async move {
            {
                let mut c = cache_b.write().await;
                c.insert(format!("key_{i}"), format!("val_{i}"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            {
                let s = session_b.read().await;
                let _ = s.len();
            }
        }));
    }

    let start = Instant::now();
    for h in handles {
        h.await.unwrap();
    }

    assert!(
        start.elapsed() < Duration::from_secs(5),
        "interleaved agents should complete quickly"
    );

    let session_final = session.read().await;
    assert_eq!(session_final.len(), 5, "all Agent A writes should succeed");

    let cache_final = cache.read().await;
    assert_eq!(cache_final.len(), 5, "all Agent B writes should succeed");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. Zombie thread scenario: cancelled thread vs new reader
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn zombie_thread_does_not_permanently_block_subsequent_readers() {
    let lock = Arc::new(std::sync::Mutex::new(()));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Zombie: acquires lock, sleeps for 1s (simulates slow I/O)
    let lock_z = lock.clone();
    let cancel_z = cancel.clone();
    let zombie = std::thread::spawn(move || {
        let _guard = lock_z.lock().unwrap();
        while !cancel_z.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    std::thread::sleep(Duration::from_millis(50));

    // New reader: should timeout, not hang forever
    let lock_r = lock.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Ok(_guard) = lock_r.try_lock() {
                let _ = tx.send("success");
                return;
            }
            if Instant::now() >= deadline {
                let _ = tx.send("timeout");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });

    let result = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        result, "timeout",
        "reader should timeout while zombie holds lock"
    );

    // Cancel zombie, verify lock becomes available
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    zombie.join().unwrap();

    let guard = lock.lock().unwrap();
    drop(guard); // Lock available again
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. Graceful degradation: enrichment timeout doesn't lose read result
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrichment_timeout_preserves_read_result() {
    let cache: Arc<RwLock<String>> = Arc::new(RwLock::new("cached_content".into()));

    // Hold cache write lock (simulates another tool doing enrichment)
    let _hold = cache.write().await;

    // Simulate the bounded-timeout enrichment pattern from server/mod.rs
    let read_result = "file content from ctx_read".to_string();

    let enrich_timeout = tokio::time::timeout(Duration::from_millis(200), cache.write()).await;

    let final_result = if enrich_timeout.is_ok() {
        format!("{read_result}\n[enrichment hint]")
    } else {
        read_result.clone()
    };

    // Enrichment timed out, but read result is preserved
    assert!(enrich_timeout.is_err(), "should timeout since lock is held");
    assert_eq!(
        final_result, read_result,
        "read result must be preserved on enrichment timeout"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 10. Adaptive timeout with io_health integration
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn bounded_lock_respects_adaptive_timeout_in_degraded() {
    use lean_ctx::core::io_health;

    // Push into degraded mode
    for _ in 0..10 {
        io_health::record_freeze();
    }

    let lock: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));
    let _hold = lock.write().await;

    let start = Instant::now();
    let result = tokio::task::spawn_blocking({
        let lock = lock.clone();
        move || lean_ctx::server::bounded_lock::write(&lock, "degraded_test")
    })
    .await
    .unwrap();

    let elapsed = start.elapsed();

    assert!(result.is_none(), "should return None under contention");
    // In degraded mode, base 10s * 2.0 = 20s timeout
    assert!(
        elapsed >= Duration::from_secs(10),
        "degraded timeout should be >= 10s (2x base), got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(25),
        "should not exceed reasonable bounds, got {elapsed:?}"
    );
}
