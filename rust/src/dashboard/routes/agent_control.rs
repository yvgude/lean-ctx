// SPDX-License-Identifier: Apache-2.0

//! Local dashboard operator control. Never borrows a worker's identity.
use serde::Deserialize;
use serde_json::json;

use crate::core::work_graph::StopReason;
use crate::core::work_graph_store::{WorkGraphStore, validate_id};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
    project_root: String,
    #[serde(default)]
    graph_id: String,
    node_id: Option<String>,
    expected_revision: Option<String>,
}

pub(super) fn handle(method: &str, body: &str) -> (&'static str, &'static str, String) {
    if method != "POST" {
        return (
            "405 Method Not Allowed",
            "application/json",
            json!({"error":"POST required"}).to_string(),
        );
    }
    if body.len() > 8192 {
        return (
            "413 Payload Too Large",
            "application/json",
            json!({"error":"request too large"}).to_string(),
        );
    }
    let request: Request = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(_) => {
            return (
                "400 Bad Request",
                "application/json",
                json!({"error":"invalid control request"}).to_string(),
            );
        }
    };
    // Limit the local operator surface to known agent projects, not arbitrary
    // user-supplied filesystem roots. Finished registrations remain observable.
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    if !registry
        .agents
        .iter()
        .any(|agent| agent.project_root == request.project_root)
    {
        return (
            "403 Forbidden",
            "application/json",
            json!({"error":"unknown agent project"}).to_string(),
        );
    }
    match execute(&request) {
        Ok(value) => ("200 OK", "application/json", value.to_string()),
        Err(error) if error == "work graph revision conflict" => (
            "409 Conflict",
            "application/json",
            json!({"error":error}).to_string(),
        ),
        Err(error) => (
            "400 Bad Request",
            "application/json",
            json!({"error":error}).to_string(),
        ),
    }
}

fn execute(request: &Request) -> Result<serde_json::Value, String> {
    if request.action != "list" {
        validate_id(&request.graph_id, "graph_id")?;
    }
    match request.action.as_str() {
        "list" => {
            let store = WorkGraphStore::load(&request.project_root)?;
            Ok(json!({"graph_ids": store.graph_ids().collect::<Vec<_>>()}))
        }
        "observe" => {
            let store = WorkGraphStore::load(&request.project_root)?;
            let graph = store.graph(&request.graph_id).ok_or("graph not found")?;
            Ok(
                json!({"graph": graph.observation(), "write_revision": store.graph_revision(&request.graph_id)?}),
            )
        }
        "cancel" => {
            let node = request.node_id.as_deref().ok_or("node_id required")?;
            validate_id(node, "node_id")?;
            let revision = request
                .expected_revision
                .as_deref()
                .ok_or("expected_revision required")?;
            // The local dashboard bearer authorizes the operator, independently
            // of worker membership. Durable observers deliver the committed stop.
            let observation = WorkGraphStore::mutate_graph_if_revision(
                &request.project_root,
                &request.graph_id,
                revision,
                |graph| {
                    graph
                        .stop(node, StopReason::ManualStop)
                        .map_err(|error| error.to_string())?;
                    Ok(graph.observation())
                },
            )?;
            Ok(json!({"cancellation_requested": true, "graph": observation}))
        }
        _ => Err("unsupported control action".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_cancel_rejects_stale_revision_and_preserves_unconfirmed_exit() {
        use crate::core::work_graph::{BoundedWorkGraph, WorkNodeBudget};
        use crate::core::work_graph_store::ClaimNodeExecution;
        let isolated = crate::core::data_dir::isolated_data_dir();
        let project = isolated.path().to_str().unwrap();
        let mut graph = BoundedWorkGraph::default();
        let budget = WorkNodeBudget {
            tokens_allocated: 100,
            tokens_consumed: 0,
            cost_micros_allocated: 100,
            cost_micros_consumed: 0,
        };
        graph
            .add_root(
                "root".into(),
                "lead".into(),
                "capsule:root".into(),
                budget.clone(),
            )
            .unwrap();
        graph
            .queue_child(
                "root",
                "child".into(),
                "worker".into(),
                "capsule:child".into(),
                budget,
            )
            .unwrap();
        let fence = WorkGraphStore::mutate(project, |store| {
            store.create("graph", graph)?;
            let ClaimNodeExecution::Claimed(node) =
                store.claim_node_execution("graph", "child", 1, u64::MAX)?
            else {
                return Err("expected claim".into());
            };
            Ok(node.execution_fence.unwrap())
        })
        .unwrap();
        let mut request = Request {
            action: "observe".into(),
            project_root: project.into(),
            graph_id: "graph".into(),
            node_id: None,
            expected_revision: None,
        };
        let observed = execute(&request).unwrap();
        request.action = "cancel".into();
        request.node_id = Some("child".into());
        request.expected_revision = Some("0".repeat(64));
        assert_eq!(
            execute(&request).unwrap_err(),
            "work graph revision conflict"
        );
        assert!(
            !WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        request.expected_revision = Some(observed["write_revision"].as_str().unwrap().into());
        let cancelled = execute(&request).unwrap();
        assert_eq!(cancelled["cancellation_requested"], true);
        assert!(
            WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        let child = cancelled["graph"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node_id"] == "child")
            .unwrap();
        assert_eq!(child["process_exit"], "unconfirmed");
        assert!(!cancelled.to_string().contains(&fence));
    }

    #[test]
    fn rejects_wrong_methods_and_unknown_fields() {
        assert_eq!(handle("GET", "{}").0, "405 Method Not Allowed");
        assert_eq!(
            handle(
                "POST",
                r#"{"action":"cancel","project_root":"/tmp","graph_id":"g","agent_id":"forged"}"#
            )
            .0,
            "400 Bad Request"
        );
    }
}
