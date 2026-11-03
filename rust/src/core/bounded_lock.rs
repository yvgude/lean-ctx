// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

const BASE_READ_TIMEOUT: Duration = Duration::from_secs(10);
const BASE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Spin interval between `try_*` attempts. Kept short enough for responsiveness
/// but long enough to avoid busy-spinning on Windows where thread scheduling
/// quanta are ~15ms.
const SPIN_INTERVAL: Duration = Duration::from_millis(20);

/// Determines how an operation handles a bounded-lock timeout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LockFailBehavior {
    /// Return a [`LockTimeoutError`] when the operation cannot safely continue.
    ReturnError,
    /// Return the wrapper's default value (`Ok(None)`) when the operation is optional.
    #[default]
    ReturnDefault,
    /// Wait through one additional timeout window before applying the default value.
    RetryOnce,
}

/// Details of a lock acquisition timeout returned by [`LockFailBehavior::ReturnError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockTimeoutError {
    /// Whether the timed-out acquisition was for a read or write guard.
    pub operation: &'static str,
    /// Caller-provided description of the operation that needed the lock.
    pub context: String,
    /// Timeout applied to the final acquisition attempt.
    pub timeout: Duration,
}

impl std::fmt::Display for LockTimeoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "bounded_lock: {} timeout ({}ms) for {}",
            self.operation,
            self.timeout.as_millis(),
            self.context
        )
    }
}

impl std::error::Error for LockTimeoutError {}

/// Acquire a read lock via a non-blocking spin loop with an adaptive timeout.
///
/// Unlike the previous `Handle::block_on` approach, this never parks a blocking
/// thread waiting for the async runtime to make progress — eliminating the
/// stall-under-load anti-pattern on Windows (#1018). The loop yields the thread
/// between attempts so it does not starve other work on the blocking pool.
///
/// Returns `None` on timeout, preserving the historical default behavior.
pub fn read<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
) -> Option<OwnedRwLockReadGuard<T>> {
    read_with_behavior(lock, context, LockFailBehavior::default())
        .ok()
        .flatten()
}

/// Acquire a read lock within an explicit hard budget.
///
/// Use this on latency-critical request paths where adaptive I/O recovery must
/// not turn a best-effort session update into a multi-second user-visible wait.
pub fn read_for<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
    timeout: Duration,
) -> Option<OwnedRwLockReadGuard<T>> {
    acquire_with_behavior(
        lock,
        context,
        "read",
        timeout,
        LockFailBehavior::ReturnDefault,
        |lock| lock.clone().try_read_owned().ok(),
    )
    .ok()
    .flatten()
}

/// Acquire a read lock with explicit timeout behavior for this operation.
pub fn read_with_behavior<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
    behavior: LockFailBehavior,
) -> Result<Option<OwnedRwLockReadGuard<T>>, LockTimeoutError> {
    let timeout = crate::core::io_health::adaptive_timeout(BASE_READ_TIMEOUT);
    acquire_with_behavior(lock, context, "read", timeout, behavior, |lock| {
        lock.clone().try_read_owned().ok()
    })
}

/// Acquire a write lock with explicit timeout behavior for this operation.
pub fn write_with_behavior<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
    behavior: LockFailBehavior,
) -> Result<Option<OwnedRwLockWriteGuard<T>>, LockTimeoutError> {
    let timeout = crate::core::io_health::adaptive_timeout(BASE_WRITE_TIMEOUT);
    acquire_with_behavior(lock, context, "write", timeout, behavior, |lock| {
        lock.clone().try_write_owned().ok()
    })
}

fn acquire_with_behavior<T, Guard, Acquire>(
    lock: &Arc<RwLock<T>>,
    context: &str,
    operation: &'static str,
    timeout: Duration,
    behavior: LockFailBehavior,
    acquire: Acquire,
) -> Result<Option<Guard>, LockTimeoutError>
where
    Acquire: Fn(&Arc<RwLock<T>>) -> Option<Guard>,
{
    let attempts = if behavior == LockFailBehavior::RetryOnce {
        2
    } else {
        1
    };

    for attempt in 1..=attempts {
        let deadline = std::time::Instant::now() + timeout;

        loop {
            if let Some(guard) = acquire(lock) {
                return Ok(Some(guard));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if !remaining.is_zero() {
                std::thread::sleep(remaining.min(SPIN_INTERVAL));
                continue;
            }

            crate::core::io_health::record_freeze();
            if attempt < attempts {
                tracing::warn!(
                    "bounded_lock: {operation} timeout ({}ms) for {context}; retrying once",
                    timeout.as_millis()
                );
                break;
            }

            let error = LockTimeoutError {
                operation,
                context: context.to_owned(),
                timeout,
            };
            tracing::warn!("{error}; degrading gracefully");
            return match behavior {
                LockFailBehavior::ReturnError => Err(error),
                LockFailBehavior::ReturnDefault | LockFailBehavior::RetryOnce => Ok(None),
            };
        }
    }

    unreachable!("RetryOnce returns after its second acquisition attempt")
}

/// Acquire a write lock via a non-blocking spin loop with an adaptive timeout.
/// See `read()` for design rationale (#1018).
///
/// Returns `None` on timeout, preserving the historical default behavior.
pub fn write<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
) -> Option<OwnedRwLockWriteGuard<T>> {
    write_with_behavior(lock, context, LockFailBehavior::default())
        .ok()
        .flatten()
}

/// Acquire a write lock within an explicit hard budget.
///
/// This is the write counterpart to [`read_for`]. A timeout must degrade the
/// optional bookkeeping, never keep an interactive tool request queued.
pub fn write_for<T: Send + Sync + 'static>(
    lock: &Arc<RwLock<T>>,
    context: &str,
    timeout: Duration,
) -> Option<OwnedRwLockWriteGuard<T>> {
    acquire_with_behavior(
        lock,
        context,
        "write",
        timeout,
        LockFailBehavior::ReturnDefault,
        |lock| lock.clone().try_write_owned().ok(),
    )
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn read_succeeds_on_uncontested_lock() {
        let lock = Arc::new(RwLock::new(42u32));
        let guard = read(&lock, "test").expect("uncontested read must succeed");
        assert_eq!(*guard, 42);
    }

    #[test]
    fn write_succeeds_on_uncontested_lock() {
        let lock = Arc::new(RwLock::new(0u32));
        let mut guard = write(&lock, "test").expect("uncontested write must succeed");
        *guard = 7;
        assert_eq!(*guard, 7);
    }

    #[test]
    fn multiple_readers_concurrent() {
        let lock = Arc::new(RwLock::new(99u32));
        let g1 = read(&lock, "r1").expect("first reader");
        let g2 = read(&lock, "r2").expect("second reader");
        assert_eq!(*g1, 99);
        assert_eq!(*g2, 99);
    }

    #[test]
    fn write_excludes_readers() {
        let lock = Arc::new(RwLock::new(0u32));
        let _hold = lock.clone().try_write_owned().unwrap();
        assert!(lock.clone().try_read_owned().is_err());
    }

    #[test]
    fn explicit_budget_never_waits_for_adaptive_default() {
        let lock = Arc::new(RwLock::new(()));
        let _hold = lock.clone().try_write_owned().expect("hold write lock");
        assert!(read_for(&lock, "interactive read", Duration::ZERO).is_none());
        assert!(write_for(&lock, "interactive write", Duration::ZERO).is_none());
    }

    #[test]
    fn return_error_reports_lock_timeout() {
        let lock = Arc::new(RwLock::new(()));
        let result: Result<Option<()>, _> = acquire_with_behavior(
            &lock,
            "required operation",
            "read",
            Duration::ZERO,
            LockFailBehavior::ReturnError,
            |_| None,
        );

        let error = result.expect_err("ReturnError must expose the timeout");
        assert_eq!(error.operation, "read");
        assert_eq!(error.context, "required operation");
    }

    #[test]
    fn return_default_suppresses_lock_timeout() {
        let lock = Arc::new(RwLock::new(()));
        let result: Result<Option<()>, _> = acquire_with_behavior(
            &lock,
            "optional operation",
            "read",
            Duration::ZERO,
            LockFailBehavior::ReturnDefault,
            |_| None,
        );

        assert_eq!(result, Ok(None));
    }

    #[test]
    fn retry_once_makes_two_acquisition_attempts() {
        let lock = Arc::new(RwLock::new(()));
        let attempts = AtomicUsize::new(0);
        let result: Result<Option<()>, _> = acquire_with_behavior(
            &lock,
            "retryable operation",
            "read",
            Duration::ZERO,
            LockFailBehavior::RetryOnce,
            |_| {
                attempts.fetch_add(1, Ordering::Relaxed);
                None
            },
        );

        assert_eq!(result, Ok(None));
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }
}
