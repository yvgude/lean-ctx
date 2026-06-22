//! Opportunistic, time-gated trigger for the background Cognition Loop.
//!
//! The MCP server is request-driven, so instead of holding a wall-clock timer
//! thread (which would tick with no project context and complicate shutdown), we
//! piggyback on tool activity: after dispatch, [`maybe_run`] fires the loop at
//! most once per `autonomy.cognition_loop_interval_secs`, in a single-flight
//! background thread. When the agent is idle no maintenance is needed anyway.
//!
//! This is what turns the eight-step [`crate::core::cognition_loop`] (seed
//! promote → repair → synthesis → contradiction → hebbian → decay → compact)
//! from a manually-invoked tool into genuinely self-managing memory.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Unix seconds of the last loop start (`0` = never run this process).
static LAST_RUN_SECS: AtomicU64 = AtomicU64::new(0);
/// Unix seconds of the last dispatch seen by [`maybe_run`] (`0` = none yet).
/// Drives idle detection for replay consolidation (#7).
static LAST_ACTIVITY_SECS: AtomicU64 = AtomicU64::new(0);
/// Single-flight guard: never spawn a second loop while one is in flight.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Single-flight guard for the idle replay pass (#7).
static IDLE_REPLAY_RUNNING: AtomicBool = AtomicBool::new(false);

/// Floor for the configured interval — guards against a pathological `0`/tiny
/// value turning every dispatch into a consolidation run.
const MIN_INTERVAL_SECS: u64 = 60;

/// Default quiet gap (seconds) after which the next dispatch triggers an idle
/// replay-consolidation pass (#7). Overridable via `LEAN_CTX_COGNITION_IDLE_SECS`.
const IDLE_REPLAY_SECS: u64 = 300;

/// Resets [`RUNNING`] on drop so a panicking loop can never wedge the guard.
struct RunningGuard;
impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Resets [`IDLE_REPLAY_RUNNING`] on drop so a panicking replay can't wedge it.
struct IdleGuard;
impl Drop for IdleGuard {
    fn drop(&mut self) {
        IDLE_REPLAY_RUNNING.store(false, Ordering::Release);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Pure due-check, factored out for deterministic testing: the loop is due when
/// it has never run (`last == 0`) or `interval` seconds have elapsed since then.
fn is_due(now: u64, last: u64, interval: u64) -> bool {
    last == 0 || now.saturating_sub(last) >= interval
}

/// Pure idle-replay check (#7): true when a *prior* dispatch exists (`last != 0`)
/// and the quiet gap since it reached `idle_secs`. Unlike [`is_due`], the first
/// activity (`last == 0`) is NOT idle — there was no rest period to consolidate.
fn idle_replay_due(now: u64, last_activity: u64, idle_secs: u64) -> bool {
    last_activity != 0 && now.saturating_sub(last_activity) >= idle_secs
}

/// Quiet-gap threshold for idle replay, env-overridable (must be > 0).
fn idle_replay_secs() -> u64 {
    std::env::var("LEAN_CTX_COGNITION_IDLE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(IDLE_REPLAY_SECS)
}

/// Fire the cognition loop in the background when enabled and the configured
/// interval has elapsed. Non-blocking, single-flight, and cheap on the hot path
/// (one config read + two atomic loads when not due).
pub fn maybe_run(project_root: &str) {
    let cfg = crate::core::config::Config::load();
    if !cfg.autonomy.cognition_loop_enabled {
        return;
    }
    let now = now_secs();

    // #7 Idle replay: if the agent rested before this dispatch, replay-consolidate
    // now (on wake). Checked and armed BEFORE we stamp the new activity time.
    maybe_idle_replay(project_root, now);
    LAST_ACTIVITY_SECS.store(now, Ordering::Relaxed);

    let interval = cfg
        .autonomy
        .cognition_loop_interval_secs
        .max(MIN_INTERVAL_SECS);
    if !is_due(now, LAST_RUN_SECS.load(Ordering::Relaxed), interval) {
        return;
    }
    // Claim the slot before spawning so concurrent dispatches never double-fire.
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    LAST_RUN_SECS.store(now, Ordering::Relaxed);

    let root = project_root.to_string();
    let max_steps = cfg.autonomy.cognition_loop_max_steps;
    std::thread::spawn(move || {
        let _guard = RunningGuard;
        let report = crate::core::cognition_loop::run_cognition_loop(&root, max_steps);
        tracing::debug!(target: "cognition", "background cognition loop: {report}");
    });
}

/// Fire a focused idle replay-consolidation pass (#7) when a quiet gap preceded
/// this dispatch. Non-blocking and single-flight, like [`maybe_run`].
fn maybe_idle_replay(project_root: &str, now: u64) {
    let last = LAST_ACTIVITY_SECS.load(Ordering::Relaxed);
    if !idle_replay_due(now, last, idle_replay_secs()) {
        return;
    }
    if IDLE_REPLAY_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let root = project_root.to_string();
    std::thread::spawn(move || {
        let _guard = IdleGuard;
        let promoted = crate::core::cognition_loop::run_idle_replay(&root);
        tracing::debug!(target: "cognition", "idle replay: {promoted} facts consolidated");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_run_is_always_due() {
        assert!(is_due(1_000, 0, 3600));
        assert!(is_due(0, 0, 3600));
    }

    #[test]
    fn due_only_after_interval_elapses() {
        let last = 10_000;
        assert!(!is_due(last + 59, last, 60));
        assert!(is_due(last + 60, last, 60));
        assert!(is_due(last + 7_200, last, 3600));
    }

    #[test]
    fn clock_skew_backwards_is_not_due() {
        // A backwards clock jump must not retrigger (saturating_sub → 0).
        assert!(!is_due(9_000, 10_000, 3600));
    }

    #[test]
    fn first_activity_is_not_idle_replay() {
        // #7: with no prior dispatch there is no rest period to consolidate.
        assert!(!idle_replay_due(10_000, 0, 300));
    }

    #[test]
    fn idle_replay_fires_after_quiet_gap() {
        // #7: a gap >= idle_secs since the last dispatch triggers replay on wake.
        let last = 10_000;
        assert!(!idle_replay_due(last + 299, last, 300));
        assert!(idle_replay_due(last + 300, last, 300));
        assert!(idle_replay_due(last + 10_000, last, 300));
    }

    #[test]
    fn idle_replay_backwards_clock_is_not_due() {
        assert!(!idle_replay_due(9_000, 10_000, 300));
    }
}
