use chrono::Utc;

use super::{AgentEntry, AgentStatus};

const HARD_MAX_CONCURRENT_WORKERS: usize = 15;

pub(super) fn max_concurrent_workers() -> usize {
    crate::core::config::Config::load()
        .agents
        .max_concurrent_workers
        .clamp(1, HARD_MAX_CONCURRENT_WORKERS)
}

pub(super) fn max_concurrent_mutating_workers() -> usize {
    crate::core::config::Config::load()
        .agents
        .max_concurrent_mutating_workers
        .clamp(1, HARD_MAX_CONCURRENT_WORKERS)
}

pub(super) fn role_can_mutate(role: Option<&str>) -> bool {
    let normalized = role.unwrap_or_default().to_ascii_lowercase();
    ![
        "analysis",
        "analyst",
        "audit",
        "mapper",
        "research",
        "review",
        "reviewer",
        "read-only",
        "readonly",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

pub(super) fn active_worker_lease_seconds() -> i64 {
    i64::try_from(
        crate::core::config::Config::load()
            .agents
            .active_worker_lease_seconds,
    )
    .unwrap_or(i64::MAX)
    .max(1)
}

pub(super) fn has_active_worker_lease(agent: &AgentEntry, now: chrono::DateTime<Utc>) -> bool {
    agent.status == AgentStatus::Active
        && now.signed_duration_since(agent.last_active).num_seconds()
            <= active_worker_lease_seconds()
}

pub(super) fn consumes_worker_capacity(agent: &AgentEntry) -> bool {
    !(agent.agent_type == "mcp" && agent.role.as_deref() == Some("context-engine"))
}

pub(super) fn ensure_worker_capacity(
    active_machine_wide: usize,
    limit: usize,
    project_root: &str,
) -> Result<(), String> {
    if active_machine_wide < limit {
        return Ok(());
    }
    Err(format!(
        "machine-wide agent capacity reached while registering {project_root}: {active_machine_wide}/{limit} active leases; finish or idle a session before starting another"
    ))
}
