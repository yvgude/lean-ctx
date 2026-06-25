use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

static FREEZE_COUNT: AtomicU32 = AtomicU32::new(0);
static LAST_FREEZE_EPOCH_MS: AtomicU64 = AtomicU64::new(0);

const FREEZE_WINDOW_MS: u64 = 60_000;
const DEGRADED_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoEnvironment {
    Fast,
    SlowFs,
    Degraded,
}

#[must_use]
pub fn environment() -> IoEnvironment {
    if recent_freeze_count() >= DEGRADED_THRESHOLD {
        return IoEnvironment::Degraded;
    }
    if is_slow_environment() {
        return IoEnvironment::SlowFs;
    }
    IoEnvironment::Fast
}

pub fn record_freeze() {
    FREEZE_COUNT.fetch_add(1, Ordering::Relaxed);
    let now = epoch_ms();
    LAST_FREEZE_EPOCH_MS.store(now, Ordering::Relaxed);
    tracing::debug!(
        "io_health: freeze recorded (total in window: {})",
        recent_freeze_count()
    );
}

pub fn recent_freeze_count() -> u32 {
    let last = LAST_FREEZE_EPOCH_MS.load(Ordering::Relaxed);
    if last == 0 {
        return 0;
    }
    let now = epoch_ms();
    if now.saturating_sub(last) > FREEZE_WINDOW_MS {
        FREEZE_COUNT.store(0, Ordering::Relaxed);
        return 0;
    }
    FREEZE_COUNT.load(Ordering::Relaxed)
}

/// Returns an adaptive timeout: longer in slow/degraded environments to avoid
/// a death spiral where shorter timeouts cause more timeouts.
#[must_use]
pub fn adaptive_timeout(base: Duration) -> Duration {
    match environment() {
        IoEnvironment::Fast => base,
        IoEnvironment::SlowFs => base.mul_f32(1.5),
        IoEnvironment::Degraded => base.mul_f32(2.0),
    }
}

pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        static IS_WSL: OnceLock<bool> = OnceLock::new();
        *IS_WSL.get_or_init(|| {
            std::fs::read_to_string("/proc/version").is_ok_and(|v| {
                let lower = v.to_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Detects if a path is likely on a slow filesystem (`DrvFS`, NFS, FUSE, sshfs).
pub fn is_slow_mount(path: &str) -> bool {
    if is_wsl() && path.starts_with("/mnt/") {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        static SLOW_PREFIXES: OnceLock<Vec<String>> = OnceLock::new();
        let prefixes = SLOW_PREFIXES.get_or_init(detect_slow_mount_prefixes);
        for prefix in prefixes {
            if path.starts_with(prefix.as_str()) {
                return true;
            }
        }
    }
    false
}

fn is_slow_environment() -> bool {
    static SLOW_ENV: OnceLock<bool> = OnceLock::new();
    *SLOW_ENV.get_or_init(|| {
        if is_wsl() {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            if has_nfs_or_fuse_mounts() {
                return true;
            }
        }
        false
    })
}

#[cfg(target_os = "linux")]
fn detect_slow_mount_prefixes() -> Vec<String> {
    let mut prefixes = Vec::new();
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return prefixes;
    };
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let mount_point = parts[1];
        let fs_type = parts[2];
        if matches!(
            fs_type,
            "nfs" | "nfs4" | "cifs" | "smbfs" | "fuse" | "fuse.sshfs" | "9p" | "drvfs"
        ) {
            prefixes.push(mount_point.to_string());
        }
    }
    prefixes
}

#[cfg(target_os = "linux")]
fn has_nfs_or_fuse_mounts() -> bool {
    !detect_slow_mount_prefixes().is_empty()
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_returns_valid_state() {
        let env = environment();
        assert!(matches!(
            env,
            IoEnvironment::Fast | IoEnvironment::SlowFs | IoEnvironment::Degraded
        ));
    }

    #[test]
    fn record_freeze_increments_count() {
        let before = recent_freeze_count();
        record_freeze();
        assert!(recent_freeze_count() > before);
    }

    #[test]
    fn adaptive_timeout_increases_in_degraded() {
        let base = Duration::from_secs(10);
        for _ in 0..5 {
            record_freeze();
        }
        let adapted = adaptive_timeout(base);
        assert!(
            adapted > base,
            "degraded environment should get longer timeout, got {adapted:?} for base {base:?}"
        );
    }

    #[test]
    fn is_slow_mount_false_for_local_paths() {
        assert!(!is_slow_mount("/home/user/project/src/main.rs"));
        assert!(!is_slow_mount("/tmp/test.txt"));
    }
}
