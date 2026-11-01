// SPDX-License-Identifier: Apache-2.0
use serde_json::json;

use crate::core::work_graph::{
    BoundedWorkGraph, ChildExecutionReceipt, ChildOutcome, ResultClaim, StopReason, UsageSource,
    WorkNodeBudget,
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
    pub timeout_ms: Option<u64>,
    pub path_claims: Option<&'a serde_json::Value>,
    pub task_ref: Option<&'a str>,
    pub policy_ref: Option<&'a str>,
    pub expected_outcome_ref: Option<&'a str>,
}

pub fn handle(request: Request<'_>) -> Result<String, String> {
    if request.action == "execute" {
        return Err(EXECUTE_REMOVED.to_string());
    }
    if request.agent_id.is_empty() {
        return Err("agent must be registered first via ctx_agent".to_string());
    }
    ensure_active_agent(request.project_root, request.agent_id)?;
    if request.action != "list" {
        validate_id(request.graph_id, "graph_id")?;
    }
    check_required_fields(&request)?;
    if request.if_revision.is_some()
        && matches!(
            request.action,
            "create" | "fuse" | "attribution" | "value_report" | "get" | "list"
        )
    {
        return Err(format!(
            "if_revision applies to observe and to mutating actions, not to {}",
            request.action
        ));
    }
    match request.action {
        "create" => create(&request),
        "delegate" => delegate(&request),
        "claim" => claim(&request),
        "start" => start(&request),
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

/// Replies to the removed launcher action with the host-start path.
const EXECUTE_REMOVED: &str = "execute was removed: LeanCTX no longer launches agents. Launch the \
     child agent yourself and bind it to the node with action=start (returns the launch \
     environment); finish with action=receipt or stop it with action=cancel";

/// Bind a host-launched child agent to one fenced claim of its node. The host
/// starts the process; LeanCTX verifies the handed-over context capsule, holds
/// the node and path leases, and issues the child's delivery profile when the
/// host is configured as delivery issuer. A receipt or cancel ends it; an
/// expired lease is stopped on the next claim. Every stop revokes delivery.
fn start(request: &Request<'_>) -> Result<String, String> {
    use crate::core::work_graph_delegation as delegation;

    if let Some(policy) =
        crate::core::policy::runtime::for_project(std::path::Path::new(request.project_root))?
        && !policy.tool_allowed("ctx_work_graph")
    {
        return Err("project policy denies Work Graph execution".into());
    }
    let node_id = required(request.node_id, "node_id")?;
    let window_ms = delegation::start_window_ms(
        request
            .timeout_ms
            .unwrap_or(delegation::DEFAULT_START_WINDOW_MS),
    )?;
    // A malformed issuer configuration must fail before any node is claimed.
    let issuer = delegation::DeliveryIssuerConfig::from_environment()?;
    // The issuer signs as the root host. A process that holds the host's
    // issuer configuration under another agent identity (a child whose host
    // ignored launch.unset_env) must not sign for its own delegations.
    if let Some(issuer) = &issuer
        && issuer.host_agent() != request.agent_id
    {
        return Err(format!(
            "{} belongs to host agent {}; it must not reach agent {}",
            delegation::ISSUER_ENV,
            issuer.host_agent(),
            request.agent_id
        ));
    }
    let now_epoch_ms = epoch_ms()?;
    let lease_expires_epoch_ms = now_epoch_ms
        .checked_add(window_ms)
        .ok_or_else(|| "execution lease expiry overflow".to_string())?;
    let claimed = mutate_store(request, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node_or_parent(graph, node_id, request.agent_id)?;
        let claim = store.claim_node_execution(
            request.graph_id,
            node_id,
            now_epoch_ms,
            lease_expires_epoch_ms,
        )?;
        let ClaimNodeExecution::Claimed(node) = claim else {
            return Ok(None);
        };
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
        Ok(Some((*node, root_agent, parent_capsule_ref)))
    })?;
    let Some((node, root_agent, parent_capsule_ref)) = claimed else {
        return Err("expired execution was stopped and cannot be reclaimed".into());
    };
    let fence = node
        .execution_fence
        .clone()
        .ok_or_else(|| "claim did not produce execution fence".to_string())?;
    // Every failure from here on stops the claim again, so a refused start
    // never strands a claimed node. The shared leases must never outlive the
    // node lease: a stopped child's paths are released by the graph at that
    // instant, and a later claimant must then find the shared leases free.
    let acquired = incarnation_leases(request.project_root, request.graph_id, &node, &fence)
        .and_then(|leases| {
            let remaining_ms = lease_expires_epoch_ms.saturating_sub(epoch_ms()?);
            if remaining_ms == 0 {
                return Err("execution window elapsed before its leases were acquired".into());
            }
            acquire_incarnation_leases(&node.agent_id, &leases, remaining_ms)
        });
    if let Err(error) = acquired {
        abort_start(
            request,
            node_id,
            &node.agent_id,
            &fence,
            StopReason::LeaseLost,
        )?;
        return Err(error);
    }
    let plan = delegation::StartPlan {
        graph_id: request.graph_id.to_string(),
        node,
        root_agent_id: root_agent,
        parent_capsule_ref,
        project_root: request.project_root.into(),
        window_ms,
    };
    let transport = crate::core::a2a::health::local_transport();
    let mut reserved = None;
    let launch = (|| {
        plan.validate()?;
        let (signed_capsule, delivery) = transport
            .reserve(&plan.node.agent_id, &fence)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "no unreserved signed context capsule is queued for agent {}",
                    plan.node.agent_id
                )
            })?;
        reserved = Some(delivery.clone());
        if delivery.capsule_ref != plan.node.capsule_ref {
            return Err("queued delivery does not match claimed node capsule".into());
        }
        let root_key = crate::core::agent_identity::get_stored_public_key(&plan.root_agent_id)
            .map_err(|error| format!("root-agent trust key unavailable: {error}"))?;
        plan.verify_capsule(&signed_capsule, &root_key)?;
        // Publish the attempt before any delivery authority exists for it.
        WorkGraphStore::mutate(request.project_root, |store| {
            store
                .graph_mut(request.graph_id)?
                .begin_delivery_attempt(node_id, &fence, 1)
                .map_err(|error| error.to_string())
        })?;
        let task_id = delegation::attempt_task_id(
            &delegation::execution_key(
                std::path::Path::new(request.project_root),
                request.graph_id,
                node_id,
                &fence,
            )?,
            1,
        );
        let profile = match &issuer {
            Some(issuer) => {
                let issued =
                    issuer.issue(&plan, 1, &task_id, signed_capsule.capsule.sensitivity)?;
                Some(
                    issued
                        .launch_value(&task_id, std::path::Path::new(request.project_root))
                        .ok_or("issued delivery profile failed launch validation")?,
                )
            }
            None => None,
        };
        if !transport.acknowledge(&plan.node.agent_id, &delivery.relay_id, &fence) {
            let _ = transport.quarantine_reserved(&plan.node.agent_id, &delivery.relay_id, &fence);
            reserved = None;
            crate::core::a2a::health::record_delivery(false);
            return Err("capsule acknowledgement lost its reservation".into());
        }
        reserved = None;
        crate::core::a2a::health::record_delivery(true);
        Ok((task_id, profile))
    })();
    let (task_id, profile) = match launch {
        Ok(launch) => launch,
        Err(error) => {
            if let Some(delivery) = reserved {
                let _ =
                    transport.negative_acknowledge(&plan.node.agent_id, &delivery.relay_id, &fence);
                crate::core::a2a::health::record_delivery(false);
            }
            abort_start(
                request,
                node_id,
                &plan.node.agent_id,
                &fence,
                StopReason::ExecutionFailed,
            )?;
            return Err(error);
        }
    };
    let graph: serde_json::Value =
        serde_json::from_str(&get(request)?).map_err(|error| error.to_string())?;
    serde_json::to_string(&json!({
        "graph": graph,
        "launch": {
            "graph_id": request.graph_id,
            "node_id": node_id,
            "agent_id": plan.node.agent_id,
            "execution_fence": fence,
            "task_id": task_id,
            "lease_expires_epoch_ms": lease_expires_epoch_ms,
            "delegated_delivery": profile.is_some(),
            "env": {
                delegation::CHILD_DELIVERY_ENV:
                    profile.as_deref().unwrap_or(delegation::NO_CHILD_DELIVERY),
            },
            "unset_env": [delegation::ISSUER_ENV],
            "finish": "action=receipt with this execution_fence, or action=cancel; \
                       the lease ends at lease_expires_epoch_ms",
        },
    }))
    .map_err(|error| error.to_string())
}

fn epoch_ms() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock before Unix epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock exceeds u64 milliseconds".to_string())
}

type IncarnationLease = (
    crate::core::agent_lease::AgentLeaseResourceKindV1,
    String,
    String,
);

/// Node and path leases of one execution incarnation, keyed by its fence.
/// Path leases are project-bound and shared with `ctx_agent` leases, so an
/// agent holding a path there excludes a node claiming it here, and back.
fn incarnation_leases(
    project_root: &str,
    graph_id: &str,
    node: &crate::core::work_graph::WorkNode,
    fence: &str,
) -> Result<Vec<IncarnationLease>, String> {
    use crate::core::agent_lease::AgentLeaseResourceKindV1;
    let mut leases = vec![(
        AgentLeaseResourceKindV1::WorkGraphNode,
        crate::core::work_graph_delegation::node_lease_resource(
            project_root,
            graph_id,
            &node.node_id,
        )?,
        fence.to_string(),
    )];
    for (index, claim) in node.path_claims.iter().enumerate() {
        leases.push((
            AgentLeaseResourceKindV1::Path,
            crate::core::work_graph_delegation::path_lease_resource(project_root, claim)?,
            format!("{fence}:{index}"),
        ));
    }
    Ok(leases)
}

fn acquire_incarnation_leases(
    owner: &str,
    leases: &[IncarnationLease],
    duration_ms: u64,
) -> Result<(), String> {
    use crate::core::agent_lease::{
        AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1, acquire_shared,
    };
    for (kind, resource, lease_request_ref) in leases {
        let acquired = acquire_shared(AgentLeaseRequestV1 {
            schema_version: AGENT_LEASE_SCHEMA_VERSION,
            lease_request_ref: lease_request_ref.clone(),
            resource_kind: *kind,
            resource_ref: resource.clone(),
            owner_agent_id: owner.to_string(),
            duration_ms,
        })
        .map_err(|error| error.to_string())?;
        if let AgentLeaseAcquireV1::HeldBy { .. } = acquired {
            return Err(format!("lease is held by another execution: {resource}"));
        }
    }
    Ok(())
}

/// Releases every lease of one incarnation by its owner and fence, never by
/// recomputed resource names: a project root spelled differently at release
/// time must not leave the leases behind.
fn release_incarnation_leases(owner: &str, fence: &str) {
    // Leases expire with the execution window; a failed early release only
    // delays reuse and never extends authority.
    if let Err(error) = crate::core::agent_lease::release_fence_shared(owner, fence) {
        tracing::warn!(event = "work_graph_lease_release_failed", %error);
    }
}

/// Stop a claim whose launch was refused; the stop revokes delivery and frees
/// the slot before any lease of this incarnation is released.
fn abort_start(
    request: &Request<'_>,
    node_id: &str,
    owner: &str,
    fence: &str,
    reason: StopReason,
) -> Result<(), String> {
    WorkGraphStore::mutate(request.project_root, |store| {
        store
            .graph_mut(request.graph_id)?
            .stop_unlaunched(node_id, reason)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    release_incarnation_leases(owner, fence);
    Ok(())
}

/// The caller's mutation of its graph. With `if_revision` it runs only while
/// the graph still has that `write_revision` (from observe), checked under
/// the same persistence lock; otherwise it fails as a revision conflict and
/// changes nothing. The revision proves freshness, never authorization.
fn mutate_store<T>(
    request: &Request<'_>,
    mutate: impl FnOnce(&mut WorkGraphStore) -> Result<T, String>,
) -> Result<T, String> {
    match request.if_revision {
        Some(expected) => WorkGraphStore::mutate_if_revision(
            request.project_root,
            request.graph_id,
            expected,
            mutate,
        )
        .map_err(|error| {
            if error == "work graph revision conflict" {
                "work graph revision conflict: the graph changed since this write_revision; \
                 observe again for the current write_revision"
                    .to_string()
            } else {
                error
            }
        }),
        None => WorkGraphStore::mutate(request.project_root, mutate),
    }
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
    mutate_store(request, |store| {
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
    mutate_store(request, |store| {
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
    mutate_store(request, |store| {
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
    mutate_store(request, |store| {
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

/// Terminal child receipt from the node owner or the host that delegated it.
/// A claimed node is bound by its fence; the figures are reported by the
/// caller and stay bounded by the node and chain budgets.
fn receipt(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    let receipt_id = required(request.receipt_id, "receipt_id")?;
    validate_id(receipt_id, "receipt_id")?;
    let outcome_ref = required(request.outcome_ref, "outcome_ref")?;
    bounded_ref(outcome_ref, "outcome_ref")?;
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
    let finished = mutate_store(request, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node_or_parent(graph, node, request.agent_id)?;
        let work_node = graph
            .get_node(node)
            .ok_or_else(|| format!("node not found: {node}"))?
            .clone();
        if work_node.execution_fence.is_some() && request.execution_fence.is_none() {
            return Err(
                "receipt requires the execution_fence returned by action=start or action=claim"
                    .into(),
            );
        }
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
                usage_source: UsageSource::Reported,
            })
            .map_err(|error| error.to_string())?;
        Ok(work_node)
    })?;
    // The receipt reports the child finished, so its leases end with it.
    if let (true, Some(fence)) = (
        finished.execution_started,
        finished.execution_fence.as_deref(),
    ) {
        release_incarnation_leases(&finished.agent_id, fence);
    }
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
    mutate_store(request, |store| {
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

/// Durable stop by the node owner or its delegating host. The stop revokes the
/// child's delegated delivery at once; the host ends its own child process.
fn cancel(request: &Request<'_>) -> Result<String, String> {
    let node = required(request.node_id, "node_id")?;
    validate_id(node, "node_id")?;
    mutate_store(request, |store| {
        let graph = store.graph_mut(request.graph_id)?;
        authorize_node_or_parent(graph, node, request.agent_id)?;
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
        graph
            .stop(node, reason)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    // The stop revokes delivery at once. The stopped children's leases stay
    // until their windows end: the host may still be ending those processes.
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
    let mut observation = graph.observe_if_changed(request.if_revision);
    // The precondition for a guarded write (if_revision on a mutating action).
    observation["write_revision"] = store.graph_revision(request.graph_id)?.into();
    serde_json::to_string(&observation).map_err(|error| error.to_string())
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

/// Names every missing field of an action at once. Reported one at a time,
/// callers retried field by field (telemetry 10/2026: 11 of 45 failures).
fn check_required_fields(request: &Request<'_>) -> Result<(), String> {
    let fields: &[&str] = match request.action {
        "create" => &[
            "node_id",
            "capsule_ref",
            "task_ref",
            "policy_ref",
            "expected_outcome_ref",
            "tokens",
            "cost_micros",
        ],
        "delegate" => &[
            "parent_node_id",
            "node_id",
            "to_agent",
            "capsule_ref",
            "task_ref",
            "expected_outcome_ref",
            "tokens",
            "cost_micros",
        ],
        "claim" | "start" | "consume" | "cancel" => &["node_id"],
        "complete" => &["node_id", "outcome_ref"],
        "receipt" => &["node_id", "receipt_id", "outcome_ref", "outcome"],
        "accept" => &["node_id", "accepted_nodes"],
        _ => &[],
    };
    let present = |field: &str| match field {
        "node_id" => request.node_id.is_some_and(|v| !v.is_empty()),
        "parent_node_id" => request.parent_node_id.is_some_and(|v| !v.is_empty()),
        "to_agent" => request.to_agent.is_some_and(|v| !v.is_empty()),
        "capsule_ref" => request.capsule_ref.is_some_and(|v| !v.is_empty()),
        "task_ref" => request.task_ref.is_some_and(|v| !v.is_empty()),
        "policy_ref" => request.policy_ref.is_some_and(|v| !v.is_empty()),
        "expected_outcome_ref" => request.expected_outcome_ref.is_some_and(|v| !v.is_empty()),
        "outcome_ref" => request.outcome_ref.is_some_and(|v| !v.is_empty()),
        "receipt_id" => request.receipt_id.is_some_and(|v| !v.is_empty()),
        "outcome" => request.outcome.is_some_and(|v| !v.is_empty()),
        "tokens" => request.tokens.is_some(),
        "cost_micros" => request.cost_micros.is_some(),
        "accepted_nodes" => request.accepted_nodes.is_some(),
        _ => true,
    };
    let missing: Vec<&str> = fields.iter().copied().filter(|f| !present(f)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} requires {}; missing: {}",
        request.action,
        fields.join(", "),
        missing.join(", ")
    ))
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

/// The node's owner, or the owner of its direct parent that delegated it.
fn authorize_node_or_parent(
    graph: &BoundedWorkGraph,
    node_id: &str,
    agent_id: &str,
) -> Result<(), String> {
    let node = graph
        .get_node(node_id)
        .ok_or_else(|| format!("node not found: {node_id}"))?;
    let delegator = node
        .parent_node_id
        .as_deref()
        .and_then(|parent| graph.get_node(parent))
        .map(|parent| parent.agent_id.as_str());
    if node.agent_id == agent_id || delegator == Some(agent_id) {
        return Ok(());
    }
    Err(format!(
        "agent {agent_id} neither owns node {node_id} nor delegated it"
    ))
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
    Err(format!(
        "agent is not active in this project: {agent_id} — register it first with \
         ctx_agent action=register (agent ids are per project)"
    ))
}

#[cfg(test)]
#[path = "ctx_work_graph_lifecycle_tests.rs"]
mod lifecycle_tests;

/// The host-start actions with their test request and node views, for the
/// lifecycle tests here and the end-to-end test that drives a started child
/// against the HTTP delivery endpoint.
#[cfg(test)]
pub(crate) mod test_api {
    use super::{Request, WorkGraphStore};

    #[cfg(feature = "http-server")]
    pub(crate) fn start(request: &Request<'_>) -> Result<String, String> {
        super::start(request)
    }

    #[cfg(feature = "http-server")]
    pub(crate) fn receipt(request: &Request<'_>) -> Result<String, String> {
        super::receipt(request)
    }

    pub(crate) fn request<'a>(action: &'a str, project: &'a str, agent: &'a str) -> Request<'a> {
        Request {
            action,
            project_root: project,
            agent_id: agent,
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
            timeout_ms: None,
            path_claims: None,
            task_ref: None,
            policy_ref: None,
            expected_outcome_ref: None,
        }
    }

    pub(crate) fn node(project: &str) -> crate::core::work_graph::WorkNode {
        WorkGraphStore::load(project)
            .unwrap()
            .graph("graph")
            .unwrap()
            .get_node("child")
            .unwrap()
            .clone()
    }
}
