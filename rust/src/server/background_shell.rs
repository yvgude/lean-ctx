//! Explicit, in-process background jobs for `ctx_shell`.
//!
//! The MCP request may complete immediately, but the child keeps the exact
//! timeout, allow-list, path-jail and process-group policy of foreground shell
//! execution. Jobs intentionally live in the daemon: restarting it invalidates
//! outstanding jobs rather than silently orphaning subprocesses.

use std::collections::HashMap;
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Running { output: String },
    Completed { output: String, exit_code: i32 },
    Cancelled { output: String },
}

const MAX_RETAINED_COMPLETED_JOBS: usize = 64;
const MAX_RETAINED_COMPLETED_BYTES: usize = 16 * 1024 * 1024;
const COMPLETED_JOB_TTL: Duration = Duration::from_mins(5);

struct Job {
    cancel: Arc<AtomicBool>,
    state: JobState,
    finished_at: Option<Instant>,
    // #1217: shared buffer the worker streams captured output into while the
    // job runs, so `status` can report progress before the job completes.
    live: Arc<Mutex<String>>,
    // #1876: identical launches coalesce onto one entry, so several foreground
    // calls can wait on it at once. Each reads its result from the entry, so it
    // is removed only once the last waiter has read it, and never while a
    // background caller (explicit or detached) may still poll it by id.
    foreground_waiters: usize,
    background_owned: bool,
    telemetry_recorded: bool,
}

static JOBS: LazyLock<Mutex<HashMap<String, Job>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// How often a foreground run reports progress to the MCP client (#1173).
const TICK: Duration = Duration::from_secs(5);

fn prune_finished_jobs(jobs: &mut HashMap<String, Job>, now: Instant) {
    prune_finished_jobs_with_limits(
        jobs,
        now,
        MAX_RETAINED_COMPLETED_JOBS,
        MAX_RETAINED_COMPLETED_BYTES,
    );
}

fn prune_finished_jobs_with_limits(
    jobs: &mut HashMap<String, Job>,
    now: Instant,
    max_completed_jobs: usize,
    max_completed_bytes: usize,
) {
    jobs.retain(|_, job| {
        job.foreground_waiters > 0
            || job
                .finished_at
                .is_none_or(|finished_at| now.duration_since(finished_at) < COMPLETED_JOB_TTL)
    });

    let mut completed: Vec<_> = jobs
        .iter()
        .filter(|(_, job)| job.foreground_waiters == 0)
        .filter_map(|(id, job)| {
            let finished_at = job.finished_at?;
            let output_bytes = match &job.state {
                JobState::Completed { output, .. } | JobState::Cancelled { output } => output.len(),
                JobState::Running { .. } => 0,
            };
            Some((finished_at, id.clone(), output_bytes))
        })
        .collect();
    completed.sort_unstable_by_key(|(finished_at, _, _)| *finished_at);

    let mut retained_bytes = completed.iter().map(|(_, _, bytes)| bytes).sum::<usize>();
    let mut retained_jobs = completed.len();
    for (_, id, output_bytes) in completed {
        if retained_jobs <= max_completed_jobs && retained_bytes <= max_completed_bytes {
            break;
        }
        if retained_jobs == 1 {
            break;
        }
        jobs.remove(&id);
        retained_jobs -= 1;
        retained_bytes = retained_bytes.saturating_sub(output_bytes);
    }
}

pub fn start(
    command: String,
    cwd: String,
    extra_env: std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
) -> String {
    start_inner(command, cwd, extra_env, timeout_ms, false)
}

fn start_inner(
    command: String,
    cwd: String,
    extra_env: std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
    foreground: bool,
) -> String {
    // IDs are content-addressed so tool responses stay deterministic (#498).
    // An identical in-flight launch coalesces onto the same job instead of
    // creating duplicate expensive builds/tests.
    let mut env_entries: Vec<_> = extra_env.iter().collect();
    env_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let env_key = env_entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\0");
    let material = format!(
        "{command}\0{cwd}\0{}\0{env_key}",
        timeout_ms.unwrap_or_default()
    );
    let id = format!(
        "shell_{}",
        &blake3::hash(material.as_bytes()).to_hex()[..16]
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let live = Arc::new(Mutex::new(String::new()));
    let worker_live = Arc::clone(&live);
    {
        let mut jobs = JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_finished_jobs(&mut jobs, Instant::now());
        // A finished entry that a foreground waiter has not read yet is joined
        // too (#1876): replacing it would hand that waiter's release to the new
        // job, which then disappears under its own waiter.
        if let Some(job) = jobs.get_mut(&id)
            && (matches!(job.state, JobState::Running { .. }) || job.foreground_waiters > 0)
        {
            if foreground {
                job.foreground_waiters += 1;
            } else {
                job.background_owned = true;
            }
            return id;
        }
        jobs.insert(
            id.clone(),
            Job {
                cancel,
                state: JobState::Running {
                    output: String::new(),
                },
                finished_at: None,
                live,
                foreground_waiters: usize::from(foreground),
                background_owned: !foreground,
                telemetry_recorded: false,
            },
        );
    }

    let worker_id = id.clone();
    std::thread::spawn(move || {
        let (output, exit_code) = crate::server::execute::execute_command_with_env_cancellable(
            &command,
            &cwd,
            &extra_env,
            timeout_ms,
            Some(&worker_cancel),
            // #1113/#1173: bounded by output, not wall clock — a monitor loop
            // emitting a line every 45s must survive. See `idle_keyed`.
            true,
            // #1217: stream captured output into the shared buffer as it arrives.
            Some(&worker_live),
        );
        let mut jobs = JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(job) = jobs.get_mut(&worker_id) else {
            return;
        };
        let cancelled = worker_cancel.load(Ordering::Acquire);
        job.state = if cancelled {
            JobState::Cancelled { output }
        } else {
            JobState::Completed { output, exit_code }
        };
        job.finished_at = Some(Instant::now());
        if !cancelled && exit_code != 0 {
            let category = if exit_code == 124 {
                crate::core::telemetry_v2::ErrorCategory::Timeout
            } else {
                crate::core::telemetry_v2::ErrorCategory::Internal
            };
            if crate::core::telemetry_aggregate::record_error_category(category).is_ok() {
                job.telemetry_recorded = true;
            }
        }
        prune_finished_jobs(&mut jobs, Instant::now());
    });
    id
}

/// Outcome of a foreground run that is allowed to detach on a soft cap.
pub enum ForegroundResult {
    /// The command finished within the soft cap; output is returned inline.
    /// Other coalesced foreground/background callers retain their result.
    Finished { output: String, exit_code: i32 },
    /// The command was still running at the soft cap and was left running as a
    /// pollable background job (#1106).
    Detached { job_id: String },
}

/// Run `command` as a managed background job but block up to `soft_cap` waiting
/// for it to finish, so fast commands still return their output inline.
///
/// The MCP host aborts a tool call that stays in the foreground too long
/// (~120s) and hands back a task id that `background_action=status` cannot
/// resolve. By detaching *before* that deadline we always return a real
/// `shell_*` job id the caller can poll or cancel (#1106).
///
/// `on_tick` is called roughly every `TICK` while the command is still
/// running, so the caller can emit MCP progress notifications and a 3-minute
/// build stops being indistinguishable from a hang (#1173).
pub fn run_foreground_or_detach(
    command: String,
    cwd: String,
    extra_env: std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
    soft_cap: Duration,
    on_tick: Option<&dyn Fn(Duration)>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> ForegroundResult {
    let id = start_inner(command, cwd, extra_env, timeout_ms, true);
    let started = Instant::now();
    let deadline = started + soft_cap;
    let mut next_tick = started + TICK;
    loop {
        match status(&id) {
            Some(JobState::Completed { output, exit_code }) => {
                release_foreground(&id, false);
                return ForegroundResult::Finished { output, exit_code };
            }
            // A cancel can only be requested via background_action once the job
            // is detached, so an inline wait realistically only sees Completed;
            // handle Cancelled defensively with the timeout exit code.
            Some(JobState::Cancelled { output }) => {
                release_foreground(&id, false);
                return ForegroundResult::Finished {
                    output,
                    exit_code: 130,
                };
            }
            _ => {}
        }
        // #1781: when the host abandons the call, free this thread immediately.
        // The foreground wait runs on the blocking pool — dispatch hands the
        // handler to `spawn_blocking`, and that pool has
        // `(workers * 4).clamp(8, 32)` slots. Without this check the loop spun
        // on until `soft_cap` — five minutes for a `timeout_ms: 300_000` call —
        // so a burst of abandoned commands could occupy every slot, after which
        // subsequent tool calls queued behind them until the server restarted.
        // Detaching rather than killing keeps the job alive under its id, which
        // the caller already knows how to poll and cancel.
        if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            release_foreground(&id, true);
            return ForegroundResult::Detached { job_id: id };
        }
        let now = Instant::now();
        if now >= deadline {
            release_foreground(&id, true);
            return ForegroundResult::Detached { job_id: id };
        }
        if let Some(tick) = on_tick
            && now >= next_tick
        {
            tick(started.elapsed());
            next_tick = now + TICK;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One foreground waiter is done with `id` (#1876). A detached waiter hands the
/// id to its caller, so the entry becomes background-owned and stays pollable.
/// Otherwise the entry is removed once no waiter and no background caller is
/// left; a waiter that still polls it must not find it gone.
fn release_foreground(id: &str, detached: bool) {
    let mut jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(job) = jobs.get_mut(id) else {
        return;
    };
    job.foreground_waiters = job.foreground_waiters.saturating_sub(1);
    if detached {
        job.background_owned = true;
    } else if job.foreground_waiters == 0 && !job.background_owned {
        jobs.remove(id);
    }
}

#[cfg(test)]
pub(crate) fn remove_for_test(id: &str) {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(id);
}

pub fn status(id: &str) -> Option<JobState> {
    let jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let job = jobs.get(id)?;
    Some(match &job.state {
        // #1217: a running job's output lives in the shared buffer the worker
        // streams into; surface the captured-so-far snapshot on each poll.
        JobState::Running { .. } => JobState::Running {
            output: job
                .live
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        },
        other => other.clone(),
    })
}

/// Persist one aggregate for a terminal failed job, then mark that concrete
/// retained job instance. A later execution may reuse the content-addressed ID,
/// but replaces the `Job` and therefore starts with a fresh marker.
pub fn record_error_telemetry_once(
    id: &str,
    category: crate::core::telemetry_v2::ErrorCategory,
) -> Result<bool, String> {
    let mut jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(job) = jobs.get_mut(id) else {
        return Ok(false);
    };
    if job.telemetry_recorded
        || !matches!(job.state, JobState::Completed { exit_code, .. } if exit_code != 0)
    {
        return Ok(false);
    }
    crate::core::telemetry_aggregate::record_error_category(category)?;
    job.telemetry_recorded = true;
    Ok(true)
}

pub fn cancel(id: &str) -> Option<JobState> {
    let mut jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let job = jobs.get_mut(id)?;
    if matches!(job.state, JobState::Running { .. }) {
        job.cancel.store(true, Ordering::Release);
    }
    Some(job.state.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        ForegroundResult, JobState, TICK, cancel, run_foreground_or_detach, start, status,
    };
    use std::time::Duration;

    fn completed_job(exit_code: i32) -> super::Job {
        super::Job {
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            state: JobState::Completed {
                output: "opaque output".into(),
                exit_code,
            },
            finished_at: Some(std::time::Instant::now()),
            live: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            foreground_waiters: 0,
            background_owned: true,
            telemetry_recorded: false,
        }
    }

    #[test]
    #[serial_test::serial]
    fn failed_job_telemetry_is_once_per_execution_even_when_id_is_reused() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let id = "shell_same_content";
        super::JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.into(), completed_job(2));

        assert!(
            super::record_error_telemetry_once(
                id,
                crate::core::telemetry_v2::ErrorCategory::Internal
            )
            .expect("record first execution")
        );
        assert!(
            !super::record_error_telemetry_once(
                id,
                crate::core::telemetry_v2::ErrorCategory::Internal
            )
            .expect("deduplicate repeated poll")
        );

        super::JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.into(), completed_job(2));
        assert!(
            super::record_error_telemetry_once(
                id,
                crate::core::telemetry_v2::ErrorCategory::Internal
            )
            .expect("record replacement execution")
        );

        let batch = crate::core::telemetry_aggregate::preview_daily_batch().expect("preview");
        let count = batch
            .events
            .iter()
            .find_map(|envelope| match &envelope.event {
                crate::core::telemetry_v2::TelemetryEventV2::ErrorCategoryAggregate(metrics)
                    if metrics.category == crate::core::telemetry_v2::ErrorCategory::Internal =>
                {
                    Some(metrics.count)
                }
                _ => None,
            });
        assert_eq!(count, Some(2));
        super::remove_for_test(id);
    }

    #[test]
    fn completed_job_retention_is_bounded() {
        let now = std::time::Instant::now();
        let mut jobs = std::collections::HashMap::new();
        for index in 0..=super::MAX_RETAINED_COMPLETED_JOBS {
            jobs.insert(
                format!("job_{index}"),
                super::Job {
                    cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    state: JobState::Completed {
                        output: "x".repeat(1024),
                        exit_code: 0,
                    },
                    finished_at: Some(
                        now.checked_sub(Duration::from_secs((index + 1) as u64))
                            .unwrap(),
                    ),
                    live: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                    foreground_waiters: 0,
                    background_owned: true,
                    telemetry_recorded: false,
                },
            );
        }
        jobs.insert(
            "expired".to_string(),
            super::Job {
                cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                state: JobState::Completed {
                    output: "expired".to_string(),
                    exit_code: 0,
                },
                finished_at: Some(
                    now.checked_sub(super::COMPLETED_JOB_TTL + Duration::from_secs(1))
                        .unwrap(),
                ),
                live: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                foreground_waiters: 0,
                background_owned: true,
                telemetry_recorded: false,
            },
        );

        super::prune_finished_jobs(&mut jobs, now);

        assert_eq!(jobs.len(), super::MAX_RETAINED_COMPLETED_JOBS);
        assert!(!jobs.contains_key("expired"));
        assert!(!jobs.contains_key(&format!("job_{}", super::MAX_RETAINED_COMPLETED_JOBS)));
    }

    #[test]
    fn completed_job_output_bytes_are_bounded() {
        let now = std::time::Instant::now();
        let mut jobs = std::collections::HashMap::new();
        for index in 0..3 {
            jobs.insert(
                format!("job_{index}"),
                super::Job {
                    cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    state: JobState::Completed {
                        output: "x".repeat(8),
                        exit_code: 0,
                    },
                    finished_at: Some(
                        now.checked_sub(Duration::from_secs((3 - index) as u64))
                            .unwrap(),
                    ),
                    live: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                    foreground_waiters: 0,
                    background_owned: true,
                    telemetry_recorded: false,
                },
            );
        }

        super::prune_finished_jobs_with_limits(&mut jobs, now, 10, 16);

        assert_eq!(jobs.len(), 2);
        assert!(!jobs.contains_key("job_0"));
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn foreground_run_finishing_within_cap_returns_inline() {
        let result = run_foreground_or_detach(
            "printf FG_OK".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            Duration::from_secs(10),
            None,
            None,
        );
        match result {
            ForegroundResult::Finished { output, exit_code } => {
                assert_eq!(exit_code, 0);
                assert!(output.contains("FG_OK"));
            }
            ForegroundResult::Detached { .. } => panic!("fast command should not detach"),
        }
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn foreground_run_exceeding_cap_detaches_to_pollable_job() {
        let result = run_foreground_or_detach(
            "sleep 5; printf SLOW_OK".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            Duration::from_millis(100),
            None,
            None,
        );
        let ForegroundResult::Detached { job_id } = result else {
            panic!("slow command should detach");
        };
        assert!(job_id.starts_with("shell_"));
        // The returned id must resolve via status — the core #1106 guarantee.
        assert!(status(&job_id).is_some());
        cancel(&job_id);
    }

    /// #1173: a `timeout_ms` far beyond the soft cap must NOT keep the command
    /// in the foreground. Raising the cap past the MCP host's ~120s abort is
    /// what stranded results behind an unresolvable task id; the caller must
    /// still get a real `shell_*` job id at the cap.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn large_timeout_ms_still_detaches_at_the_soft_cap() {
        let result = run_foreground_or_detach(
            "sleep 5; printf NEVER_INLINE".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(600_000),
            Duration::from_millis(100),
            None,
            None,
        );
        let ForegroundResult::Detached { job_id } = result else {
            panic!("timeout_ms must not extend the foreground wait");
        };
        assert!(status(&job_id).is_some());
        cancel(&job_id);
    }

    /// #1173: a foreground run reports progress while it waits, so a slow
    /// command is distinguishable from a hang. Deliberately slower than the
    /// other tests here — it has to outlive one real [`TICK`].
    #[test]
    #[cfg_attr(windows, ignore)]
    fn foreground_run_reports_progress_while_waiting() {
        let ticks = std::sync::atomic::AtomicUsize::new(0);
        let tick = |elapsed: Duration| {
            assert!(elapsed >= TICK, "tick must report real elapsed time");
            ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        };
        let result = run_foreground_or_detach(
            "sleep 30".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(60_000),
            TICK + Duration::from_millis(500),
            Some(&tick),
            None,
        );
        let ForegroundResult::Detached { job_id } = result else {
            panic!("slow command should detach");
        };
        cancel(&job_id);
        assert!(
            ticks.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "no progress reported during a {}s+ foreground wait",
            TICK.as_secs()
        );
    }

    /// #1781: a client-side cancellation must free the waiting thread long
    /// before the soft cap. The assertion is on *elapsed time*, not just on the
    /// `Detached` shape — detaching at the cap returns the same variant, so the
    /// shape alone would pass even with the check removed.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn cancelled_token_detaches_without_waiting_for_the_soft_cap() {
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let started = std::time::Instant::now();
        let result = run_foreground_or_detach(
            // Distinct from the progress test's `sleep 30`: identical commands
            // coalesce onto one job (#1876), so a shared one would be cancelled
            // under the other test's feet.
            "sleep 31".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(60_000),
            Duration::from_mins(1),
            None,
            Some(&token),
        );
        let elapsed = started.elapsed();
        let ForegroundResult::Detached { job_id } = result else {
            panic!("a cancelled request must detach instead of waiting it out");
        };
        cancel(&job_id);
        assert!(
            elapsed < Duration::from_secs(5),
            "cancelled wait took {elapsed:?} against a one-minute cap — token not observed"
        );
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn background_job_runs_past_request_and_can_be_observed() {
        let id = start(
            "sleep 0.1; printf BG_JOB_OK".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        assert!(matches!(status(&id), Some(JobState::Running { .. })));
        for _ in 0..40 {
            if let Some(JobState::Completed { output, exit_code }) = status(&id) {
                assert_eq!(exit_code, 0);
                assert!(output.contains("BG_JOB_OK"));
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("background job did not complete");
    }

    #[test]
    #[cfg_attr(windows, ignore)]
    fn cancelling_background_job_returns_cancelled_state() {
        let id = start(
            "sleep 5".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        assert!(matches!(cancel(&id), Some(JobState::Running { .. })));
        for _ in 0..40 {
            if let Some(JobState::Cancelled { output }) = status(&id) {
                assert!(output.contains("[cancelled: command stopped on request]"));
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("background job was not cancelled");
    }

    /// #1217: a still-running job's `status` must surface the captured-so-far
    /// output, not a bare "running" with no signal of progress.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn running_background_job_status_streams_partial_output() {
        let id = start(
            "printf EARLY_LINE; sleep 5".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        let mut saw_partial = false;
        for _ in 0..80 {
            if let Some(JobState::Running { output }) = status(&id)
                && output.contains("EARLY_LINE")
            {
                saw_partial = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        cancel(&id);
        assert!(
            saw_partial,
            "status never surfaced the running job's early output"
        );
    }

    /// #1876: identical foreground calls coalesce onto one job. The first to
    /// see it finish used to remove the shared entry, so every other caller
    /// polled a missing id until the host timed out. Each must get the output.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn identical_concurrent_foreground_calls_all_observe_completion() {
        const CALLERS: usize = 3;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
        let handles: Vec<_> = (0..CALLERS)
            .map(|_| {
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    run_foreground_or_detach(
                        "sleep 0.2; printf GH1876_ALL_OK".to_string(),
                        ".".to_string(),
                        std::collections::HashMap::default(),
                        Some(10_000),
                        Duration::from_secs(10),
                        None,
                        None,
                    )
                })
            })
            .collect();
        for handle in handles {
            match handle.join().expect("foreground caller panicked") {
                ForegroundResult::Finished { output, exit_code } => {
                    assert_eq!(exit_code, 0);
                    assert!(output.contains("GH1876_ALL_OK"), "got: {output}");
                }
                ForegroundResult::Detached { job_id } => {
                    cancel(&job_id);
                    panic!("a coalesced caller lost the shared job and detached");
                }
            }
        }
    }

    /// #1876: a foreground call that joins an explicit background job must
    /// not remove the entry the background caller still polls by id.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn foreground_join_keeps_a_background_job_pollable() {
        let command = "sleep 0.2; printf GH1876_BG_OK".to_string();
        let id = start(
            command.clone(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        let result = run_foreground_or_detach(
            command,
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
            Duration::from_secs(10),
            None,
            None,
        );
        assert!(matches!(result, ForegroundResult::Finished { .. }));
        match status(&id) {
            Some(JobState::Completed { output, .. }) => assert!(output.contains("GH1876_BG_OK")),
            other => panic!("background job vanished after a foreground join: {other:?}"),
        }
        super::remove_for_test(&id);
    }

    /// #1876: a call that arrives after the job finished but before its waiter
    /// read the result must join that entry. Replacing it let the first
    /// waiter's release delete the second caller's job.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn a_caller_joins_a_finished_entry_its_waiter_has_not_read() {
        let launch = || {
            super::start_inner(
                "printf GH1876_WINDOW".to_string(),
                ".".to_string(),
                std::collections::HashMap::default(),
                Some(10_000),
                true,
            )
        };
        let id = launch();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !matches!(status(&id), Some(JobState::Completed { .. })) {
            assert!(std::time::Instant::now() < deadline, "job never completed");
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(launch(), id);
        super::release_foreground(&id, false);
        match status(&id) {
            Some(JobState::Completed { output, .. }) => assert!(output.contains("GH1876_WINDOW")),
            other => panic!("the joined caller lost its result: {other:?}"),
        }
        super::release_foreground(&id, false);
        assert!(status(&id).is_none(), "the last waiter removes the entry");
    }

    /// #1876: fast identical commands finish inside one poll interval, which
    /// is where callers used to replace an unread entry.
    #[test]
    #[cfg_attr(windows, ignore)]
    fn fast_identical_concurrent_foreground_calls_all_return() {
        const CALLERS: usize = 8;
        for _ in 0..5 {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
            let handles: Vec<_> = (0..CALLERS)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        run_foreground_or_detach(
                            "printf GH1876_FAST".to_string(),
                            ".".to_string(),
                            std::collections::HashMap::default(),
                            Some(10_000),
                            Duration::from_secs(5),
                            None,
                            None,
                        )
                    })
                })
                .collect();
            for handle in handles {
                match handle.join().expect("foreground caller panicked") {
                    ForegroundResult::Finished { output, .. } => {
                        assert!(output.contains("GH1876_FAST"), "got: {output}");
                    }
                    ForegroundResult::Detached { job_id } => {
                        cancel(&job_id);
                        panic!("a fast identical caller lost its job and detached");
                    }
                }
            }
        }
    }

    #[test]
    fn pruning_keeps_finished_jobs_that_a_waiter_has_not_read() {
        let now = std::time::Instant::now();
        let mut jobs = std::collections::HashMap::new();
        jobs.insert(
            "waited".to_string(),
            super::Job {
                cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                state: JobState::Completed {
                    output: "x".repeat(64),
                    exit_code: 0,
                },
                finished_at: Some(
                    now.checked_sub(super::COMPLETED_JOB_TTL + Duration::from_secs(1))
                        .unwrap(),
                ),
                live: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                foreground_waiters: 1,
                background_owned: false,
                telemetry_recorded: false,
            },
        );

        super::prune_finished_jobs_with_limits(&mut jobs, now, 0, 0);

        assert!(jobs.contains_key("waited"));
    }
}
