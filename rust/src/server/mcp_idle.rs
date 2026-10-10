// SPDX-License-Identifier: Apache-2.0
//! Idle behaviour of a stdio MCP server.
//!
//! A stdio server normally ends on stdin EOF or when its parent dies (parent
//! watchdog). Hosts such as the Codex app-server do neither: they keep one
//! `lean-ctx mcp` per loaded thread alive for days with stdin open. Such a
//! server now releases its caches once it has been idle for the
//! `memory_cleanup` TTL, and exits after `mcp_idle_exit_minutes` when the
//! operator opts in. Exiting stays opt-in because Codex does not restart an
//! exited server: a resumed thread would lose lean-ctx until the host restarts.
//!
//! "Idle" means no tool call in flight, no running background shell job, and
//! no call finished within the window.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Milliseconds since [`base`] at which the last call started or finished.
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

/// How often the idle task samples activity.
const POLL: Duration = Duration::from_secs(30);

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    u64::try_from(base().elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Marks one tool call as in flight until dropped.
pub(crate) struct CallActivity(());

/// Records the start of a tool call; the returned guard records its end.
pub(crate) fn begin_call() -> CallActivity {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    LAST_ACTIVITY_MS.store(now_ms(), Ordering::SeqCst);
    CallActivity(())
}

impl Drop for CallActivity {
    fn drop(&mut self) {
        LAST_ACTIVITY_MS.store(now_ms(), Ordering::SeqCst);
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Idle time, or `None` while a call or a background job keeps the server busy.
fn idle_for(now_ms: u64, last_ms: u64, in_flight: usize, running_jobs: usize) -> Option<Duration> {
    (in_flight == 0 && running_jobs == 0)
        .then(|| Duration::from_millis(now_ms.saturating_sub(last_ms)))
}

#[derive(Debug, PartialEq, Eq)]
enum IdleStep {
    Wait,
    Release,
    Exit,
}

/// `release_after` / `exit_after` of zero disable that step. `released` is
/// true once this idle stretch has already been released.
fn decide(
    idle: Option<Duration>,
    released: bool,
    release_after: Duration,
    exit_after: Duration,
) -> IdleStep {
    let Some(idle) = idle else {
        return IdleStep::Wait;
    };
    if !exit_after.is_zero() && idle >= exit_after {
        IdleStep::Exit
    } else if !released && !release_after.is_zero() && idle >= release_after {
        IdleStep::Release
    } else {
        IdleStep::Wait
    }
}

/// Run the idle policy for `server` until the process exits.
pub(crate) fn spawn(server: crate::tools::LeanCtxServer) {
    let exit_after = Duration::from_secs(
        crate::core::config::Config::load()
            .mcp_idle_exit_minutes_effective()
            .saturating_mul(60),
    );
    let release_after = server.idle_ttl();
    LAST_ACTIVITY_MS.store(now_ms(), Ordering::SeqCst);
    tokio::spawn(async move {
        // Activity stamp of the idle stretch that was released, so a stretch
        // is released once and a new call re-arms the release.
        let mut released_stretch: Option<u64> = None;
        loop {
            tokio::time::sleep(POLL).await;
            let last = LAST_ACTIVITY_MS.load(Ordering::SeqCst);
            let idle = idle_for(
                now_ms(),
                last,
                IN_FLIGHT.load(Ordering::SeqCst),
                crate::server::background_shell::running_count(),
            );
            match decide(
                idle,
                released_stretch == Some(last),
                release_after,
                exit_after,
            ) {
                IdleStep::Wait => {}
                IdleStep::Release => {
                    tracing::info!(
                        "[mcp-idle] no tool call for {}s — releasing caches and indexes",
                        release_after.as_secs()
                    );
                    server.release_idle_memory().await;
                    released_stretch = Some(last);
                }
                IdleStep::Exit => {
                    tracing::info!(
                        "[mcp-idle] no tool call for {} min — exiting (mcp_idle_exit_minutes)",
                        exit_after.as_secs() / 60
                    );
                    // Same flush set as the parent watchdog and the clean
                    // shutdown path (#550).
                    let _ = tokio::task::spawn_blocking(|| {
                        crate::core::tool_lifecycle::flush_all();
                        crate::core::telemetry_features::record_mcp_session_end();
                        crate::cloud_sync::send_telemetry(
                            crate::core::telemetry_aggregate::SendTrigger::Exit,
                        );
                    })
                    .await;
                    std::process::exit(0);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_mins(1);

    #[test]
    fn busy_server_is_never_idle() {
        assert_eq!(idle_for(10_000, 0, 1, 0), None);
        assert_eq!(idle_for(10_000, 0, 0, 2), None);
        assert_eq!(idle_for(10_000, 4_000, 0, 0), Some(Duration::from_secs(6)));
        assert_eq!(idle_for(0, 5, 0, 0), Some(Duration::ZERO));
    }

    #[test]
    fn release_once_per_idle_stretch_then_exit_only_when_enabled() {
        let release = 60 * MIN;
        assert_eq!(decide(None, false, release, 5 * MIN), IdleStep::Wait);
        assert_eq!(
            decide(Some(59 * MIN), false, release, Duration::ZERO),
            IdleStep::Wait
        );
        assert_eq!(
            decide(Some(60 * MIN), false, release, Duration::ZERO),
            IdleStep::Release
        );
        assert_eq!(
            decide(Some(90 * MIN), true, release, Duration::ZERO),
            IdleStep::Wait
        );
        // Exit is off by default: no idle time ever exits.
        assert_eq!(
            decide(Some(10_000 * MIN), true, release, Duration::ZERO),
            IdleStep::Wait
        );
        assert_eq!(
            decide(Some(120 * MIN), true, release, 120 * MIN),
            IdleStep::Exit
        );
        assert_eq!(
            decide(Some(5 * MIN), false, Duration::ZERO, Duration::ZERO),
            IdleStep::Wait
        );
    }
}
