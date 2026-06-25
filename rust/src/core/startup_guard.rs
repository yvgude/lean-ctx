use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

pub const CRASH_LOOP_WINDOW_SECS: u64 = 60;
pub const CRASH_LOOP_THRESHOLD: usize = 8;
pub const CRASH_LOOP_MAX_BACKOFF_SECS: u64 = 30;

pub const MCP_PROCESS_NAME: &str = "mcp-server";

#[must_use]
pub fn crash_loop_log_path(process_name: &str) -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join(format!(".{}-starts.log", sanitize_lock_name(process_name))))
}

pub struct StartupLockGuard {
    path: PathBuf,
}

impl StartupLockGuard {
    pub fn touch(&self) {
        // Refresh the lock's mtime so stale eviction doesn't reclaim an active
        // long-running holder, while preserving the owner PID line so a crashed
        // holder can still be detected as dead by other processes.
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{}", std::process::id());
        }
    }
}

/// Decides whether a currently-held lock file can be reclaimed by a waiter.
///
/// A lock whose recorded owner PID is no longer alive is reclaimed immediately —
/// this is what stops a crashed/killed holder's lock from lingering until
/// `stale_after` elapses (the cause of the stale `.graph-idx-*.lock` build-up).
/// If the owner is alive, or the lock predates PID tracking (legacy 0-byte
/// file), we fall back to the long-standing mtime staleness safety valve.
fn lock_is_reclaimable(path: &std::path::Path, stale_after: Duration) -> bool {
    if let Ok(content) = std::fs::read_to_string(path)
        && let Some(pid) = content
            .lines()
            .next()
            .and_then(|l| l.trim().parse::<u32>().ok())
        && !crate::ipc::process::is_alive(pid)
    {
        return true;
    }
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(modified) = meta.modified()
    {
        return modified.elapsed().unwrap_or_default() > stale_after;
    }
    false
}

impl Drop for StartupLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn sanitize_lock_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Best-effort cross-process lock (`create_new` + stale eviction).
///
/// Returns `None` if the data dir can't be resolved or if the lock can't be acquired
/// within `timeout`.
#[must_use]
pub fn try_acquire_lock(
    name: &str,
    timeout: Duration,
    stale_after: Duration,
) -> Option<StartupLockGuard> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);

    let name = sanitize_lock_name(name);
    let path = dir.join(format!(".{name}.lock"));

    let deadline = std::time::Instant::now().checked_add(timeout)?;
    let mut sleep_ms: u64 = 10;

    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                // Record the owner PID so a crashed holder's lock can be
                // reclaimed immediately instead of waiting out `stale_after`.
                let _ = writeln!(f, "{}", std::process::id());
                return Some(StartupLockGuard { path });
            }
            Err(_) => {
                if lock_is_reclaimable(&path, stale_after) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        if std::time::Instant::now() >= deadline {
            return None;
        }

        std::thread::sleep(Duration::from_millis(sleep_ms));
        sleep_ms = (sleep_ms.saturating_mul(2)).min(120);
    }
}

/// Detects rapid restart loops (e.g., IDE keeps respawning a crashing MCP server).
/// Records each startup timestamp; if too many happen within the window, sleeps
/// with exponential backoff to break the loop and avoid host degradation.
pub fn crash_loop_backoff(process_name: &str) {
    let Some(dir) = crate::core::data_dir::lean_ctx_data_dir().ok() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let ts_path = dir.join(format!(".{}-starts.log", sanitize_lock_name(process_name)));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cutoff = now.saturating_sub(CRASH_LOOP_WINDOW_SECS);

    let mut recent: Vec<u64> = std::fs::read_to_string(&ts_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .filter(|&ts| ts >= cutoff)
        .collect();
    recent.push(now);

    if let Ok(mut f) = std::fs::File::create(&ts_path) {
        for ts in &recent {
            let _ = writeln!(f, "{ts}");
        }
    }

    if recent.len() > CRASH_LOOP_THRESHOLD {
        let restarts_over = recent.len() - CRASH_LOOP_THRESHOLD;
        let backoff_secs =
            (2u64.saturating_pow(restarts_over as u32)).min(CRASH_LOOP_MAX_BACKOFF_SECS);
        let msg = format!(
            "lean-ctx: crash-loop protection — {process_name} started {} times in {CRASH_LOOP_WINDOW_SECS}s, \
             waiting {backoff_secs}s before accepting connections. \
             If your IDE is slow to initialize, this is normal.",
            recent.len()
        );
        tracing::warn!("{msg}");
        eprintln!("{msg}");
        std::thread::sleep(Duration::from_secs(backoff_secs));
    }
}

/// Clears the crash-loop history file, resetting any active backoff.
pub fn reset_crash_loop(process_name: &str) {
    let Some(dir) = crate::core::data_dir::lean_ctx_data_dir().ok() else {
        return;
    };
    let ts_path = dir.join(format!(".{}-starts.log", sanitize_lock_name(process_name)));
    let _ = std::fs::remove_file(&ts_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let prev = std::env::var(key).ok();
            crate::test_env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.as_deref() {
                Some(v) => crate::test_env::set_var(self.key, v),
                None => crate::test_env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn lock_acquire_and_release() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        let g = try_acquire_lock(
            "unit-test",
            Duration::from_millis(200),
            Duration::from_secs(30),
        );
        assert!(g.is_some());

        let lock_path = dir.path().join(".unit-test.lock");
        assert!(lock_path.exists());

        drop(g);
        assert!(!lock_path.exists());
    }

    #[test]
    fn lock_times_out_while_held() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        let g1 = try_acquire_lock(
            "unit-test-2",
            Duration::from_millis(200),
            Duration::from_secs(30),
        )
        .expect("first lock should acquire");
        let g2 = try_acquire_lock(
            "unit-test-2",
            Duration::from_millis(60),
            Duration::from_secs(30),
        );
        assert!(g2.is_none());

        drop(g1);
        let g3 = try_acquire_lock(
            "unit-test-2",
            Duration::from_millis(200),
            Duration::from_secs(30),
        );
        assert!(g3.is_some());
    }

    #[test]
    fn dead_owner_lock_is_reclaimed_immediately() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        // Pre-seed a held lock owned by a PID that cannot be alive.
        let lock_path = dir.path().join(".dead-owner.lock");
        std::fs::write(&lock_path, "4294967294\n").unwrap();

        // The lock's mtime is fresh (just written), so the mtime safety valve
        // would NOT reclaim it within stale_after — only the dead-PID check can.
        let g = try_acquire_lock(
            "dead-owner",
            Duration::from_millis(300),
            Duration::from_secs(30),
        );
        assert!(
            g.is_some(),
            "lock with a dead owner PID must be reclaimable"
        );
    }

    #[test]
    fn crash_loop_thresholds_are_resilient() {
        let threshold = CRASH_LOOP_THRESHOLD;
        let window = CRASH_LOOP_WINDOW_SECS;
        let backoff = CRASH_LOOP_MAX_BACKOFF_SECS;
        assert!(
            threshold >= 8,
            "threshold must tolerate IDE restart patterns (was {threshold})"
        );
        assert!(
            window >= 60,
            "window must cover slow IDE startup (was {window}s)"
        );
        assert!(
            backoff <= 30,
            "max backoff must not be too aggressive (was {backoff}s)"
        );
    }

    #[test]
    fn crash_loop_backoff_under_threshold_no_sleep() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        let start = std::time::Instant::now();
        for _ in 0..CRASH_LOOP_THRESHOLD {
            crash_loop_backoff("test-no-sleep");
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "under threshold should not sleep"
        );
    }

    #[test]
    fn reset_crash_loop_clears_history() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        for _ in 0..5 {
            crash_loop_backoff("test-reset");
        }
        let log_path = dir.path().join(".test-reset-starts.log");
        assert!(log_path.exists(), "crash loop log should exist after calls");

        reset_crash_loop("test-reset");
        assert!(
            !log_path.exists(),
            "crash loop log should be removed after reset"
        );
    }

    #[test]
    fn reset_crash_loop_nonexistent_is_noop() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        reset_crash_loop("never-existed");
    }

    #[test]
    fn crash_loop_log_only_keeps_recent_entries() {
        let _env = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = EnvVarGuard::set("LEAN_CTX_DATA_DIR", dir.path());

        let log_path = dir.path().join(".test-prune-starts.log");
        let old_ts = 1000u64;
        std::fs::write(&log_path, format!("{old_ts}\n")).unwrap();

        crash_loop_backoff("test-prune");

        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "old entry should be pruned, only current remains"
        );
        let ts: u64 = lines[0].parse().unwrap();
        assert!(ts > old_ts, "remaining entry should be recent");
    }

    #[test]
    fn sanitize_lock_name_strips_special_chars() {
        assert_eq!(sanitize_lock_name("mcp-stdio"), "mcp-stdio");
        assert_eq!(sanitize_lock_name("mcp_http"), "mcp_http");
        assert_eq!(sanitize_lock_name("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize_lock_name("name with spaces"), "name_with_spaces");
    }

    #[test]
    fn crash_loop_backoff_formula_correctness() {
        assert_eq!(
            2u64.saturating_pow(1).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            2,
            "1 over threshold = 2s backoff"
        );
        assert_eq!(
            2u64.saturating_pow(2).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            4,
            "2 over threshold = 4s backoff"
        );
        assert_eq!(
            2u64.saturating_pow(3).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            8,
            "3 over threshold = 8s backoff"
        );
        assert_eq!(
            2u64.saturating_pow(4).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            16,
            "4 over threshold = 16s backoff"
        );
        assert_eq!(
            2u64.saturating_pow(5).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            30,
            "5 over threshold = capped at 30s"
        );
        assert_eq!(
            2u64.saturating_pow(10).min(CRASH_LOOP_MAX_BACKOFF_SECS),
            30,
            "10 over threshold = still capped at 30s"
        );
    }
}
