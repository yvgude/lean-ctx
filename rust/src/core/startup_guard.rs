use std::path::PathBuf;
use std::time::Duration;

pub struct StartupLockGuard {
    path: PathBuf,
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

/// Best-effort cross-process lock (create_new + stale eviction).
///
/// Returns `None` if the data dir can't be resolved or if the lock can't be acquired
/// within `timeout`.
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
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .is_ok()
        {
            return Some(StartupLockGuard { path });
        }

        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if modified
                    .elapsed()
                    .unwrap_or_default()
                    .saturating_sub(stale_after)
                    > Duration::from_secs(0)
                {
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
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.as_deref() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
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
}
