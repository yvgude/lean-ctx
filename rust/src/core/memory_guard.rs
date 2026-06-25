//! Process-level RAM guardian with adaptive eviction and hard OOM protection.
//!
//! Monitors RSS via platform-specific APIs and triggers tiered cache eviction
//! when memory usage exceeds configurable thresholds (default: 5% of system RAM).
//! At critical levels, performs aggressive eviction and signals background tasks
//! to abort. It never exits the process — recovery is always via eviction.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

static PEAK_RSS: AtomicU64 = AtomicU64::new(0);
static GUARD_RUNNING: AtomicBool = AtomicBool::new(false);
static ABORT_REQUESTED: AtomicBool = AtomicBool::new(false);
static CURRENT_PRESSURE: AtomicU8 = AtomicU8::new(0);

/// Current process RSS in bytes, or `None` if unavailable.
#[must_use]
pub fn get_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// RSS of an arbitrary process by PID, or `None` if unavailable/dead.
#[must_use]
pub fn get_rss_bytes_for_pid(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss_for_pid(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss_for_pid(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Total physical RAM in bytes, or `None` if unavailable.
#[must_use]
pub fn get_system_ram_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_memtotal()
    }
    #[cfg(target_os = "macos")]
    {
        macos_memsize()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Returns the RSS limit in bytes based on `max_ram_percent` config.
#[must_use]
pub fn rss_limit_bytes() -> Option<u64> {
    let sys_ram = get_system_ram_bytes()?;
    let cfg = super::config::Config::load();
    let pct = super::config::MemoryGuardConfig::effective(&cfg).max_ram_percent;
    Some(sys_ram / 100 * u64::from(pct))
}

/// Recorded peak RSS since process start.
pub fn peak_rss_bytes() -> u64 {
    PEAK_RSS.load(Ordering::Relaxed)
}

/// Snapshot of current memory state for diagnostics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemorySnapshot {
    pub rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub system_ram_bytes: u64,
    pub rss_limit_bytes: u64,
    pub rss_percent: f64,
    pub pressure_level: PressureLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum PressureLevel {
    Normal = 0,
    Soft = 1,
    Medium = 2,
    Hard = 3,
    Critical = 4,
}

impl PressureLevel {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Soft,
            2 => Self::Medium,
            3 => Self::Hard,
            4 => Self::Critical,
            _ => Self::Normal,
        }
    }
}

impl MemorySnapshot {
    /// Capture memory snapshot of the **current** process.
    #[must_use]
    pub fn capture() -> Option<Self> {
        Self::capture_impl(get_rss_bytes()?)
    }

    /// Capture memory snapshot for the **daemon** process (by PID).
    /// Falls back to the current process if the PID is dead or unreadable.
    pub fn capture_for_pid(pid: u32) -> Option<Self> {
        let rss = get_rss_bytes_for_pid(pid).or_else(get_rss_bytes)?;
        Self::capture_impl(rss)
    }

    fn capture_impl(rss: u64) -> Option<Self> {
        let sys = get_system_ram_bytes()?;
        let limit = rss_limit_bytes()?;
        let pct = if sys > 0 {
            (rss as f64 / sys as f64) * 100.0
        } else {
            0.0
        };

        PEAK_RSS.fetch_max(rss, Ordering::Relaxed);

        let cfg = super::config::Config::load();
        let guard_cfg = super::config::MemoryGuardConfig::effective(&cfg);
        let base = f64::from(guard_cfg.max_ram_percent);

        let level = if pct > base * 3.0 {
            PressureLevel::Critical
        } else if pct > base * 2.0 {
            PressureLevel::Hard
        } else if pct > base * 1.4 {
            PressureLevel::Medium
        } else if pct > base {
            PressureLevel::Soft
        } else {
            PressureLevel::Normal
        };

        Some(Self {
            rss_bytes: rss,
            peak_rss_bytes: PEAK_RSS.load(Ordering::Relaxed),
            system_ram_bytes: sys,
            rss_limit_bytes: limit,
            rss_percent: pct,
            pressure_level: level,
        })
    }
}

/// Force-purge all jemalloc arenas to return memory to the OS.
/// Uses `MALLCTL_ARENAS_ALL` (value 4096) which is the jemalloc sentinel
/// for "all arenas". Logs errors instead of silently swallowing them.
pub fn jemalloc_purge() {
    #[cfg(all(feature = "jemalloc", not(windows)))]
    {
        use tikv_jemalloc_ctl::raw;
        let purge_mib = b"arena.4096.purge\0";
        // SAFETY: `purge_mib` is a static, NUL-terminated jemalloc MIB name and
        // the value type (`u64`) matches the `arena.<i>.purge` ctl; `raw::write`
        // validates the name and surfaces errors via `Result`.
        unsafe {
            if let Err(e) = raw::write(purge_mib, 0u64) {
                tracing::debug!("[memory_guard] jemalloc purge failed: {e}");
            }
        }
    }
}

/// Returns `true` if the guardian has requested background tasks to abort.
pub fn abort_requested() -> bool {
    ABORT_REQUESTED.load(Ordering::Relaxed)
}

/// Quick, non-allocating memory pressure check for hot loops (scanners, indexers).
/// Reads the cached atomic flag set by the guardian thread — O(1), no syscalls.
#[must_use]
pub fn is_under_pressure() -> bool {
    current_pressure() >= PressureLevel::Soft
}

/// Returns the current pressure level as last observed by the guardian thread.
pub fn current_pressure() -> PressureLevel {
    PressureLevel::from_u8(CURRENT_PRESSURE.load(Ordering::Relaxed))
}

/// Start the background memory guardian task (idempotent).
/// Polls every 3s (normal), 1s (under pressure), or up to 15s once RSS has been
/// stably calm (idle backoff). At Critical level, performs aggressive eviction
/// and signals background tasks to abort — never exits the process.
pub fn start_guard(eviction_callback: Arc<dyn Fn(PressureLevel) + Send + Sync>) {
    // The guardian is a long-lived background monitor for the running
    // server/daemon. Under `cargo test` a single OS process executes the entire
    // suite, so its RSS routinely exceeds the per-operation pressure threshold
    // (default 5% of system RAM). A test that constructs a server (e.g. the
    // `http_server` tests via `new_shared_with_context`) would start this thread,
    // which then flips the process-global `CURRENT_PRESSURE` / `ABORT_REQUESTED`
    // flags. Unrelated later tests in the same binary read those flags and skip
    // work — notably `graph_index::build_edges_with_cache` aborts edge-building
    // under pressure, leaving indexed files with no edges. That manifested as an
    // intermittent, macOS-only flake ("No files depend on Base.gd"). The guardian
    // has no purpose inside the test harness, so never start it there. Production
    // and the daemon compile without `cfg!(test)` and are unaffected.
    if cfg!(test) {
        return;
    }
    if GUARD_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("memory-guard".into())
        .spawn(move || {
            // Idle backoff: once RSS has stayed below the Soft threshold for
            // CALM_TICKS_BEFORE_BACKOFF consecutive samples, stretch the poll
            // interval to IDLE_POLL_SECS. An idle server allocates nothing, so 3s
            // RSS sampling is just wasted wakeups; any pressure resets the cadence
            // instantly (below), leaving OOM reaction time during real work
            // unchanged (#453 idle hygiene).
            const CALM_TICKS_BEFORE_BACKOFF: u64 = 5;
            const IDLE_POLL_SECS: u64 = 15;
            let mut poll_secs = 3u64;
            let mut calm_ticks = 0u64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(poll_secs));
                let Some(snap) = MemorySnapshot::capture() else {
                    continue;
                };

                CURRENT_PRESSURE.store(snap.pressure_level as u8, Ordering::Relaxed);

                if snap.pressure_level == PressureLevel::Critical {
                    tracing::error!(
                        "[memory_guard] CRITICAL: RSS={:.0}MB ({:.1}% of {:.0}GB) — \
                         aggressive eviction to prevent OS OOM kill",
                        snap.rss_bytes as f64 / 1_048_576.0,
                        snap.rss_percent,
                        snap.system_ram_bytes as f64 / 1_073_741_824.0,
                    );
                    ABORT_REQUESTED.store(true, Ordering::SeqCst);
                    (eviction_callback)(PressureLevel::Critical);
                    jemalloc_purge();

                    for attempt in 1..=3 {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        (eviction_callback)(PressureLevel::Critical);
                        jemalloc_purge();
                        if let Some(recheck) = MemorySnapshot::capture() {
                            if recheck.pressure_level < PressureLevel::Hard {
                                tracing::info!(
                                    "[memory_guard] eviction attempt {attempt} succeeded — \
                                     RSS={:.0}MB, pressure={:?}",
                                    recheck.rss_bytes as f64 / 1_048_576.0,
                                    recheck.pressure_level,
                                );
                                break;
                            }
                            tracing::error!(
                                "[memory_guard] eviction attempt {attempt}/3 — still {:?} \
                                 (RSS={:.0}MB)",
                                recheck.pressure_level,
                                recheck.rss_bytes as f64 / 1_048_576.0,
                            );
                        }
                    }
                }

                if snap.pressure_level >= PressureLevel::Soft {
                    poll_secs = 1;
                    calm_ticks = 0;
                    ABORT_REQUESTED
                        .store(snap.pressure_level >= PressureLevel::Hard, Ordering::SeqCst);
                    tracing::warn!(
                        "[memory_guard] pressure={:?} RSS={:.0}MB limit={:.0}MB ({:.1}% of {:.0}GB)",
                        snap.pressure_level,
                        snap.rss_bytes as f64 / 1_048_576.0,
                        snap.rss_limit_bytes as f64 / 1_048_576.0,
                        snap.rss_percent,
                        snap.system_ram_bytes as f64 / 1_073_741_824.0,
                    );
                    (eviction_callback)(snap.pressure_level);

                    if snap.pressure_level >= PressureLevel::Hard {
                        jemalloc_purge();
                    }
                } else {
                    calm_ticks = calm_ticks.saturating_add(1);
                    poll_secs = if calm_ticks >= CALM_TICKS_BEFORE_BACKOFF {
                        IDLE_POLL_SECS
                    } else {
                        3
                    };
                    if ABORT_REQUESTED.load(Ordering::Relaxed) {
                        ABORT_REQUESTED.store(false, Ordering::SeqCst);
                        tracing::info!("[memory_guard] pressure normalized, clearing abort flag");
                    }
                }
            }
        })
        .ok();
}

/// Force immediate purge of all caches and jemalloc arenas.
pub fn force_purge() {
    jemalloc_purge();
    tracing::info!("[memory_guard] force_purge completed");
}

// --- Platform-specific implementations ---

#[cfg(target_os = "linux")]
fn linux_rss() -> Option<u64> {
    linux_rss_for_pid(std::process::id())
}

#[cfg(target_os = "linux")]
fn linux_rss_for_pid(pid: u32) -> Option<u64> {
    let path = format!("/proc/{pid}/status");
    let status = std::fs::read_to_string(path).ok()?;
    for line in status.lines() {
        if let Some(val) = line.strip_prefix("VmRSS:") {
            let kb: u64 = val.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_memtotal() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in info.lines() {
        if let Some(val) = line.strip_prefix("MemTotal:") {
            let kb: u64 = val.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(target_os = "macos")]
#[allow(deprecated, clippy::borrow_as_ptr, clippy::ptr_as_ptr)]
fn macos_rss() -> Option<u64> {
    use std::mem;
    // SAFETY: `mach_task_basic_info_data_t` is a plain C struct for which an
    // all-zero bit pattern is a valid initial value.
    let mut info: libc::mach_task_basic_info_data_t = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<libc::mach_task_basic_info_data_t>()
        / mem::size_of::<libc::natural_t>()) as libc::mach_msg_type_number_t;
    // SAFETY: `mach_task_self()` returns the current task port; `info` and
    // `count` are live stack locals passed as out-pointers, sized to match the
    // requested `MACH_TASK_BASIC_INFO` flavour.
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            std::ptr::from_mut(&mut info).cast::<i32>(),
            std::ptr::from_mut(&mut count),
        )
    };
    if kr == libc::KERN_SUCCESS {
        Some(info.resident_size)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_rss_for_pid(pid: u32) -> Option<u64> {
    // Use `ps -o rss= -p <pid>` as a portable fallback.
    // `task_for_pid` requires root/entitlements, `proc_pid_rusage` is private API.
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let kb: u64 = text.trim().parse().ok()?;
    Some(kb * 1024)
}

#[cfg(target_os = "macos")]
#[allow(clippy::borrow_as_ptr, clippy::ptr_as_ptr)]
fn macos_memsize() -> Option<u64> {
    use std::mem;
    let mut memsize: u64 = 0;
    let mut len = mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    // SAFETY: `name` is a static, NUL-terminated sysctl name; `memsize` and
    // `len` are live stack out-pointers whose sizes match the queried value.
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            std::ptr::from_mut(&mut memsize).cast::<libc::c_void>(),
            std::ptr::from_mut(&mut len),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(memsize) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_returns_some_on_supported_os() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let rss = get_rss_bytes();
            assert!(rss.is_some(), "RSS should be readable");
            assert!(rss.unwrap() > 0, "RSS should be > 0");
        }
    }

    #[test]
    fn system_ram_returns_some_on_supported_os() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let ram = get_system_ram_bytes();
            assert!(ram.is_some(), "System RAM should be readable");
            assert!(ram.unwrap() > 1_000_000, "System RAM should be > 1MB");
        }
    }

    #[test]
    fn snapshot_captures_correctly() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let snap = MemorySnapshot::capture();
            assert!(snap.is_some());
            let s = snap.unwrap();
            assert!(s.rss_bytes > 0);
            assert!(s.system_ram_bytes > s.rss_bytes);
            assert!(s.rss_percent > 0.0 && s.rss_percent < 100.0);
        }
    }

    #[test]
    fn peak_rss_tracks_maximum() {
        PEAK_RSS.store(0, Ordering::Relaxed);
        PEAK_RSS.fetch_max(100, Ordering::Relaxed);
        PEAK_RSS.fetch_max(50, Ordering::Relaxed);
        assert_eq!(PEAK_RSS.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn pressure_level_roundtrip() {
        for level in [
            PressureLevel::Normal,
            PressureLevel::Soft,
            PressureLevel::Medium,
            PressureLevel::Hard,
            PressureLevel::Critical,
        ] {
            assert_eq!(PressureLevel::from_u8(level as u8), level);
        }
    }

    #[test]
    fn atomic_pressure_defaults_to_normal() {
        assert_eq!(current_pressure(), PressureLevel::Normal);
    }

    #[test]
    fn start_guard_is_noop_under_test() {
        // Regression guard: the background guardian must never run inside the
        // test harness. If it did, its 3s poll would observe the suite's large
        // RSS, flip the global pressure/abort flags, and silently make unrelated
        // tests (e.g. graph edge-building) skip work — an order/timing-dependent
        // flake. `start_guard` must be a no-op under `cfg!(test)`.
        let fired = Arc::new(AtomicBool::new(false));
        let fired_cb = fired.clone();
        start_guard(Arc::new(move |_| fired_cb.store(true, Ordering::SeqCst)));

        assert!(
            !GUARD_RUNNING.load(Ordering::Relaxed),
            "guardian thread must not start under cfg!(test)"
        );
        assert_eq!(current_pressure(), PressureLevel::Normal);
        assert!(!abort_requested());
        assert!(
            !fired.load(Ordering::Relaxed),
            "eviction callback must never fire in tests"
        );
    }

    #[test]
    fn rss_for_own_pid_matches_self() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let self_rss = get_rss_bytes().unwrap();
            let pid_rss = get_rss_bytes_for_pid(std::process::id()).unwrap();
            let ratio = self_rss as f64 / pid_rss as f64;
            assert!(
                (0.5..2.0).contains(&ratio),
                "self RSS ({self_rss}) and pid-based RSS ({pid_rss}) should be within 2x"
            );
        }
    }

    #[test]
    fn rss_for_dead_pid_returns_none() {
        let dead_pid = 999_999_999u32;
        assert!(get_rss_bytes_for_pid(dead_pid).is_none());
    }

    #[test]
    fn capture_for_pid_falls_back_on_dead_pid() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let snap = MemorySnapshot::capture_for_pid(999_999_999);
            assert!(snap.is_some(), "should fall back to self RSS");
        }
    }
}
