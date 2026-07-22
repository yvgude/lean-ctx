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
    Running,
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
        job.finished_at
            .is_none_or(|finished_at| now.duration_since(finished_at) < COMPLETED_JOB_TTL)
    });

    let mut completed: Vec<_> = jobs
        .iter()
        .filter_map(|(id, job)| {
            let finished_at = job.finished_at?;
            let output_bytes = match &job.state {
                JobState::Completed { output, .. } | JobState::Cancelled { output } => output.len(),
                JobState::Running => 0,
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
    {
        let mut jobs = JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_finished_jobs(&mut jobs, Instant::now());
        if matches!(jobs.get(&id).map(|job| &job.state), Some(JobState::Running)) {
            return id;
        }
        jobs.insert(
            id.clone(),
            Job {
                cancel,
                state: JobState::Running,
                finished_at: None,
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
        );
        let mut jobs = JOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(job) = jobs.get_mut(&worker_id) else {
            return;
        };
        job.state = if worker_cancel.load(Ordering::Acquire) {
            JobState::Cancelled { output }
        } else {
            JobState::Completed { output, exit_code }
        };
        job.finished_at = Some(Instant::now());
        prune_finished_jobs(&mut jobs, Instant::now());
    });
    id
}

/// Outcome of a foreground run that is allowed to detach on a soft cap.
pub enum ForegroundResult {
    /// The command finished within the soft cap; output is returned inline and
    /// the job has been removed from the registry.
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
) -> ForegroundResult {
    let id = start(command, cwd, extra_env, timeout_ms);
    let started = Instant::now();
    let deadline = started + soft_cap;
    let mut next_tick = started + TICK;
    loop {
        match status(&id) {
            Some(JobState::Completed { output, exit_code }) => {
                remove(&id);
                return ForegroundResult::Finished { output, exit_code };
            }
            // A cancel can only be requested via background_action once the job
            // is detached, so an inline wait realistically only sees Completed;
            // handle Cancelled defensively with the timeout exit code.
            Some(JobState::Cancelled { output }) => {
                remove(&id);
                return ForegroundResult::Finished {
                    output,
                    exit_code: 130,
                };
            }
            _ => {}
        }
        let now = Instant::now();
        if now >= deadline {
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

/// Drop a finished job from the registry so inline foreground runs do not
/// accumulate completed entries.
fn remove(id: &str) {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(id);
}

pub fn status(id: &str) -> Option<JobState> {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .map(|job| job.state.clone())
}

pub fn cancel(id: &str) -> Option<JobState> {
    let mut jobs = JOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let job = jobs.get_mut(id)?;
    if matches!(job.state, JobState::Running) {
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
                    finished_at: Some(now - Duration::from_secs((index + 1) as u64)),
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
                finished_at: Some(now - super::COMPLETED_JOB_TTL - Duration::from_secs(1)),
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
                    finished_at: Some(now - Duration::from_secs((3 - index) as u64)),
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

    #[test]
    #[cfg_attr(windows, ignore)]
    fn background_job_runs_past_request_and_can_be_observed() {
        let id = start(
            "sleep 0.1; printf BG_JOB_OK".to_string(),
            ".".to_string(),
            std::collections::HashMap::default(),
            Some(10_000),
        );
        assert_eq!(status(&id), Some(JobState::Running));
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
        assert!(matches!(cancel(&id), Some(JobState::Running)));
        for _ in 0..40 {
            if let Some(JobState::Cancelled { output }) = status(&id) {
                assert!(output.contains("command cancelled"));
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("background job was not cancelled");
    }
}
