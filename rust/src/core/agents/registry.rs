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
const MAX_RETAINED_FINISHED_AGENTS: usize = 32;
/// Active collaboration slots are leases, not process guesses: MCP workers can
/// share the daemon PID, so process identity alone cannot distinguish sessions.
/// Workers renew via `read` or `status=active`; expiry releases abandoned slots.
const DEFAULT_ACTIVE_WORKER_LEASE_SECONDS: i64 = 120;

fn active_worker_lease_seconds() -> i64 {
    let configured = crate::core::config::Config::load()
        .agents
        .active_worker_lease_seconds;
    i64::try_from(configured)
        .unwrap_or(DEFAULT_ACTIVE_WORKER_LEASE_SECONDS)
        .max(1)
}

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
        durable_identity_id: Option<&str>,
    ) -> Result<String, String> {
        self.register_process(
            agent_type,
            role,
            project_root,
            durable_identity_id,
            std::process::id(),
        )
    }

    fn register_process(
        &mut self,
        agent_type: &str,
        role: Option<&str>,
        project_root: &str,
        durable_identity_id: Option<&str>,
        pid: u32,
    ) -> Result<String, String> {
        if let Some(identity_id) = durable_identity_id {
            crate::core::agent_registry::require_active(identity_id)?;
        }
        let identity = crate::ipc::process::identity(pid)
            .ok_or_else(|| format!("cannot establish immutable process identity for PID {pid}"))?;
        let agent_id = format!("{}-{}-{}", agent_type, pid, generate_short_id());

        if let Some(existing_index) = self.agents.iter().position(|a| {
            a.pid == pid
                && a.status != AgentStatus::Finished
                && a.process_identity.as_ref() == Some(&identity)
        }) {
            let existing_binding = self.agents[existing_index].durable_identity_id.clone();
            if let (Some(existing_id), Some(requested_id)) =
                (existing_binding.as_deref(), durable_identity_id)
                && existing_id != requested_id
            {
                return Err(format!(
                    "agent presence '{}' is already bound to durable identity '{}'; refusing rebind to '{}'",
                    self.agents[existing_index].agent_id, existing_id, requested_id
                ));
            }
            if existing_binding.is_some() {
                self.require_active_for_presence(
                    &self.agents[existing_index].agent_id,
                    "registration",
                )?;
            }
            let existing = &mut self.agents[existing_index];
            existing.last_active = Utc::now();
            existing.status = AgentStatus::Active;
            existing.agent_type = agent_type.to_string();
            existing.project_root = project_root.to_string();
            // #1766: the presence retry registers as the `context-engine`
            // placeholder. It never replaces a role the process already holds
            // (a `reviewer` silently became a context engine); an explicit role
            // still replaces the placeholder — the `initialize` upgrade.
            match role {
                Some("context-engine") if existing.role.is_some() => {}
                Some(r) => existing.role = Some(r.to_string()),
                None => {}
            }
            if existing.durable_identity_id.is_none() {
                existing.durable_identity_id = durable_identity_id.map(str::to_string);
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

        // Registration is identity/presence bookkeeping, not resource admission.
        // Any number of agents may join the bus; scarce work (builds, tests and
        // process resources) is bounded where it is actually consumed.

        self.agents.push(AgentEntry {
            agent_id: agent_id.clone(),
            agent_type: agent_type.to_string(),
            role: role.map(std::string::ToString::to_string),
            project_root: project_root.to_string(),
            started_at: Utc::now(),
            last_active: Utc::now(),
            pid,
            process_identity: Some(identity),
            durable_identity_id: durable_identity_id.map(str::to_string),
            status: AgentStatus::Active,
            status_message: None,
        });

        self.updated_at = Utc::now();
        crate::core::events::emit_agent_action(&agent_id, "register", None);
        Ok(agent_id)
    }

    /// Atomically registers this MCP process in the shared on-disk registry.
    pub(crate) fn register_mcp_process(
        project_root: &str,
        durable_identity_id: Option<&str>,
    ) -> Result<String, String> {
        Self::register_mcp_process_as(project_root, "context-engine", durable_identity_id)
    }

    /// Registers this MCP process with the role the session resolved at
    /// `initialize` (#1766), so the presence retry does not fall back to the
    /// `context-engine` placeholder.
    pub(crate) fn register_mcp_process_as(
        project_root: &str,
        role: &str,
        durable_identity_id: Option<&str>,
    ) -> Result<String, String> {
        mutate_persistent(|registry| {
            registry.cleanup_stale(presence_ttl());
            registry.register("mcp", Some(role), project_root, durable_identity_id)
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
        let index = self
            .agents
            .iter()
            .position(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| format!("agent presence '{agent_id}' was not found"))?;
        if self.agents[index].status == AgentStatus::Finished {
            return Err(format!(
                "agent presence '{agent_id}' is finished; register a new process presence"
            ));
        }
        self.require_active_for_presence(agent_id, "heartbeat")?;
        let agent = &mut self.agents[index];
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
        let index = self
            .agents
            .iter()
            .position(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| format!("agent presence '{agent_id}' was not found"))?;
        // Finished is the safe cleanup path: reapers and shutdown must be
        // able to release a presence after its durable identity is revoked.
        if status != AgentStatus::Finished {
            self.require_active_for_presence(agent_id, "status transition")?;
        }
        let agent = &mut self.agents[index];
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

    /// Gate operations for an explicitly bound presence. Legacy unbound
    /// entries retain their prior behavior until a migration enables a
    /// mandatory-binding mode.
    pub(crate) fn require_active_for_presence(
        &self,
        agent_id: &str,
        operation: &str,
    ) -> Result<(), String> {
        let Some(agent) = self.agents.iter().find(|agent| agent.agent_id == agent_id) else {
            return Ok(());
        };
        let Some(durable_id) = agent.durable_identity_id.as_deref() else {
            return Ok(());
        };
        if agent.status == AgentStatus::Finished {
            return Err(format!(
                "agent presence '{agent_id}' is finished; cannot {operation}"
            ));
        }
        crate::core::agent_registry::require_active(durable_id).map_err(|error| {
            format!("agent presence '{agent_id}' is not authorized for {operation}: {error}")
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
                        && self
                            .require_active_for_presence(&a.agent_id, "listing")
                            .is_ok()
                } else {
                    a.status != AgentStatus::Finished
                        && process_identity_matches(a, &compatibility_identities)
                        && self
                            .require_active_for_presence(&a.agent_id, "listing")
                            .is_ok()
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
    ) -> Result<String, String> {
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
    ) -> Result<String, String> {
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
    ) -> Result<String, String> {
        self.require_active_for_presence(from_agent, "posting a message")?;
        if let Some(target) = to_agent {
            self.require_active_for_presence(target, "receiving a message")?;
        }
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
        Ok(id)
    }

    pub(crate) fn read_messages(
        &mut self,
        agent_id: &str,
    ) -> Result<Vec<&ScratchpadEntry>, String> {
        self.require_active_for_presence(agent_id, "reading messages")?;
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

        Ok(self
            .scratchpad
            .iter()
            .filter(|e| {
                (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && e.from_agent != agent_id
                    && e.expires_at.is_none_or(|exp| exp > now)
            })
            .collect())
    }

    pub(crate) fn read_unread(&mut self, agent_id: &str) -> Result<Vec<&ScratchpadEntry>, String> {
        self.read_unread_for_project(agent_id, None)
    }

    pub(crate) fn read_unread_scoped(
        &mut self,
        agent_id: &str,
        project_root: &str,
    ) -> Result<Vec<&ScratchpadEntry>, String> {
        self.read_unread_for_project(agent_id, Some(project_root))
    }

    fn read_unread_for_project(
        &mut self,
        agent_id: &str,
        project_root: Option<&str>,
    ) -> Result<Vec<&ScratchpadEntry>, String> {
        self.require_active_for_presence(agent_id, "reading messages")?;
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

        Ok(unread_indices
            .into_iter()
            .map(|index| &self.scratchpad[index])
            .collect())
    }

    pub(crate) fn cleanup_stale(&mut self, max_age_hours: u64) {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::hours(max_age_hours as i64);
        let worker_cutoff = now - chrono::Duration::seconds(active_worker_lease_seconds());
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
                // A live MCP process is not proof that its worker is still
                // participating.  The worker lease is renewed by `read` or
                // `status=active`; expire quiet Active presences even when the
                // hosting daemon itself remains alive.  This prevents stale
                // sessions from occupying capacity or appearing as peers.
                if agent.status == AgentStatus::Active && agent.last_active < worker_cutoff {
                    agent.status = AgentStatus::Finished;
                    agent.status_message =
                        Some("worker lease expired; register again before continuing".to_string());
                }
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
#[path = "registry_tests.rs"]
mod tests;

#[cfg(test)]
mod logical_session_tests {
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
                None,
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
                None,
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
            let _ = registry.register_process("mcp", Some("context-engine"), "/project", None, 101);
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
            let _ = registry.register_process("mcp", Some("context-engine"), "/project", None, 101);
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
            .register_process("unknown", None, "/old", None, pid)
            .expect("current process has an identity");
        let second = registry
            .register_process("mcp", Some("context-engine"), "/new", None, pid)
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
                None,
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
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
#[path = "registry_presence_tests.rs"]
mod presence_tests;
