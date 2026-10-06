// SPDX-License-Identifier: Apache-2.0

//! Bounded Work Graph for multi-agent orchestration (P11 / DIM 4).
//!
//! Manages parent/child agent delegation with:
//! - Budget inheritance (child cannot exceed parent)
//! - Fan-out limits (max concurrent children)
//! - Stop conditions (stale, over-budget, redundant)
//! - Provenance tracking for attribution

use std::collections::{BTreeMap, BTreeSet};

use crate::core::a2a::budget_cascade::{
    BudgetAllocation, CascadeError, cascade_budget, validate_cascade,
};
use serde::{Deserialize, Serialize};

pub const WORK_GRAPH_SCHEMA_VERSION: u16 = 1;
const MAX_GRAPH_NODES: usize = 256;
const MAX_FAN_OUT: usize = 16;
const MAX_DEPTH: u16 = 8;
const MAX_LOCAL_CONCURRENCY: usize = 32;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_RESULT_CLAIMS: usize = 64;
const MAX_PATH_CLAIMS: usize = 64;

fn default_local_concurrency() -> usize {
    MAX_FAN_OUT
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildOutcome {
    Accepted,
    Rejected,
    Partial,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResultClaim {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChildExecutionReceipt {
    pub receipt_id: String,
    pub node_id: String,
    pub outcome_ref: String,
    pub outcome: ChildOutcome,
    pub tokens_consumed: u64,
    pub cost_micros_consumed: u64,
    /// Opaque claim fence returned when queued work becomes active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_fence: Option<String>,
    #[serde(default)]
    pub claims: Vec<ResultClaim>,
}

/// Canonically verified provider spend that exceeded its authorization.
///
/// This is deliberately separate from [`WorkNodeBudget`]: authorization stays
/// bounded while attribution preserves the provider-reported fact in full.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MeasuredExecutionSpend {
    pub spend_id: String,
    pub node_id: String,
    pub outcome_refs: Vec<String>,
    pub tokens_consumed: u64,
    pub cost_micros_consumed: u64,
    pub execution_fence: String,
}

impl MeasuredExecutionSpend {
    pub fn new(
        node_id: String,
        outcome_refs: Vec<String>,
        tokens_consumed: u64,
        cost_micros_consumed: u64,
        execution_fence: String,
    ) -> Self {
        let spend_id = measured_spend_id(&outcome_refs);
        Self {
            spend_id,
            node_id,
            outcome_refs,
            tokens_consumed,
            cost_micros_consumed,
            execution_fence,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResultConflict {
    pub key: String,
    pub values: Vec<String>,
    pub node_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FusedResults {
    pub accepted: BTreeMap<String, String>,
    pub conflicts: Vec<ResultConflict>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkAttribution {
    pub accepted_tokens: u64,
    pub accepted_cost_micros: u64,
    pub waste_tokens: u64,
    pub waste_cost_micros: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TeamValueReport {
    pub accepted_tokens: u64,
    pub total_tokens: u64,
    pub accepted_cost_micros: u64,
    pub total_cost_micros: u64,
    pub waste_tokens: u64,
    pub waste_cost_micros: u64,
    pub accepted_cost_basis_points: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Active,
    Completed,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    BudgetExhausted,
    Stale,
    Redundant,
    ParentStopped,
    ManualStop,
    DepthExceeded,
    FanOutExceeded,
    PolicyDenied,
    LeaseLost,
    Duplicate,
    LowValue,
    ExecutionFailed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkNodeBudget {
    pub tokens_allocated: u64,
    pub tokens_consumed: u64,
    pub cost_micros_allocated: u64,
    pub cost_micros_consumed: u64,
}

impl WorkNodeBudget {
    pub fn tokens_remaining(&self) -> u64 {
        self.tokens_allocated.saturating_sub(self.tokens_consumed)
    }

    pub fn cost_remaining(&self) -> u64 {
        self.cost_micros_allocated
            .saturating_sub(self.cost_micros_consumed)
    }

    pub fn is_exhausted(&self) -> bool {
        self.tokens_remaining() == 0 || self.cost_remaining() == 0
    }
}

/// Tracks the total budget across an entire delegation chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainBudget {
    pub chain_id: String,
    pub root_budget_tokens: u64,
    pub total_consumed_tokens: u64,
    pub total_allocated_tokens: u64,
    #[serde(default)]
    pub root_budget_cost_micros: u64,
    #[serde(default)]
    pub total_consumed_cost_micros: u64,
    #[serde(default)]
    pub total_allocated_cost_micros: u64,
    /// Verified provider spend above node authorization; never reusable.
    #[serde(default)]
    pub unauthorized_tokens: u64,
    #[serde(default)]
    pub unauthorized_cost_micros: u64,
    pub depth: u16,
}

impl ChainBudget {
    pub fn remaining(&self) -> u64 {
        self.root_budget_tokens
            .saturating_sub(self.total_consumed_tokens)
            .saturating_sub(self.unauthorized_tokens)
    }

    pub fn utilization_pct(&self) -> f64 {
        if self.root_budget_tokens == 0 {
            return 0.0;
        }

        (self
            .total_consumed_tokens
            .saturating_add(self.unauthorized_tokens) as f64
            / self.root_budget_tokens as f64)
            * 100.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkNode {
    pub node_id: String,
    pub agent_id: String,
    pub parent_node_id: Option<String>,
    pub capsule_ref: String,
    pub status: NodeStatus,
    pub budget: WorkNodeBudget,
    pub depth: u16,
    pub stop_reason: Option<StopReason>,
    pub outcome_ref: Option<String>,
    #[serde(default)]
    pub claim_attempt: u32,
    /// Current connector attempt within this execution fence, persisted before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_attempt: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_fence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_epoch_ms: Option<u64>,
    #[serde(default)]
    pub path_claims: Vec<String>,
    #[serde(default)]
    pub task_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_ref: Option<String>,
    #[serde(default)]
    pub policy_ref: String,
    #[serde(default)]
    pub expected_outcome_ref: String,
    #[serde(default)]
    pub execution_started: bool,
}

/// Bounded, acyclic work graph with enforced fan-out and budget constraints.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoundedWorkGraph {
    #[serde(default)]
    graph_id: String,
    nodes: BTreeMap<String, WorkNode>,
    children: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    chain_budgets: BTreeMap<String, ChainBudget>,
    #[serde(default)]
    pending_child_budgets: BTreeMap<String, WorkNodeBudget>,
    #[serde(default)]
    pending_child_parents: BTreeMap<String, String>,
    #[serde(default)]
    receipts: BTreeMap<String, ChildExecutionReceipt>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    failed_spend: BTreeMap<String, MeasuredExecutionSpend>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    reaped_executions: BTreeMap<String, String>,
    #[serde(default)]
    accepted_path: BTreeSet<String>,
    #[serde(default = "default_local_concurrency")]
    max_local_concurrency: usize,
    max_fan_out: usize,
    max_depth: u16,
}

impl Default for BoundedWorkGraph {
    fn default() -> Self {
        Self::new(MAX_FAN_OUT, MAX_DEPTH)
    }
}

#[path = "work_graph/control_ops.rs"]
mod control_ops;
#[path = "work_graph/graph_ops.rs"]
mod graph_ops;

fn execution_fence(graph_id: &str, node: &WorkNode) -> String {
    let material = format!(
        "{}\0{}\0{}\0{}\0{}",
        graph_id, node.node_id, node.agent_id, node.capsule_ref, node.claim_attempt
    );
    format!("fence:{}", blake3::hash(material.as_bytes()).to_hex())
}

pub(crate) fn path_claims_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), WorkGraphError> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES || value.chars().any(char::is_control)
    {
        return Err(WorkGraphError::InvalidIdentifier(field));
    }
    Ok(())
}

fn validate_budget(budget: &WorkNodeBudget) -> Result<(), WorkGraphError> {
    if budget.tokens_consumed > budget.tokens_allocated
        || budget.cost_micros_consumed > budget.cost_micros_allocated
    {
        return Err(WorkGraphError::InvalidBudget);
    }
    Ok(())
}

fn validate_receipt(receipt: &ChildExecutionReceipt) -> Result<(), WorkGraphError> {
    validate_identifier(&receipt.receipt_id, "receipt_id")?;
    validate_identifier(&receipt.node_id, "node_id")?;
    validate_identifier(&receipt.outcome_ref, "outcome_ref")?;
    if let Some(fence) = receipt.execution_fence.as_deref()
        && (fence.len() != 70
            || !fence.starts_with("fence:")
            || !fence[6..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(WorkGraphError::InvalidIdentifier("execution_fence"));
    }
    if receipt.claims.len() > MAX_RESULT_CLAIMS {
        return Err(WorkGraphError::TooManyClaims);
    }
    let mut keys = BTreeSet::new();
    for claim in &receipt.claims {
        validate_identifier(&claim.key, "claim_key")?;
        validate_identifier(&claim.value, "claim_value")?;
        if !keys.insert(&claim.key) {
            return Err(WorkGraphError::DuplicateClaim(claim.key.clone()));
        }
    }
    Ok(())
}

fn validate_measured_spend(spend: &MeasuredExecutionSpend) -> Result<(), WorkGraphError> {
    validate_identifier(&spend.spend_id, "spend_id")?;
    validate_identifier(&spend.node_id, "node_id")?;
    if spend.outcome_refs.is_empty() || spend.outcome_refs.len() > 2 {
        return Err(WorkGraphError::InvalidGraph(
            "failed spend must reference one or two receipts".into(),
        ));
    }
    let mut outcome_refs = BTreeSet::new();
    for outcome_ref in &spend.outcome_refs {
        validate_identifier(outcome_ref, "outcome_ref")?;
        let digest = outcome_ref
            .strip_prefix("id:sha256:")
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(WorkGraphError::InvalidIdentifier("outcome_ref"))?;
        if !outcome_refs.insert(digest) {
            return Err(WorkGraphError::InvalidGraph(
                "duplicate failed spend receipt".into(),
            ));
        }
    }
    if spend.spend_id != measured_spend_id(&spend.outcome_refs) {
        return Err(WorkGraphError::InvalidIdentifier("spend_id"));
    }
    if spend.execution_fence.len() != 70
        || !spend.execution_fence.starts_with("fence:")
        || !spend.execution_fence[6..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkGraphError::InvalidIdentifier("execution_fence"));
    }
    Ok(())
}

fn measured_spend_id(outcome_refs: &[String]) -> String {
    format!(
        "spend:{}",
        blake3::hash(outcome_refs.join("\0").as_bytes()).to_hex()
    )
}

fn validate_path_claim(value: &str) -> Result<(), WorkGraphError> {
    let relative = value
        .strip_prefix("path:")
        .ok_or(WorkGraphError::InvalidPathClaim)?;
    if relative.is_empty()
        || relative.len() > 1_024
        || relative.starts_with('/')
        || relative.starts_with('\\')
        || relative
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
        || relative.chars().any(char::is_control)
    {
        return Err(WorkGraphError::InvalidPathClaim);
    }
    Ok(())
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum WorkGraphError {
    #[error("graph at capacity ({MAX_GRAPH_NODES} nodes)")]
    CapacityExceeded,
    #[error("duplicate node: {0}")]
    DuplicateNode(String),
    #[error("node not found: {0}")]
    NodeNotFound(String),
    #[error("parent not active: {0}")]
    ParentNotActive(String),
    #[error("depth exceeds max {0}")]
    DepthExceeded(u16),
    #[error("fan-out exceeds max {0}")]
    FanOutExceeded(usize),
    #[error("child budget ({child_requested}) exceeds parent remaining ({parent_remaining})")]
    BudgetExceedsParent {
        child_requested: u64,
        parent_remaining: u64,
    },
    #[error("invalid status transition for node: {0}")]
    InvalidTransition(String),
    #[error("local concurrency exceeds max {0}")]
    LocalConcurrencyExceeded(usize),
    #[error("invalid or oversized {0}")]
    InvalidIdentifier(&'static str),
    #[error("consumed budget exceeds allocation")]
    InvalidBudget,
    #[error("duplicate execution receipt: {0}")]
    DuplicateReceipt(String),
    #[error("execution receipt exceeds node or chain budget: {0}")]
    ReceiptExceedsBudget(String),
    #[error("missing execution receipt for accepted node: {0}")]
    MissingReceipt(String),
    #[error("unaccepted node cannot establish an accepted path: {0}")]
    RejectedAcceptedPath(String),
    #[error("execution receipt has too many result claims")]
    TooManyClaims,
    #[error("node has too many path claims")]
    TooManyPathClaims,
    #[error("invalid project-relative path claim")]
    InvalidPathClaim,
    #[error("duplicate result claim key: {0}")]
    DuplicateClaim(String),
    #[error("work graph accounting overflow")]
    Overflow,
    #[error("stale or missing execution fence for node: {0}")]
    StaleExecutionFence(String),
    #[error("invalid work graph: {0}")]
    InvalidGraph(String),
    #[error("budget cascade error: {0}")]
    Cascade(#[from] CascadeError),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "work_graph/tests.rs"]
pub mod tests;
