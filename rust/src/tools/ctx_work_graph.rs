// SPDX-License-Identifier: Apache-2.0
use serde_json::json;

use crate::core::work_graph::{
    BoundedWorkGraph, ChildExecutionReceipt, ChildOutcome, ResultClaim, StopReason, WorkNodeBudget,
};
use crate::core::work_graph_store::{ClaimNodeExecution, WorkGraphStore, validate_id};

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub action: &'a str,
    pub project_root: &'a str,
    pub agent_id: &'a str,
    pub graph_id: &'a str,
    pub if_revision: Option<&'a str>,
    pub node_id: Option<&'a str>,
    pub parent_node_id: Option<&'a str>,
    pub to_agent: Option<&'a str>,
    pub capsule_ref: Option<&'a str>,
    pub outcome_ref: Option<&'a str>,
    pub tokens: Option<u64>,
    pub cost_micros: Option<u64>,
    pub max_concurrency: Option<usize>,
    pub receipt_id: Option<&'a str>,
    pub execution_fence: Option<&'a str>,
    pub outcome: Option<&'a str>,
    pub claims: Option<&'a serde_json::Value>,
    pub accepted_nodes: Option<&'a serde_json::Value>,
    pub stop_reason: Option<&'a str>,
    pub connector: Option<&'a str>,
    pub model: Option<&'a str>,
    pub timeout_ms: Option<u64>,
    pub max_attempts: Option<u8>,
    pub path_claims: Option<&'a serde_json::Value>,
    pub task_ref: Option<&'a str>,
    pub policy_ref: Option<&'a str>,
    pub expected_outcome_ref: Option<&'a str>,
}

pub fn handle(request: Request<'_>) -> Result<String, String> {
    if request.agent_id.is_empty() {
        return Err("agent must be registered first via ctx_agent".to_string());
    }
    ensure_active_agent(request.project_root, request.agent_id)?;
    if request.action != "list" {
        validate_id(request.graph_id, "graph_id")?;
    }
    match request.action {
        "create" => create(&request),
        "delegate" => delegate(&request),
        "claim" => claim(&request),
        "execute" => execute(&request),
        "consume" => consume(&request),
        "complete" => complete(&request),
        "receipt" => receipt(&request),
        "accept" => accept(&request),
        "fuse" => fuse(&request),
        "attribution" => attribution(&request),
        "value_report" => value_report(&request),
        "cancel" => cancel(&request),
        "get" => get(&request),
        "observe" => observe(&request),
        "list" => list(request.project_root, request.agent_id),
        other => Err(format!("unknown work graph action: {other}")),
    }
}

fn execute(request: &Request<'_>) -> Result<String, String> {
    use crate::core::agent_lease::{
        AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1,
        AgentLeaseResourceKindV1, acquire_shared, release_shared,
    };
    use crate::core::work_graph_executor::{NodeExecutionPlan, execute_claimed_node};

    if let Some(policy) =
        crate::core::policy::runtime::for_project(std::path::Path::new(request.project_root))?
    {
        if !policy.tool_allowed("ctx_work_graph") {
            return Err("project policy denies Work Graph execution".into());
        }
        let routing = &policy.resolved.routing;
        if (!routing.allowed_models.is_empty() || !routing.model_ceiling_groups.is_empty())
            && !request
                .model
                .is_some_and(|model| !model.trim().is_empty() && routing.model_allowed(model))
        {
            // An implicit connector default cannot prove compliance with a
            // configured ceiling. Reject before claiming or mutating a node.
            return Err("project policy denies Work Graph model selection".into());
        }
    }
    let node_id = required(request.node_id, "node_id")?;
    let connector_name = required(request.connector, "connector")?;
    let attempt_timeout_ms = request.timeout_ms.unwrap_or(300_000);
    let max_attempts = request.max_attempts.unwrap_or(1);
    let execution_window_ms =
        crate::core::work_graph_executor::execution_window_ms(attempt_timeout_ms, max_attempts)?;
    let now_epoch_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock before Unix epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock exceeds u64 milliseconds".to_string())?;
    let lease_expires_epoch_ms = now_epoch_ms
        .checked_add(execution_window_ms)
        .ok_or_else(|| "execution lease expiry overflow".to_string())?;
    let claimed = WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node_id, request.agent_id)?;
        let claim = store.claim_node_execution(
            request.graph_id,
            node_id,
            now_epoch_ms,
            lease_expires_epoch_ms,
        )?;
        let ClaimNodeExecution::Claimed(node) = claim else {
            return Ok(None);
        };
        let node = *node;
        let graph = store.graph(request.graph_id).expect("graph checked above");
        let root_agent = graph
            .root_agent_for(node_id)
            .map_err(|error| error.to_string())?
            .to_string();
        let parent_capsule_ref = node
            .parent_node_id
            .as_deref()
            .and_then(|parent| graph.get_node(parent))
            .map(|parent| parent.capsule_ref.clone());
        Ok(Some((node, root_agent, parent_capsule_ref)))
    })?;
    let Some((node, root_agent, parent_capsule_ref)) = claimed else {
        return Err("expired execution was stopped and cannot be reclaimed".into());
    };
    let fence = node
        .execution_fence
        .clone()
        .ok_or_else(|| "claim did not produce execution fence".to_string())?;
    let cancellation_key = crate::core::work_graph_executor::execution_key(
        std::path::Path::new(request.project_root),
        request.graph_id,
        node_id,
        &fence,
    )?;
    let resource_ref = crate::core::work_graph_executor::node_lease_resource(
        request.project_root,
        request.graph_id,
        node_id,
    )?;
    let node_lease = match acquire_shared(AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: fence.clone(),
        resource_kind: AgentLeaseResourceKindV1::WorkGraphNode,
        resource_ref: resource_ref.clone(),
        owner_agent_id: request.agent_id.to_string(),
        duration_ms: execution_window_ms,
    }) {
        Err(error) => {
            stop_unstarted_claim(request, node_id, &fence)?;
            return Err(error.to_string());
        }
        Ok(AgentLeaseAcquireV1::Granted(lease)) => lease,
        Ok(AgentLeaseAcquireV1::HeldBy { .. }) => {
            stop_unstarted_claim(request, node_id, &fence)?;
            return Err("work graph node lease is held by another execution".into());
        }
    };
    let mut leases = vec![(
        AgentLeaseResourceKindV1::WorkGraphNode,
        resource_ref,
        node_lease.lease_ref,
    )];
    for (index, path_ref) in node.path_claims.iter().enumerate() {
        let acquired = match acquire_shared(AgentLeaseRequestV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_request_ref: format!("{fence}:{index}"),
            resource_kind: AgentLeaseResourceKindV1::Path,
            resource_ref: path_ref.clone(),
            owner_agent_id: request.agent_id.to_string(),
            duration_ms: execution_window_ms,
        }) {
            Ok(acquired) => acquired,
            Err(error) => {
                for (kind, resource, lease_ref) in &leases {
                    let _ = release_shared(*kind, resource, request.agent_id, lease_ref);
                }
                stop_unstarted_claim(request, node_id, &fence)?;
                return Err(error.to_string());
            }
        };
        match acquired {
            AgentLeaseAcquireV1::Granted(lease) => leases.push((
                AgentLeaseResourceKindV1::Path,
                path_ref.clone(),
                lease.lease_ref,
            )),
            AgentLeaseAcquireV1::HeldBy { .. } => {
                for (kind, resource, lease_ref) in &leases {
                    let _ = release_shared(*kind, resource, request.agent_id, lease_ref);
                }
                stop_unstarted_claim(request, node_id, &fence)?;
                return Err(format!(
                    "path lease is held by another execution: {path_ref}"
                ));
            }
        }
    }

    let transport = crate::core::a2a::health::local_transport();
    let mut reserved_delivery = None;
    let mut capture_tracker = None;
    let result = (|| {
        let cancellation_watch = crate::core::agent_connector::timeout::durable::Guard::install(
            request.project_root,
            request.graph_id,
            node_id,
            &fence,
        )?;
        capture_tracker = Some(cancellation_watch.captures());
        let (signed_capsule, delivery) = transport
            .reserve(request.agent_id, &fence)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "no unreserved signed context capsule is queued for this agent".to_string()
            })?;
        reserved_delivery = Some(delivery.clone());
        if delivery.capsule_ref != node.capsule_ref {
            return Err("queued delivery does not match claimed node capsule".into());
        }
        let pinned_key = crate::core::agent_identity::get_stored_public_key(&root_agent)
            .map_err(|error| format!("root-agent trust key unavailable: {error}"))?;
        let connector = crate::core::agent_connector::detect_and_create_connectors()
            .into_iter()
            .find(|candidate| candidate.name() == connector_name)
            .ok_or_else(|| format!("supported connector not available: {connector_name}"))?;
        let execution = execute_claimed_node(
            &NodeExecutionPlan {
                graph_id: request.graph_id.to_string(),
                node: node.clone(),
                root_agent_id: root_agent.clone(),
                parent_capsule_ref: parent_capsule_ref.clone(),
                project_root: request.project_root.into(),
                timeout_ms: attempt_timeout_ms,
                model: request.model.map(str::to_string),
                max_attempts,
            },
            &signed_capsule,
            &pinned_key,
            connector.as_ref(),
        );
        let receipt = match execution {
            Ok(receipt) => receipt,
            Err(failure) => {
                if let Some(spend) = failure.measured_spend {
                    if let Err(persist_error) =
                        WorkGraphStore::mutate(request.project_root, |store| {
                            store
                                .graph_mut(request.graph_id)?
                                .record_failed_execution_spend(*spend)
                                .map_err(|error| error.to_string())
                        })
                    {
                        return Err(format!(
                            "{}; measured provider spend was not persisted: {persist_error}",
                            failure.message
                        ));
                    }
                }
                return Err(failure.message);
            }
        };
        if !cancellation_watch.captures().all_reaped() {
            return Err("execution process exit is unconfirmed; receipt withheld".into());
        }
        WorkGraphStore::mutate(request.project_root, |store| {
            store
                .graph_mut(request.graph_id)?
                .record_child_receipt(receipt)
                .map_err(|error| error.to_string())
        })?;
        if !transport.acknowledge(request.agent_id, &delivery.relay_id, &fence) {
            let _ = transport.quarantine_reserved(request.agent_id, &delivery.relay_id, &fence);
            return Err("capsule acknowledgement lost its reservation".into());
        }
        get(request)
    })();
    let all_reaped = capture_tracker
        .as_ref()
        .is_none_or(crate::core::agent_connector::timeout::durable::CaptureTracker::all_reaped);
    if all_reaped {
        WorkGraphStore::mutate(request.project_root, |store| {
            store
                .graph_mut(request.graph_id)?
                .acknowledge_execution_exit(node_id, &fence)
                .map_err(|error| error.to_string())
        })
        .map_err(|error| {
            format!("process exit acknowledgement failed; leases retained: {error}")
        })?;
        for (kind, resource, lease_ref) in &leases {
            let _ = release_shared(*kind, resource, request.agent_id, lease_ref);
        }
    }
    if result.is_err() {
        if let Some(delivery) = reserved_delivery {
            let _ = transport.negative_acknowledge(request.agent_id, &delivery.relay_id, &fence);
        }
        let _ = WorkGraphStore::mutate(request.project_root, |store| {
            store
                .graph_mut(request.graph_id)?
                .stop(node_id, StopReason::ExecutionFailed)
                .map_err(|error| error.to_string())
        });
    }
    crate::core::agent_connector::timeout::clear_cancellation(&cancellation_key);
    if !all_reaped {
        return Err(format!(
            "execution process exit is unconfirmed; leases retained; {}",
            result
                .as_ref()
                .map_or_else(String::as_str, |_| "execution result withheld")
        ));
    }
    result
}

/// Called only on admission failures before any connector/process dispatch.
fn stop_unstarted_claim(request: &Request<'_>, node_id: &str, fence: &str) -> Result<(), String> {
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        graph
            .stop(node_id, StopReason::LeaseLost)
            .map_err(|error| error.to_string())?;
        graph
            .acknowledge_execution_exit(node_id, fence)
            .map_err(|error| error.to_string())
    })
}

fn create(request: &Request<'_>) -> Result<String, String> {
    let node_id = required(request.node_id, "node_id")?;
    validate_id(node_id, "node_id")?;
    let tokens = positive(request.tokens, "tokens")?;
    let cost = positive(request.cost_micros, "cost_micros")?;
    let capsule = required(request.capsule_ref, "capsule_ref")?;
    let task_ref = required(request.task_ref, "task_ref")?;
    let policy_ref = required(request.policy_ref, "policy_ref")?;
    let expected_outcome_ref = required(request.expected_outcome_ref, "expected_outcome_ref")?;
    bounded_ref(capsule, "capsule_ref")?;
    let mut graph = BoundedWorkGraph::default()
        .with_local_concurrency(request.max_concurrency.unwrap_or(4))
        .with_graph_id(request.graph_id)
        .map_err(|error| error.to_string())?;
    graph
        .add_root(
            node_id.to_string(),
            request.agent_id.to_string(),
            capsule.to_string(),
            budget(tokens, cost),
        )
        .map_err(|error| error.to_string())?;
    graph
        .configure_root_context(node_id, task_ref, policy_ref, expected_outcome_ref)
        .map_err(|error| error.to_string())?;
    WorkGraphStore::mutate(request.project_root, |store| {
        store.create(request.graph_id, graph)
    })?;
    get(request)
}

fn delegate(request: &Request<'_>) -> Result<String, String> {
    let parent = required(request.parent_node_id, "parent_node_id")?;
    let node = required(request.node_id, "node_id")?;
    validate_id(parent, "parent_node_id")?;
    validate_id(node, "node_id")?;
    let target = required(request.to_agent, "to_agent")?;
    validate_id(target, "to_agent")?;
    ensure_active_agent(request.project_root, target)?;
    let capsule = required(request.capsule_ref, "capsule_ref")?;
    let task_ref = required(request.task_ref, "task_ref")?;
    let expected_outcome_ref = required(request.expected_outcome_ref, "expected_outcome_ref")?;
    bounded_ref(capsule, "capsule_ref")?;
    let tokens = positive(request.tokens, "tokens")?;
    let cost = positive(request.cost_micros, "cost_micros")?;
    let path_claims: Vec<String> = request
        .path_claims
        .map_or_else(
            || Ok(Vec::new()),
            |value| serde_json::from_value::<Vec<String>>(value.clone()),
        )
        .map_err(|error| format!("invalid path_claims: {error}"))?
        .into_iter()
        .map(|claim| canonical_path_claim(request.project_root, &claim))
        .collect::<Result<_, _>>()?;
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, parent, request.agent_id)?;
        graph
            .queue_child(
                parent,
                node.to_string(),
                target.to_string(),
                capsule.to_string(),
                budget(tokens, cost),
            )
            .map_err(|error| error.to_string())?;
        graph
            .set_path_claims(node, path_claims)
            .map_err(|error| error.to_string())?;
        graph
            .configure_child_context(node, task_ref, expected_outcome_ref)
            .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    get(request)
}

fn claim(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let lease_window_ms = request.timeout_ms.unwrap_or(300_000).clamp(1, 3_600_000);
    let now_epoch_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock before Unix epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock exceeds u64 milliseconds".to_string())?;
    let lease_expires_epoch_ms = now_epoch_ms
        .checked_add(lease_window_ms)
        .ok_or_else(|| "execution lease expiry overflow".to_string())?;
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node, request.agent_id)?;
        graph
            .claim_and_activate_until(node, Some(lease_expires_epoch_ms))
            .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    get(request)
}

fn consume(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let tokens = request.tokens.unwrap_or(0);
    let cost = request.cost_micros.unwrap_or(0);
    if tokens == 0 && cost == 0 {
        return Err("tokens or cost_micros must be positive".to_string());
    }
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node, request.agent_id)?;
        graph
            .consume_budget(node, tokens, cost)
            .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    get(request)
}

fn complete(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let outcome = required(request.outcome_ref, "outcome_ref")?;
    bounded_ref(outcome, "outcome_ref")?;
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node, request.agent_id)?;
        if graph
            .get_node(node)
            .is_some_and(|work_node| work_node.parent_node_id.is_some())
        {
            return Err("child nodes must finish with a child receipt".to_string());
        }
        if has_unfinished_descendants(graph, node) {
            return Err("root cannot complete while descendants are unfinished".to_string());
        }
        graph
            .complete(node, outcome.to_string())
            .map_err(|error| error.to_string())
    })?;
    get(request)
}

fn receipt(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let receipt_id = required(request.receipt_id, "receipt_id")?;
    validate_id(receipt_id, "receipt_id")?;
    let outcome_ref = required(request.outcome_ref, "outcome_ref")?;
    bounded_ref(outcome_ref, "outcome_ref")?;
    if request.execution_fence.is_some() {
        let (measured_tokens, measured_cost) =
            crate::core::agent_connector::receipt::verified_receipt_usage_for_task(
                outcome_ref,
                &format!("{}:{}", request.graph_id, node),
            )
            .map_err(|error| format!("canonical receipt verification failed: {error}"))?;
        if request.tokens != Some(measured_tokens) || request.cost_micros != Some(measured_cost) {
            return Err("receipt token/cost claims do not match canonical measurements".into());
        }
    }
    let outcome = match required(request.outcome, "outcome")? {
        "accepted" => ChildOutcome::Accepted,
        "rejected" => ChildOutcome::Rejected,
        "partial" => ChildOutcome::Partial,
        _ => return Err("outcome must be accepted|rejected|partial".to_string()),
    };
    let claims: Vec<ResultClaim> = request
        .claims
        .map_or_else(
            || Ok(Vec::new()),
            |value| serde_json::from_value(value.clone()),
        )
        .map_err(|error| format!("invalid claims: {error}"))?;
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node, request.agent_id)?;
        graph
            .record_child_receipt(ChildExecutionReceipt {
                receipt_id: receipt_id.to_string(),
                node_id: node.to_string(),
                outcome_ref: outcome_ref.to_string(),
                outcome,
                tokens_consumed: request.tokens.unwrap_or(0),
                cost_micros_consumed: request.cost_micros.unwrap_or(0),
                execution_fence: request.execution_fence.map(str::to_string),
                claims,
            })
            .map_err(|error| error.to_string())
    })?;
    get(request)
}

fn accept(request: &Request<'_>) -> Result<String, String> {
    let authorizer = required(request.node_id, "node_id")?;
    let accepted: Vec<String> = request
        .accepted_nodes
        .ok_or_else(|| "accepted_nodes is required".to_string())
        .and_then(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid accepted_nodes: {error}"))
        })?;
    if accepted.is_empty() {
        return Err("accepted_nodes must not be empty".to_string());
    }
    WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        let root = graph
            .get_node(authorizer)
            .ok_or_else(|| format!("node not found: {authorizer}"))?;
        if root.parent_node_id.is_some() || root.agent_id != request.agent_id {
            return Err("accepted path may only be selected by the root owner".to_string());
        }
        for node in &accepted {
            validate_id(node, "accepted_node")?;
        }
        graph
            .mark_accepted_path(&accepted)
            .map_err(|error| error.to_string())
    })?;
    get(request)
}

fn fuse(request: &Request<'_>) -> Result<String, String> {
    let store = WorkGraphStore::load(request.project_root)?;
    let graph = store
        .graph(request.graph_id)
        .ok_or_else(|| format!("graph not found: {}", request.graph_id))?;
    authorize_member(graph, request.agent_id)?;
    let result = graph
        .fuse_accepted_results()
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&result).map_err(|error| error.to_string())
}

fn attribution(request: &Request<'_>) -> Result<String, String> {
    let store = WorkGraphStore::load(request.project_root)?;
    let graph = store
        .graph(request.graph_id)
        .ok_or_else(|| format!("graph not found: {}", request.graph_id))?;
    authorize_member(graph, request.agent_id)?;
    let result = graph.attribution().map_err(|error| error.to_string())?;
    serde_json::to_string(&result).map_err(|error| error.to_string())
}

fn value_report(request: &Request<'_>) -> Result<String, String> {
    let store = WorkGraphStore::load(request.project_root)?;
    let graph = store
        .graph(request.graph_id)
        .ok_or_else(|| format!("graph not found: {}", request.graph_id))?;
    authorize_member(graph, request.agent_id)?;
    serde_json::to_string(
        &graph
            .team_value_report()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn cancel(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let cancellation_keys = WorkGraphStore::mutate(request.project_root, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node(graph, node, request.agent_id)?;
        let reason = match request.stop_reason.unwrap_or("manual_stop") {
            "manual_stop" => StopReason::ManualStop,
            "stale" => StopReason::Stale,
            "redundant" => StopReason::Redundant,
            "policy_denied" => StopReason::PolicyDenied,
            "lease_lost" => StopReason::LeaseLost,
            "duplicate" => StopReason::Duplicate,
            "low_value" => StopReason::LowValue,
            "execution_failed" => StopReason::ExecutionFailed,
            other => return Err(format!("unsupported stop_reason: {other}")),
        };
        let stopped = graph
            .stop(node, reason)
            .map_err(|error| error.to_string())?;
        let cancellation_keys = stopped
            .iter()
            .filter_map(|node_id| {
                graph
                    .get_node(node_id)
                    .and_then(|node| node.execution_fence.as_deref())
                    .map(|fence| {
                        crate::core::work_graph_executor::execution_key(
                            std::path::Path::new(request.project_root),
                            request.graph_id,
                            node_id,
                            fence,
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cancellation_keys)
    })?;
    // The durable stop is authoritative. Never signal a process before the
    // store has validated and persisted the requested transition.
    crate::core::agent_connector::timeout::request_cancellations(
        cancellation_keys.iter().map(String::as_str),
    )
    .map_err(|error| error.to_string())?;
    get(request)
}

fn get(request: &Request<'_>) -> Result<String, String> {
    let store = WorkGraphStore::load(request.project_root)?;
    let graph = store
        .graph(request.graph_id)
        .ok_or_else(|| format!("graph not found: {}", request.graph_id))?;
    authorize_member(graph, request.agent_id)?;
    serde_json::to_string(graph).map_err(|error| error.to_string())
}

fn observe(request: &Request<'_>) -> Result<String, String> {
    let store = WorkGraphStore::load(request.project_root)?;
    let graph = store
        .graph(request.graph_id)
        .ok_or_else(|| format!("graph not found: {}", request.graph_id))?;
    authorize_member(graph, request.agent_id)?;
    serde_json::to_string(&graph.observe_if_changed(request.if_revision))
        .map_err(|error| error.to_string())
}

fn list(project_root: &str, agent_id: &str) -> Result<String, String> {
    let store = WorkGraphStore::load(project_root)?;
    serde_json::to_string(
        &json!({"graph_ids": store.graph_ids_for_agent(agent_id).collect::<Vec<_>>() }),
    )
    .map_err(|error| error.to_string())
}

fn budget(tokens: u64, cost: u64) -> WorkNodeBudget {
    WorkNodeBudget {
        tokens_allocated: tokens,
        tokens_consumed: 0,
        cost_micros_allocated: cost,
        cost_micros_consumed: 0,
    }
}

fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is required"))
}

fn positive(value: Option<u64>, field: &str) -> Result<u64, String> {
    value
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{field} must be positive"))
}

fn bounded_ref(value: &str, field: &str) -> Result<(), String> {
    if value.len() > 4096 {
        return Err(format!("{field} exceeds 4096 bytes"));
    }
    Ok(())
}

fn canonical_path_claim(project_root: &str, claim: &str) -> Result<String, String> {
    let relative = claim
        .strip_prefix("path:")
        .ok_or_else(|| "path claim must use path: scheme".to_string())?;
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || relative
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
        || relative.chars().any(char::is_control)
    {
        return Err("path claim must be a normalized project-relative path".into());
    }
    let root = std::path::Path::new(project_root)
        .canonicalize()
        .map_err(|error| format!("invalid project root: {error}"))?;
    let candidate = root.join(relative);
    let mut cursor = root.clone();
    for component in std::path::Path::new(relative).components() {
        cursor.push(component);
        if let Ok(metadata) = std::fs::symlink_metadata(&cursor)
            && metadata.file_type().is_symlink()
        {
            return Err(format!("path claim traverses symlink: {relative}"));
        }
    }
    if candidate.exists() {
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("invalid path claim: {error}"))?;
        if !canonical.starts_with(&root) {
            return Err("path claim escapes project root".into());
        }
    }
    let project_hash = crate::core::project_hash::hash_project_root(project_root);
    Ok(format!("path:{project_hash}/{relative}"))
}

fn authorize_node(graph: &BoundedWorkGraph, node_id: &str, agent_id: &str) -> Result<(), String> {
    let node = graph
        .get_node(node_id)
        .ok_or_else(|| format!("node not found: {node_id}"))?;
    if node.agent_id != agent_id {
        return Err(format!("agent {agent_id} does not own node {node_id}"));
    }
    Ok(())
}

fn authorize_member(graph: &BoundedWorkGraph, agent_id: &str) -> Result<(), String> {
    if graph.contains_agent(agent_id) {
        return Ok(());
    }
    Err(format!("agent {agent_id} is not a member of this graph"))
}

fn has_unfinished_descendants(graph: &BoundedWorkGraph, node_id: &str) -> bool {
    graph.children_of(node_id).iter().any(|child_id| {
        graph.get_node(child_id).is_some_and(|node| {
            matches!(
                node.status,
                crate::core::work_graph::NodeStatus::Pending
                    | crate::core::work_graph::NodeStatus::Active
            )
        }) || has_unfinished_descendants(graph, child_id)
    })
}

fn ensure_active_agent(project_root: &str, agent_id: &str) -> Result<(), String> {
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    if registry
        .list_active(Some(project_root))
        .iter()
        .any(|agent| agent.agent_id == agent_id)
    {
        return Ok(());
    }
    Err(format!("agent is not active in this project: {agent_id}"))
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn failed_persistence_does_not_signal_and_retry_commits_before_signal() {
        let isolated = crate::core::data_dir::isolated_data_dir();
        let project = isolated.path().to_str().unwrap();
        let budget = WorkNodeBudget {
            tokens_allocated: 100,
            tokens_consumed: 0,
            cost_micros_allocated: 100,
            cost_micros_consumed: 0,
        };
        let mut graph = BoundedWorkGraph::default();
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
        let key = crate::core::work_graph_executor::execution_key(
            std::path::Path::new(project),
            "graph",
            "child",
            &fence,
        )
        .unwrap();
        let request = Request {
            action: "cancel",
            project_root: project,
            agent_id: "worker",
            graph_id: "graph",
            node_id: Some("child"),
            if_revision: None,
            parent_node_id: None,
            to_agent: None,
            capsule_ref: None,
            outcome_ref: None,
            tokens: None,
            cost_micros: None,
            max_concurrency: None,
            receipt_id: None,
            execution_fence: None,
            outcome: None,
            claims: None,
            accepted_nodes: None,
            stop_reason: None,
            connector: None,
            model: None,
            timeout_ms: None,
            max_attempts: None,
            path_claims: None,
            task_ref: None,
            policy_ref: None,
            expected_outcome_ref: None,
        };
        let error =
            crate::core::work_graph_store::with_save_failure(|| cancel(&request)).unwrap_err();
        assert!(error.contains("injected work graph persistence failure"));
        assert!(!crate::core::agent_connector::timeout::cancellation_requested(&key));
        assert!(
            !WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        cancel(&request).unwrap();
        let signalled = crate::core::agent_connector::timeout::cancellation_requested(&key);
        crate::core::agent_connector::timeout::clear_cancellation(&key);
        assert!(signalled);
        assert!(
            WorkGraphStore::execution_stop_requested(project, "graph", "child", &fence).unwrap()
        );
        // Invalid execution bounds are rejected before a stopped node can be
        // claimed (which would otherwise produce a different error).
        for (timeout, attempts, expected) in [
            (0, 1, "timeout_ms"),
            (1_800_001, 1, "timeout_ms"),
            (1000, 0, "max_attempts"),
            (1000, 3, "max_attempts"),
        ] {
            let invalid = Request {
                action: "execute",
                connector: Some("codex"),
                timeout_ms: Some(timeout),
                max_attempts: Some(attempts),
                ..request
            };
            assert!(execute(&invalid).unwrap_err().contains(expected));
        }
    }
}
