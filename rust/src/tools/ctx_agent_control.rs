// SPDX-License-Identifier: Apache-2.0
//! `ctx_agent` lease targets and the `control` snapshot.

use super::presence_ttl_hours;
use crate::core::agents::AgentRegistry;

/// Convert a raw path/symbol target into a project-bound lease resource. A
/// path is relative to the project root or absolute inside it; it names the
/// same resource as a Work Graph path claim of that file.
pub(super) fn lease_resource(
    project_root: &str,
    target: &str,
) -> Result<(crate::core::agent_lease::AgentLeaseResourceKindV1, String), String> {
    use crate::core::agent_lease::{self, AgentLeaseResourceKindV1};
    if let Some(symbol) = target.strip_prefix("symbol:") {
        return Ok((
            AgentLeaseResourceKindV1::Symbol,
            agent_lease::project_symbol_resource(project_root, symbol),
        ));
    }
    agent_lease::project_path_target_resource(project_root, target)
        .map(|resource| (AgentLeaseResourceKindV1::Path, resource))
        .map_err(|error| error.to_string())
}

pub(super) fn control_status(project_root: &str, current_agent_id: Option<&str>) -> String {
    let registry = AgentRegistry::mutate_locked(|registry| {
        registry.cleanup_stale(presence_ttl_hours());
    })
    .map(|(registry, ())| registry)
    .unwrap_or_else(|_| AgentRegistry::load_or_create());

    let scoped_agents = registry
        .agents
        .iter()
        .filter(|agent| crate::core::pathutil::same_project(&agent.project_root, project_root));
    let mut presence = std::collections::BTreeMap::from([
        ("active", 0usize),
        ("idle", 0usize),
        ("finished", 0usize),
    ]);
    for agent in scoped_agents {
        let key = agent.status.to_string();
        if let Some(count) = presence.get_mut(key.as_str()) {
            *count += 1;
        }
    }

    let pending_messages = current_agent_id.map_or(0, |agent_id| {
        registry
            .scratchpad
            .iter()
            .filter(|entry| {
                !entry.read_by.iter().any(|reader| reader == agent_id)
                    && entry.from_agent != agent_id
                    && (entry.to_agent.is_none() || entry.to_agent.as_deref() == Some(agent_id))
                    && entry
                        .project_root
                        .as_deref()
                        .is_some_and(|own| crate::core::pathutil::same_project(own, project_root))
            })
            .count()
    });

    let mut graphs = Vec::new();
    let mut queued_nodes = 0usize;
    let mut active_nodes = 0usize;
    let mut stale_nodes = 0usize;
    match crate::core::work_graph_store::WorkGraphStore::load(project_root) {
        Ok(store) => {
            for graph_id in store.graph_ids().take(32) {
                let Some(graph) = store.graph(graph_id) else {
                    continue;
                };
                let observation = graph.observation();
                let nodes = observation
                    .get("nodes")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut counts = std::collections::BTreeMap::new();
                for node in &nodes {
                    let state = node
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    *counts.entry(state.to_string()).or_insert(0usize) += 1;
                }
                queued_nodes += counts.get("pending").copied().unwrap_or(0);
                active_nodes += counts.get("active").copied().unwrap_or(0);
                stale_nodes += counts.get("stale").copied().unwrap_or(0);
                graphs.push(serde_json::json!({
                    "graph_id": graph_id,
                    "revision": store.graph_revision(graph_id).ok(),
                    "node_counts": counts,
                }));
            }
        }
        Err(error) => {
            tracing::debug!("lean-ctx: control snapshot could not load Work Graph: {error}");
        }
    }

    let next_action = if stale_nodes > 0 {
        "reconcile stale Work Graph executions"
    } else if queued_nodes > 0 {
        "claim one pending Work Graph node"
    } else if active_nodes > 0 {
        "continue current work and renew leases"
    } else {
        "lead may enqueue the next independent task"
    };

    serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "project_root": project_root,
        "presence": presence,
        "pending_messages": pending_messages,
        "work_graphs": graphs,
        "queued_nodes": queued_nodes,
        "active_nodes": active_nodes,
        "stale_nodes": stale_nodes,
        "next_action": next_action,
    }))
    .unwrap_or_else(|_| "{\"error\":\"control snapshot serialization failed\"}".to_string())
}
