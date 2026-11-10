// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx presence list --json` (agent-bus/1): the local agents on the
//! bus — every MCP presence and worker the registry knows, with whether its
//! process still lives — and the host sessions hooks reported open. Read
//! only; the same liveness rule as `lean-ctx agent presence`.

use serde::Serialize;

use lean_ctx::core::agents::{AgentEntry, AgentRegistry, ProcessIdentityIndex};

use super::bus::BUS_VERSION;
use super::{Failure, rfc3339};

#[derive(Debug, Serialize)]
pub(crate) struct PresenceList {
    pub bus: &'static str,
    pub agents: Vec<AgentRow>,
    pub logical_sessions: Vec<LogicalSessionRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AgentRow {
    pub agent_id: String,
    pub agent_type: String,
    pub role: Option<String>,
    pub project_root: String,
    pub status: &'static str,
    pub status_message: Option<String>,
    pub started_at: String,
    pub last_active: String,
    pub alive: bool,
    pub pid: u32,
    pub transport: Option<TransportRow>,
    pub parent: Option<String>,
    pub app_session_id: Option<String>,
    /// Always `agent` here (a person posts, it holds no presence).
    pub kind: &'static str,
    /// The launching app's name of the session (`LEAN_CTX_SESSION_NAME`),
    /// else `null`.
    pub session_name: Option<String>,
    /// What a reader shows: the session name, else `null` (the reader
    /// falls back to the role, then the id; nothing is invented).
    pub display_name: Option<String>,
    /// The agent host (`claude-code`, `codex`, …) from the MCP handshake,
    /// else `null`.
    pub host: Option<String>,
    /// The Instance (machine) the presence runs on.
    pub instance: InstanceRow,
    pub project: ProjectRow,
}

/// The machine of a presence: the Hive Instance id when this machine is
/// enrolled, and its name (the enrollment label, else the hostname).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct InstanceRow {
    pub id: Option<String>,
    pub name: Option<String>,
}

/// The project of a presence and the Hive vault linked to it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProjectRow {
    pub root: Option<String>,
    /// `ns/name` of the linked vault, else `null`.
    pub vault: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TransportRow {
    pub host: String,
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LogicalSessionRow {
    pub source: String,
    pub workspace: String,
    pub session_id: String,
    pub opened_at: String,
    pub last_heartbeat: String,
}

/// `agent` as a row, given its liveness.
pub(crate) fn agent_row(agent: &AgentEntry, alive: bool, status: &'static str) -> AgentRow {
    AgentRow {
        agent_id: agent.agent_id.clone(),
        agent_type: agent.agent_type.clone(),
        role: agent.role.clone(),
        project_root: agent.project_root.clone(),
        status,
        status_message: agent.status_message.clone(),
        started_at: rfc3339(agent.started_at),
        last_active: rfc3339(agent.last_active),
        alive,
        pid: agent.pid,
        transport: agent.transport.as_ref().map(|t| TransportRow {
            host: t.host.clone(),
            name: t.name.clone(),
            version: t.version.clone(),
        }),
        parent: agent.parent.clone(),
        app_session_id: agent.app_session_id.clone(),
        kind: "agent",
        session_name: agent.display_name.clone(),
        display_name: agent.display_name.clone(),
        host: agent
            .transport
            .as_ref()
            .map(|t| t.host.clone())
            .filter(|h| h != "unknown"),
        instance: InstanceRow {
            id: None,
            name: None,
        },
        project: ProjectRow {
            root: (!agent.project_root.is_empty()).then(|| agent.project_root.clone()),
            vault: None,
        },
    }
}

/// This machine as an [`InstanceRow`]: the enrolled Instance's id and
/// label, else the hostname.
fn this_instance(store: Option<&lean_ctx_hive::HiveStore>) -> InstanceRow {
    let record = store.and_then(|s| s.instance().ok().flatten());
    let hostname = gethostname::gethostname().into_string().ok();
    InstanceRow {
        id: record.as_ref().map(|r| r.instance_id.clone()),
        name: record
            .map(|r| r.label)
            .filter(|label| !label.trim().is_empty())
            .or(hostname),
    }
}

/// `rows` with the machine and each project's linked vault filled in.
pub(crate) fn place(
    rows: &mut [AgentRow],
    instance: &InstanceRow,
    vault_of: impl Fn(&str) -> Option<String>,
) {
    for row in rows {
        row.instance = instance.clone();
        row.project.vault = row.project.root.as_deref().and_then(&vault_of);
    }
}

/// The agents of `registry` in `project_root` (and those without one; every
/// project with `all_projects`), live ones only unless `include_finished`,
/// most recently active first (then by id).
pub(crate) fn rows(
    registry: &AgentRegistry,
    project_root: &str,
    all_projects: bool,
    include_finished: bool,
    liveness: impl Fn(&AgentEntry) -> (bool, &'static str),
) -> Vec<AgentRow> {
    let mut out: Vec<(chrono::DateTime<chrono::Utc>, AgentRow)> = registry
        .agents
        .iter()
        .filter(|a| {
            all_projects
                || a.project_root.is_empty()
                || lean_ctx::core::pathutil::same_project(&a.project_root, project_root)
        })
        .filter_map(|a| {
            let (alive, status) = liveness(a);
            (include_finished || status != "finished")
                .then(|| (a.last_active, agent_row(a, alive, status)))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.agent_id.cmp(&b.1.agent_id)));
    out.into_iter().map(|(_, row)| row).collect()
}

/// `presence list --json [--project-root P] [--all] [--all-projects]`.
pub(crate) fn build(args: &[String]) -> Result<PresenceList, Failure> {
    let root = super::super::common::detect_project_root(args);
    let include_finished = args.iter().any(|a| a == "--all");
    let all_projects = args.iter().any(|a| a == "--all-projects");
    let registry = AgentRegistry::load().unwrap_or_default();
    let identities = ProcessIdentityIndex::load();
    let now = chrono::Utc::now();
    let mut agents = rows(&registry, &root, all_projects, include_finished, |a| {
        super::super::agent_cmd::agent_liveness(a, now, &identities)
    });
    let store = lean_ctx_hive::HiveStore::open().ok();
    let vaults = store
        .as_ref()
        .map(|s| lean_ctx_hive::vault::VaultMap::load(s.data_dir()));
    place(&mut agents, &this_instance(store.as_ref()), |root| {
        vaults
            .as_ref()?
            .for_root(root)
            .map(|record| format!("{}/{}", record.ns, record.name))
    });
    let mut logical_sessions = logical_rows(&registry, &root, all_projects);
    logical_sessions.sort_by(|a, b| {
        b.last_heartbeat
            .cmp(&a.last_heartbeat)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(PresenceList {
        bus: BUS_VERSION,
        agents,
        logical_sessions,
    })
}

/// The host sessions of `registry` open in `project_root` (every project
/// with `all_projects`): another project's sessions are never listed.
pub(crate) fn logical_rows(
    registry: &AgentRegistry,
    project_root: &str,
    all_projects: bool,
) -> Vec<LogicalSessionRow> {
    registry
        .logical_sessions
        .iter()
        .filter(|s| {
            all_projects || lean_ctx::core::pathutil::same_project(&s.workspace, project_root)
        })
        .map(|s| LogicalSessionRow {
            source: s.source.clone(),
            workspace: s.workspace.clone(),
            session_id: s.session_id.clone(),
            opened_at: rfc3339(s.opened_at),
            last_heartbeat: rfc3339(s.last_heartbeat),
        })
        .collect()
}

/// `presence list --json …`.
pub(crate) fn run(args: &[String]) {
    let action = args.first().map_or("", String::as_str);
    if matches!(action, "" | "--help" | "-h" | "help") {
        println!(
            "Usage: lean-ctx presence list --json [--project-root P] [--all] [--all-projects]\n\n\
             Local agents on the bus with liveness (Local Read v1, presence.list)."
        );
        return;
    }
    let rest = args.get(1..).unwrap_or(&[]);
    if action != "list" || !super::wants_json(rest) {
        super::fail(&Failure::invalid("usage: lean-ctx presence list --json"));
    }
    super::kinds::run("presence.list", rest);
}
