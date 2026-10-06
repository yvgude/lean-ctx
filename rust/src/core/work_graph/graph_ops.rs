// SPDX-License-Identifier: Apache-2.0

use super::{
    BTreeMap, BTreeSet, BoundedWorkGraph, BudgetAllocation, ChainBudget, ChildExecutionReceipt,
    ChildOutcome, FusedResults, MAX_DEPTH, MAX_FAN_OUT, MAX_GRAPH_NODES, MAX_LOCAL_CONCURRENCY,
    MAX_PATH_CLAIMS, MeasuredExecutionSpend, NodeStatus, ResultConflict, StopReason,
    TeamValueReport, WorkAttribution, WorkGraphError, WorkNode, WorkNodeBudget, cascade_budget,
    execution_fence, validate_budget, validate_cascade, validate_identifier,
    validate_measured_spend, validate_path_claim, validate_receipt,
};

impl BoundedWorkGraph {
    #[must_use]
    pub fn new(max_fan_out: usize, max_depth: u16) -> Self {
        Self {
            graph_id: String::new(),
            nodes: BTreeMap::new(),
            children: BTreeMap::new(),
            chain_budgets: BTreeMap::new(),
            pending_child_budgets: BTreeMap::new(),
            pending_child_parents: BTreeMap::new(),
            receipts: BTreeMap::new(),
            failed_spend: BTreeMap::new(),
            reaped_executions: BTreeMap::new(),
            accepted_path: BTreeSet::new(),
            max_local_concurrency: MAX_FAN_OUT,
            max_fan_out: max_fan_out.clamp(1, MAX_FAN_OUT),
            max_depth: max_depth.clamp(1, MAX_DEPTH),
        }
    }

    #[must_use]
    pub fn with_local_concurrency(mut self, max_local_concurrency: usize) -> Self {
        self.max_local_concurrency = max_local_concurrency.clamp(1, MAX_LOCAL_CONCURRENCY);
        self
    }

    pub fn with_graph_id(mut self, graph_id: &str) -> Result<Self, WorkGraphError> {
        validate_identifier(graph_id, "graph_id")?;
        self.graph_id = graph_id.to_string();
        Ok(self)
    }

    pub fn bind_graph_id(&mut self, graph_id: &str) -> Result<(), WorkGraphError> {
        validate_identifier(graph_id, "graph_id")?;
        if !self.graph_id.is_empty() && self.graph_id != graph_id {
            return Err(WorkGraphError::InvalidGraph("graph id mismatch".into()));
        }
        self.graph_id = graph_id.to_string();
        Ok(())
    }

    pub fn graph_id(&self) -> &str {
        &self.graph_id
    }

    /// Compact, deterministic observation for controllers. Payloads, capability
    /// references and execution fences remain in the authorized detail API.
    pub fn observation(&self) -> serde_json::Value {
        self.observe_if_changed(None)
    }

    /// Return only a stable revision when the caller already has this state.
    /// Revision is a cache validator, never an authorization or execution token.
    pub fn observe_if_changed(&self, if_revision: Option<&str>) -> serde_json::Value {
        let nodes: Vec<_> = self
            .nodes
            .values()
            .map(|node| {
                serde_json::json!({
                    "node_id": node.node_id,
                    "agent_id": node.agent_id,
                    "parent_node_id": node.parent_node_id,
                    "status": node.status,
                    "execution_started": node.execution_started,
                    "process_exit": if !node.execution_started { "not_started" }
                        else if self.execution_exit_unconfirmed(node) { "unconfirmed" }
                        else { "confirmed" },
                    "stop_reason": node.stop_reason,
                    "tokens_consumed": node.budget.tokens_consumed,
                    "tokens_remaining": node.budget.tokens_remaining(),
                    "cost_micros_consumed": node.budget.cost_micros_consumed,
                    "cost_micros_remaining": node.budget.cost_remaining(),
                    "accepted": self.accepted_path.contains(&node.node_id),
                })
            })
            .collect();
        let mut observation = serde_json::json!({
            "schema_version": 1,
            "graph_id": self.graph_id,
            "max_concurrency": self.local_concurrency_limit(),
            "nodes": nodes,
        });
        let revision = blake3::hash(observation.to_string().as_bytes())
            .to_hex()
            .to_string();
        if if_revision == Some(revision.as_str()) {
            return serde_json::json!({
                "schema_version": 1,
                "graph_id": self.graph_id,
                "revision": revision,
                "unchanged": true,
            });
        }
        observation["revision"] = revision.into();
        observation["unchanged"] = false.into();
        observation
    }

    pub(super) fn local_concurrency_limit(&self) -> usize {
        self.max_local_concurrency.clamp(1, MAX_LOCAL_CONCURRENCY)
    }

    /// Add a root node (no parent).
    pub fn add_root(
        &mut self,
        node_id: String,
        agent_id: String,
        capsule_ref: String,
        budget: WorkNodeBudget,
    ) -> Result<&WorkNode, WorkGraphError> {
        if self.nodes.len() >= MAX_GRAPH_NODES {
            return Err(WorkGraphError::CapacityExceeded);
        }
        validate_identifier(&node_id, "node_id")?;
        validate_identifier(&agent_id, "agent_id")?;
        validate_identifier(&capsule_ref, "capsule_ref")?;
        validate_budget(&budget)?;
        let concurrency_limit = self.local_concurrency_limit();
        if self.active_count() >= concurrency_limit {
            return Err(WorkGraphError::LocalConcurrencyExceeded(concurrency_limit));
        }
        if self.nodes.contains_key(&node_id) {
            return Err(WorkGraphError::DuplicateNode(node_id));
        }
        let node = WorkNode {
            node_id: node_id.clone(),
            agent_id,
            parent_node_id: None,
            capsule_ref,
            status: NodeStatus::Active,
            budget,
            depth: 0,
            stop_reason: None,
            outcome_ref: None,
            claim_attempt: 0,
            delivery_attempt: None,
            execution_fence: None,
            lease_expires_epoch_ms: None,
            path_claims: Vec::new(),
            task_ref: String::new(),
            parent_task_ref: None,
            policy_ref: String::new(),
            expected_outcome_ref: String::new(),
            execution_started: false,
        };
        self.nodes.insert(node_id.clone(), node);
        self.chain_budgets.insert(
            node_id.clone(),
            ChainBudget {
                chain_id: node_id.clone(),
                root_budget_tokens: self.nodes[&node_id].budget.tokens_allocated,
                total_consumed_tokens: 0,
                total_allocated_tokens: self.nodes[&node_id].budget.tokens_allocated,
                root_budget_cost_micros: self.nodes[&node_id].budget.cost_micros_allocated,
                total_consumed_cost_micros: 0,
                total_allocated_cost_micros: self.nodes[&node_id].budget.cost_micros_allocated,
                unauthorized_tokens: 0,
                unauthorized_cost_micros: 0,
                depth: 0,
            },
        );
        Ok(self.nodes.get(&node_id).expect("root node inserted above"))
    }

    /// Delegate work to a child node. Validates budget inheritance and fan-out.
    pub fn delegate(
        &mut self,
        parent_node_id: &str,
        child_node_id: String,
        child_agent_id: String,
        capsule_ref: String,
        child_budget: WorkNodeBudget,
    ) -> Result<&WorkNode, WorkGraphError> {
        self.insert_child(
            parent_node_id,
            child_node_id,
            child_agent_id,
            capsule_ref,
            child_budget,
            NodeStatus::Active,
        )
    }

    /// Queue child work without consuming a local execution slot.
    pub fn queue_child(
        &mut self,
        parent_node_id: &str,
        child_node_id: String,
        child_agent_id: String,
        capsule_ref: String,
        child_budget: WorkNodeBudget,
    ) -> Result<&WorkNode, WorkGraphError> {
        self.insert_child(
            parent_node_id,
            child_node_id,
            child_agent_id,
            capsule_ref,
            child_budget,
            NodeStatus::Pending,
        )
    }

    /// Atomically claim pending local work and activate it when a slot is free.
    pub fn claim_and_activate(&mut self, node_id: &str) -> Result<&WorkNode, WorkGraphError> {
        self.claim_and_activate_until(node_id, None)
    }

    pub fn claim_and_activate_until(
        &mut self,
        node_id: &str,
        lease_expires_epoch_ms: Option<u64>,
    ) -> Result<&WorkNode, WorkGraphError> {
        validate_identifier(node_id, "node_id")?;
        let concurrency_limit = self.local_concurrency_limit();
        if self.active_count() >= concurrency_limit {
            return Err(WorkGraphError::LocalConcurrencyExceeded(concurrency_limit));
        }
        let parent_id = self
            .nodes
            .get(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?
            .parent_node_id
            .clone();
        let Some(parent_id) = parent_id else {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        };
        if self.nodes.get(&parent_id).map(|node| node.status) != Some(NodeStatus::Active) {
            return Err(WorkGraphError::ParentNotActive(parent_id));
        }
        let graph_id = self.graph_id.clone();
        let node = self.nodes.get_mut(node_id).expect("node checked above");
        if node.status != NodeStatus::Pending {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.claim_attempt = node
            .claim_attempt
            .checked_add(1)
            .ok_or(WorkGraphError::Overflow)?;
        node.execution_fence = Some(execution_fence(&graph_id, node));
        node.delivery_attempt = None;
        node.lease_expires_epoch_ms = lease_expires_epoch_ms;
        node.status = NodeStatus::Active;
        Ok(node)
    }

    /// Claim pending work or renew the same owner's already-issued execution fence.
    pub fn claim_or_resume_active_until(
        &mut self,
        node_id: &str,
        lease_expires_epoch_ms: u64,
    ) -> Result<&WorkNode, WorkGraphError> {
        let capsule_ref = self
            .nodes
            .get(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?
            .capsule_ref
            .clone();
        if self.nodes.iter().any(|(candidate_id, candidate)| {
            candidate_id != node_id
                && candidate.execution_started
                && candidate.capsule_ref == capsule_ref
        }) {
            return Err(WorkGraphError::InvalidGraph(
                "context capsule was already consumed by another node".into(),
            ));
        }
        match self.nodes.get(node_id).map(|node| node.status) {
            Some(NodeStatus::Pending) => {
                self.claim_and_activate_until(node_id, Some(lease_expires_epoch_ms))?;
                let node = self.nodes.get_mut(node_id).expect("node claimed above");
                node.execution_started = true;
                Ok(node)
            }
            Some(NodeStatus::Active) => {
                let node = self.nodes.get_mut(node_id).expect("node checked above");
                if node.parent_node_id.is_none()
                    || node.execution_fence.is_none()
                    || node.execution_started
                {
                    return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
                }
                node.lease_expires_epoch_ms = Some(lease_expires_epoch_ms);
                node.execution_started = true;
                Ok(node)
            }
            Some(_) => Err(WorkGraphError::InvalidTransition(node_id.to_string())),
            None => Err(WorkGraphError::NodeNotFound(node_id.to_string())),
        }
    }

    pub fn recover_expired_execution(
        &mut self,
        node_id: &str,
        now_epoch_ms: u64,
    ) -> Result<Vec<String>, WorkGraphError> {
        let node = self
            .nodes
            .get(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        let expired = node.parent_node_id.is_some()
            && node.status == NodeStatus::Active
            && node
                .lease_expires_epoch_ms
                .is_some_and(|expires| now_epoch_ms >= expires);
        if expired {
            return self.stop(node_id, StopReason::LeaseLost);
        }
        Ok(Vec::new())
    }

    /// Bind deterministic project-relative path leases before a queued node is claimed.
    pub fn set_path_claims(
        &mut self,
        node_id: &str,
        mut path_claims: Vec<String>,
    ) -> Result<(), WorkGraphError> {
        if path_claims.len() > MAX_PATH_CLAIMS {
            return Err(WorkGraphError::TooManyPathClaims);
        }
        path_claims.sort();
        path_claims.dedup();
        for claim in &path_claims {
            validate_path_claim(claim)?;
        }
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        if node.status != NodeStatus::Pending {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.path_claims = path_claims;
        Ok(())
    }

    pub fn configure_root_context(
        &mut self,
        node_id: &str,
        task_ref: &str,
        policy_ref: &str,
        expected_outcome_ref: &str,
    ) -> Result<(), WorkGraphError> {
        validate_identifier(task_ref, "task_ref")?;
        validate_identifier(policy_ref, "policy_ref")?;
        validate_identifier(expected_outcome_ref, "expected_outcome_ref")?;
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        if node.parent_node_id.is_some() {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.task_ref = task_ref.to_string();
        node.parent_task_ref = None;
        node.policy_ref = policy_ref.to_string();
        node.expected_outcome_ref = expected_outcome_ref.to_string();
        Ok(())
    }

    pub fn configure_child_context(
        &mut self,
        node_id: &str,
        task_ref: &str,
        expected_outcome_ref: &str,
    ) -> Result<(), WorkGraphError> {
        validate_identifier(task_ref, "task_ref")?;
        validate_identifier(expected_outcome_ref, "expected_outcome_ref")?;
        let parent_id = self
            .nodes
            .get(node_id)
            .and_then(|node| node.parent_node_id.clone())
            .ok_or_else(|| WorkGraphError::InvalidTransition(node_id.to_string()))?;
        let parent = self
            .nodes
            .get(&parent_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(parent_id.clone()))?;
        if parent.task_ref.is_empty() || parent.policy_ref.is_empty() {
            return Err(WorkGraphError::InvalidGraph(
                "parent context is not configured".into(),
            ));
        }
        let parent_task_ref = parent.task_ref.clone();
        let policy_ref = parent.policy_ref.clone();
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        if node.status != NodeStatus::Pending {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.task_ref = task_ref.to_string();
        node.parent_task_ref = Some(parent_task_ref);
        node.policy_ref = policy_ref;
        node.expected_outcome_ref = expected_outcome_ref.to_string();
        Ok(())
    }

    fn insert_child(
        &mut self,
        parent_node_id: &str,
        child_node_id: String,
        child_agent_id: String,
        capsule_ref: String,
        child_budget: WorkNodeBudget,
        status: NodeStatus,
    ) -> Result<&WorkNode, WorkGraphError> {
        if self.nodes.len() >= MAX_GRAPH_NODES {
            return Err(WorkGraphError::CapacityExceeded);
        }
        validate_identifier(parent_node_id, "parent_node_id")?;
        validate_identifier(&child_node_id, "child_node_id")?;
        validate_identifier(&child_agent_id, "child_agent_id")?;
        validate_identifier(&capsule_ref, "capsule_ref")?;
        validate_budget(&child_budget)?;
        if self.nodes.contains_key(&child_node_id) {
            return Err(WorkGraphError::DuplicateNode(child_node_id));
        }
        let pending_budget = self.pending_child_budgets.get(&child_node_id);
        let pending_parent = self.pending_child_parents.get(&child_node_id);
        let pending_reservation_matches = match (pending_budget, pending_parent) {
            (None, None) => true,
            (Some(_), Some(parent)) => parent == parent_node_id,
            _ => false,
        };
        if !pending_reservation_matches {
            return Err(WorkGraphError::InvalidGraph(
                "pending child reservation is incomplete or belongs to another parent".into(),
            ));
        }
        let (reserved_tokens, reserved_cost) =
            self.reserved_child_budget(parent_node_id, Some(&child_node_id))?;
        let parent = self
            .nodes
            .get(parent_node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(parent_node_id.to_string()))?;
        if parent.status != NodeStatus::Active {
            return Err(WorkGraphError::ParentNotActive(parent_node_id.to_string()));
        }
        let new_depth = parent.depth + 1;
        if new_depth > self.max_depth {
            return Err(WorkGraphError::DepthExceeded(self.max_depth));
        }
        let available_tokens = parent
            .budget
            .tokens_remaining()
            .saturating_sub(reserved_tokens);
        let available_cost = parent.budget.cost_remaining().saturating_sub(reserved_cost);
        if child_budget.tokens_allocated > available_tokens {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: child_budget.tokens_allocated,
                parent_remaining: available_tokens,
            });
        }
        if child_budget.cost_micros_allocated > available_cost {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: child_budget.cost_micros_allocated,
                parent_remaining: available_cost,
            });
        }
        let current_children = self.children.get(parent_node_id).map_or(0, Vec::len);
        if current_children >= self.max_fan_out {
            return Err(WorkGraphError::FanOutExceeded(self.max_fan_out));
        }
        let concurrency_limit = self.local_concurrency_limit();
        if status == NodeStatus::Active && self.active_count() >= concurrency_limit {
            return Err(WorkGraphError::LocalConcurrencyExceeded(concurrency_limit));
        }
        let child_tokens_allocated = child_budget.tokens_allocated;
        let child_cost_allocated = child_budget.cost_micros_allocated;
        let node = WorkNode {
            node_id: child_node_id.clone(),
            agent_id: child_agent_id,
            parent_node_id: Some(parent_node_id.to_string()),
            capsule_ref,
            status,
            budget: child_budget,
            depth: new_depth,
            stop_reason: None,
            outcome_ref: None,
            claim_attempt: 0,
            delivery_attempt: None,
            execution_fence: None,
            lease_expires_epoch_ms: None,
            path_claims: Vec::new(),
            task_ref: String::new(),
            parent_task_ref: None,
            policy_ref: String::new(),
            expected_outcome_ref: String::new(),
            execution_started: false,
        };
        self.nodes.insert(child_node_id.clone(), node);
        self.children
            .entry(parent_node_id.to_string())
            .or_default()
            .push(child_node_id.clone());
        let chain_id = self
            .chain_id_for_node(&child_node_id)
            .expect("delegated child always has a root node");
        let pending_budget =
            self.pending_child_budgets
                .remove(&child_node_id)
                .unwrap_or(WorkNodeBudget {
                    tokens_allocated: 0,
                    tokens_consumed: 0,
                    cost_micros_allocated: 0,
                    cost_micros_consumed: 0,
                });
        self.pending_child_parents.remove(&child_node_id);
        self.record_chain_allocation(
            &chain_id,
            pending_budget.tokens_allocated,
            child_tokens_allocated,
            pending_budget.cost_micros_allocated,
            child_cost_allocated,
            new_depth,
        );
        Ok(self
            .nodes
            .get(&child_node_id)
            .expect("child node inserted above"))
    }

    /// Allocates budget for a child node using cascade rules.
    ///
    /// The returned budget is reserved for `child_id` until it is passed to
    /// [`Self::delegate`], so chain allocation is not counted twice.
    pub fn allocate_child_budget(
        &mut self,
        parent_id: &str,
        child_id: &str,
        fraction: f64,
    ) -> Result<WorkNodeBudget, WorkGraphError> {
        validate_identifier(parent_id, "parent_node_id")?;
        validate_identifier(child_id, "child_node_id")?;
        if self.nodes.contains_key(child_id) || self.pending_child_budgets.contains_key(child_id) {
            return Err(WorkGraphError::DuplicateNode(child_id.to_string()));
        }
        let (parent_budget_tokens, parent_used_tokens, parent_cost_remaining, parent_depth) = {
            let parent = self
                .nodes
                .get(parent_id)
                .ok_or_else(|| WorkGraphError::NodeNotFound(parent_id.to_string()))?;
            if parent.status != NodeStatus::Active {
                return Err(WorkGraphError::ParentNotActive(parent_id.to_string()));
            }
            if parent.depth >= self.max_depth {
                return Err(WorkGraphError::DepthExceeded(self.max_depth));
            }
            (
                parent.budget.tokens_allocated,
                parent.budget.tokens_consumed,
                parent.budget.cost_remaining(),
                parent.depth,
            )
        };

        let (reserved_tokens, reserved_cost) =
            self.reserved_child_budget(parent_id, Some(child_id))?;
        let parent_remaining = parent_budget_tokens
            .saturating_sub(parent_used_tokens)
            .saturating_sub(reserved_tokens);
        if parent_remaining == 0 {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: 1,
                parent_remaining,
            });
        }

        let allocation = BudgetAllocation {
            parent_budget_tokens,
            parent_used_tokens,
            child_fraction: fraction,
            minimum_budget: 0,
            maximum_budget: parent_remaining,
        };
        let mut cascaded = cascade_budget(&allocation);
        cascaded.depth = u32::from(parent_depth) + 1;
        cascaded.lineage = self.node_lineage(parent_id);
        cascaded.lineage.push(child_id.to_string());
        validate_cascade(&cascaded)?;
        if cascaded.allocated_tokens > parent_remaining {
            return Err(WorkGraphError::BudgetExceedsParent {
                child_requested: cascaded.allocated_tokens,
                parent_remaining,
            });
        }

        let parent_cost_remaining = parent_cost_remaining.saturating_sub(reserved_cost);
        let cost_micros_allocated = if parent_cost_remaining == 0 {
            0
        } else {
            ((parent_cost_remaining as f64 * fraction) as u64)
                .max(1)
                .min(parent_cost_remaining)
        };
        let child_budget = WorkNodeBudget {
            tokens_allocated: cascaded.allocated_tokens,
            tokens_consumed: 0,
            cost_micros_allocated,
            cost_micros_consumed: 0,
        };
        let chain_id = self
            .chain_id_for_node(parent_id)
            .expect("parent node always has a root node");
        self.pending_child_budgets
            .insert(child_id.to_string(), child_budget.clone());
        self.pending_child_parents
            .insert(child_id.to_string(), parent_id.to_string());
        self.record_chain_allocation(
            &chain_id,
            0,
            child_budget.tokens_allocated,
            0,
            child_budget.cost_micros_allocated,
            parent_depth + 1,
        );

        Ok(child_budget)
    }

    /// Mark a node as completed with an outcome reference.
    pub fn complete(&mut self, node_id: &str, outcome_ref: String) -> Result<(), WorkGraphError> {
        validate_identifier(&outcome_ref, "outcome_ref")?;
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        if node.status != NodeStatus::Active {
            return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
        }
        node.status = NodeStatus::Completed;
        node.outcome_ref = Some(outcome_ref);
        node.lease_expires_epoch_ms = None;
        Ok(())
    }

    /// Validate and record a terminal receipt for active child execution.
    pub fn record_child_receipt(
        &mut self,
        receipt: ChildExecutionReceipt,
    ) -> Result<(), WorkGraphError> {
        validate_receipt(&receipt)?;
        if self.receipts.contains_key(&receipt.receipt_id)
            || self
                .receipts
                .values()
                .any(|known| known.node_id == receipt.node_id)
        {
            return Err(WorkGraphError::DuplicateReceipt(receipt.receipt_id));
        }
        let node = self
            .nodes
            .get(&receipt.node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(receipt.node_id.clone()))?;
        if node.parent_node_id.is_none() || node.status != NodeStatus::Active {
            return Err(WorkGraphError::InvalidTransition(receipt.node_id));
        }
        if node.execution_fence.is_some() && receipt.execution_fence != node.execution_fence {
            return Err(WorkGraphError::StaleExecutionFence(receipt.node_id));
        }
        if receipt.tokens_consumed > node.budget.tokens_allocated
            || receipt.cost_micros_consumed > node.budget.cost_micros_allocated
            || receipt.tokens_consumed < node.budget.tokens_consumed
            || receipt.cost_micros_consumed < node.budget.cost_micros_consumed
        {
            return Err(WorkGraphError::ReceiptExceedsBudget(receipt.node_id));
        }
        let token_delta = receipt.tokens_consumed - node.budget.tokens_consumed;
        let cost_delta = receipt.cost_micros_consumed - node.budget.cost_micros_consumed;
        let (reserved_tokens, reserved_cost) =
            self.reserved_child_budget(&receipt.node_id, None)?;
        if token_delta
            > node
                .budget
                .tokens_remaining()
                .saturating_sub(reserved_tokens)
            || cost_delta > node.budget.cost_remaining().saturating_sub(reserved_cost)
        {
            return Err(WorkGraphError::ReceiptExceedsBudget(receipt.node_id));
        }

        let node_id = receipt.node_id.clone();
        let receipt_id = receipt.receipt_id.clone();
        let chain_id = self
            .chain_id_for_node(&node_id)
            .expect("receipt node always has a root");
        self.ensure_chain_budget(&chain_id);
        let chain = self
            .chain_budgets
            .get(&chain_id)
            .expect("chain budget initialized");
        let chain_consumed = chain
            .total_consumed_tokens
            .checked_add(token_delta)
            .ok_or(WorkGraphError::Overflow)?;
        let chain_cost = chain
            .total_consumed_cost_micros
            .checked_add(cost_delta)
            .ok_or(WorkGraphError::Overflow)?;
        if chain_consumed.saturating_add(chain.unauthorized_tokens) > chain.root_budget_tokens
            || chain_cost.saturating_add(chain.unauthorized_cost_micros)
                > chain.root_budget_cost_micros
        {
            return Err(WorkGraphError::ReceiptExceedsBudget(node_id));
        }
        let node = self
            .nodes
            .get_mut(&node_id)
            .expect("receipt node checked above");
        node.budget.tokens_consumed = receipt.tokens_consumed;
        node.budget.cost_micros_consumed = receipt.cost_micros_consumed;
        node.status = match receipt.outcome {
            ChildOutcome::Accepted | ChildOutcome::Partial => NodeStatus::Completed,
            ChildOutcome::Rejected => NodeStatus::Failed,
        };
        node.outcome_ref = Some(receipt.outcome_ref.clone());
        node.lease_expires_epoch_ms = None;
        self.chain_budgets
            .get_mut(&chain_id)
            .expect("chain budget initialized")
            .total_consumed_tokens = chain_consumed;
        self.chain_budgets
            .get_mut(&chain_id)
            .expect("chain budget initialized")
            .total_consumed_cost_micros = chain_cost;
        self.receipts.insert(receipt_id, receipt);
        Ok(())
    }

    /// Persist verified provider spend that exceeded the node authorization.
    ///
    /// The node remains unsettled so its full reservation can never become
    /// reusable headroom. Actual spend is retained separately for waste
    /// attribution, including the amount above the allocation.
    pub fn record_failed_execution_spend(
        &mut self,
        spend: MeasuredExecutionSpend,
    ) -> Result<(), WorkGraphError> {
        validate_measured_spend(&spend)?;
        if self.failed_spend.get(&spend.node_id) == Some(&spend) {
            return Ok(());
        }
        if self.failed_spend.contains_key(&spend.node_id)
            || self
                .receipts
                .values()
                .any(|receipt| receipt.node_id == spend.node_id)
        {
            return Err(WorkGraphError::DuplicateReceipt(spend.spend_id));
        }
        let node = self
            .nodes
            .get(&spend.node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(spend.node_id.clone()))?;
        if node.parent_node_id.is_none() || node.status != NodeStatus::Active {
            return Err(WorkGraphError::InvalidTransition(spend.node_id));
        }
        if node.execution_fence.as_deref() != Some(spend.execution_fence.as_str()) {
            return Err(WorkGraphError::StaleExecutionFence(spend.node_id));
        }
        if spend.tokens_consumed <= node.budget.tokens_allocated
            && spend.cost_micros_consumed <= node.budget.cost_micros_allocated
        {
            return Err(WorkGraphError::InvalidBudget);
        }

        let node_id = spend.node_id.clone();
        let chain_id = self
            .chain_id_for_node(&node_id)
            .expect("failed spend node always has a root");
        let unauthorized_tokens = spend
            .tokens_consumed
            .saturating_sub(node.budget.tokens_allocated);
        let unauthorized_cost = spend
            .cost_micros_consumed
            .saturating_sub(node.budget.cost_micros_allocated);
        self.stop(&node_id, StopReason::BudgetExhausted)?;
        let chain = self
            .chain_budgets
            .get_mut(&chain_id)
            .expect("failed spend chain budget initialized");
        chain.unauthorized_tokens = chain
            .unauthorized_tokens
            .saturating_add(unauthorized_tokens);
        chain.unauthorized_cost_micros = chain
            .unauthorized_cost_micros
            .saturating_add(unauthorized_cost);
        self.failed_spend.insert(node_id, spend);
        Ok(())
    }

    /// Return the validated terminal outcome for a child node.
    pub fn evaluate_child_outcome(&self, node_id: &str) -> Result<ChildOutcome, WorkGraphError> {
        validate_identifier(node_id, "node_id")?;
        self.receipts
            .values()
            .find(|receipt| receipt.node_id == node_id)
            .map(|receipt| receipt.outcome)
            .ok_or_else(|| WorkGraphError::MissingReceipt(node_id.to_string()))
    }

    /// Mark accepted result nodes. Every marked node must have an accepted
    /// terminal receipt; partial results cannot establish acceptance. Ancestors
    /// are included deterministically.
    pub fn mark_accepted_path(&mut self, node_ids: &[String]) -> Result<(), WorkGraphError> {
        let mut accepted = BTreeSet::new();
        for node_id in node_ids {
            validate_identifier(node_id, "node_id")?;
            let receipt = self
                .receipts
                .values()
                .find(|receipt| receipt.node_id == *node_id)
                .ok_or_else(|| WorkGraphError::MissingReceipt(node_id.clone()))?;
            if receipt.outcome != ChildOutcome::Accepted {
                return Err(WorkGraphError::RejectedAcceptedPath(node_id.clone()));
            }
            accepted.extend(self.node_lineage(node_id));
        }
        self.accepted_path = accepted;
        Ok(())
    }

    pub fn is_on_accepted_path(&self, node_id: &str) -> bool {
        self.accepted_path.contains(node_id)
    }

    /// Fuse accepted claims in stable key/value/node order. Conflicting keys
    /// are excluded from accepted output and represented explicitly.
    pub fn fuse_accepted_results(&self) -> Result<FusedResults, WorkGraphError> {
        let mut candidates: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        for receipt in self.receipts.values() {
            if !self.accepted_path.contains(&receipt.node_id) {
                continue;
            }
            for claim in &receipt.claims {
                candidates
                    .entry(claim.key.clone())
                    .or_default()
                    .entry(claim.value.clone())
                    .or_default()
                    .push(receipt.node_id.clone());
            }
        }
        let mut accepted = BTreeMap::new();
        let mut conflicts = Vec::new();
        for (key, values) in candidates {
            if values.len() == 1 {
                let (value, _) = values.into_iter().next().expect("one value");
                accepted.insert(key, value);
            } else {
                let mut node_ids: Vec<String> = values.values().flatten().cloned().collect();
                node_ids.sort();
                node_ids.dedup();
                conflicts.push(ResultConflict {
                    key,
                    values: values.into_keys().collect(),
                    node_ids,
                });
            }
        }
        Ok(FusedResults {
            accepted,
            conflicts,
        })
    }

    pub fn attribution(&self) -> Result<WorkAttribution, WorkGraphError> {
        let mut result = WorkAttribution {
            accepted_tokens: 0,
            accepted_cost_micros: 0,
            waste_tokens: 0,
            waste_cost_micros: 0,
        };
        for node in self.nodes.values() {
            let (tokens, cost) = self.failed_spend.get(&node.node_id).map_or(
                (
                    node.budget.tokens_consumed,
                    node.budget.cost_micros_consumed,
                ),
                |spend| (spend.tokens_consumed, spend.cost_micros_consumed),
            );
            let (token_total, cost_total) = if self.accepted_path.contains(&node.node_id) {
                (
                    &mut result.accepted_tokens,
                    &mut result.accepted_cost_micros,
                )
            } else {
                (&mut result.waste_tokens, &mut result.waste_cost_micros)
            };
            *token_total = token_total
                .checked_add(tokens)
                .ok_or(WorkGraphError::Overflow)?;
            *cost_total = cost_total
                .checked_add(cost)
                .ok_or(WorkGraphError::Overflow)?;
        }
        Ok(result)
    }

    pub fn team_value_report(&self) -> Result<TeamValueReport, WorkGraphError> {
        let attribution = self.attribution()?;
        let total_tokens = attribution
            .accepted_tokens
            .checked_add(attribution.waste_tokens)
            .ok_or(WorkGraphError::Overflow)?;
        let total_cost_micros = attribution
            .accepted_cost_micros
            .checked_add(attribution.waste_cost_micros)
            .ok_or(WorkGraphError::Overflow)?;
        let accepted_cost_basis_points = if total_cost_micros == 0 {
            0
        } else {
            u16::try_from(
                u128::from(attribution.accepted_cost_micros).saturating_mul(10_000)
                    / u128::from(total_cost_micros),
            )
            .unwrap_or(10_000)
        };
        Ok(TeamValueReport {
            accepted_tokens: attribution.accepted_tokens,
            total_tokens,
            accepted_cost_micros: attribution.accepted_cost_micros,
            total_cost_micros,
            waste_tokens: attribution.waste_tokens,
            waste_cost_micros: attribution.waste_cost_micros,
            accepted_cost_basis_points,
        })
    }
}
