// SPDX-License-Identifier: Apache-2.0

use super::path_claims_overlap;
use super::{
    BTreeSet, BoundedWorkGraph, ChainBudget, ChildOutcome, MAX_DEPTH, MAX_FAN_OUT, MAX_GRAPH_NODES,
    MAX_LOCAL_CONCURRENCY, MAX_PATH_CLAIMS, NodeStatus, StopReason, WorkGraphError, WorkNode,
    WorkNodeBudget, execution_fence, validate_budget, validate_identifier, validate_measured_spend,
    validate_path_claim, validate_receipt,
};

impl BoundedWorkGraph {
    /// Validate every persisted structural and accounting invariant before use.
    pub fn validate_invariants(&self) -> Result<(), WorkGraphError> {
        for (node_id, fence) in &self.reaped_executions {
            if !self.nodes.get(node_id).is_some_and(|node| {
                node.execution_started && node.execution_fence.as_ref() == Some(fence)
            }) {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid execution exit acknowledgement".into(),
                ));
            }
        }
        if !self.graph_id.is_empty() {
            validate_identifier(&self.graph_id, "graph_id")?;
        }
        if self.nodes.len() > MAX_GRAPH_NODES
            || self.max_fan_out == 0
            || self.max_fan_out > MAX_FAN_OUT
            || self.max_depth == 0
            || self.max_depth > MAX_DEPTH
            || self.max_local_concurrency == 0
            || self.max_local_concurrency > MAX_LOCAL_CONCURRENCY
            || self.active_count() > self.local_concurrency_limit()
        {
            return Err(WorkGraphError::InvalidGraph(
                "configured limit exceeded".to_string(),
            ));
        }
        for (node_id, node) in &self.nodes {
            validate_identifier(node_id, "node_id")?;
            validate_identifier(&node.node_id, "node_id")?;
            validate_identifier(&node.agent_id, "agent_id")?;
            validate_identifier(&node.capsule_ref, "capsule_ref")?;
            validate_budget(&node.budget)?;
            for (field, value) in [
                ("task_ref", node.task_ref.as_str()),
                ("policy_ref", node.policy_ref.as_str()),
                ("expected_outcome_ref", node.expected_outcome_ref.as_str()),
            ] {
                if !value.is_empty() {
                    validate_identifier(value, field)?;
                }
            }
            if let Some(parent_task_ref) = node.parent_task_ref.as_deref() {
                validate_identifier(parent_task_ref, "parent_task_ref")?;
            }
            if let Some(parent_id) = node.parent_node_id.as_deref()
                && !node.task_ref.is_empty()
            {
                let parent = self
                    .nodes
                    .get(parent_id)
                    .ok_or_else(|| WorkGraphError::InvalidGraph("missing context parent".into()))?;
                if node.parent_task_ref.as_deref() != Some(parent.task_ref.as_str())
                    || node.policy_ref != parent.policy_ref
                {
                    return Err(WorkGraphError::InvalidGraph(
                        "task lineage or policy inheritance mismatch".into(),
                    ));
                }
            }
            if node.path_claims.len() > MAX_PATH_CLAIMS {
                return Err(WorkGraphError::TooManyPathClaims);
            }
            let mut path_claims = BTreeSet::new();
            for claim in &node.path_claims {
                validate_path_claim(claim)?;
                if !path_claims.insert(claim) {
                    return Err(WorkGraphError::InvalidGraph(
                        "duplicate path claim".to_string(),
                    ));
                }
            }
            if node_id != &node.node_id {
                return Err(WorkGraphError::InvalidGraph(
                    "node key mismatch".to_string(),
                ));
            }
            if let Some(fence) = node.execution_fence.as_deref()
                && fence != execution_fence(&self.graph_id, node)
            {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid execution fence".to_string(),
                ));
            }
            if node.delivery_attempt.is_some_and(|attempt| {
                !(1..=crate::core::work_graph_executor::MAX_ATTEMPTS).contains(&attempt)
            }) || (node.delivery_attempt.is_some()
                && (!node.execution_started || node.execution_fence.is_none()))
            {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid delivery attempt".into(),
                ));
            }
            let lineage = self.checked_lineage(node_id)?;
            if usize::from(node.depth) + 1 != lineage.len() || node.depth > self.max_depth {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid node depth".to_string(),
                ));
            }
            if let Some(parent_id) = node.parent_node_id.as_deref()
                && !self
                    .children
                    .get(parent_id)
                    .is_some_and(|children| children.contains(node_id))
            {
                return Err(WorkGraphError::InvalidGraph(
                    "parent/child mismatch".to_string(),
                ));
            }
            if node.parent_node_id.is_none() && !self.chain_budgets.contains_key(node_id) {
                return Err(WorkGraphError::InvalidGraph(
                    "missing chain budget".to_string(),
                ));
            }
        }
        for (parent_id, children) in &self.children {
            if !self.nodes.contains_key(parent_id)
                || children.len() > self.max_fan_out
                || children.iter().collect::<BTreeSet<_>>().len() != children.len()
            {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid child index".to_string(),
                ));
            }
            for child_id in children {
                if self
                    .nodes
                    .get(child_id)
                    .and_then(|node| node.parent_node_id.as_deref())
                    != Some(parent_id)
                {
                    return Err(WorkGraphError::InvalidGraph("dangling child".to_string()));
                }
            }
            let (tokens, cost) = self.reserved_child_budget(parent_id, None)?;
            let parent = &self.nodes[parent_id];
            if tokens > parent.budget.tokens_remaining() || cost > parent.budget.cost_remaining() {
                return Err(WorkGraphError::InvalidGraph(
                    "child budget over-allocation".to_string(),
                ));
            }
        }
        for (child_id, budget) in &self.pending_child_budgets {
            validate_identifier(child_id, "pending_child_id")?;
            validate_budget(budget)?;
            if self.nodes.contains_key(child_id)
                || !self.pending_child_parents.contains_key(child_id)
            {
                return Err(WorkGraphError::InvalidGraph(
                    "invalid pending budget".to_string(),
                ));
            }
        }
        for receipt in self.receipts.values() {
            validate_receipt(receipt)?;
            let node = self
                .nodes
                .get(&receipt.node_id)
                .ok_or_else(|| WorkGraphError::InvalidGraph("orphan receipt".to_string()))?;
            if node.parent_node_id.is_none()
                || node.budget.tokens_consumed != receipt.tokens_consumed
                || node.budget.cost_micros_consumed != receipt.cost_micros_consumed
                || (node.execution_fence.is_some()
                    && receipt.execution_fence != node.execution_fence)
            {
                return Err(WorkGraphError::InvalidGraph(
                    "receipt/node mismatch".to_string(),
                ));
            }
        }
        for (node_id, spend) in &self.failed_spend {
            validate_measured_spend(spend)?;
            let node = self
                .nodes
                .get(node_id)
                .ok_or_else(|| WorkGraphError::InvalidGraph("orphan failed spend".into()))?;
            if node_id != &spend.node_id
                || node.parent_node_id.is_none()
                || node.status != NodeStatus::Stopped
                || node.stop_reason != Some(StopReason::BudgetExhausted)
                || node.execution_fence.as_deref() != Some(spend.execution_fence.as_str())
                || (spend.tokens_consumed <= node.budget.tokens_allocated
                    && spend.cost_micros_consumed <= node.budget.cost_micros_allocated)
                || self
                    .receipts
                    .values()
                    .any(|receipt| receipt.node_id == *node_id)
                || self.accepted_path.contains(node_id)
            {
                return Err(WorkGraphError::InvalidGraph(
                    "failed spend/node mismatch".into(),
                ));
            }
        }
        for node_id in &self.accepted_path {
            if !self.nodes.contains_key(node_id) {
                return Err(WorkGraphError::InvalidGraph(
                    "orphan accepted path".to_string(),
                ));
            }
        }
        let mut supported_path = BTreeSet::new();
        for receipt in self.receipts.values() {
            if receipt.outcome == ChildOutcome::Accepted
                && self.accepted_path.contains(&receipt.node_id)
            {
                supported_path.extend(self.checked_lineage(&receipt.node_id)?);
            }
        }
        if supported_path != self.accepted_path {
            return Err(WorkGraphError::InvalidGraph(
                "accepted path lacks accepted receipt lineage".to_string(),
            ));
        }
        for (chain_id, chain) in &self.chain_budgets {
            let root = self
                .nodes
                .get(chain_id)
                .filter(|node| node.parent_node_id.is_none())
                .ok_or_else(|| WorkGraphError::InvalidGraph("invalid chain root".to_string()))?;
            let mut allocated_tokens = 0_u64;
            let mut allocated_cost = 0_u64;
            let mut consumed_tokens = 0_u64;
            let mut consumed_cost = 0_u64;
            let mut unauthorized_tokens = 0_u64;
            let mut unauthorized_cost = 0_u64;
            let mut depth = 0_u16;
            for node in self.nodes.values() {
                if self.checked_lineage(&node.node_id)?.first() != Some(chain_id) {
                    continue;
                }
                allocated_tokens = allocated_tokens
                    .checked_add(node.budget.tokens_allocated)
                    .ok_or(WorkGraphError::Overflow)?;
                allocated_cost = allocated_cost
                    .checked_add(node.budget.cost_micros_allocated)
                    .ok_or(WorkGraphError::Overflow)?;
                consumed_tokens = consumed_tokens
                    .checked_add(node.budget.tokens_consumed)
                    .ok_or(WorkGraphError::Overflow)?;
                consumed_cost = consumed_cost
                    .checked_add(node.budget.cost_micros_consumed)
                    .ok_or(WorkGraphError::Overflow)?;
                depth = depth.max(node.depth);
                if let Some(spend) = self.failed_spend.get(&node.node_id) {
                    unauthorized_tokens = unauthorized_tokens.saturating_add(
                        spend
                            .tokens_consumed
                            .saturating_sub(node.budget.tokens_allocated),
                    );
                    unauthorized_cost = unauthorized_cost.saturating_add(
                        spend
                            .cost_micros_consumed
                            .saturating_sub(node.budget.cost_micros_allocated),
                    );
                }
            }
            for (child_id, budget) in &self.pending_child_budgets {
                let Some(parent_id) = self.pending_child_parents.get(child_id) else {
                    continue;
                };
                if self.checked_lineage(parent_id)?.first() == Some(chain_id) {
                    let pending_depth = self.nodes[parent_id]
                        .depth
                        .checked_add(1)
                        .ok_or(WorkGraphError::Overflow)?;
                    depth = depth.max(pending_depth);
                    allocated_tokens = allocated_tokens
                        .checked_add(budget.tokens_allocated)
                        .ok_or(WorkGraphError::Overflow)?;
                    allocated_cost = allocated_cost
                        .checked_add(budget.cost_micros_allocated)
                        .ok_or(WorkGraphError::Overflow)?;
                }
            }
            if chain.chain_id != *chain_id
                || chain.root_budget_tokens != root.budget.tokens_allocated
                || chain.root_budget_cost_micros != root.budget.cost_micros_allocated
                || chain.total_allocated_tokens != allocated_tokens
                || chain.total_allocated_cost_micros != allocated_cost
                || chain.total_consumed_tokens != consumed_tokens
                || chain.total_consumed_cost_micros != consumed_cost
                || chain.unauthorized_tokens != unauthorized_tokens
                || chain.unauthorized_cost_micros != unauthorized_cost
                || chain.depth != depth
                || consumed_tokens > chain.root_budget_tokens
                || consumed_cost > chain.root_budget_cost_micros
            {
                return Err(WorkGraphError::InvalidGraph(
                    "chain accounting mismatch".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn root_agent_for(&self, node_id: &str) -> Result<&str, WorkGraphError> {
        let root_id = self
            .checked_lineage(node_id)?
            .into_iter()
            .next()
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
        Ok(self.nodes[&root_id].agent_id.as_str())
    }

    fn checked_lineage(&self, node_id: &str) -> Result<Vec<String>, WorkGraphError> {
        let mut lineage = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current_id = Some(node_id);
        while let Some(id) = current_id {
            if !seen.insert(id.to_string()) {
                return Err(WorkGraphError::InvalidGraph("cycle detected".to_string()));
            }
            let node = self
                .nodes
                .get(id)
                .ok_or_else(|| WorkGraphError::NodeNotFound(id.to_string()))?;
            lineage.push(id.to_string());
            current_id = node.parent_node_id.as_deref();
        }
        lineage.reverse();
        Ok(lineage)
    }

    /// Stop a node and all its descendants (cascade).
    pub fn stop(
        &mut self,
        node_id: &str,
        reason: StopReason,
    ) -> Result<Vec<String>, WorkGraphError> {
        if !self.nodes.contains_key(node_id) {
            return Err(WorkGraphError::NodeNotFound(node_id.to_string()));
        }
        let mut stopped = Vec::new();
        self.stop_recursive(node_id, reason, &mut stopped);
        Ok(stopped)
    }

    /// Record token consumption on a node.
    pub fn consume_budget(
        &mut self,
        node_id: &str,
        tokens: u64,
        cost_micros: u64,
    ) -> Result<bool, WorkGraphError> {
        let (next_node_tokens, next_node_cost, exhausted) = {
            let node = self
                .nodes
                .get(node_id)
                .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.to_string()))?;
            if node.status != NodeStatus::Active {
                return Err(WorkGraphError::InvalidTransition(node_id.to_string()));
            }
            let next_tokens = node
                .budget
                .tokens_consumed
                .checked_add(tokens)
                .ok_or(WorkGraphError::Overflow)?;
            let next_cost = node
                .budget
                .cost_micros_consumed
                .checked_add(cost_micros)
                .ok_or(WorkGraphError::Overflow)?;
            if next_tokens > node.budget.tokens_allocated
                || next_cost > node.budget.cost_micros_allocated
            {
                return Err(WorkGraphError::InvalidBudget);
            }
            let (reserved_tokens, reserved_cost) = self.reserved_child_budget(node_id, None)?;
            if tokens
                > node
                    .budget
                    .tokens_remaining()
                    .saturating_sub(reserved_tokens)
                || cost_micros > node.budget.cost_remaining().saturating_sub(reserved_cost)
            {
                return Err(WorkGraphError::InvalidBudget);
            }
            (
                next_tokens,
                next_cost,
                next_tokens == node.budget.tokens_allocated
                    || next_cost == node.budget.cost_micros_allocated,
            )
        };
        let chain_id = self
            .chain_id_for_node(node_id)
            .expect("node always has a root node");
        let (next_chain_tokens, next_chain_cost, chain_exhausted) = {
            let chain = self
                .chain_budgets
                .get(&chain_id)
                .ok_or_else(|| WorkGraphError::InvalidGraph("missing chain budget".into()))?;
            let next_tokens = chain
                .total_consumed_tokens
                .checked_add(tokens)
                .ok_or(WorkGraphError::Overflow)?;
            let next_cost = chain
                .total_consumed_cost_micros
                .checked_add(cost_micros)
                .ok_or(WorkGraphError::Overflow)?;
            if next_tokens.saturating_add(chain.unauthorized_tokens) > chain.root_budget_tokens
                || next_cost.saturating_add(chain.unauthorized_cost_micros)
                    > chain.root_budget_cost_micros
            {
                return Err(WorkGraphError::InvalidBudget);
            }
            (
                next_tokens,
                next_cost,
                next_tokens.saturating_add(chain.unauthorized_tokens) == chain.root_budget_tokens
                    || next_cost.saturating_add(chain.unauthorized_cost_micros)
                        == chain.root_budget_cost_micros,
            )
        };

        // Commit only after all node, child-reservation and chain checks pass.
        let node = self.nodes.get_mut(node_id).expect("node checked above");
        node.budget.tokens_consumed = next_node_tokens;
        node.budget.cost_micros_consumed = next_node_cost;
        let chain = self
            .chain_budgets
            .get_mut(&chain_id)
            .expect("chain checked above");
        chain.total_consumed_tokens = next_chain_tokens;
        chain.total_consumed_cost_micros = next_chain_cost;

        if chain_exhausted {
            self.stop_recursive(&chain_id, StopReason::BudgetExhausted, &mut Vec::new());
            return Ok(true);
        }
        if exhausted {
            let node = self.nodes.get_mut(node_id).expect("node checked above");
            node.status = NodeStatus::Stopped;
            node.stop_reason = Some(StopReason::BudgetExhausted);
            node.lease_expires_epoch_ms = None;
            return Ok(true);
        }

        Ok(false)
    }

    /// Records token consumption for a node and updates its chain budget.
    pub fn consume_tokens(&mut self, node_id: &str, tokens: u64) -> Result<(), WorkGraphError> {
        self.consume_budget(node_id, tokens, 0)?;
        Ok(())
    }

    /// Returns the chain budget for a given node's chain.
    pub fn chain_budget_for(&self, node_id: &str) -> Option<&ChainBudget> {
        let chain_id = self.chain_id_for_node(node_id)?;
        self.chain_budgets.get(&chain_id)
    }

    /// Returns all chains at or above their utilization threshold.
    pub fn over_budget_chains(&self, threshold_pct: f64) -> Vec<&ChainBudget> {
        self.chain_budgets
            .values()
            .filter(|budget| budget.utilization_pct() >= threshold_pct)
            .collect()
    }

    /// Check all nodes for stop conditions and cascade.
    pub fn enforce_stop_conditions(&mut self) -> Vec<(String, StopReason)> {
        let exhausted: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.status == NodeStatus::Active && n.budget.is_exhausted())
            .map(|(id, _)| id.clone())
            .collect();
        let mut stopped = Vec::new();
        for node_id in exhausted {
            let mut cascade = Vec::new();
            self.stop_recursive(&node_id, StopReason::BudgetExhausted, &mut cascade);
            for id in cascade {
                stopped.push((id, StopReason::BudgetExhausted));
            }
        }
        stopped
    }

    pub fn get_node(&self, node_id: &str) -> Option<&WorkNode> {
        self.nodes.get(node_id)
    }

    /// Advance only the current running incarnation; retries revoke the prior
    /// attempt's delivery authority without changing the node claim fence.
    pub(crate) fn begin_delivery_attempt(
        &mut self,
        node_id: &str,
        fence: &str,
        attempt: u8,
    ) -> Result<(), WorkGraphError> {
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| WorkGraphError::InvalidTransition(node_id.into()))?;
        if node.status != NodeStatus::Active
            || !node.execution_started
            || node.execution_fence.as_deref() != Some(fence)
            || !(1..=crate::core::work_graph_executor::MAX_ATTEMPTS).contains(&attempt)
            || node.delivery_attempt.unwrap_or(0).checked_add(1) != Some(attempt)
        {
            return Err(WorkGraphError::InvalidTransition(node_id.into()));
        }
        node.delivery_attempt = Some(attempt);
        Ok(())
    }

    pub fn contains_agent(&self, agent_id: &str) -> bool {
        self.nodes.values().any(|node| node.agent_id == agent_id)
    }

    pub fn children_of(&self, node_id: &str) -> &[String] {
        self.children.get(node_id).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn has_live_path_conflict(
        &self,
        requested: &[String],
        _now_epoch_ms: u64,
        except_node_id: Option<&str>,
    ) -> bool {
        self.nodes.values().any(|node| {
            Some(node.node_id.as_str()) != except_node_id
                && self.execution_exit_unconfirmed(node)
                && node.path_claims.iter().any(|held| {
                    requested
                        .iter()
                        .any(|claim| path_claims_overlap(held, claim))
                })
        })
    }

    pub fn active_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.status == NodeStatus::Active || self.execution_exit_unconfirmed(n))
            .count()
    }

    pub(super) fn execution_exit_unconfirmed(&self, node: &WorkNode) -> bool {
        node.execution_started
            && (node.execution_fence.is_none()
                || self.reaped_executions.get(&node.node_id) != node.execution_fence.as_ref())
    }

    /// Only the owning executor's verified capture path may acknowledge exit.
    pub(crate) fn acknowledge_execution_exit(
        &mut self,
        node_id: &str,
        fence: &str,
    ) -> Result<(), WorkGraphError> {
        let node = self
            .nodes
            .get(node_id)
            .ok_or_else(|| WorkGraphError::NodeNotFound(node_id.into()))?;
        if !node.execution_started || node.execution_fence.as_deref() != Some(fence) {
            return Err(WorkGraphError::InvalidTransition(node_id.into()));
        }
        self.reaped_executions.insert(node_id.into(), fence.into());
        Ok(())
    }

    pub fn total_count(&self) -> usize {
        self.nodes.len()
    }

    #[allow(clippy::collapsible_if)]
    fn stop_recursive(&mut self, node_id: &str, reason: StopReason, stopped: &mut Vec<String>) {
        if let Some(node) = self.nodes.get_mut(node_id) {
            if matches!(node.status, NodeStatus::Active | NodeStatus::Pending) {
                node.status = NodeStatus::Stopped;
                node.stop_reason = Some(reason);
                node.lease_expires_epoch_ms = None;
                stopped.push(node_id.to_string());
            }
        }
        let children: Vec<String> = self.children.get(node_id).cloned().unwrap_or_default();
        for child_id in children {
            self.stop_recursive(&child_id, StopReason::ParentStopped, stopped);
        }
    }

    pub(super) fn chain_id_for_node(&self, node_id: &str) -> Option<String> {
        let mut current_id = node_id;
        let mut current = self.nodes.get(current_id)?;
        while let Some(parent_id) = current.parent_node_id.as_deref() {
            current_id = parent_id;
            current = self.nodes.get(current_id)?;
        }
        Some(current_id.to_string())
    }

    pub(super) fn node_lineage(&self, node_id: &str) -> Vec<String> {
        let mut lineage = Vec::new();
        let mut current_id = Some(node_id);
        while let Some(id) = current_id {
            let Some(node) = self.nodes.get(id) else {
                break;
            };
            lineage.push(id.to_string());
            current_id = node.parent_node_id.as_deref();
        }
        lineage.reverse();
        lineage
    }

    pub(super) fn reserved_child_budget(
        &self,
        parent_id: &str,
        exclude_child_id: Option<&str>,
    ) -> Result<(u64, u64), WorkGraphError> {
        let mut tokens = 0_u64;
        let mut cost = 0_u64;
        let mut pending_parents = vec![parent_id];
        let mut visited = BTreeSet::from([parent_id]);
        while let Some(current_parent) = pending_parents.pop() {
            for child_id in self.children.get(current_parent).into_iter().flatten() {
                if current_parent == parent_id && Some(child_id.as_str()) == exclude_child_id {
                    continue;
                }
                if !visited.insert(child_id.as_str()) {
                    return Err(WorkGraphError::InvalidGraph(
                        "duplicate child or cycle".into(),
                    ));
                }
                let child = self
                    .nodes
                    .get(child_id)
                    .ok_or_else(|| WorkGraphError::InvalidGraph("dangling child".to_string()))?;
                // A terminal status alone does not settle usage: complete() can run
                // without a receipt, and cancellation can leave provider usage unknown.
                let settled = matches!(child.status, NodeStatus::Completed | NodeStatus::Failed)
                    // Unfenced legacy receipts bypass canonical usage verification
                    // at the registered tool boundary and cannot authorize reuse.
                    && child.execution_fence.is_some()
                    && self.receipts.values().any(|receipt| {
                        receipt.node_id == *child_id
                            && receipt.tokens_consumed == child.budget.tokens_consumed
                            && receipt.cost_micros_consumed == child.budget.cost_micros_consumed
                            && receipt.execution_fence == child.execution_fence
                    });
                let (child_tokens, child_cost) = if settled {
                    // Keep actual spend and all descendant commitments reserved; only
                    // the settled node's unused allocation becomes available again.
                    pending_parents.push(child_id.as_str());
                    (
                        child.budget.tokens_consumed,
                        child.budget.cost_micros_consumed,
                    )
                } else {
                    (
                        child.budget.tokens_allocated,
                        child.budget.cost_micros_allocated,
                    )
                };
                tokens = tokens
                    .checked_add(child_tokens)
                    .ok_or(WorkGraphError::Overflow)?;
                cost = cost
                    .checked_add(child_cost)
                    .ok_or(WorkGraphError::Overflow)?;
            }
            for (child_id, budget) in &self.pending_child_budgets {
                if (current_parent == parent_id && Some(child_id.as_str()) == exclude_child_id)
                    || self.pending_child_parents.get(child_id).map(String::as_str)
                        != Some(current_parent)
                {
                    continue;
                }
                tokens = tokens
                    .checked_add(budget.tokens_allocated)
                    .ok_or(WorkGraphError::Overflow)?;
                cost = cost
                    .checked_add(budget.cost_micros_allocated)
                    .ok_or(WorkGraphError::Overflow)?;
            }
        }
        Ok((tokens, cost))
    }

    pub(super) fn ensure_chain_budget(&mut self, chain_id: &str) {
        let root_budget = self
            .nodes
            .get(chain_id)
            .map(|node| node.budget.clone())
            .unwrap_or(WorkNodeBudget {
                tokens_allocated: 0,
                tokens_consumed: 0,
                cost_micros_allocated: 0,
                cost_micros_consumed: 0,
            });
        self.chain_budgets
            .entry(chain_id.to_string())
            .or_insert(ChainBudget {
                chain_id: chain_id.to_string(),
                root_budget_tokens: root_budget.tokens_allocated,
                total_consumed_tokens: 0,
                total_allocated_tokens: root_budget.tokens_allocated,
                root_budget_cost_micros: root_budget.cost_micros_allocated,
                total_consumed_cost_micros: 0,
                total_allocated_cost_micros: root_budget.cost_micros_allocated,
                unauthorized_tokens: 0,
                unauthorized_cost_micros: 0,
                depth: 0,
            });
    }

    pub(super) fn record_chain_allocation(
        &mut self,
        chain_id: &str,
        previous_tokens: u64,
        tokens_allocated: u64,
        previous_cost_micros: u64,
        cost_micros_allocated: u64,
        depth: u16,
    ) {
        self.ensure_chain_budget(chain_id);
        let chain = self
            .chain_budgets
            .get_mut(chain_id)
            .expect("chain budget initialized");
        chain.total_allocated_tokens = chain
            .total_allocated_tokens
            .saturating_sub(previous_tokens)
            .saturating_add(tokens_allocated);
        chain.total_allocated_cost_micros = chain
            .total_allocated_cost_micros
            .saturating_sub(previous_cost_micros)
            .saturating_add(cost_micros_allocated);
        chain.depth = chain.depth.max(depth);
    }
}
