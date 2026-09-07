use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[cfg(test)]
use super::diary::{AgentDiary, DiaryEntryType, truncate};
use super::persistence::{
    FileLock, ProcessIdentityIndex, agents_dir, create_agents_dir, generate_short_id,
    load_registry_file, mutate_persistent, save_registry_file,
};
use super::{AgentEntry, AgentRegistry, AgentStatus, LogicalSessionPresence, ScratchpadEntry};
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

const LOGICAL_SESSION_SOURCE_MAX_BYTES: usize = 64;
const LOGICAL_SESSION_WORKSPACE_MAX_BYTES: usize = 4096;
const LOGICAL_SESSION_ID_MAX_BYTES: usize = 256;
const HARD_MAX_CONCURRENT_WORKERS: usize = 15;
const MAX_RETAINED_FINISHED_AGENTS: usize = 32;

fn presence_ttl() -> u64 {
    crate::core::config::Config::load()
        .agents
        .presence_ttl_hours
}

fn max_scratchpad() -> usize {
    crate::core::config::Config::load()
        .agents
        .max_scratchpad_entries
}

fn max_concurrent_workers() -> usize {
    crate::core::config::Config::load()
        .agents
        .max_concurrent_workers
        .clamp(1, HARD_MAX_CONCURRENT_WORKERS)
}

fn max_concurrent_mutating_workers() -> usize {
    crate::core::config::Config::load()
        .agents
        .max_concurrent_mutating_workers
        .clamp(1, HARD_MAX_CONCURRENT_WORKERS)
}

fn role_can_mutate(role: Option<&str>) -> bool {
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

fn active_worker_lease_seconds() -> i64 {
    i64::try_from(
        crate::core::config::Config::load()
            .agents
            .active_worker_lease_seconds,
    )
    .unwrap_or(i64::MAX)
    .max(1)
}

fn has_active_worker_lease(agent: &AgentEntry, now: chrono::DateTime<Utc>) -> bool {
    agent.status == AgentStatus::Active
        && now.signed_duration_since(agent.last_active).num_seconds()
            <= active_worker_lease_seconds()
}

fn consumes_worker_capacity(agent: &AgentEntry) -> bool {
    !(agent.agent_type == "mcp" && agent.role.as_deref() == Some("context-engine"))
}

pub(crate) fn process_identity_matches(
    agent: &AgentEntry,
    compatibility_identities: &ProcessIdentityIndex,
) -> bool {
    agent
        .process_identity
        .as_ref()
        .or_else(|| compatibility_identities.get(&agent.agent_id))
        .is_some_and(|identity| crate::ipc::process::matches_identity(agent.pid, identity))
}

fn safe_legacy_identity(agent: &AgentEntry) -> Option<crate::ipc::process::ProcessIdentity> {
    if agent.process_identity.is_some() {
        return None;
    }
    let identity = crate::ipc::process::identity(agent.pid)?;
    legacy_identity_matches_registration(agent, &identity).then_some(identity)
}

fn legacy_identity_matches_registration(
    agent: &AgentEntry,
    identity: &crate::ipc::process::ProcessIdentity,
) -> bool {
    if Path::new(&identity.executable)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("lean-ctx")
    {
        return false;
    }
    let started_at = agent.started_at.timestamp_micros();
    let Ok(process_started_at) = i64::try_from(identity.start_marker) else {
        return false;
    };
    const REGISTRATION_GRACE_MICROS: i64 = 5 * 60 * 1_000_000;
    process_started_at <= started_at
        && started_at.saturating_sub(process_started_at) <= REGISTRATION_GRACE_MICROS
}

fn is_recoverable_legacy_finished(agent: &AgentEntry) -> bool {
    agent.process_identity.is_none()
        && agent.status == AgentStatus::Finished
        && agent.status_message.as_deref() == Some("process identity no longer matches")
}

fn ensure_worker_capacity(
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

impl AgentRegistry {
    pub(crate) fn new() -> Self {
        Self {
            agents: Vec::new(),
            scratchpad: Vec::new(),
            logical_sessions: Vec::new(),
            logical_session_telemetry_seen: false,
            updated_at: Utc::now(),
        }
    }

    pub(crate) fn register(
        &mut self,
        agent_type: &str,
        role: Option<&str>,
        project_root: &str,
    ) -> Result<String, String> {
        self.register_process(agent_type, role, project_root, std::process::id())
    }

    fn register_process(
        &mut self,
        agent_type: &str,
        role: Option<&str>,
        project_root: &str,
        pid: u32,
    ) -> Result<String, String> {
        let identity = crate::ipc::process::identity(pid)
            .ok_or_else(|| format!("cannot establish immutable process identity for PID {pid}"))?;
        let agent_id = format!("{}-{}-{}", agent_type, pid, generate_short_id());

        if let Some(existing) = self.agents.iter_mut().find(|a| {
            a.pid == pid
                && a.status != AgentStatus::Finished
                && a.process_identity.as_ref() == Some(&identity)
        }) {
            existing.last_active = Utc::now();
            existing.status = AgentStatus::Active;
            existing.agent_type = agent_type.to_string();
            existing.project_root = project_root.to_string();
            if let Some(r) = role {
                existing.role = Some(r.to_string());
            }
            return Ok(existing.agent_id.clone());
        }

        // A legacy record or a PID-reused record can share this numeric PID.
        // Retire it before admitting the new owner; a bare PID is never enough
        // to keep a presence alive.
        for existing in self
            .agents
            .iter_mut()
            .filter(|agent| agent.pid == pid && agent.status != AgentStatus::Finished)
        {
            existing.status = AgentStatus::Finished;
            existing.status_message =
                Some("superseded by a different process identity".to_string());
        }

        let compatibility_identities = ProcessIdentityIndex::load();
        let now = Utc::now();
        let active_machine_wide = self
            .agents
            .iter()
            .filter(|agent| {
                consumes_worker_capacity(agent)
                    && has_active_worker_lease(agent, now)
                    && process_identity_matches(agent, &compatibility_identities)
            })
            .count();
        let limit = max_concurrent_workers();
        ensure_worker_capacity(active_machine_wide, limit, project_root)?;

        if role_can_mutate(role) {
            let active_mutating = self
                .agents
                .iter()
                .filter(|agent| {
                    consumes_worker_capacity(agent)
                        && role_can_mutate(agent.role.as_deref())
                        && has_active_worker_lease(agent, now)
                        && process_identity_matches(agent, &compatibility_identities)
                })
                .count();
            let limit = max_concurrent_mutating_workers();
            if active_mutating >= limit {
                return Err(format!(
                    "machine-wide mutating-agent capacity reached: {active_mutating}/{limit}; queue this task or use an explicitly read-only role"
                ));
            }
        }

        if !(agent_type == "mcp" && role == Some("context-engine"))
            && let Some(role) = role.map(str::trim).filter(|role| !role.is_empty())
            && self.agents.iter().any(|agent| {
                agent.project_root == project_root
                    && agent
                        .role
                        .as_deref()
                        .is_some_and(|active| active.eq_ignore_ascii_case(role))
                    && has_active_worker_lease(agent, now)
                    && process_identity_matches(agent, &compatibility_identities)
            })
        {
            return Err(format!(
                "duplicate active role rejected for {project_root}: {role}; reuse or finish the existing worker"
            ));
        }

        self.agents.push(AgentEntry {
            agent_id: agent_id.clone(),
            agent_type: agent_type.to_string(),
            role: role.map(std::string::ToString::to_string),
            project_root: project_root.to_string(),
            started_at: Utc::now(),
            last_active: Utc::now(),
            pid,
            process_identity: Some(identity),
            status: AgentStatus::Active,
            status_message: None,
        });

        self.updated_at = Utc::now();
        crate::core::events::emit_agent_action(&agent_id, "register", None);
        Ok(agent_id)
    }

    /// Atomically registers this MCP process in the shared on-disk registry.
    pub(crate) fn register_mcp_process(project_root: &str) -> Result<String, String> {
        mutate_persistent(|registry| {
            registry.cleanup_stale(presence_ttl());
            registry.register("mcp", Some("context-engine"), project_root)
        })
        .and_then(|result| result)
    }

    /// Atomically refreshes a registered MCP process heartbeat.
    pub(crate) fn heartbeat_persistent(agent_id: &str) -> Result<(), String> {
        mutate_persistent(|registry| registry.update_heartbeat(agent_id))?
    }

    /// Atomically marks a registered MCP process as finished.
    pub(crate) fn finish_persistent(agent_id: &str) -> Result<(), String> {
        mutate_persistent(|registry| {
            registry.set_status(agent_id, AgentStatus::Finished, Some("connection closed"))
        })?
    }

    pub(crate) fn update_heartbeat(&mut self, agent_id: &str) -> Result<(), String> {
        let agent = self
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| format!("agent presence '{agent_id}' was not found"))?;
        if agent.status == AgentStatus::Finished {
            return Err(format!(
                "agent presence '{agent_id}' is finished; register a new process presence"
            ));
        }
        let pid = std::process::id();
        if agent.pid != pid {
            return Err(format!(
                "agent presence '{agent_id}' belongs to PID {}; heartbeat came from PID {pid}",
                agent.pid
            ));
        }
        let identity = crate::ipc::process::identity(pid)
            .ok_or_else(|| format!("cannot establish immutable process identity for PID {pid}"))?;
        if let Some(expected) = &agent.process_identity
            && expected != &identity
        {
            return Err(format!(
                "agent presence '{agent_id}' process identity no longer matches"
            ));
        }
        // Upgrade legacy records only from their owning process. This avoids a
        // migration gap while still rejecting a PID reused by unrelated work.
        agent.process_identity = Some(identity);
        agent.status = AgentStatus::Active;
        agent.last_active = Utc::now();
        Ok(())
    }

    pub(crate) fn set_status(
        &mut self,
        agent_id: &str,
        status: AgentStatus,
        message: Option<&str>,
    ) -> Result<(), String> {
        let agent = self
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| format!("agent presence '{agent_id}' was not found"))?;
        agent.status = status;
        agent.status_message = message.map(std::string::ToString::to_string);
        agent.last_active = Utc::now();
        self.updated_at = Utc::now();
        Ok(())
    }
    /// Records explicit logical-session presence supplied by an owning editor
    /// integration. Tool activity is deliberately never treated as a session.
    pub(crate) fn open_or_heartbeat_logical_session(
        &mut self,
        source: &str,
        workspace: &str,
        session_id: &str,
    ) {
        let now = Utc::now();
        self.logical_session_telemetry_seen = true;
        if let Some(session) = self.logical_sessions.iter_mut().find(|session| {
            session.source == source
                && session.workspace == workspace
                && session.session_id == session_id
        }) {
            session.last_heartbeat = now;
        } else {
            self.logical_sessions.push(LogicalSessionPresence {
                source: source.to_string(),
                workspace: workspace.to_string(),
                session_id: session_id.to_string(),
                opened_at: now,
                last_heartbeat: now,
            });
        }
        self.updated_at = now;
    }

    pub(crate) fn close_logical_session(
        &mut self,
        source: &str,
        workspace: &str,
        session_id: &str,
    ) -> bool {
        self.logical_session_telemetry_seen = true;
        let previous_len = self.logical_sessions.len();
        self.logical_sessions.retain(|session| {
            session.source != source
                || session.workspace != workspace
                || session.session_id != session_id
        });
        let removed = self.logical_sessions.len() != previous_len;
        self.updated_at = Utc::now();
        removed
    }

    pub(crate) fn cleanup_stale_logical_sessions(&mut self, max_age_seconds: u64) {
        let seconds = i64::try_from(max_age_seconds).unwrap_or(i64::MAX);
        let cutoff = Utc::now() - chrono::Duration::seconds(seconds);
        self.logical_sessions
            .retain(|session| session.last_heartbeat >= cutoff);
        self.updated_at = Utc::now();
    }

    pub(crate) fn record_logical_session_presence(
        event: &str,
        source: &str,
        workspace: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let valid_field = |value: &str, max_bytes: usize| {
            !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
        };
        if !valid_field(source, LOGICAL_SESSION_SOURCE_MAX_BYTES)
            || !valid_field(workspace, LOGICAL_SESSION_WORKSPACE_MAX_BYTES)
            || !valid_field(session_id, LOGICAL_SESSION_ID_MAX_BYTES)
        {
            return Err(
                "presence fields are empty, too long, or contain control characters".to_string(),
            );
        }
        if !matches!(event, "open" | "heartbeat" | "close") {
            return Err("event must be open, heartbeat, or close".to_string());
        }

        let ttl = crate::core::config::Config::load()
            .agents
            .logical_session_ttl_seconds;
        mutate_persistent(|registry| {
            registry.cleanup_stale_logical_sessions(ttl);
            match event {
                "open" | "heartbeat" => {
                    registry.open_or_heartbeat_logical_session(source, workspace, session_id);
                }
                "close" => {
                    registry.close_logical_session(source, workspace, session_id);
                }
                _ => unreachable!("event validated above"),
            }
        })
    }

    pub(crate) fn list_active(&self, project_root: Option<&str>) -> Vec<&AgentEntry> {
        let compatibility_identities = ProcessIdentityIndex::load();
        self.agents
            .iter()
            .filter(|a| {
                if let Some(root) = project_root {
                    a.project_root == root
                        && a.status != AgentStatus::Finished
                        && process_identity_matches(a, &compatibility_identities)
                } else {
                    a.status != AgentStatus::Finished
                        && process_identity_matches(a, &compatibility_identities)
                }
            })
            .collect()
    }

    pub(crate) fn list_all(&self) -> &[AgentEntry] {
        &self.agents
    }

    pub(crate) fn post_message(
        &mut self,
        from_agent: &str,
        to_agent: Option<&str>,
        category: &str,
        message: &str,
    ) -> String {
        self.post_message_full(
            from_agent,
            to_agent,
            category,
            message,
            PrivacyLevel::default(),
            MessagePriority::default(),
            None,
        )
    }

    pub(crate) fn post_message_full(
        &mut self,
        from_agent: &str,
        to_agent: Option<&str>,
        category: &str,
        message: &str,
        privacy: PrivacyLevel,
        priority: MessagePriority,
        ttl_hours: Option<u64>,
    ) -> String {
        self.post_message_scoped(
            None, from_agent, to_agent, category, message, privacy, priority, ttl_hours,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn post_message_scoped(
        &mut self,
        project_root: Option<&str>,
        from_agent: &str,
        to_agent: Option<&str>,
        category: &str,
        message: &str,
        privacy: PrivacyLevel,
        priority: MessagePriority,
        ttl_hours: Option<u64>,
    ) -> String {
        let id = generate_short_id();
        let default_ttl_hours = crate::core::config::Config::load()
            .agents
            .scratchpad_default_ttl_hours;
        let expires_at = Some(match ttl_hours {
            Some(hours) => Utc::now() + chrono::Duration::hours(hours as i64),
            None => Utc::now() + chrono::Duration::hours(default_ttl_hours as i64),
        });
        self.scratchpad.push(ScratchpadEntry {
            id: id.clone(),
            from_agent: from_agent.to_string(),
            to_agent: to_agent.map(std::string::ToString::to_string),
            task_id: None,
            category: category.to_string(),
            priority,
            privacy,
            message: message.to_string(),
            metadata: HashMap::new(),
            project_root: project_root.map(std::string::ToString::to_string),
            timestamp: Utc::now(),
            read_by: vec![from_agent.to_string()],
            expires_at,
        });

        let max_scratchpad_entries = max_scratchpad();
        if self.scratchpad.len() > max_scratchpad_entries {
            self.scratchpad
                .drain(0..self.scratchpad.len() - max_scratchpad_entries);
        }

        self.updated_at = Utc::now();
        id
    }

    pub(crate) fn read_messages(&mut self, agent_id: &str) -> Vec<&ScratchpadEntry> {
        let now = Utc::now();
        let unread: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                !e.read_by.contains(&agent_id.to_string())
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && e.expires_at.is_none_or(|exp| exp > now)
            })
            .map(|(i, _)| i)
            .collect();

        for i in &unread {
            self.scratchpad[*i].read_by.push(agent_id.to_string());
        }

        self.scratchpad
            .iter()
            .filter(|e| {
                (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && e.from_agent != agent_id
                    && e.expires_at.is_none_or(|exp| exp > now)
            })
            .collect()
    }

    pub(crate) fn read_unread(&mut self, agent_id: &str) -> Vec<&ScratchpadEntry> {
        self.read_unread_for_project(agent_id, None)
    }

    pub(crate) fn read_unread_scoped(
        &mut self,
        agent_id: &str,
        project_root: &str,
    ) -> Vec<&ScratchpadEntry> {
        self.read_unread_for_project(agent_id, Some(project_root))
    }

    fn read_unread_for_project(
        &mut self,
        agent_id: &str,
        project_root: Option<&str>,
    ) -> Vec<&ScratchpadEntry> {
        let now = Utc::now();
        let unread_indices: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                !e.read_by.contains(&agent_id.to_string())
                    && e.from_agent != agent_id
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && project_root.is_none_or(|root| e.project_root.as_deref() == Some(root))
                    && e.expires_at.is_none_or(|exp| exp > now)
            })
            .map(|(i, _)| i)
            .collect();

        for i in &unread_indices {
            self.scratchpad[*i].read_by.push(agent_id.to_string());
        }

        self.updated_at = Utc::now();

        self.scratchpad
            .iter()
            .filter(|e| {
                e.from_agent != agent_id
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && project_root.is_none_or(|root| e.project_root.as_deref() == Some(root))
                    && e.read_by.contains(&agent_id.to_string())
                    && e.read_by.iter().filter(|r| *r == agent_id).count() == 1
                    && e.expires_at.is_none_or(|exp| exp > now)
            })
            .collect()
    }

    pub(crate) fn cleanup_stale(&mut self, max_age_hours: u64) {
        let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours as i64);
        let mut compatibility_identities = ProcessIdentityIndex::load();
        let mut identities_changed = false;
        let mut newly_certified = Vec::new();

        for agent in &mut self.agents {
            if agent.status == AgentStatus::Finished {
                if is_recoverable_legacy_finished(agent) {
                    if let Some(identity) = safe_legacy_identity(agent) {
                        identities_changed |=
                            compatibility_identities.insert(&agent.agent_id, &identity);
                        newly_certified.push(agent.agent_id.clone());
                        agent.status = AgentStatus::Active;
                        agent.status_message =
                            Some("legacy process identity recovered".to_string());
                    }
                }
                continue;
            }
            if let Some(identity) = &agent.process_identity {
                identities_changed |= compatibility_identities.insert(&agent.agent_id, identity);
            }
            if process_identity_matches(agent, &compatibility_identities) {
                continue;
            }
            if let Some(identity) = safe_legacy_identity(agent) {
                identities_changed |= compatibility_identities.insert(&agent.agent_id, &identity);
                newly_certified.push(agent.agent_id.clone());
                continue;
            }
            {
                agent.status = AgentStatus::Finished;
                agent.status_message = Some("process identity no longer matches".to_string());
            }
        }

        // Presence is operational state, not an audit log. Keep the newest
        // completed sessions for diagnostics but bound their in-memory/on-disk
        // history so a burst of short-lived agents cannot slow later checks.
        let mut finished_by_recency: Vec<_> = self
            .agents
            .iter()
            .filter(|agent| agent.status == AgentStatus::Finished && agent.last_active >= cutoff)
            .collect();
        finished_by_recency.sort_unstable_by(|left, right| {
            right
                .last_active
                .cmp(&left.last_active)
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });
        let retained_finished_ids: HashSet<String> = finished_by_recency
            .into_iter()
            .take(MAX_RETAINED_FINISHED_AGENTS)
            .map(|agent| agent.agent_id.clone())
            .collect();

        // Drop each retired agent's budget entry too — a finished/dead agent can't read
        // again, so removing its budget loses no live enforcement and bounds BUDGETS.
        self.agents.retain(|a| {
            let retire = a.status == AgentStatus::Finished
                && (a.last_active < cutoff || !retained_finished_ids.contains(&a.agent_id));
            if retire {
                crate::core::agent_budget::remove(&a.agent_id);
            }
            !retire
        });
        identities_changed |= compatibility_identities.retain_agents(self);
        if identities_changed && compatibility_identities.save().is_err() {
            for agent in &mut self.agents {
                if newly_certified.contains(&agent.agent_id) {
                    agent.status = AgentStatus::Finished;
                    agent.status_message =
                        Some("legacy process identity could not be persisted".to_string());
                }
            }
        }

        // Remove expired scratchpad entries.
        let now = Utc::now();
        self.scratchpad
            .retain(|entry| entry.expires_at.is_none_or(|exp| exp > now));

        self.updated_at = Utc::now();
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let dir = agents_dir()?;
        create_agents_dir(&dir)?;

        let lock_path = dir.join("registry.lock");
        let _lock = FileLock::acquire(&lock_path)?;

        self.save_locked(&dir)
    }

    fn save_locked(&self, dir: &std::path::Path) -> Result<(), String> {
        let path = dir.join("registry.json");
        save_registry_file(&path, self)
    }

    pub(crate) fn load() -> Option<Self> {
        let dir = agents_dir().ok()?;
        let path = dir.join("registry.json");
        load_registry_file(&path).ok().flatten()
    }

    pub(crate) fn load_or_create() -> Self {
        Self::load().unwrap_or_default()
    }

    /// Atomically load, mutate, and persist the registry under a single file
    /// lock. `load_or_create()` + mutate + `save()` is a read-modify-write
    /// race: `save()` only locks the final write, so two concurrent callers
    /// (two MCP sessions registering, or the dashboard's own poll-triggered
    /// `cleanup_stale` + save) can each load a stale snapshot and the last
    /// writer silently drops the other's changes — e.g. a second session's
    /// registration vanishing from the dashboard. Holding the lock across
    /// the re-read closes that window: the read inside always sees the
    /// latest on-disk state.
    pub(crate) fn mutate_locked<T>(f: impl FnOnce(&mut Self) -> T) -> Result<(Self, T), String> {
        let dir = agents_dir()?;
        create_agents_dir(&dir)?;

        let lock_path = dir.join("registry.lock");
        let _lock = FileLock::acquire(&lock_path)?;

        let path = dir.join("registry.json");
        let mut registry = load_registry_file(&path)?.unwrap_or_default();
        let out = f(&mut registry);
        registry.save_locked(&dir)?;
        Ok((registry, out))
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use super::{
        AgentDiary, AgentEntry, AgentRegistry, AgentStatus, DiaryEntryType, ScratchpadEntry,
        truncate,
    };
    use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

    #[test]
    fn register_and_list() {
        let mut reg = AgentRegistry::new();
        let id = reg
            .register("cursor", Some("dev"), "/tmp/project")
            .expect("current process can be registered");
        assert!(!id.is_empty());
        assert_eq!(reg.list_active(None).len(), 1);
        assert_eq!(reg.list_active(None)[0].agent_type, "cursor");
    }

    #[test]
    fn reregister_same_pid() {
        let mut reg = AgentRegistry::new();
        let id1 = reg
            .register("cursor", Some("dev"), "/tmp/project")
            .expect("current process can be registered");
        let id2 = reg
            .register("cursor", Some("review"), "/tmp/project")
            .expect("same process can be re-registered");
        assert_eq!(id1, id2);
        assert_eq!(reg.agents.len(), 1);
        assert_eq!(reg.agents[0].role, Some("review".to_string()));
    }

    #[test]
    fn post_and_read_messages() {
        let mut reg = AgentRegistry::new();
        reg.post_message("agent-a", None, "finding", "Found a bug in auth.rs");
        reg.post_message("agent-b", Some("agent-a"), "request", "Please review");

        let msgs = reg.read_unread("agent-a");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].category, "request");
    }

    #[test]
    fn scoped_messages_are_invisible_to_other_projects() {
        let mut reg = AgentRegistry::new();
        reg.post_message_scoped(
            Some("/project-a"),
            "agent-a",
            Some("agent-b"),
            "finding",
            "private project-a finding",
            PrivacyLevel::Private,
            MessagePriority::Normal,
            Some(1),
        );

        assert!(reg.read_unread_scoped("agent-b", "/project-b").is_empty());

        let messages = reg.read_unread_scoped("agent-b", "/project-a");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].project_root.as_deref(), Some("/project-a"));
    }

    #[test]
    fn expired_messages_are_skipped_in_read_unread() {
        let mut reg = AgentRegistry::new();
        reg.scratchpad.push(ScratchpadEntry {
            id: "expired-1".to_string(),
            from_agent: "agent-a".to_string(),
            to_agent: None,
            task_id: None,
            category: "test".to_string(),
            priority: MessagePriority::default(),
            privacy: PrivacyLevel::default(),
            message: "I am expired".to_string(),
            metadata: HashMap::new(),
            project_root: None,
            timestamp: Utc::now(),
            read_by: vec![],
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        });
        reg.post_message("agent-a", None, "test", "I am fresh");

        let msgs = reg.read_unread("agent-b");

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message, "I am fresh");
    }

    #[test]
    fn cleanup_stale_removes_expired_scratchpad() {
        let mut reg = AgentRegistry::new();
        reg.scratchpad.push(ScratchpadEntry {
            id: "exp-1".to_string(),
            from_agent: "a".to_string(),
            to_agent: None,
            task_id: None,
            category: "test".to_string(),
            priority: MessagePriority::default(),
            privacy: PrivacyLevel::default(),
            message: "expired".to_string(),
            metadata: HashMap::new(),
            project_root: None,
            timestamp: Utc::now(),
            read_by: vec![],
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        });

        reg.cleanup_stale(super::presence_ttl());

        assert!(reg.scratchpad.is_empty());
    }

    #[test]
    fn post_message_gets_default_ttl() {
        let mut reg = AgentRegistry::new();

        reg.post_message("agent-a", None, "finding", "test");

        assert!(
            reg.scratchpad[0].expires_at.is_some(),
            "default TTL must be set"
        );
    }

    #[test]
    fn set_status() {
        let mut reg = AgentRegistry::new();
        let id = reg
            .register("claude", None, "/tmp/project")
            .expect("current process can be registered");
        reg.set_status(&id, AgentStatus::Idle, Some("waiting for review"))
            .expect("registered agent exists");
        assert_eq!(reg.agents[0].status, AgentStatus::Idle);
        assert_eq!(
            reg.agents[0].status_message,
            Some("waiting for review".to_string())
        );
    }

    #[test]
    fn unknown_status_update_fails_closed() {
        let mut reg = AgentRegistry::new();
        let error = reg
            .set_status("missing-agent", AgentStatus::Finished, None)
            .expect_err("unknown agent must be rejected");
        assert!(error.contains("missing-agent"));
        assert!(reg.agents.is_empty());
    }

    #[test]
    fn broadcast_message() {
        let mut reg = AgentRegistry::new();
        reg.post_message("agent-a", None, "status", "Starting refactor");

        let msgs_b = reg.read_unread("agent-b");
        assert_eq!(msgs_b.len(), 1);
        assert_eq!(msgs_b[0].message, "Starting refactor");

        let msgs_a = reg.read_unread("agent-a");
        assert!(msgs_a.is_empty());
    }

    #[test]
    fn diary_add_and_format() {
        let mut diary = AgentDiary::new("test-agent-001", "cursor", "/tmp/project");
        diary.add_entry(
            DiaryEntryType::Discovery,
            "Found auth module at src/auth.rs",
            Some("auth"),
        );
        diary.add_entry(
            DiaryEntryType::Decision,
            "Use JWT RS256 for token signing",
            None,
        );
        diary.add_entry(
            DiaryEntryType::Progress,
            "Implemented login endpoint",
            Some("auth"),
        );

        assert_eq!(diary.entries.len(), 3);

        let summary = diary.format_summary();
        assert!(summary.contains("test-agent-001"));
        assert!(summary.contains("FOUND"));
        assert!(summary.contains("DECIDED"));
        assert!(summary.contains("DONE"));
    }

    #[test]
    fn diary_compact_format() {
        let mut diary = AgentDiary::new("test-agent-002", "claude", "/tmp/project");
        diary.add_entry(DiaryEntryType::Insight, "DB queries are N+1", None);
        diary.add_entry(
            DiaryEntryType::Blocker,
            "Missing API credentials",
            Some("deploy"),
        );

        let compact = diary.format_compact();
        assert!(compact.contains("diary:test-agent-002"));
        assert!(compact.contains("B:Missing API credentials"));
        assert!(compact.contains("I:DB queries are N+1"));
    }

    #[test]
    fn diary_entry_types() {
        let types = vec![
            DiaryEntryType::Discovery,
            DiaryEntryType::Decision,
            DiaryEntryType::Blocker,
            DiaryEntryType::Progress,
            DiaryEntryType::Insight,
        ];
        for t in types {
            assert!(!format!("{t}").is_empty());
        }
    }

    #[test]
    fn diary_truncation() {
        let mut diary = AgentDiary::new("test-agent", "cursor", "/tmp");
        for i in 0..150 {
            diary.add_entry(DiaryEntryType::Progress, &format!("Step {i}"), None);
        }
        assert!(diary.entries.len() <= 100);
    }

    #[test]
    fn truncate_utf8_emoji_no_panic() {
        let result = truncate("Agent 🤖 Name ist lang genug", 15);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_utf8_cyrillic_no_panic() {
        let result = truncate("агент выполняет длинную задачу", 15);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_short_utf8_unchanged() {
        assert_eq!(truncate("短い", 20), "短い");
    }

    fn test_entry(agent_id: &str, project_root: &str, pid: u32) -> AgentEntry {
        let now = Utc::now();
        AgentEntry {
            agent_id: agent_id.to_string(),
            agent_type: "cursor".to_string(),
            role: Some("dev".to_string()),
            project_root: project_root.to_string(),
            started_at: now,
            last_active: now,
            pid,
            process_identity: crate::ipc::process::identity(pid),
            status: AgentStatus::Active,
            status_message: None,
        }
    }

    #[test]
    fn cleanup_stale_caps_recent_finished_history() {
        let mut reg = AgentRegistry::new();
        let now = Utc::now();
        reg.agents = (0..(super::MAX_RETAINED_FINISHED_AGENTS + 2))
            .map(|offset| {
                let mut agent = test_entry(
                    &format!("finished-{offset}"),
                    "/project",
                    std::process::id(),
                );
                agent.status = AgentStatus::Finished;
                agent.last_active = now - chrono::Duration::seconds(offset as i64);
                agent
            })
            .collect();

        reg.cleanup_stale(1);

        assert_eq!(reg.agents.len(), super::MAX_RETAINED_FINISHED_AGENTS);
        assert!(
            reg.agents
                .iter()
                .any(|agent| agent.agent_id == "finished-0")
        );
        assert!(!reg.agents.iter().any(|agent| {
            agent.agent_id == format!("finished-{}", super::MAX_RETAINED_FINISHED_AGENTS + 1)
        }));
    }

    /// #419: the wake-up briefing scopes agents to the current project via
    /// `list_active(Some(root))`. Peers working on *other* projects must never
    /// leak into the briefing.
    #[test]
    fn list_active_scopes_to_project_root() {
        let mut reg = AgentRegistry::new();
        reg.agents
            .push(test_entry("a-1", "/proj/a", std::process::id()));
        reg.agents
            .push(test_entry("b-1", "/proj/b", std::process::id()));

        let active_a = reg.list_active(Some("/proj/a"));
        assert_eq!(active_a.len(), 1);
        assert_eq!(active_a[0].agent_id, "a-1");

        // Unscoped still sees both.
        assert_eq!(reg.list_active(None).len(), 2);
    }

    /// #419: a crashed/exited MCP process leaves an `Active` entry behind.
    /// `cleanup_stale` must flip it to `Finished` (regardless of age) so
    /// `list_active` no longer surfaces it as a live peer — the ghost the
    /// briefing used to show. Previously `#[cfg(unix)]`-only, which is why
    /// the non-unix `is_process_alive` hardcoded-`true` regression (see its
    /// doc comment) shipped unnoticed: this exact test never ran on Windows.
    #[test]
    fn cleanup_stale_prunes_dead_pid_from_active_list() {
        // Reap a child so its PID is guaranteed dead at assertion time.
        let reaped = {
            let mut cmd = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", "exit"]);
                c
            } else {
                std::process::Command::new("true")
            };
            let mut child = cmd.spawn().expect("spawn short-lived helper process");
            let pid = child.id();
            child.wait().expect("reap helper process");
            pid
        };

        let mut reg = AgentRegistry::new();
        reg.agents.push(test_entry("ghost", "/proj/a", reaped));
        reg.agents
            .push(test_entry("live", "/proj/a", std::process::id()));

        reg.cleanup_stale(super::presence_ttl());

        let ids: Vec<&str> = reg
            .list_active(Some("/proj/a"))
            .iter()
            .map(|a| a.agent_id.as_str())
            .collect();
        assert!(ids.contains(&"live"), "live same-project agent must remain");
        assert!(
            !ids.contains(&"ghost"),
            "dead-pid agent must be pruned from the active list (#419)"
        );
    }

    #[test]
    fn cleanup_stale_rejects_a_reused_pid_with_the_wrong_identity() {
        let pid = std::process::id();
        let mut stale = test_entry("reused-pid", "/proj/a", pid);
        let identity = stale
            .process_identity
            .as_mut()
            .expect("current process identity");
        identity.start_marker = identity.start_marker.saturating_add(1);
        let mut registry = AgentRegistry::new();
        registry.agents.push(stale);

        registry.cleanup_stale(super::presence_ttl());

        assert_eq!(registry.agents[0].status, AgentStatus::Finished);
        assert_eq!(
            registry.agents[0].status_message.as_deref(),
            Some("process identity no longer matches")
        );
    }

    #[test]
    fn legacy_identity_recovery_requires_the_original_lean_ctx_process() {
        let started_at = Utc::now();
        let agent = AgentEntry {
            agent_id: "legacy".to_string(),
            agent_type: "mcp".to_string(),
            role: None,
            project_root: "/project".to_string(),
            started_at,
            last_active: started_at,
            pid: 1,
            process_identity: None,
            status: AgentStatus::Active,
            status_message: None,
        };
        let registered_after_boot = crate::ipc::process::ProcessIdentity {
            start_marker: u64::try_from(started_at.timestamp_micros() - 1_000_000)
                .expect("current timestamps fit u64"),
            executable: "/Users/test/.local/bin/lean-ctx".to_string(),
        };
        assert!(super::legacy_identity_matches_registration(
            &agent,
            &registered_after_boot
        ));

        let reused_pid = crate::ipc::process::ProcessIdentity {
            start_marker: u64::try_from(started_at.timestamp_micros() + 1).expect("fits u64"),
            executable: "/Users/test/.local/bin/lean-ctx".to_string(),
        };
        assert!(!super::legacy_identity_matches_registration(
            &agent,
            &reused_pid
        ));

        let unrelated_process = crate::ipc::process::ProcessIdentity {
            start_marker: registered_after_boot.start_marker,
            executable: "/Applications/Firefox.app/Contents/MacOS/firefox".to_string(),
        };
        assert!(!super::legacy_identity_matches_registration(
            &agent,
            &unrelated_process
        ));
    }

    #[test]
    fn only_the_known_legacy_false_positive_is_recoverable() {
        let now = Utc::now();
        let mut agent = AgentEntry {
            agent_id: "legacy".to_string(),
            agent_type: "mcp".to_string(),
            role: None,
            project_root: "/project".to_string(),
            started_at: now,
            last_active: now,
            pid: 1,
            process_identity: None,
            status: AgentStatus::Finished,
            status_message: Some("process identity no longer matches".to_string()),
        };
        assert!(super::is_recoverable_legacy_finished(&agent));

        agent.status_message = Some("connection closed".to_string());
        assert!(!super::is_recoverable_legacy_finished(&agent));
        agent.status = AgentStatus::Active;
        assert!(!super::is_recoverable_legacy_finished(&agent));
    }

    #[test]
    fn resource_broker_capacity_fails_closed_at_the_hard_limit() {
        assert!(super::ensure_worker_capacity(14, 15, "/project").is_ok());
        let error = super::ensure_worker_capacity(15, 15, "/project")
            .expect_err("a sixteenth worker must be rejected");
        assert!(error.contains("15/15"));
    }

    #[test]
    fn resource_broker_context_engine_presence_does_not_consume_capacity() {
        let mut agent = test_entry("mcp", "/project", std::process::id());
        agent.agent_type = "mcp".to_string();
        agent.role = Some("context-engine".to_string());
        assert!(!super::consumes_worker_capacity(&agent));

        agent.role = Some("implementer".to_string());
        assert!(super::consumes_worker_capacity(&agent));
    }

    #[test]
    fn resource_broker_classifies_lightweight_roles() {
        for role in [
            "security reviewer",
            "read-only mapper",
            "API research",
            "performance audit",
        ] {
            assert!(!super::role_can_mutate(Some(role)), "{role}");
        }
        for role in [None, Some("implementer"), Some("integration lead")] {
            assert!(super::role_can_mutate(role));
        }
    }

    #[test]
    fn resource_broker_idle_or_expired_workers_release_capacity() {
        let mut agent = test_entry("worker", "/project", std::process::id());
        assert!(super::has_active_worker_lease(&agent, Utc::now()));

        agent.status = AgentStatus::Idle;
        assert!(!super::has_active_worker_lease(&agent, Utc::now()));

        agent.status = AgentStatus::Active;
        agent.last_active =
            Utc::now() - chrono::Duration::seconds(super::active_worker_lease_seconds() + 1);
        assert!(!super::has_active_worker_lease(&agent, Utc::now()));
    }

    /// Regression: concurrent load-mutate-save cycles must not silently drop
    /// each other's changes. Before `mutate_locked`, `save()` only locked the
    /// final write — the preceding `load()` was unlocked, so a second writer
    /// could load a stale snapshot and overwrite the first writer's addition
    /// (e.g. a second Claude Code session's agent registration vanishing
    /// from the dashboard).
    #[test]
    fn mutate_locked_survives_concurrent_writers() {
        let _iso = crate::core::data_dir::isolated_data_dir();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    AgentRegistry::mutate_locked(|registry| {
                        registry.agents.push(AgentEntry {
                            agent_id: format!("agent-{i}"),
                            agent_type: "test".to_string(),
                            role: None,
                            project_root: "/tmp/project".to_string(),
                            started_at: Utc::now(),
                            last_active: Utc::now(),
                            pid: 10_000 + i,
                            process_identity: None,
                            status: AgentStatus::Active,
                            status_message: None,
                        });
                    })
                })
            })
            .collect();

        for h in handles {
            h.join()
                .expect("writer thread must not panic")
                .expect("mutate_locked must succeed");
        }

        let registry = AgentRegistry::load_or_create();
        assert_eq!(
            registry.agents.len(),
            8,
            "all 8 concurrent registrations must survive, got {}",
            registry.agents.len()
        );
    }
}

#[cfg(test)]
mod presence_tests {
    use chrono::Utc;

    use super::{AgentRegistry, AgentStatus};

    #[test]
    fn persistent_presence_roundtrips_lifecycle_for_owning_process() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        let first = registry
            .register_process(
                "mcp",
                Some("context-engine"),
                "/project",
                std::process::id(),
            )
            .expect("current process has an identity");
        registry.save().expect("save registry");

        assert_eq!(AgentRegistry::load().expect("registry").agents.len(), 1);

        AgentRegistry::heartbeat_persistent(&first).expect("heartbeat");
        AgentRegistry::finish_persistent(&first).expect("finish");
        let loaded = AgentRegistry::load().expect("registry");
        assert_eq!(
            loaded
                .agents
                .iter()
                .find(|agent| agent.agent_id == first)
                .expect("registered agent")
                .status,
            AgentStatus::Finished
        );
        assert!(isolated.path().join("agents/registry.json").exists());
    }

    #[test]
    fn compatibility_index_survives_an_old_registry_writer() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let mut registry = AgentRegistry::new();
        registry
            .register_process(
                "mcp",
                Some("context-engine"),
                "/project",
                std::process::id(),
            )
            .expect("current process has an identity");
        registry.cleanup_stale(super::presence_ttl());

        // An MCP process from the prior release serializes the old schema and
        // therefore drops this new registry field. The sidecar retains the
        // binding, so current readers still recognize the process as live.
        registry.agents[0].process_identity = None;
        assert_eq!(registry.list_active(Some("/project")).len(), 1);
    }

    #[test]
    fn unknown_persistent_heartbeat_fails_closed() {
        let _isolated = crate::core::data_dir::isolated_data_dir();

        let error = AgentRegistry::heartbeat_persistent("missing-agent")
            .expect_err("unknown presence must not report a successful heartbeat");

        assert!(error.contains("was not found"));
    }

    #[test]
    fn corrupt_registry_fails_closed_without_overwrite() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let agents_dir = isolated.path().join("agents");
        std::fs::create_dir_all(&agents_dir).expect("agents directory");
        let registry_path = agents_dir.join("registry.json");
        let corrupt = "{not valid json";
        std::fs::write(&registry_path, corrupt).expect("corrupt fixture");

        let error = AgentRegistry::mutate_locked(|registry| {
            let _ = registry.register_process("mcp", Some("context-engine"), "/project", 101);
        })
        .expect_err("corrupt registry must reject mutation");

        assert!(error.contains("agent registry is corrupt"));
        assert_eq!(std::fs::read_to_string(registry_path).unwrap(), corrupt);
    }

    /// #1619: a sandbox or filesystem policy denies exactly one path, and the
    /// error was reported as a bare `File exists (os error 17)` — no path, no
    /// operation, nothing to allow-list. The reporter had to guess
    /// `~/.local/share/lean-ctx/agents` to unblock themselves.
    #[test]
    fn agents_dir_creation_failure_names_the_path_and_operation() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        // A plain file where the directory belongs reproduces the reported
        // EEXIST without needing a sandbox.
        let blocker = isolated.path().join("agents");
        std::fs::write(&blocker, b"not a directory").expect("blocking file");

        let error = AgentRegistry::mutate_locked(|registry| {
            let _ = registry.register_process("mcp", Some("context-engine"), "/project", 101);
        })
        .expect_err("a file in place of the agents directory must fail");

        assert!(
            error.contains("create agent registry directory"),
            "the failing operation must be named; got: {error}"
        );
        assert!(
            error.contains(&blocker.display().to_string()),
            "the conflicting path must be in the message so it can be allow-listed; got: {error}"
        );
    }

    #[test]
    fn reregistering_process_refreshes_metadata_without_duplication() {
        let mut registry = AgentRegistry::new();
        let pid = std::process::id();
        let first = registry
            .register_process("unknown", None, "/old", pid)
            .expect("current process has an identity");
        let second = registry
            .register_process("mcp", Some("context-engine"), "/new", pid)
            .expect("same process can re-register");

        assert_eq!(first, second);
        assert_eq!(registry.agents.len(), 1);
        assert_eq!(registry.agents[0].agent_type, "mcp");
        assert_eq!(registry.agents[0].project_root, "/new");
        assert_eq!(registry.agents[0].role.as_deref(), Some("context-engine"));
    }

    #[test]
    fn logical_sessions_are_keyed_independently_of_transport_processes() {
        let mut registry = AgentRegistry::new();
        registry
            .register_process(
                "mcp",
                Some("context-engine"),
                "/project",
                std::process::id(),
            )
            .expect("current process has an identity");
        registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");
        registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-b");
        let opened_at = registry.logical_sessions[0].opened_at;

        registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");

        assert_eq!(registry.agents.len(), 1);
        assert_eq!(registry.logical_sessions.len(), 2);
        assert_eq!(registry.logical_sessions[0].opened_at, opened_at);
        assert!(registry.logical_session_telemetry_seen);
        assert!(registry.close_logical_session("vscode", "/project", "chat-b"));
        assert_eq!(registry.logical_sessions.len(), 1);
    }

    #[test]
    fn persistent_logical_session_presence_validates_and_roundtrips() {
        let _isolated = crate::core::data_dir::isolated_data_dir();

        AgentRegistry::record_logical_session_presence(
            "open",
            "vscode",
            "/project",
            "editor-session-a",
        )
        .expect("open presence");

        let registry = AgentRegistry::load().expect("persisted registry");
        assert_eq!(registry.logical_sessions.len(), 1);
        assert_eq!(registry.logical_sessions[0].session_id, "editor-session-a");
        assert!(registry.logical_session_telemetry_seen);

        assert!(
            AgentRegistry::record_logical_session_presence(
                "invalid",
                "vscode",
                "/project",
                "editor-session-a",
            )
            .is_err()
        );
        assert!(
            AgentRegistry::record_logical_session_presence(
                "heartbeat",
                "",
                "/project",
                "editor-session-a",
            )
            .is_err()
        );

        AgentRegistry::record_logical_session_presence(
            "close",
            "vscode",
            "/project",
            "editor-session-a",
        )
        .expect("close presence");
        assert!(
            AgentRegistry::load()
                .expect("persisted registry")
                .logical_sessions
                .is_empty()
        );
    }

    #[test]
    fn logical_session_expiry_is_bounded_by_heartbeat_not_tool_activity() {
        let mut registry = AgentRegistry::new();
        registry.open_or_heartbeat_logical_session("vscode", "/project", "chat-a");
        registry.logical_sessions[0].last_heartbeat = Utc::now() - chrono::Duration::seconds(181);

        registry.cleanup_stale_logical_sessions(180);

        assert!(registry.logical_sessions.is_empty());
        assert!(registry.logical_session_telemetry_seen);
    }

    #[test]
    fn legacy_registry_deserializes_without_claiming_session_telemetry() {
        let registry: AgentRegistry = serde_json::from_str(
            r#"{"agents":[],"scratchpad":[],"updated_at":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("legacy registry");

        assert!(registry.logical_sessions.is_empty());
        assert!(!registry.logical_session_telemetry_seen);
    }
}
