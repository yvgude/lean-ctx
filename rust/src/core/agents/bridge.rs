//! Unified identity and MCP-process presence view for the OCLA wire API.
//!
//! Durable identities intentionally do not claim process liveness. The
//! presence registry remains the sole source for PID-backed `alive` state.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UnifiedAgent {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durable_identity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_id: Option<String>,
    pub source: AgentSource,
    pub role: Option<String>,
    pub status: String,
    pub pid: Option<u32>,
    pub alive: bool,
    pub last_active: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentSource {
    Identity,
    Presence,
    Both,
}

/// Merge durable identities with live MCP-process presence.
pub(crate) fn list_unified() -> Result<Vec<UnifiedAgent>, String> {
    let mut result = Vec::new();
    let identities: BTreeMap<_, _> = crate::core::agent_registry::try_list()?
        .into_iter()
        .map(|record| (record.agent_id.clone(), record))
        .collect();

    for record in crate::core::agent_registry::try_list()? {
        let status = match record.status {
            crate::core::agent_registry::AgentStatus::Active => "active",
            crate::core::agent_registry::AgentStatus::Suspended => "suspended",
            crate::core::agent_registry::AgentStatus::Decommissioned => "decommissioned",
        };
        result.push(UnifiedAgent {
            agent_id: record.agent_id.clone(),
            durable_identity_id: Some(record.agent_id.clone()),
            presence_id: None,
            source: AgentSource::Identity,
            role: Some(record.role.clone()),
            status: status.to_string(),
            pid: None,
            alive: false,
            last_active: record.last_heartbeat.clone(),
            owner: Some(record.owner.clone()),
        });
    }

    if let Some(registry) = super::AgentRegistry::load() {
        let compatibility_identities = super::ProcessIdentityIndex::load();
        for agent in &registry.agents {
            let Some(durable) = agent
                .durable_identity_id
                .as_deref()
                .and_then(|id| identities.get(id))
            else {
                let alive = agent.durable_identity_id.is_none()
                    && agent
                        .process_identity
                        .as_ref()
                        .or_else(|| compatibility_identities.get(&agent.agent_id))
                        .is_some_and(|identity| {
                            crate::ipc::process::matches_identity(agent.pid, identity)
                        });
                result.push(UnifiedAgent {
                    agent_id: agent.agent_id.clone(),
                    durable_identity_id: agent.durable_identity_id.clone(),
                    presence_id: Some(agent.agent_id.clone()),
                    source: AgentSource::Presence,
                    role: agent.role.clone(),
                    status: if agent.durable_identity_id.is_some() {
                        "invalid".to_string()
                    } else if alive {
                        agent.status.to_string()
                    } else {
                        "stale".to_string()
                    },
                    pid: Some(agent.pid),
                    alive,
                    last_active: Some(agent.last_active.to_rfc3339()),
                    owner: None,
                });
                continue;
            };

            if let Some(existing) = result
                .iter_mut()
                .find(|item| item.durable_identity_id.as_deref() == Some(durable.agent_id.as_str()))
            {
                let durable_active =
                    durable.status == crate::core::agent_registry::AgentStatus::Active;
                let alive = durable_active
                    && agent.status != super::AgentStatus::Finished
                    && agent
                        .process_identity
                        .as_ref()
                        .or_else(|| compatibility_identities.get(&agent.agent_id))
                        .is_some_and(|identity| {
                            crate::ipc::process::matches_identity(agent.pid, identity)
                        });
                existing.source = AgentSource::Both;
                existing.presence_id = Some(agent.agent_id.clone());
                existing.pid = Some(agent.pid);
                existing.alive = alive;
                existing.last_active = Some(agent.last_active.to_rfc3339());
                continue;
            }

            let alive = agent
                .process_identity
                .as_ref()
                .or_else(|| compatibility_identities.get(&agent.agent_id))
                .is_some_and(|identity| crate::ipc::process::matches_identity(agent.pid, identity));
            result.push(UnifiedAgent {
                agent_id: agent.agent_id.clone(),
                durable_identity_id: None,
                presence_id: Some(agent.agent_id.clone()),
                source: AgentSource::Presence,
                role: agent.role.clone(),
                status: if alive {
                    agent.status.to_string()
                } else {
                    "stale".to_string()
                },
                pid: Some(agent.pid),
                alive,
                last_active: Some(agent.last_active.to_rfc3339()),
                owner: None,
            });
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{AgentSource, list_unified};
    use crate::core::agents::{AgentEntry, AgentRegistry, AgentStatus};

    #[test]
    fn list_unified_returns_empty_on_fresh_install() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let result = list_unified().expect("fresh registry");
        assert!(result.is_empty());
    }

    #[test]
    fn bridge_joins_only_on_explicit_durable_binding() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::agent_registry::register("durable-1", "coder", "owner")
            .expect("durable identity");
        let mut registry = AgentRegistry::new();
        let presence = registry
            .register("mcp", Some("context-engine"), "/project", Some("durable-1"))
            .expect("bound presence");
        registry.save().expect("presence registry");

        let rows = list_unified().expect("unified rows");
        let row = rows
            .iter()
            .find(|row| row.durable_identity_id.as_deref() == Some("durable-1"))
            .expect("durable row");
        assert!(matches!(row.source, AgentSource::Both));
        assert_eq!(row.presence_id.as_deref(), Some(presence.as_str()));
    }

    #[test]
    fn bridge_does_not_join_a_generated_presence_id_collision() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let collision = "mcp-123-collision";
        crate::core::agent_registry::register(collision, "coder", "owner")
            .expect("durable identity");
        let now = chrono::Utc::now();
        let pid = std::process::id();
        let mut registry = AgentRegistry::new();
        registry.agents.push(AgentEntry {
            agent_id: collision.to_string(),
            agent_type: "mcp".to_string(),
            role: Some("context-engine".to_string()),
            project_root: "/project".to_string(),
            started_at: now,
            last_active: now,
            pid,
            process_identity: crate::ipc::process::identity(pid),
            durable_identity_id: None,
            status: AgentStatus::Active,
            status_message: None,
        });
        registry.save().expect("presence registry");

        let rows = list_unified().expect("unified rows");
        assert_eq!(
            rows.iter().filter(|row| row.agent_id == collision).count(),
            2,
            "equal source IDs must remain distinct rows"
        );
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.source, AgentSource::Both))
        );
    }

    #[test]
    fn bridge_marks_missing_and_revoked_bindings_non_routable() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let now = chrono::Utc::now();
        let pid = std::process::id();
        let mut registry = AgentRegistry::new();
        registry.agents.push(AgentEntry {
            agent_id: "missing-presence".to_string(),
            agent_type: "mcp".to_string(),
            role: None,
            project_root: "/project".to_string(),
            started_at: now,
            last_active: now,
            pid,
            process_identity: crate::ipc::process::identity(pid),
            durable_identity_id: Some("missing-durable".to_string()),
            status: AgentStatus::Active,
            status_message: None,
        });
        registry.save().expect("presence registry");
        let missing = list_unified()
            .expect("unified rows")
            .into_iter()
            .find(|row| row.presence_id.as_deref() == Some("missing-presence"))
            .expect("invalid presence row");
        assert_eq!(missing.status, "invalid");
        assert!(!missing.alive);

        crate::core::agent_registry::register("revoked", "coder", "owner")
            .expect("durable identity");
        let mut revoked = AgentRegistry::new();
        revoked
            .register("mcp", Some("context-engine"), "/project", Some("revoked"))
            .expect("presence may exist before revocation");
        revoked.save().expect("presence registry");
        crate::core::agent_registry::suspend("revoked", "review").expect("suspend identity");
        let row = list_unified()
            .expect("unified rows")
            .into_iter()
            .find(|row| row.durable_identity_id.as_deref() == Some("revoked"))
            .expect("revoked durable row");
        assert_eq!(row.status, "suspended");
        assert!(!row.alive);
    }
}
