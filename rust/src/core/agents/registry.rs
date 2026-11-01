use chrono::Utc;
use std::collections::HashSet;
use std::path::Path;

#[cfg(test)]
use super::diary::{AgentDiary, DiaryEntryType};
use super::persistence::{
    FileLock, ProcessIdentityIndex, agents_dir, create_agents_dir, generate_short_id,
    load_registry_file, mutate_persistent, save_registry_file,
};
use super::{
    AgentEntry, AgentRegistry, AgentStatus, LogicalSessionPresence, REQUEST_DIGEST_METADATA_KEY,
    REQUEST_ID_METADATA_KEY, RoleSource, ScratchpadEntry, TransportClient,
};
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

/// Role an MCP transport holds until `initialize` resolves the session role.
const PLACEHOLDER_ROLE: &str = "context-engine";
const WORKER_LEASE_EXPIRED: &str = "worker lease expired; register again before continuing";
/// A live MCP connection without a recent tool call is idle, not gone: the
/// worker lease governs collaboration slots, never transport presence.
const TRANSPORT_IDLE: &str = "connected; no tool call within the worker lease";

/// Registration metadata beyond the legacy `register` arguments.
#[derive(Default)]
struct RegistrationMeta {
    role_source: Option<RoleSource>,
    transport: Option<TransportClient>,
}

/// Explicit callers (`ctx_agent register`) declare their role; only the
/// transport placeholder is assumed.
fn explicit_role_source(role: Option<&str>) -> Option<RoleSource> {
    match role {
        None => None,
        Some(PLACEHOLDER_ROLE) => Some(RoleSource::Assumed),
        Some(_) => Some(RoleSource::Declared),
    }
}

/// Agents of this process whose expired registration a heartbeat renewed,
/// not yet reported by `ctx_agent read`/`wait`.
static RENEWED: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn note_renewed(agent_id: &str) {
    if let Ok(mut renewed) = RENEWED.lock() {
        renewed.insert(agent_id.to_string());
    }
    crate::core::agents::friction::record_for(
        crate::core::agents::friction::REGISTRATION_RENEWED,
        Some(agent_id),
        None,
    );
}

/// Whether `agent_id`'s registration had expired and was renewed since the
/// last call (each renewal is reported once).
pub(crate) fn take_renewed(agent_id: &str) -> bool {
    RENEWED
        .lock()
        .is_ok_and(|mut renewed| renewed.remove(agent_id))
}

/// How often `wait` looks at the registry file's modification time.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(500);
/// How often `wait` keeps the waiting agent's lease alive.
const WAIT_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);
/// A registry whose modification time is this recent may be replaced again
/// within the same timestamp tick (coarse kernel clocks, 1–2 s on HFS+/FAT),
/// so an unchanged mtime does not yet prove an unchanged file.
const WAIT_RACY_MTIME: std::time::Duration = std::time::Duration::from_secs(3);

/// How far before its registration an agent still receives broadcasts: the
/// seconds a reconnect leaves between the old session's last read and the new
/// one, with room to spare.
const BROADCAST_GRACE: chrono::Duration = chrono::Duration::minutes(10);

/// A message `agent_id` has not read: addressed to it, or a broadcast posted
/// after `floor` (the agent's registration less [`BROADCAST_GRACE`]), not its
/// own, in `project_root` (any when `None`), not expired. Without the floor a
/// reconnected session (a new id) got hours of old broadcasts as unread
/// (25.10.: 123 after the MCP reconnect).
fn unread_for(
    e: &ScratchpadEntry,
    agent_id: &str,
    project_root: Option<&str>,
    now: chrono::DateTime<Utc>,
    floor: Option<chrono::DateTime<Utc>>,
) -> bool {
    !e.read_by.iter().any(|reader| reader == agent_id)
        && e.from_agent != agent_id
        && (e.to_agent.as_deref() == Some(agent_id)
            || (e.to_agent.is_none() && floor.is_none_or(|floor| e.timestamp >= floor)))
        && project_root.is_none_or(|root| {
            e.project_root
                .as_deref()
                .is_some_and(|own| crate::core::pathutil::same_project(own, root))
        })
        && e.expires_at.is_none_or(|exp| exp > now)
}

/// True when `agent` is owned by this exact process (PID and start marker).
fn owned_by_current_process(agent: &AgentEntry) -> bool {
    let pid = std::process::id();
    agent.pid == pid
        && agent.process_identity.is_some()
        && crate::ipc::process::identity(pid).as_ref() == agent.process_identity.as_ref()
}

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
        self.register_process_with(
            agent_type,
            role,
            project_root,
            durable_identity_id,
            pid,
            RegistrationMeta {
                role_source: explicit_role_source(role),
                transport: None,
            },
        )
    }

    /// Registers (or refreshes) `role` as its own presence next to
    /// `parent_id`, which this process holds: a subagent shares its parent's
    /// MCP connection, and under the parent's id its reads consumed the
    /// messages directed to the parent (WEB-2 read the lead's, 25.10.).
    pub(crate) fn register_child(
        &mut self,
        agent_type: &str,
        role: &str,
        project_root: &str,
        parent_id: &str,
    ) -> Result<String, String> {
        let pid = std::process::id();
        let identity = crate::ipc::process::identity(pid)
            .ok_or_else(|| format!("cannot establish immutable process identity for PID {pid}"))?;
        if !self.held_by_this_process(parent_id) {
            return Err(format!(
                "agent {parent_id} is not registered by this process"
            ));
        }
        let now = Utc::now();
        if let Some(child) = self.agents.iter_mut().find(|a| {
            a.parent.as_deref() == Some(parent_id)
                && a.role.as_deref() == Some(role)
                && a.status != AgentStatus::Finished
        }) {
            child.last_active = now;
            child.status = AgentStatus::Active;
            child.project_root = project_root.to_string();
            return Ok(child.agent_id.clone());
        }
        let agent_id = format!("{}-{}-{}", agent_type, pid, generate_short_id());
        self.agents.push(AgentEntry {
            agent_id: agent_id.clone(),
            agent_type: agent_type.to_string(),
            role: Some(role.to_string()),
            project_root: project_root.to_string(),
            started_at: now,
            last_active: now,
            pid,
            process_identity: Some(identity),
            durable_identity_id: None,
            status: AgentStatus::Active,
            status_message: None,
            transport: None,
            role_source: Some(RoleSource::Declared),
            parent: Some(parent_id.to_string()),
        });
        self.updated_at = now;
        crate::core::events::emit_agent_action(&agent_id, "register", None);
        Ok(agent_id)
    }

    /// The oldest broadcast `agent_id` still receives: its registration less
    /// [`BROADCAST_GRACE`]; `None` (all) for an id the registry doesn't know.
    fn broadcast_floor(&self, agent_id: &str) -> Option<chrono::DateTime<Utc>> {
        self.agents
            .iter()
            .find(|a| a.agent_id == agent_id)
            .map(|a| a.started_at - BROADCAST_GRACE)
    }

    /// Whether `agent_id` is a live presence this very process registered
    /// (its own or a child): only those may be named as `agent_id` on a call.
    pub(crate) fn held_by_this_process(&self, agent_id: &str) -> bool {
        let pid = std::process::id();
        let identity = crate::ipc::process::identity(pid);
        self.agents.iter().any(|a| {
            a.agent_id == agent_id
                && a.pid == pid
                && a.status != AgentStatus::Finished
                && a.process_identity.is_some()
                && a.process_identity == identity
        })
    }

    /// The declared role of `agent_id`, if it has one.
    pub(crate) fn declared_role(&self, agent_id: &str) -> Option<String> {
        self.agents
            .iter()
            .find(|a| a.agent_id == agent_id && a.role_source == Some(RoleSource::Declared))
            .and_then(|a| a.role.clone())
    }

    /// Registers (or refreshes) the presence of this process's own MCP
    /// connection, attributed to its host and with the role's provenance.
    pub(crate) fn register_transport(
        &mut self,
        role: &str,
        role_source: RoleSource,
        project_root: &str,
        durable_identity_id: Option<&str>,
        transport: TransportClient,
    ) -> Result<String, String> {
        self.register_process_with(
            "mcp",
            Some(role),
            project_root,
            durable_identity_id,
            std::process::id(),
            RegistrationMeta {
                role_source: Some(role_source),
                transport: Some(transport),
            },
        )
    }

    fn register_process_with(
        &mut self,
        agent_type: &str,
        role: Option<&str>,
        project_root: &str,
        durable_identity_id: Option<&str>,
        pid: u32,
        meta: RegistrationMeta,
    ) -> Result<String, String> {
        if let Some(identity_id) = durable_identity_id {
            crate::core::agent_registry::require_active(identity_id)?;
        }
        let identity = crate::ipc::process::identity(pid)
            .ok_or_else(|| format!("cannot establish immutable process identity for PID {pid}"))?;
        let agent_id = format!("{}-{}-{}", agent_type, pid, generate_short_id());

        if let Some(existing_index) = self.agents.iter().position(|a| {
            a.pid == pid
                && a.parent.is_none()
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
            if existing.status_message.as_deref() == Some(TRANSPORT_IDLE) {
                existing.status_message = None;
            }
            // A transport refresh keeps the type an agent declared through
            // `ctx_agent register` on the same process.
            if meta.transport.is_none() || existing.agent_type == "mcp" {
                existing.agent_type = agent_type.to_string();
            }
            existing.project_root = project_root.to_string();
            // #1766: the presence retry registers as the `context-engine`
            // placeholder. It never replaces a role the process already holds
            // (a `reviewer` silently became a context engine); an explicit role
            // still replaces the placeholder — the `initialize` upgrade. An
            // assumed role (heuristic/default) never replaces a declared one.
            match role {
                Some(PLACEHOLDER_ROLE) if existing.role.is_some() => {}
                Some(_)
                    if meta.role_source == Some(RoleSource::Assumed)
                        && existing.role_source == Some(RoleSource::Declared) => {}
                Some(r) => {
                    existing.role = Some(r.to_string());
                    existing.role_source = meta.role_source;
                }
                None => {}
            }
            // Never downgrade a host the handshake identified to `unknown`.
            if let Some(transport) = meta.transport
                && (transport.is_identified() || existing.transport.is_none())
            {
                existing.transport = Some(transport);
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
            transport: meta.transport,
            role_source: meta.role_source,
            parent: None,
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
        Self::register_mcp_process_as(
            project_root,
            PLACEHOLDER_ROLE,
            RoleSource::Assumed,
            durable_identity_id,
        )
    }

    /// Registers this MCP process with the role the session resolved at
    /// `initialize` (#1766), so the presence retry does not fall back to the
    /// `context-engine` placeholder. The host comes from this process's
    /// recorded MCP handshake.
    pub(crate) fn register_mcp_process_as(
        project_root: &str,
        role: &str,
        role_source: RoleSource,
        durable_identity_id: Option<&str>,
    ) -> Result<String, String> {
        let transport = TransportClient::current();
        mutate_persistent(|registry| {
            registry.cleanup_stale(presence_ttl());
            registry.register_transport(
                role,
                role_source,
                project_root,
                durable_identity_id,
                transport,
            )
        })
        .and_then(|result| result)
    }

    /// Attributes this process's live transport presences to the host from
    /// its MCP handshake. Covers sessions whose `initialize` registers no
    /// project-scoped presence (no resolvable root).
    pub(crate) fn record_transport_client_persistent(
        transport: &TransportClient,
    ) -> Result<usize, String> {
        mutate_persistent(|registry| registry.record_transport_client(transport))
    }

    pub(crate) fn record_transport_client(&mut self, transport: &TransportClient) -> usize {
        let mut updated = 0;
        for agent in self.agents.iter_mut().filter(|agent| {
            agent.transport.is_some()
                && agent.status != AgentStatus::Finished
                && owned_by_current_process(agent)
        }) {
            agent.transport = Some(transport.clone());
            updated += 1;
        }
        if updated > 0 {
            self.updated_at = Utc::now();
        }
        updated
    }

    /// Whether `agent_id` is still a listed presence owned by this process.
    /// Read-only: used by the MCP server to notice a presence that another
    /// process retired (for example an older binary's lease expiry) or pruned.
    pub(crate) fn transport_presence_is_live(agent_id: &str) -> bool {
        Self::load().is_some_and(|registry| {
            registry.agents.iter().any(|agent| {
                agent.agent_id == agent_id
                    && agent.status != AgentStatus::Finished
                    && owned_by_current_process(agent)
            })
        })
    }

    /// Whether any LeanCTX MCP server runs on this machine: a transport
    /// presence that is not finished and whose recorded process is still the
    /// one that registered it. Every MCP connection registers one, the stdio
    /// server a host starts as well as the daemon's sessions. Read-only; the
    /// deny hook uses it to tell "no daemon" from "no MCP server at all".
    pub(crate) fn any_live_mcp_transport() -> bool {
        let Some(registry) = Self::load() else {
            return false;
        };
        let identities = ProcessIdentityIndex::load();
        registry.agents.iter().any(|agent| {
            agent.transport.is_some()
                && agent.status != AgentStatus::Finished
                && process_identity_matches(agent, &identities)
        })
    }

    /// Atomically refreshes a registered MCP process heartbeat. The caller is
    /// the transport itself, so a presence that only lost its worker lease is
    /// revived rather than rejected (an older binary in another process still
    /// finishes quiet transports).
    pub(crate) fn heartbeat_persistent(agent_id: &str) -> Result<(), String> {
        mutate_persistent(|registry| registry.update_transport_heartbeat(agent_id))?
    }

    pub(crate) fn update_transport_heartbeat(&mut self, agent_id: &str) -> Result<(), String> {
        if self.revive_lease_expired(agent_id) {
            note_renewed(agent_id);
        }
        self.update_heartbeat(agent_id)
    }

    /// Revive `agent_id` when only its worker lease ran out: an unbound
    /// presence of this very process that `cleanup_stale` finished for
    /// being quiet. `true` when it was revived.
    fn revive_lease_expired(&mut self, agent_id: &str) -> bool {
        let Some(agent) = self
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == agent_id)
        else {
            return false;
        };
        if agent.status == AgentStatus::Finished
            && agent.status_message.as_deref() == Some(WORKER_LEASE_EXPIRED)
            && agent.durable_identity_id.is_none()
            && owned_by_current_process(agent)
        {
            agent.status = AgentStatus::Active;
            agent.status_message = None;
            return true;
        }
        false
    }

    /// The heartbeat of an agent this process registered (`ctx_agent
    /// register`), sent on every tool call of its session: renews its worker
    /// lease, and revives it when the lease had already run out. `Ok(true)`
    /// when it was revived; [`take_renewed`] then says so once.
    pub(crate) fn renew_worker(&mut self, agent_id: &str) -> Result<bool, String> {
        let revived = self.revive_lease_expired(agent_id);
        self.update_heartbeat(agent_id)?;
        if revived {
            note_renewed(agent_id);
            crate::core::hive::bus::presence::request_announce();
        }
        Ok(revived)
    }

    /// [`Self::renew_worker`] under the registry lock.
    pub(crate) fn renew_worker_persistent(agent_id: &str) -> Result<bool, String> {
        mutate_persistent(|registry| registry.renew_worker(agent_id))?
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
        if agent.status_message.as_deref() == Some(TRANSPORT_IDLE) {
            agent.status_message = None;
        }
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
                    crate::core::pathutil::same_project(&a.project_root, root)
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
        self.post_message_keyed(
            project_root,
            from_agent,
            to_agent,
            category,
            message,
            privacy,
            priority,
            ttl_hours,
            None,
        )
    }

    /// [`Self::post_message_scoped`] with an optional idempotency key
    /// (AP07-G9). A key is scoped to the sender and project: a retry with the
    /// same key and the same request returns the id of the message it first
    /// posted and writes nothing; the same key with another request is
    /// refused. A key lives as long as its message — the scratchpad's entry
    /// cap and TTL bound both — and an expired message frees its key.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn post_message_keyed(
        &mut self,
        project_root: Option<&str>,
        from_agent: &str,
        to_agent: Option<&str>,
        category: &str,
        message: &str,
        privacy: PrivacyLevel,
        priority: MessagePriority,
        ttl_hours: Option<u64>,
        request_id: Option<&str>,
    ) -> Result<String, String> {
        self.require_active_for_presence(from_agent, "posting a message")?;
        if let Some(target) = to_agent {
            self.require_active_for_presence(target, "receiving a message")?;
        }
        let request = match request_id {
            Some(key) => {
                super::check_request_id(key)?;
                let digest = super::request_digest(
                    "leanctx/agent-post/v1",
                    &serde_json::json!([
                        project_root,
                        to_agent,
                        category,
                        message,
                        privacy,
                        priority,
                        ttl_hours
                    ]),
                );
                let now = Utc::now();
                if let Some(entry) = self.scratchpad.iter().find(|entry| {
                    entry.from_agent == from_agent
                        && entry.project_root.as_deref() == project_root
                        && entry.expires_at.is_none_or(|expires| expires > now)
                        && entry
                            .metadata
                            .get(REQUEST_ID_METADATA_KEY)
                            .map(String::as_str)
                            == Some(key)
                }) {
                    return if entry.metadata.get(REQUEST_DIGEST_METADATA_KEY) == Some(&digest) {
                        Ok(entry.id.clone())
                    } else {
                        Err(super::request_id_conflict())
                    };
                }
                Some((key, digest))
            }
            None => None,
        };
        // Admission precedes persistence: only admitted text is stored, with
        // the binding a later read re-authorizes. A withheld message is refused
        // with a typed, content-free reason and nothing is written.
        let (message, mut metadata) =
            ScratchpadEntry::admit_message(from_agent, project_root, message)?;
        if let Some((key, digest)) = request {
            metadata.insert(REQUEST_ID_METADATA_KEY.to_owned(), key.to_owned());
            metadata.insert(REQUEST_DIGEST_METADATA_KEY.to_owned(), digest);
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
            message,
            metadata,
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

    #[cfg(test)]
    pub(crate) fn read_unread(&mut self, agent_id: &str) -> Result<Vec<&ScratchpadEntry>, String> {
        self.read_unread_for_project(agent_id, None)
    }

    #[cfg(test)]
    pub(crate) fn read_unread_scoped(
        &mut self,
        agent_id: &str,
        project_root: &str,
    ) -> Result<Vec<&ScratchpadEntry>, String> {
        self.read_unread_for_project(agent_id, Some(project_root))
    }

    /// The unread messages of `agent_id` in `project_root`, oldest first, as
    /// `render` shows them, up to `budget` bytes (at least one); only those
    /// are marked read. Returns them and how many stay unread for the next
    /// read: a host that cuts a long answer used to lose the rest, already
    /// marked read (WEB saw 14 of 55, 25.10.).
    pub(crate) fn read_unread_page(
        &mut self,
        agent_id: &str,
        project_root: &str,
        budget: usize,
        render: impl Fn(&ScratchpadEntry) -> String,
    ) -> Result<(Vec<String>, usize), String> {
        self.require_active_for_presence(agent_id, "reading messages")?;
        let now = Utc::now();
        let floor = self.broadcast_floor(agent_id);
        let unread: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| unread_for(e, agent_id, Some(project_root), now, floor))
            .map(|(i, _)| i)
            .collect();
        let (mut shown, mut used) = (Vec::new(), 0_usize);
        for &index in &unread {
            let line = render(&self.scratchpad[index]);
            if !shown.is_empty() && used + line.len() > budget {
                break;
            }
            used += line.len();
            shown.push(line);
            self.scratchpad[index].read_by.push(agent_id.to_string());
        }
        if !shown.is_empty() {
            self.updated_at = now;
        }
        let left = unread.len() - shown.len();
        Ok((shown, left))
    }

    /// Whether `agent_id` has a message waiting in `project_root` (what
    /// [`Self::read_unread_scoped`] would return), without marking it read.
    pub(crate) fn has_unread_scoped(&self, agent_id: &str, project_root: &str) -> bool {
        let now = Utc::now();
        let floor = self.broadcast_floor(agent_id);
        self.scratchpad
            .iter()
            .any(|e| unread_for(e, agent_id, Some(project_root), now, floor))
    }

    /// Block until `agent_id` has a message waiting in `project_root`
    /// (`true`) or `timeout` passed (`false`). Looks at the registry only when
    /// its file changed (polled every [`WAIT_POLL`]); `on_poll` runs about every
    /// 30 s meanwhile (the caller keeps the agent's lease alive there).
    pub(crate) fn wait_for_unread(
        agent_id: &str,
        project_root: &str,
        timeout: std::time::Duration,
        mut on_poll: impl FnMut(),
    ) -> bool {
        let path = agents_dir().ok().map(|dir| dir.join("registry.json"));
        let modified = || {
            path.as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
        };
        let deadline = std::time::Instant::now() + timeout;
        let mut last_poll = std::time::Instant::now();
        let mut seen = None;
        loop {
            let now_modified = modified();
            if seen.is_none() || now_modified != seen.flatten() {
                seen = Some(now_modified);
                if Self::load().is_some_and(|r| r.has_unread_scoped(agent_id, project_root)) {
                    return true;
                }
                // A post replacing the file in the same timestamp tick as the
                // version just read leaves the mtime unchanged; without this
                // the wait slept through it until the next keepalive (30 s).
                let racy = now_modified.is_some_and(|mtime| {
                    !std::time::SystemTime::now()
                        .duration_since(mtime)
                        .is_ok_and(|age| age >= WAIT_RACY_MTIME)
                });
                if racy {
                    seen = None;
                }
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            if now.duration_since(last_poll) >= WAIT_KEEPALIVE {
                on_poll();
                last_poll = now;
                // Look again even without a visible change (coarse mtimes).
                seen = None;
            }
            std::thread::sleep(WAIT_POLL.min(deadline - now));
        }
    }

    #[cfg(test)]
    fn read_unread_for_project(
        &mut self,
        agent_id: &str,
        project_root: Option<&str>,
    ) -> Result<Vec<&ScratchpadEntry>, String> {
        self.require_active_for_presence(agent_id, "reading messages")?;
        let now = Utc::now();
        let floor = self.broadcast_floor(agent_id);
        let unread_indices: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| unread_for(e, agent_id, project_root, now, floor))
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
                // The MCP server's own transport presence is the exception:
                // its process is the connection, so it idles instead of
                // vanishing while the editor keeps it open.
                if agent.status == AgentStatus::Active && agent.last_active < worker_cutoff {
                    if agent.transport.is_some() {
                        agent.status = AgentStatus::Idle;
                        agent.status_message = Some(TRANSPORT_IDLE.to_string());
                    } else {
                        agent.status = AgentStatus::Finished;
                        agent.status_message = Some(WORKER_LEASE_EXPIRED.to_string());
                    }
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

    #[cfg(test)]
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
#[path = "registry_logical_session_tests.rs"]
mod logical_session_tests;
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
#[path = "registry_presence_tests.rs"]
mod presence_tests;
