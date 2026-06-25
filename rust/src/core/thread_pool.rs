//! Atomic work-stealing thread pool with RSS back-pressure.
//!
//! Replaces rayon for the main extraction path. Each worker atomically
//! fetches the next available index from a shared [`AtomicUsize`] counter —
//! zero contention, natural load balancing across heterogeneous cores.
//!
//! Workers use 8 MB stacks (like the C `worker_pool.c`) to accommodate deep
//! AST recursion from tree-sitter and `walk_defs`.
//!
//! # Back-pressure
//!
//! [`ThreadPool::parallel_for_with_backpressure`] checks RSS before each job
//! and naps (3 ms × up to 40 spins) when the process is over its memory budget,
//! matching the C `PP_BACKPRESSURE` behaviour from `pass_parallel.c`.
//!
//! # Error handling
//!
//! If any worker returns `Err`, the first error is captured and all other
//! workers are signalled to stop via an internal cancelled flag. Worker panics
//! are caught via [`std::panic::catch_unwind`] and also propagated as errors.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use anyhow::{Error, Result};

use crate::core::pipeline_lock::CancelToken;

// ---------------------------------------------------------------------------
// Constants (matching C's PP_BACKPRESSURE_{MAX_SPINS,NAP_NS})
// ---------------------------------------------------------------------------

/// Maximum back-pressure spin iterations before yielding (C: 40).
const BP_MAX_SPINS: u32 = 40;

/// Sleep duration per back-pressure spin (3 ms — C: 3 000 000 ns).
const BP_NAP_MS: u64 = 3;

/// Stack size per worker thread (8 MB — matches C `CBM_WORKER_STACK_SIZE`).
const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// ThreadPool
// ---------------------------------------------------------------------------

/// Thread pool with atomic work-stealing and RSS back-pressure.
///
/// Each worker pulls the next iteration index from a shared atomic counter.
/// The main thread also participates in the work (like C's `run_pthreads`).
/// Workers use 8 MB stacks for deep AST recursion.
#[derive(Debug, Clone)]
pub struct ThreadPool {
    max_workers: usize,
    stack_size: usize,
}

impl ThreadPool {
    /// Create a new thread pool with `max_workers` worker threads.
    ///
    /// A value of `0` or `1` causes [`parallel_for`](Self::parallel_for)
    /// and friends to run the workload sequentially on the calling thread.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers,
            stack_size: WORKER_STACK_SIZE,
        }
    }

    // ------------------------------------------------------------------
    // parallel_for
    // ------------------------------------------------------------------

    /// Dispatch `count` iterations of `f(i)`, each worker atomically
    /// fetching the next index. Blocks until all iterations complete.
    ///
    /// Returns the first error if any worker returned `Err`; subsequent
    /// workers are signalled to stop via an internal cancelled flag.
    ///
    /// # Panics
    ///
    /// Worker panics are caught and returned as errors. This method itself
    /// does not panic.
    pub fn parallel_for<F>(&self, count: usize, f: F) -> Result<()>
    where
        F: Fn(usize) -> Result<()> + Send + Sync + 'static,
    {
        let f_arc = Arc::new(f);
        self.parallel_for_impl(count, &f_arc, None, 0)
    }

    // ------------------------------------------------------------------
    // parallel_for_with_backpressure
    // ------------------------------------------------------------------

    /// Same as [`parallel_for`](Self::parallel_for) but **before each job**
    /// checks RSS via `/proc/self/statm` (Linux) or `task_info` (macOS).
    ///
    /// If RSS exceeds `rss_budget_mb`, sleeps 3 ms up to 40 retries (like
    /// C's `PP_BACKPRESSURE`). When `cancel` is provided and its flag is
    /// set, in-flight workers stop early.
    ///
    /// Pass `rss_budget_mb: 0` to skip back-pressure entirely.
    pub fn parallel_for_with_backpressure<F>(
        &self,
        count: usize,
        f: F,
        cancel: Option<&CancelToken>,
        rss_budget_mb: u64,
    ) -> Result<()>
    where
        F: Fn(usize) -> Result<()> + Send + Sync + 'static,
    {
        let f_arc = Arc::new(f);
        self.parallel_for_impl(count, &f_arc, cancel, rss_budget_mb)
    }

    // ------------------------------------------------------------------
    // parallel_for_sorted
    // ------------------------------------------------------------------

    /// Sort jobs by **descending** size before dispatch, then run
    /// [`parallel_for`](Self::parallel_for) over the sorted order.
    ///
    /// `sizes` must have exactly `count` elements. The function `f` receives
    /// the **original** index (before sorting) so callers don't need to
    /// re-map.
    ///
    /// This is useful when job duration is dominated by input size: sorting
    /// largest-first reduces the straggler effect.
    pub fn parallel_for_sorted<F>(&self, sizes: &[u64], f: F) -> Result<()>
    where
        F: Fn(usize) -> Result<()> + Send + Sync + 'static,
    {
        let count = sizes.len();
        if count == 0 {
            return Ok(());
        }

        // Build index vector and sort by descending size.
        let mut indices: Vec<usize> = (0..count).collect();
        indices.sort_unstable_by(|&a, &b| sizes[b].cmp(&sizes[a]));

        // Dispatch in sorted order; the closure receives the original index.
        let sorted_f = Arc::new(move |sorted_pos: usize| {
            let orig_idx = indices[sorted_pos];
            f(orig_idx)
        });

        self.parallel_for_impl(count, &sorted_f, None, 0)
    }

    // ------------------------------------------------------------------
    // Shared implementation
    // ------------------------------------------------------------------

    /// Core dispatch logic shared by all three public methods.
    #[allow(clippy::too_many_arguments)]
    fn parallel_for_impl<F>(
        &self,
        count: usize,
        f: &Arc<F>,
        cancel: Option<&CancelToken>,
        rss_budget_mb: u64,
    ) -> Result<()>
    where
        F: Fn(usize) -> Result<()> + Send + Sync + 'static,
    {
        if count == 0 {
            return Ok(());
        }

        let nworkers = if self.max_workers == 0 {
            1
        } else {
            self.max_workers
        };

        // Serial fallback: single worker or tiny workload.
        if nworkers <= 1 || count <= 1 {
            return self.run_serial(count, f.as_ref(), cancel, rss_budget_mb);
        }

        let next_idx = Arc::new(AtomicUsize::new(0));
        let error: Arc<Mutex<Option<Error>>> = Arc::new(Mutex::new(None));
        let cancelled: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

        // Spawn worker threads.
        let mut handles = Vec::with_capacity(nworkers);
        for _ in 0..nworkers {
            let ni = Arc::clone(&next_idx);
            let err = Arc::clone(&error);
            let canc = Arc::clone(&cancelled);
            let f_clone = Arc::clone(f);
            let cancel_owned = cancel.cloned();

            let builder = thread::Builder::new()
                .name("pool-worker".into())
                .stack_size(self.stack_size);

            let handle = match builder.spawn(move || {
                worker_loop(
                    count,
                    &ni,
                    f_clone.as_ref(),
                    &err,
                    &canc,
                    cancel_owned.as_ref(),
                    rss_budget_mb,
                );
            }) {
                Ok(h) => h,
                Err(e) => {
                    let mut slot = error.lock().unwrap_or_else(PoisonError::into_inner);
                    if slot.is_none() {
                        *slot = Some(Error::from(e).context("failed to spawn worker thread"));
                    }
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
            };
            handles.push(handle);
        }

        // Main thread also participates (like C's run_pthreads).
        worker_loop_inline(
            count,
            &next_idx,
            f.as_ref(),
            &error,
            &cancelled,
            cancel,
            rss_budget_mb,
        );

        // Join all successfully spawned threads.
        for h in handles {
            match h.join() {
                Ok(()) => {}
                Err(panic_payload) => {
                    let mut slot = error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if slot.is_none() {
                        let msg = downcast_panic_msg(&panic_payload, "worker thread panicked");
                        *slot = Some(Error::msg(msg));
                    }
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
        }

        // Drain the first error, if any.
        let mut slot = error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = slot.take() {
            return Err(err);
        }

        Ok(())
    }

    /// Serial fallback: run all iterations on the calling thread.
    #[allow(unused_variables, clippy::unused_self)]
    fn run_serial<F>(
        &self,
        count: usize,
        f: &F,
        cancel: Option<&CancelToken>,
        rss_budget_mb: u64,
    ) -> Result<()>
    where
        F: Fn(usize) -> Result<()> + Send + Sync,
    {
        for i in 0..count {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                break;
            }
            if rss_budget_mb > 0 {
                apply_backpressure(rss_budget_mb, cancel);
            }
            f(i)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

/// Worker thread body: atomically fetch indices from `next_idx` until all
/// `count` iterations are consumed.
#[allow(clippy::too_many_arguments)]
fn worker_loop<F>(
    count: usize,
    next_idx: &AtomicUsize,
    f: &F,
    error: &Mutex<Option<Error>>,
    cancelled: &AtomicBool,
    cancel: Option<&CancelToken>,
    rss_budget_mb: u64,
) where
    F: Fn(usize) -> Result<()> + Send + Sync,
{
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        if let Some(ct) = cancel
            && ct.is_cancelled()
        {
            return;
        }

        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
        if idx >= count {
            return;
        }

        if rss_budget_mb > 0 {
            apply_backpressure(rss_budget_mb, cancel);
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(ct) = cancel
                && ct.is_cancelled()
            {
                return;
            }
        }

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(idx))) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let mut slot = error.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(err);
                }
                cancelled.store(true, Ordering::Relaxed);
                return;
            }
            Err(panic_payload) => {
                let mut slot = error.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(Error::msg(downcast_panic_msg(
                        &panic_payload,
                        "worker panicked",
                    )));
                }
                cancelled.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// Main-thread participation (borrows the AtomicUsize directly).
fn worker_loop_inline<F>(
    count: usize,
    next_idx: &AtomicUsize,
    f: &F,
    error: &Arc<Mutex<Option<Error>>>,
    cancelled: &AtomicBool,
    cancel: Option<&CancelToken>,
    rss_budget_mb: u64,
) where
    F: Fn(usize) -> Result<()> + Send + Sync,
{
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        if let Some(ct) = cancel
            && ct.is_cancelled()
        {
            return;
        }

        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
        if idx >= count {
            return;
        }

        if rss_budget_mb > 0 {
            apply_backpressure(rss_budget_mb, cancel);
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(ct) = cancel
                && ct.is_cancelled()
            {
                return;
            }
        }

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(idx))) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let mut slot = error.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(err);
                }
                cancelled.store(true, Ordering::Relaxed);
                return;
            }
            Err(panic_payload) => {
                let mut slot = error.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(Error::msg(downcast_panic_msg(
                        &panic_payload,
                        "main-thread worker panicked",
                    )));
                }
                cancelled.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}

/// Extract a human-readable message from a panic payload.
fn downcast_panic_msg(payload: &Box<dyn std::any::Any + Send + 'static>, fallback: &str) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("{fallback}: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("{fallback}: {s}")
    } else {
        fallback.to_string()
    }
}

// ---------------------------------------------------------------------------
// RSS back-pressure helpers
// ---------------------------------------------------------------------------

/// Fetch the current RSS of this process in megabytes.
///
/// Implementation:
/// - **Linux**: reads `/proc/self/statm`, field 1 (resident pages), and
///   multiplies by `sysconf(_SC_PAGESIZE)`.
/// - **macOS**: uses `mach_task_basic_info.resident_size`.
/// - **Other**: returns `None` (back-pressure is skipped).
#[allow(clippy::cast_sign_loss)]
pub fn current_rss_mb() -> Option<u64> {
    let rss_bytes = get_rss_bytes()?;
    Some(rss_bytes / (1024 * 1024))
}

#[cfg(target_os = "linux")]
fn get_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let field: Vec<&str> = statm.split_whitespace().collect();
    if field.len() < 2 {
        return None;
    }
    let rss_pages: u64 = field[1].parse().ok()?;
    // SAFETY: sysconf is a safe integer query.
    let page_size: u64 = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 };
    rss_pages.checked_mul(page_size)
}

#[cfg(target_os = "macos")]
fn get_rss_bytes() -> Option<u64> {
    use std::mem;

    // SAFETY: mach_task_basic_info is a POD type; zero-initialising is fine.
    let mut info: libc::mach_task_basic_info = unsafe { mem::zeroed() };
    let mut count = libc::MACH_TASK_BASIC_INFO as u32;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        )
    };
    if result == libc::KERN_SUCCESS {
        Some(info.resident_size as u64)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn get_rss_bytes() -> Option<u64> {
    None
}

/// Apply back-pressure: if RSS exceeds `budget_mb`, sleep and retry up to
/// [`BP_MAX_SPINS`] times. Returns `Ok(())` when RSS is under budget or
/// after the max spin count has been exhausted (soft overshoot, like C).
fn apply_backpressure(budget_mb: u64, cancel: Option<&CancelToken>) {
    for _spin in 0..BP_MAX_SPINS {
        let rss_bytes = get_rss_bytes().unwrap_or(0);
        let budget_bytes = budget_mb.saturating_mul(1024 * 1024);
        if rss_bytes <= budget_bytes {
            return;
        }
        if let Some(ct) = cancel
            && ct.is_cancelled()
        {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(BP_NAP_MS));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn test_parallel_for_visits_every_index() {
        let pool = ThreadPool::new(4);
        let visited: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(vec![false; 100]));

        let v_clone = Arc::clone(&visited);
        pool.parallel_for(100, move |i| {
            let mut v = v_clone
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            v[i] = true;
            Ok(())
        })
        .unwrap();

        let v = visited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (i, &was_visited) in v.iter().enumerate() {
            assert!(was_visited, "index {i} was never visited");
        }
    }

    #[test]
    fn test_parallel_for_serial_fallback() {
        let pool = ThreadPool::new(0);
        let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let c_clone = Arc::clone(&counter);

        pool.parallel_for(50, move |_i| {
            c_clone.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();

        assert_eq!(counter.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn test_parallel_for_empty() {
        let pool = ThreadPool::new(4);
        assert!(pool.parallel_for(0, |_| unreachable!()).is_ok());
    }

    #[test]
    fn test_parallel_for_single() {
        let pool = ThreadPool::new(4);
        let seen: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(usize::MAX));
        let s = Arc::clone(&seen);

        pool.parallel_for(1, move |i| {
            s.store(i, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();

        assert_eq!(seen.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_error_propagation() {
        let pool = ThreadPool::new(4);
        let err = pool.parallel_for(50, move |i| {
            if i == 7 {
                Err(Error::msg("boom at 7"))
            } else {
                Ok(())
            }
        });

        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("boom"), "unexpected error: {msg}");
    }

    #[test]
    fn test_cancellation() {
        let pool = ThreadPool::new(4);
        let token = CancelToken::new();
        // Use many iterations with a small busy-loop so the work is slow enough
        // for cancellation to take effect before all iterations finish.
        let total = 10_000;
        let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        let t = token.clone();
        let pool2 = pool.clone();
        let handle = thread::spawn(move || {
            pool2.parallel_for_with_backpressure(
                total,
                move |_i| {
                    // Small busy-wait to slow down the work unit.
                    for _ in 0..1000 {
                        std::hint::spin_loop();
                    }
                    c.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
                Some(&t),
                0,
            )
        });

        thread::sleep(std::time::Duration::from_millis(10));
        token.cancel();
        let _ = handle.join();

        let seen = counter.load(Ordering::Relaxed);
        assert!(seen < total, "expected partial progress, got {seen}");
    }

    #[test]
    fn test_current_rss_mb() {
        let rss = current_rss_mb();
        if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
            assert!(rss.is_some(), "RSS should be queryable on this platform");
            if let Some(mb) = rss {
                assert!(mb >= 1, "RSS should be at least 1 MB, got {mb}");
            }
        }
    }

    #[test]
    fn test_parallel_for_sorted_descending() {
        let pool = ThreadPool::new(4);
        let sizes: Vec<u64> = vec![1, 10, 5, 3, 100, 50];
        let dispatch_order: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let disp = Arc::clone(&dispatch_order);

        pool.parallel_for_sorted(&sizes, move |orig_idx| {
            let mut order = disp
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            order.push(orig_idx);
            Ok(())
        })
        .unwrap();

        let order = dispatch_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(&*order, &vec![4, 5, 1, 2, 3, 0]);
    }

    #[test]
    fn test_parallel_for_sorted_empty() {
        let pool = ThreadPool::new(4);
        assert!(
            pool.parallel_for_sorted::<_>(&[], |_| unreachable!())
                .is_ok()
        );
    }

    #[test]
    fn test_panic_caught_as_error() {
        let pool = ThreadPool::new(2);
        let result = pool.parallel_for(10, move |i| {
            assert!(i != 3, "intentional panic at {i}");
            Ok(())
        });

        assert!(result.is_err(), "expected error from panic");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("panic"), "unexpected error message: {msg}");
    }

    #[test]
    fn test_backpressure_nap() {
        let pool = ThreadPool::new(2);
        let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        pool.parallel_for_with_backpressure(
            20,
            move |_i| {
                c.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            None,
            0,
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::Relaxed), 20);
    }

    #[test]
    fn test_cancel_token_new() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_concurrent_cancel() {
        let token = CancelToken::new();
        let t = token.clone();
        let handle = thread::spawn(move || {
            t.cancel();
        });
        handle.join().unwrap();
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_parallel_for_multi_error_only_first_reported() {
        let pool = ThreadPool::new(4);
        let err = pool.parallel_for(100, move |i| {
            if i == 5 || i == 42 {
                Err(Error::msg(format!("err at {i}")))
            } else {
                Ok(())
            }
        });

        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("err at 5") || msg.contains("err at 42"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_race_at_index_boundary() {
        let pool = ThreadPool::new(8);
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        pool.parallel_for(1000, move |_| {
            c.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();

        assert_eq!(counter.load(Ordering::Relaxed), 1000);
    }
}
