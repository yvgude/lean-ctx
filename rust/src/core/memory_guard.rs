//! Process-level RAM guardian with adaptive eviction and hard OOM protection.
//!
//! Monitors RSS via platform-specific APIs and triggers tiered cache eviction
//! when memory usage exceeds configurable thresholds (default: 5% of system RAM,
//! lowered to `mcp_max_rss_mb` — 512 MB — inside a stdio MCP server).
//! At critical levels, performs aggressive eviction and signals background tasks
//! to abort. It never exits the process — recovery is always via eviction.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

static PEAK_RSS: AtomicU64 = AtomicU64::new(0);
static GUARD_RUNNING: AtomicBool = AtomicBool::new(false);
static ABORT_REQUESTED: AtomicBool = AtomicBool::new(false);
static CURRENT_PRESSURE: AtomicU8 = AtomicU8::new(0);
/// Absolute per-process RSS target in bytes (0 = none). The stdio MCP server
/// sets it at startup: hosts such as the Codex app-server keep one server per
/// loaded thread alive for days, and on a large machine the percentage target
/// alone let each of them settle near 1 GB before any eviction ran.
static PROCESS_RSS_CAP: AtomicU64 = AtomicU64::new(0);

/// Install an absolute RSS target for this process; the effective limit is the
/// lower of it and the `max_ram_percent` target. `0` removes it.
pub fn set_process_rss_cap(bytes: u64) {
    PROCESS_RSS_CAP.store(bytes, Ordering::Relaxed);
}

/// Current process memory in bytes, or `None` if unavailable: resident set
/// size on Linux and Windows, physical footprint on macOS (see
/// `macos_footprint` — resident size there hides compressed pages).
pub fn get_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss()
    }
    #[cfg(windows)]
    {
        windows_rss()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// RSS of an arbitrary process by PID, or `None` if unavailable/dead.
pub fn get_rss_bytes_for_pid(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss_for_pid(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss_for_pid(pid)
    }
    #[cfg(windows)]
    {
        windows_rss_for_pid(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = pid;
        None
    }
}

/// Total physical RAM in bytes, or `None` if unavailable.
pub fn get_system_ram_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_memtotal()
    }
    #[cfg(target_os = "macos")]
    {
        macos_memsize()
    }
    #[cfg(windows)]
    {
        windows_system_ram()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_rss() -> Option<u64> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetCurrentProcess returns a pseudo-handle valid for the current
    // process and it must not be closed.
    let process = unsafe { GetCurrentProcess() };
    windows_rss_for_handle(process)
}

#[cfg(windows)]
fn windows_rss_for_pid(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: OpenProcess is called with query-only access. A successful real
    // handle is closed before returning.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };

    if process.is_null() {
        return None;
    }

    let rss = windows_rss_for_handle(process);

    // SAFETY: `process` is a real handle returned successfully by OpenProcess.
    unsafe {
        let _ = CloseHandle(process);
    }

    rss
}

#[cfg(windows)]
fn windows_rss_for_handle(process: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };

    // SAFETY: callers pass the current-process pseudo-handle or a live handle
    // returned by OpenProcess; `counters` is correctly sized and writable.
    let ok = unsafe { K32GetProcessMemoryInfo(process, &raw mut counters, counters.cb) };

    if ok == 0 {
        None
    } else {
        Some(counters.WorkingSetSize as u64)
    }
}

#[cfg(windows)]
fn windows_system_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..MEMORYSTATUSEX::default()
    };

    // SAFETY: `status` has the required `dwLength` and is writable for the call.
    let ok = unsafe { GlobalMemoryStatusEx(&raw mut status) };

    if ok == 0 {
        None
    } else {
        Some(status.ullTotalPhys)
    }
}

/// Returns the RSS limit in bytes: the `max_ram_percent` target, lowered to
/// the process cap when one is installed ([`set_process_rss_cap`]).
pub fn rss_limit_bytes() -> Option<u64> {
    let sys_ram = get_system_ram_bytes()?;
    let cfg = super::config::Config::load();
    let pct = super::config::MemoryGuardConfig::effective(&cfg).max_ram_percent;
    Some(effective_limit(
        sys_ram / 100 * u64::from(pct),
        PROCESS_RSS_CAP.load(Ordering::Relaxed),
    ))
}

fn effective_limit(percent_limit: u64, cap: u64) -> u64 {
    if cap == 0 {
        percent_limit
    } else {
        percent_limit.min(cap)
    }
}

/// Pressure tier for `rss` against the base `limit` (#790 multipliers:
/// Soft above 1×, Medium above 1.2×, Hard above 1.5×, Critical above 2×).
fn pressure_for(rss: u64, limit: u64) -> PressureLevel {
    if limit == 0 {
        return PressureLevel::Normal;
    }
    let ratio = rss as f64 / limit as f64;
    if ratio > 2.0 {
        PressureLevel::Critical
    } else if ratio > 1.5 {
        PressureLevel::Hard
    } else if ratio > 1.2 {
        PressureLevel::Medium
    } else if ratio > 1.0 {
        PressureLevel::Soft
    } else {
        PressureLevel::Normal
    }
}

/// Computes a memory-bounded work batch from current RSS and the guardian's
/// hard threshold (1.5× the configured base limit). Falls back to `max_files`
/// when RSS is unavailable. The lower bound keeps progress deterministic while
/// allowing callers to reduce in-flight work substantially under low headroom.
pub fn adaptive_batch_size(
    min_files: usize,
    max_files: usize,
    estimated_bytes_per_file: u64,
) -> usize {
    let headroom = match (rss_limit_bytes(), get_rss_bytes()) {
        (Some(limit), Some(rss)) => hard_headroom_bytes(limit, rss),
        _ => return max_files.max(1),
    };
    batch_size_for_headroom(headroom, min_files, max_files, estimated_bytes_per_file)
}

fn hard_headroom_bytes(base_limit: u64, rss: u64) -> u64 {
    base_limit
        .saturating_mul(3)
        .saturating_div(2)
        .saturating_sub(rss)
}

fn batch_size_for_headroom(
    headroom_bytes: u64,
    min_files: usize,
    max_files: usize,
    estimated_bytes_per_file: u64,
) -> usize {
    let min_files = min_files.max(1);
    let max_files = max_files.max(min_files);
    let estimate = estimated_bytes_per_file.max(1);
    let by_headroom = (headroom_bytes / estimate).min(max_files as u64) as usize;
    by_headroom.clamp(min_files, max_files)
}

#[cfg(test)]
pub mod adaptive_batch_tests {
    use super::{batch_size_for_headroom, hard_headroom_bytes};

    #[test]
    fn hard_headroom_uses_guardian_hard_threshold() {
        assert_eq!(hard_headroom_bytes(1_000, 1_100), 400);
        assert_eq!(hard_headroom_bytes(1_000, 1_500), 0);
        assert_eq!(hard_headroom_bytes(1_000, 2_000), 0);
    }

    #[test]
    fn batch_size_tracks_headroom_and_bounds() {
        assert_eq!(batch_size_for_headroom(1_000, 1, 500, 100), 10);
        assert_eq!(batch_size_for_headroom(0, 1, 500, 100), 1);
        assert_eq!(batch_size_for_headroom(100_000, 1, 500, 100), 500);
    }

    #[test]
    fn batch_size_sanitizes_zero_bounds_and_estimate() {
        assert_eq!(batch_size_for_headroom(0, 0, 0, 0), 1);
    }
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

        // #790: tightened multipliers so ABORT fires earlier — e.g. 10% config
        // on 64 GB → Hard at 9.6 GB, Critical at 12.8 GB. Users expect the
        // limit to be a meaningful cap, not a 3× suggestion. Measured against
        // bytes so the per-process cap and the percentage share one ladder.
        let level = pressure_for(rss, limit);

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

/// Live heap bytes and allocator-resident bytes (`stats.allocated`,
/// `stats.resident`), when jemalloc is the allocator. The gap between process
/// RSS and `allocated` is what eviction cannot reach: allocator retention,
/// code and thread stacks.
pub fn allocator_stats() -> Option<(u64, u64)> {
    #[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
    {
        tikv_jemalloc_ctl::epoch::advance().ok()?;
        let allocated = tikv_jemalloc_ctl::stats::allocated::read().ok()?;
        let resident = tikv_jemalloc_ctl::stats::resident::read().ok()?;
        Some((allocated as u64, resident as u64))
    }
    #[cfg(not(all(feature = "jemalloc", not(windows), not(target_env = "musl"))))]
    {
        None
    }
}

/// Force-purge all jemalloc arenas to return memory to the OS.
/// Uses `MALLCTL_ARENAS_ALL` (value 4096) which is the jemalloc sentinel
/// for "all arenas". Logs errors instead of silently swallowing them.
pub fn jemalloc_purge() {
    #[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
    if let Err(rc) = purge_all_arenas() {
        tracing::warn!("[memory_guard] jemalloc purge failed: mallctl rc={rc}");
    }
}

/// `arena.<i>.purge` is a void ctl: it must be called with no old and no new
/// value. Writing one (as this did before #2075) fails with EPERM, which left
/// every purge a silent no-op: measured on macOS, a dropped 900 MB parse
/// working set stayed resident instead of falling back to ~50 MB.
#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
fn purge_all_arenas() -> Result<(), i32> {
    // SAFETY: static NUL-terminated ctl name; null old/new pointers with zero
    // length are exactly the calling convention of a void mallctl.
    let rc = unsafe {
        rjem::mallctl(
            c"arena.4096.purge".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

/// jemalloc's C entry points. tikv-jemalloc-sys builds jemalloc with the
/// `_rjem_` symbol prefix (see `_rjem_malloc_conf` in `main.rs`) and
/// tikv-jemallocator already links it, so declaring the few symbols needed
/// here avoids a direct dependency on the -sys crate.
#[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
mod rjem {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        #[cfg(feature = "tree-sitter")]
        #[link_name = "_rjem_malloc"]
        pub(super) fn malloc(size: usize) -> *mut c_void;
        #[cfg(feature = "tree-sitter")]
        #[link_name = "_rjem_calloc"]
        pub(super) fn calloc(count: usize, size: usize) -> *mut c_void;
        #[cfg(feature = "tree-sitter")]
        #[link_name = "_rjem_realloc"]
        pub(super) fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        #[cfg(feature = "tree-sitter")]
        #[link_name = "_rjem_free"]
        pub(super) fn free(ptr: *mut c_void);
        #[link_name = "_rjem_mallctl"]
        pub(super) fn mallctl(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }
}

/// Returns `true` if the guardian has requested background tasks to abort.
pub fn abort_requested() -> bool {
    ABORT_REQUESTED.load(Ordering::Relaxed)
}

/// Quick, non-allocating memory pressure check for hot loops (scanners, indexers).
/// Reads the cached atomic flag set by the guardian thread — O(1), no syscalls.
pub fn is_under_pressure() -> bool {
    current_pressure() >= PressureLevel::Soft
}

/// Returns the current pressure level as last observed by the guardian thread.
pub fn current_pressure() -> PressureLevel {
    PressureLevel::from_u8(CURRENT_PRESSURE.load(Ordering::Relaxed))
}

#[inline]
const fn pressure_requests_abort(level: PressureLevel) -> bool {
    level as u8 >= PressureLevel::Hard as u8
}

#[inline]
fn publish_pressure(level: PressureLevel) {
    CURRENT_PRESSURE.store(level as u8, Ordering::Relaxed);
    ABORT_REQUESTED.store(pressure_requests_abort(level), Ordering::SeqCst);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvictionSchedule {
    RetryAfter(u64),
    PauseFor(u64),
}

impl EvictionSchedule {
    fn poll_secs(self) -> u64 {
        match self {
            Self::RetryAfter(secs) | Self::PauseFor(secs) => secs,
        }
    }
}

/// Backoff state for eviction rounds (any pressure level) that reclaimed no memory.
#[derive(Default)]
struct EvictionBackoff {
    consecutive_zero_progress: u32,
}

impl EvictionBackoff {
    const BASE_POLL_SECS: u64 = 1;
    const MAX_POLL_SECS: u64 = 60;
    const PAUSE_AFTER_ZERO_PROGRESS_ROUNDS: u32 = 10;
    const PAUSE_SECS: u64 = 5 * 60;

    fn record(&mut self, made_progress: bool) -> EvictionSchedule {
        if made_progress {
            self.consecutive_zero_progress = 0;
            return EvictionSchedule::RetryAfter(Self::BASE_POLL_SECS);
        }

        self.consecutive_zero_progress = self.consecutive_zero_progress.saturating_add(1);
        if self.consecutive_zero_progress >= Self::PAUSE_AFTER_ZERO_PROGRESS_ROUNDS {
            return EvictionSchedule::PauseFor(Self::PAUSE_SECS);
        }

        let exponent = self.consecutive_zero_progress.saturating_sub(2).min(6);
        let poll_secs = Self::BASE_POLL_SECS
            .saturating_mul(1u64 << exponent)
            .min(Self::MAX_POLL_SECS);
        EvictionSchedule::RetryAfter(poll_secs)
    }

    fn reset(&mut self) {
        self.consecutive_zero_progress = 0;
    }
}

/// When the next eviction round may run. Sampling continues every second
/// under pressure; the gate only spaces out rounds that reclaimed nothing,
/// and lets a rise to a higher pressure level through at once.
#[derive(Default)]
struct EvictionGate {
    backoff: EvictionBackoff,
    last: Option<(PressureLevel, std::time::Instant)>,
}

impl EvictionGate {
    fn due(&self, level: PressureLevel, now: std::time::Instant) -> bool {
        self.last
            .is_none_or(|(last_level, next_at)| level > last_level || now >= next_at)
    }

    fn record(
        &mut self,
        level: PressureLevel,
        made_progress: bool,
        now: std::time::Instant,
    ) -> EvictionSchedule {
        if self.last.is_some_and(|(last_level, _)| level > last_level) {
            self.backoff.reset();
        }
        let schedule = self.backoff.record(made_progress);
        self.last = Some((
            level,
            now + std::time::Duration::from_secs(schedule.poll_secs()),
        ));
        schedule
    }
}

/// Start the background memory guardian task (idempotent).
/// Polls every 3s (normal), 1s (under pressure), or up to 15s once RSS has been
/// stably calm (idle backoff). Eviction rounds that reclaim nothing back off to
/// 60s, then pause for five minutes, at every pressure level; pressure logs
/// repeat at most once a minute per level. The callback returns whether it reclaimed
/// memory so the guard can distinguish a real retry from an empty-cache loop.
pub fn start_guard(eviction_callback: Arc<dyn Fn(PressureLevel) -> bool + Send + Sync>) {
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
            let mut gate = EvictionGate::default();

            // Pressure logging: on every level change, then at most once per
            // LOG_EVERY while the level holds — a process whose baseline sits
            // above a small cap must not write a line per second for hours.
            const LOG_EVERY: std::time::Duration = std::time::Duration::from_mins(1);
            let mut logged: Option<(PressureLevel, std::time::Instant)> = None;

            // #790: the first sample runs immediately — close the 3s blind
            // window so builders that start right after start_guard() see
            // real pressure.
            let mut first = true;
            loop {
                if !first {
                    std::thread::sleep(std::time::Duration::from_secs(poll_secs));
                }
                first = false;
                let Some(snap) = MemorySnapshot::capture() else {
                    continue;
                };

                publish_pressure(snap.pressure_level);

                if snap.pressure_level < PressureLevel::Soft {
                    gate = EvictionGate::default();
                    logged = None;
                    calm_ticks = calm_ticks.saturating_add(1);
                    poll_secs = if calm_ticks >= CALM_TICKS_BEFORE_BACKOFF {
                        IDLE_POLL_SECS
                    } else {
                        3
                    };
                    continue;
                }
                calm_ticks = 0;
                // Under pressure the guard samples every second, so
                // `publish_pressure` (and with it build aborts) never lags;
                // only eviction attempts follow the gate's backoff.
                poll_secs = 1;

                if logged.is_none_or(|(level, at)| {
                    level != snap.pressure_level || at.elapsed() >= LOG_EVERY
                }) {
                    logged = Some((snap.pressure_level, std::time::Instant::now()));
                    let heap = allocator_stats()
                        .map(|(allocated, _)| format!(" heap={}MB", allocated >> 20))
                        .unwrap_or_default();
                    let line = format!(
                        "[memory_guard] pressure={:?} RSS={:.0}MB{heap} limit={:.0}MB ({:.1}% of {:.0}GB)",
                        snap.pressure_level,
                        snap.rss_bytes as f64 / 1_048_576.0,
                        snap.rss_limit_bytes as f64 / 1_048_576.0,
                        snap.rss_percent,
                        snap.system_ram_bytes as f64 / 1_073_741_824.0,
                    );
                    if snap.pressure_level == PressureLevel::Critical {
                        tracing::error!("{line} — aggressive eviction to prevent OS OOM kill");
                    } else {
                        tracing::warn!("{line}");
                    }
                }

                let now = std::time::Instant::now();
                if !gate.due(snap.pressure_level, now) {
                    continue;
                }
                let made_progress = (eviction_callback)(snap.pressure_level);
                if snap.pressure_level >= PressureLevel::Hard {
                    jemalloc_purge();
                    release_system_heap();
                }

                // Rounds that reclaim nothing back off (and finally pause) at
                // every level: what is left above the limit is not evictable.
                // An escalation to a higher level is due at once regardless.
                if let EvictionSchedule::PauseFor(secs) =
                    gate.record(snap.pressure_level, made_progress, now)
                {
                    tracing::warn!(
                        "[memory_guard] eviction made no progress for {} rounds at {:?}; \
                         pausing eviction for {secs}s unless pressure rises",
                        gate.backoff.consecutive_zero_progress,
                        snap.pressure_level,
                    );
                }
            }
        })
        .ok();
}

/// Force immediate purge of all caches and jemalloc arenas.
pub fn force_purge() {
    jemalloc_purge();
    release_system_heap();
    tracing::info!("[memory_guard] force_purge completed");
}

/// Return freed pages to the OS now, then measure and publish the pressure
/// again. For work that would otherwise refuse under pressure the guardian
/// published earlier: memory a finished request dropped is not pressure.
pub fn reclaim_now() -> PressureLevel {
    jemalloc_purge();
    release_system_heap();
    match MemorySnapshot::capture() {
        Some(snap) => {
            publish_pressure(snap.pressure_level);
            snap.pressure_level
        }
        None => current_pressure(),
    }
}

/// Route tree-sitter's C allocations (parse trees, parser stacks, query
/// cursors) through jemalloc, whose arenas [`jemalloc_purge`] returns to the
/// OS. The platform malloc keeps them instead: measured on macOS, freed
/// parser-sized blocks stayed in the footprint and `malloc_zone_pressure_relief`
/// released nothing, so a daemon that parses every file of every root grew
/// with each new root (#2075).
///
/// Must run once at process start, before the first tree-sitter object
/// exists: a block allocated by one allocator must never be freed by the other.
pub fn route_tree_sitter_to_jemalloc() {
    #[cfg(all(
        feature = "jemalloc",
        feature = "tree-sitter",
        not(windows),
        not(target_env = "musl")
    ))]
    // SAFETY: called first thing in `main`, before any thread or tree-sitter
    // object exists, so no allocation can cross allocators. The jemalloc entry
    // points have exactly the C `malloc` family's signatures and semantics.
    unsafe {
        tree_sitter::set_allocator(
            Some(rjem::malloc),
            Some(rjem::calloc),
            Some(rjem::realloc),
            Some(rjem::free),
        );
    }
}

/// Return glibc's free pages to the OS. C dependencies other than tree-sitter
/// (SQLite, regex engines) still allocate through the platform malloc, whose
/// per-thread arenas otherwise keep every page an index build touched.
/// macOS has no equivalent: `malloc_zone_pressure_relief` released nothing
/// in measurement, which is why tree-sitter is routed to jemalloc instead.
pub fn release_system_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only walks glibc's own arenas and releases pages
    // that hold no live allocation; it is thread-safe.
    unsafe {
        libc::malloc_trim(0);
    }
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
fn macos_rss() -> Option<u64> {
    macos_footprint(std::process::id()).or_else(macos_resident_size)
}

/// Physical footprint (`ri_phys_footprint`): the figure Activity Monitor and
/// the kernel's memory-pressure (jetsam) accounting use. Unlike
/// `resident_size` it keeps counting pages the memory compressor squeezed out
/// of RAM, so a long-idle process cannot hide a large heap from the guardian —
/// measured: an idle MCP server at 40 MB resident still had a ~180 MB heap.
/// `proc_pid_rusage` is public libproc API and works for any same-user pid.
#[cfg(target_os = "macos")]
fn macos_footprint(pid: u32) -> Option<u64> {
    // SAFETY: `rusage_info_v2` is a plain C struct for which all-zero bytes
    // are a valid value.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: `info` is a live, correctly sized `rusage_info_v2` for the
    // requested `RUSAGE_INFO_V2` flavour; libproc writes at most that struct.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            std::ptr::from_mut(&mut info).cast::<libc::rusage_info_t>(),
        )
    };
    (rc == 0).then_some(info.ri_phys_footprint)
}

#[cfg(target_os = "macos")]
#[allow(deprecated, clippy::borrow_as_ptr, clippy::ptr_as_ptr)]
fn macos_resident_size() -> Option<u64> {
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
    if let Some(footprint) = macos_footprint(pid) {
        return Some(footprint);
    }
    // Fallback: `ps -o rss= -p <pid>` (`task_for_pid` needs entitlements).
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
pub mod tests {
    use super::*;

    /// #1899: jemalloc reads the conf as a C string and rejects
    /// `background_thread` wherever it lacks pthread support.
    #[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
    #[test]
    fn jemalloc_conf_is_c_string_with_platform_safe_options() {
        let conf = crate::JEMALLOC_CONF;
        assert_eq!(conf.last(), Some(&0), "must be NUL-terminated");
        let body = std::str::from_utf8(&conf[..conf.len() - 1]).unwrap();
        assert!(!body.contains('\0'), "no interior NUL");
        assert!(body.contains("dirty_decay_ms:1000"));
        assert_eq!(
            body.contains("background_thread"),
            cfg!(target_os = "linux"),
            "background_thread is only supported on Linux: {body}"
        );
    }

    /// #2075: the purge ctl is void; passing a value made it fail with EPERM,
    /// so no purge ever returned a page to the OS.
    #[cfg(all(feature = "jemalloc", not(windows), not(target_env = "musl")))]
    #[test]
    fn jemalloc_purge_is_accepted_by_the_allocator() {
        assert_eq!(purge_all_arenas(), Ok(()));
    }

    #[test]
    fn rss_returns_some_on_supported_os() {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            let rss = get_rss_bytes();
            assert!(rss.is_some(), "RSS should be readable");
            assert!(rss.unwrap() > 0, "RSS should be > 0");
        }
    }

    #[test]
    fn system_ram_returns_some_on_supported_os() {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            let ram = get_system_ram_bytes();
            assert!(ram.is_some(), "System RAM should be readable");
            assert!(ram.unwrap() > 1_000_000, "System RAM should be > 1MB");
        }
    }

    #[test]
    fn snapshot_captures_correctly() {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
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
    fn process_cap_lowers_but_never_raises_the_percent_limit() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(effective_limit(3200 * MB, 0), 3200 * MB);
        assert_eq!(effective_limit(3200 * MB, 512 * MB), 512 * MB);
        assert_eq!(effective_limit(256 * MB, 512 * MB), 256 * MB);
    }

    #[test]
    fn pressure_ladder_against_a_512mb_cap() {
        const MB: u64 = 1024 * 1024;
        let limit = 512 * MB;
        assert_eq!(pressure_for(500 * MB, limit), PressureLevel::Normal);
        assert_eq!(pressure_for(520 * MB, limit), PressureLevel::Soft);
        assert_eq!(pressure_for(650 * MB, limit), PressureLevel::Medium);
        assert_eq!(pressure_for(800 * MB, limit), PressureLevel::Hard);
        // The ~1 GB Codex instances land in Critical → emergency drop.
        assert_eq!(pressure_for(1100 * MB, limit), PressureLevel::Critical);
        assert_eq!(pressure_for(u64::MAX, 0), PressureLevel::Normal);
    }

    #[test]
    fn hard_pressure_requests_abort_immediately() {
        assert!(!pressure_requests_abort(PressureLevel::Normal));
        assert!(!pressure_requests_abort(PressureLevel::Soft));
        assert!(!pressure_requests_abort(PressureLevel::Medium));
        assert!(pressure_requests_abort(PressureLevel::Hard));
        assert!(pressure_requests_abort(PressureLevel::Critical));
    }

    #[test]
    fn eviction_gate_spaces_fruitless_rounds_but_lets_escalation_through() {
        let start = std::time::Instant::now();
        let at = |secs: u64| start + std::time::Duration::from_secs(secs);
        let mut gate = EvictionGate::default();
        assert!(gate.due(PressureLevel::Soft, start));

        // Ten fruitless Soft rounds: 1,1,2,4,…,60 s apart, then a 5 min pause.
        let mut now = start;
        let mut paused_from = start;
        let mut schedule = EvictionSchedule::RetryAfter(0);
        for _ in 0..10 {
            assert!(gate.due(PressureLevel::Soft, now));
            paused_from = now;
            schedule = gate.record(PressureLevel::Soft, false, now);
            now += std::time::Duration::from_secs(schedule.poll_secs());
        }
        assert_eq!(schedule, EvictionSchedule::PauseFor(5 * 60));
        assert!(!gate.due(
            PressureLevel::Soft,
            paused_from + std::time::Duration::from_secs(1)
        ));
        assert!(gate.due(
            PressureLevel::Medium,
            paused_from + std::time::Duration::from_secs(1)
        ));

        // A rise to Critical during the pause is evicted at once, with a fresh backoff.
        assert!(gate.due(
            PressureLevel::Critical,
            paused_from + std::time::Duration::from_secs(1)
        ));
        assert_eq!(
            gate.record(PressureLevel::Critical, true, at(1000)),
            EvictionSchedule::RetryAfter(1)
        );
        assert!(!gate.due(PressureLevel::Critical, at(1000)));
        assert!(gate.due(PressureLevel::Critical, at(1001)));
    }

    #[test]
    fn critical_zero_progress_backoff_doubles_then_pauses_and_resets() {
        let mut backoff = EvictionBackoff::default();

        assert_eq!(backoff.record(false), EvictionSchedule::RetryAfter(1));
        assert_eq!(backoff.record(false), EvictionSchedule::RetryAfter(1));
        assert_eq!(backoff.record(false), EvictionSchedule::RetryAfter(2));
        assert_eq!(backoff.record(false), EvictionSchedule::RetryAfter(4));

        for _ in 0..4 {
            backoff.record(false);
        }
        assert_eq!(backoff.record(false), EvictionSchedule::RetryAfter(60));
        assert_eq!(backoff.record(false), EvictionSchedule::PauseFor(5 * 60));

        assert_eq!(backoff.record(true), EvictionSchedule::RetryAfter(1));
        assert_eq!(backoff.consecutive_zero_progress, 0);
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
        start_guard(Arc::new(move |_| {
            fired_cb.store(true, Ordering::SeqCst);
            false
        }));

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
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
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
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            let snap = MemorySnapshot::capture_for_pid(999_999_999);
            assert!(snap.is_some(), "should fall back to self RSS");
        }
    }
}
