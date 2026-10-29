// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent list` as workers (SPEC-ENGINE §3.6, V5.3): active sessions
//! are grouped by the worker they attach to; process ids are no longer
//! shown as agents, sessions only as a detail line.

use chrono::{DateTime, Utc};

use super::attach::{LocalWorker, attach_in, detect_host_app, host_display};
use super::normalize_role;
use crate::core::agents::{AgentEntry, AgentStatus};

struct Row<'a> {
    host_app: String,
    role: String,
    worker_id: Option<String>,
    sessions: Vec<&'a AgentEntry>,
}

/// Render the roster. `instance` is the enrolled instance id and its known
/// workers; without one, workers have no `wkr_` id yet ("not on Hive").
pub(crate) fn render(
    agents: &[&AgentEntry],
    instance: Option<(&str, &[LocalWorker])>,
    now: DateTime<Utc>,
) -> String {
    let mut book: Vec<LocalWorker> = instance.map(|(_, w)| w.to_vec()).unwrap_or_default();
    let mut rows: Vec<Row<'_>> = Vec::new();
    for agent in agents {
        let transport = agent.transport.as_ref();
        let host_app = detect_host_app(
            transport.map(|t| t.host.as_str()),
            Some(agent.agent_type.as_str()),
            std::iter::empty::<(&str, &str)>(),
            transport.and_then(|t| t.name.as_deref()),
        );
        let mut role = normalize_role(agent.role.as_deref().unwrap_or_default());
        let mut worker_id = None;
        if let Some((instance_id, _)) = instance {
            let attached = attach_in(&mut book, instance_id, &host_app, &role);
            role = attached.worker.role;
            worker_id = Some(attached.worker.worker_id);
        }
        match rows
            .iter_mut()
            .find(|r| r.host_app == host_app && r.role == role)
        {
            Some(row) => row.sessions.push(agent),
            None => rows.push(Row {
                host_app,
                role,
                worker_id,
                sessions: vec![agent],
            }),
        }
    }
    rows.sort_by(|a, b| (&a.host_app, &a.role).cmp(&(&b.host_app, &b.role)));
    let mut out = format!("Active workers ({}):\n", rows.len());
    for row in &rows {
        let state = if row.sessions.iter().any(|s| s.status == AgentStatus::Active) {
            "active"
        } else {
            "idle"
        };
        let last = row
            .sessions
            .iter()
            .map(|s| s.last_active)
            .max()
            .map_or(0, |at| (now - at).num_minutes().max(0));
        let n = row.sessions.len();
        out.push_str(&format!(
            "  {} {} · {} state={state} ({n} session{}, last active {last}m ago)\n",
            row.worker_id.as_deref().unwrap_or("(not on Hive)"),
            host_display(&row.host_app),
            row.role,
            if n == 1 { "" } else { "s" },
        ));
        for session in &row.sessions {
            let status = session
                .status_message
                .as_deref()
                .map(|m| format!(" — {m}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "      session {} status={}{status}\n",
                session.agent_id, session.status
            ));
        }
    }
    out
}
