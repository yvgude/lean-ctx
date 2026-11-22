//! Background agent reaper: periodically GCs dead presence processes, expired
//! scratchpad entries, and stale logical sessions.
//!
//! Spawned once by the daemon; runs until the process exits.
//! Reaper TTLs and interval are configured through `[agents]`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static RUNNING: OnceLock<AtomicBool> = OnceLock::new();

fn load_config() -> crate::core::config::AgentsConfig {
    crate::core::config::Config::load().agents
}

fn interval_from_minutes(minutes: u64) -> Option<Duration> {
    minutes
        .checked_mul(60)
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

/// Spawn the background reaper thread. Safe to call multiple times -- only the
/// first call starts the thread; subsequent calls are no-ops.
pub(crate) fn spawn_reaper() {
    // A unit-test binary must be able to terminate deterministically. Its
    // reaper would otherwise sleep for ten minutes in a background thread and
    // retain shared registry state after the test that created it has ended.
    // Reaping behavior itself is covered through `reap_cycle` below.
    if cfg!(test) {
        return;
    }
    let Some(interval) = interval_from_minutes(load_config().gc_interval_minutes) else {
        tracing::debug!("agent reaper disabled by agents.gc_interval_minutes=0");
        return;
    };
    let flag = RUNNING.get_or_init(|| AtomicBool::new(false));
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(error) = reap_cycle() {
        tracing::warn!("agent reaper initial cycle failed: {error}");
    }
    if let Err(error) = std::thread::Builder::new()
        .name("agent-reaper".to_string())
        .spawn(move || reaper_loop(interval))
    {
        // Keep retry semantics honest: a failed OS thread creation must not
        // permanently mark the reaper as running.
        flag.store(false, Ordering::SeqCst);
        tracing::warn!("agent reaper thread did not start: {error}");
    }
}

fn reaper_loop(interval: Duration) {
    loop {
        std::thread::sleep(interval);
        if let Err(error) = reap_cycle() {
            tracing::warn!("agent reaper cycle failed: {error}");
        }
    }
}

/// Run one reap cycle. Public for testing.
pub(crate) fn reap_cycle() -> Result<ReapStats, String> {
    let mut stats = ReapStats::default();
    let cfg = load_config();

    // Presence registry: cleanup_stale marks dead PIDs as Finished and removes
    // old Finished entries.
    super::AgentRegistry::mutate_locked(|registry| {
        let agents_before = registry.agents.len();
        let scratchpad_before = registry.scratchpad.len();

        // cleanup_stale handles: dead PIDs → Finished, old agents removal,
        // AND expired scratchpad entries (since #502).
        registry.cleanup_stale(cfg.presence_ttl_hours);

        stats.presence_removed = agents_before.saturating_sub(registry.agents.len());
        stats.scratchpad_expired = scratchpad_before.saturating_sub(registry.scratchpad.len());

        // Logical sessions: cleanup stale.
        let sessions_before = registry.logical_sessions.len();
        registry.cleanup_stale_logical_sessions(cfg.logical_session_ttl_seconds);
        stats.sessions_expired = sessions_before.saturating_sub(registry.logical_sessions.len());
    })?;

    tracing::debug!(
        "reaper: presence={} scratchpad={} sessions={}",
        stats.presence_removed,
        stats.scratchpad_expired,
        stats.sessions_expired,
    );

    Ok(stats)
}

/// Statistics from one reap cycle.
#[derive(Debug, Default, Clone)]
pub(crate) struct ReapStats {
    pub presence_removed: usize,
    pub scratchpad_expired: usize,
    pub sessions_expired: usize,
}

impl ReapStats {
    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.presence_removed + self.scratchpad_expired + self.sessions_expired
    }
}

#[cfg(test)]
mod tests {
    use super::{interval_from_minutes, reap_cycle, spawn_reaper};

    #[test]
    fn reaper_interval_respects_disabled_and_configured_values() {
        assert_eq!(interval_from_minutes(0), None);
        assert_eq!(
            interval_from_minutes(1),
            Some(std::time::Duration::from_mins(1))
        );
    }

    #[test]
    fn spawn_reaper_is_idempotent() {
        spawn_reaper();
        spawn_reaper();
    }

    #[test]
    fn reap_cycle_succeeds_on_empty_registries() {
        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        let stats = reap_cycle().expect("reap on empty");
        assert_eq!(stats.total(), 0);
    }

    #[test]
    fn reap_cycle_removes_expired_scratchpad() {
        use super::super::{AgentRegistry, ScratchpadEntry};
        use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        AgentRegistry::mutate_locked(|registry| {
            registry.scratchpad.push(ScratchpadEntry {
                id: "expired-1".to_string(),
                from_agent: "a".to_string(),
                to_agent: None,
                task_id: None,
                category: "test".to_string(),
                priority: MessagePriority::default(),
                privacy: PrivacyLevel::default(),
                message: "old".to_string(),
                metadata: std::collections::HashMap::new(),
                project_root: None,
                timestamp: lean_ctx_time::UtcDateTime::now(),
                read_by: vec![],
                expires_at: Some(
                    lean_ctx_time::UtcDateTime::now() - lean_ctx_time::Duration::hours(1),
                ),
            });
        })
        .expect("setup");

        let stats = reap_cycle().expect("reap");
        assert!(stats.scratchpad_expired >= 1);
    }
}
