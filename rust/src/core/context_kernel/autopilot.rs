// SPDX-License-Identifier: Apache-2.0

//! Canonical Context Autopilot over the existing Context Kernel.
//!
//! The kernel remains the sole owner of candidate selection and budgeting. This
//! module selects the planner tier and the policies around that semantic plan.

#[cfg(test)]
#[path = "autopilot_determinism_tests.rs"]
mod determinism_tests;

pub mod learning_store;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::{DateTime, FixedOffset};
use lean_ctx_protocol::{AcceptanceState, ContextPlanProjectionV1, ExecutionPlanV1, TaskId};
use serde::{Deserialize, Serialize};

use crate::core::{
    active_inference,
    auto_mode_resolver::resolve_mode_precedence,
    context_field::{ContextField, FieldWeights},
    execution_protocol::ValidatedExecutionProtocolV1,
    outcome::contracts::TaskClass,
    provider_bandit::ProviderBandit,
};

use super::{
    enforce::KernelMode,
    orchestrator::{ContextKernel, KernelPlanError},
    policy::ContextPolicy,
    projection::project_context_plan,
    types::{
        ContextOriginV1, ContextPlanV1, DeferredEntry, ExcludedEntry, PlanBudget, PlanEntry,
        ProviderStat, ReceiptOutcome, RetrievalContext,
    },
};

/// The kernel plan is the one canonical semantic context plan.
pub type ContextPlan = ContextPlanV1;

const DEFAULT_CONFIDENCE_THRESHOLD_MILLI: u16 = 650;
const MAX_LEARNING_OBSERVATIONS: u32 = 10_000;
const MAX_LEARNING_MODES: usize = 32;
const MAX_PRELOAD_ITEMS: usize = 4;
const MAX_PRELOAD_TOKENS: usize = 4_096;
const MAX_PRELOAD_TOKENS_PER_ITEM: usize = 2_048;
const AUTOPILOT_DECISION_REF: &str = "context_autopilot_decision_ref";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerTier {
    Community,
    AdaptivePro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextNeed {
    Minimal,
    Balanced,
    Deep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningStrategy {
    Deterministic,
    Adaptive,
    SafeFallback,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOverrides {
    pub read_mode: Option<String>,
    pub no_compression: bool,
    pub no_routing: bool,
    pub no_telemetry: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopilotEconomics {
    pub baseline_cost_micros: u64,
    pub candidate_cost_micros: u64,
    pub annotation_overhead_micros: u64,
    pub tool_schema_overhead_micros: u64,
    pub cache_read_micros: u64,
    pub cache_write_micros: u64,
    pub expected_quality_value_micros: u64,
    pub expected_latency_value_micros: u64,
}

impl AutopilotEconomics {
    #[must_use]
    pub fn candidate_total_micros(&self) -> u64 {
        self.candidate_cost_micros
            .saturating_add(self.annotation_overhead_micros)
            .saturating_add(self.tool_schema_overhead_micros)
            .saturating_add(self.cache_read_micros)
            .saturating_add(self.cache_write_micros)
    }

    #[must_use]
    pub fn expected_value_micros(&self) -> u64 {
        self.expected_quality_value_micros
            .saturating_add(self.expected_latency_value_micros)
    }

    #[must_use]
    pub fn net_value_micros(&self) -> i64 {
        let benefit =
            i128::from(self.baseline_cost_micros) + i128::from(self.expected_value_micros());
        let cost = i128::from(self.candidate_total_micros());
        i64::try_from((benefit - cost).clamp(i128::from(i64::MIN), i128::from(i64::MAX)))
            .expect("clamped net value fits i64")
    }

    #[must_use]
    pub fn is_positive(&self) -> bool {
        self.net_value_micros() > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreloadBudget {
    pub max_items: usize,
    pub max_tokens: usize,
    pub tokens_per_item: usize,
    pub cost_per_item_micros: u64,
    pub value_per_hit_micros: u64,
    pub allow_remote_queries: bool,
    pub remote_query_consent: bool,
}

impl Default for PreloadBudget {
    fn default() -> Self {
        Self {
            max_items: 3,
            max_tokens: 1_500,
            tokens_per_item: 500,
            cost_per_item_micros: 0,
            value_per_hit_micros: 0,
            allow_remote_queries: false,
            remote_query_consent: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLearningState {
    accepted: u32,
    rejected: u32,
    partial: u32,
    preload_hits: u32,
    preload_misses: u32,
    learned_mode: Option<String>,
    modes: BTreeMap<String, ModeLearningState>,
    #[serde(default)]
    processed_receipts: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeLearningState {
    pub accepted: u32,
    pub rejected: u32,
    pub partial: u32,
}

impl AdaptiveLearningState {
    /// Unknown outcomes never train the paid planner.
    fn observe_outcome(&mut self, outcome: ReceiptOutcome) {
        match outcome {
            ReceiptOutcome::Accepted => bounded_increment(&mut self.accepted),
            ReceiptOutcome::Rejected => bounded_increment(&mut self.rejected),
            ReceiptOutcome::Partial => bounded_increment(&mut self.partial),
            ReceiptOutcome::Unknown => {}
        }
    }

    /// Attribute a typed terminal outcome to the mode that produced it.
    fn observe_mode_outcome(&mut self, mode: &str, outcome: ReceiptOutcome) -> bool {
        if outcome == ReceiptOutcome::Unknown
            || !valid_learned_mode(mode)
            || (!self.modes.contains_key(mode) && self.modes.len() >= MAX_LEARNING_MODES)
        {
            return false;
        }
        self.observe_outcome(outcome);
        let state = self.modes.entry(mode.to_owned()).or_default();
        match outcome {
            ReceiptOutcome::Accepted => bounded_increment(&mut state.accepted),
            ReceiptOutcome::Rejected => bounded_increment(&mut state.rejected),
            ReceiptOutcome::Partial => bounded_increment(&mut state.partial),
            ReceiptOutcome::Unknown => unreachable!("unknown returned before mutation"),
        }
        self.learned_mode = best_learned_mode(&self.modes);
        true
    }

    #[must_use]
    pub fn preload_hit_rate_milli(&self) -> Option<u16> {
        let total = self.preload_hits.saturating_add(self.preload_misses);
        (total > 0).then(|| {
            u16::try_from(self.preload_hits.saturating_mul(1_000) / total)
                .expect("preload hit rate is bounded to 1000")
        })
    }

    #[must_use]
    pub fn predictive_preload_enabled(&self) -> bool {
        let observations = self.preload_hits.saturating_add(self.preload_misses);
        observations < 4
            || self
                .preload_hit_rate_milli()
                .is_some_and(|rate| rate >= 250)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Explicit state restoration; runtime outcomes must use validated protocol admission.
    pub fn import_json(value: &str) -> Result<Self, serde_json::Error> {
        if value.len() > learning_store::MAX_STATE_BYTES {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "learning state exceeds byte limit",
            ));
        }
        let mut state: Self = serde_json::from_str(value)?;
        if state.processed_receipts.len() > MAX_LEARNING_OBSERVATIONS as usize {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "replay history exceeds capacity; receipt IDs must not be discarded",
            ));
        }
        for receipt in &state.processed_receipts {
            lean_ctx_protocol::ReceiptId::new(receipt.clone())
                .map_err(<serde_json::Error as serde::de::Error>::custom)?;
        }
        state.accepted = state.accepted.min(MAX_LEARNING_OBSERVATIONS);
        state.rejected = state.rejected.min(MAX_LEARNING_OBSERVATIONS);
        state.partial = state.partial.min(MAX_LEARNING_OBSERVATIONS);
        state.preload_hits = state.preload_hits.min(MAX_LEARNING_OBSERVATIONS);
        state.preload_misses = state.preload_misses.min(MAX_LEARNING_OBSERVATIONS);
        state.modes.retain(|mode, _| valid_learned_mode(mode));
        state.modes = state.modes.into_iter().take(MAX_LEARNING_MODES).collect();
        for mode in state.modes.values_mut() {
            mode.accepted = mode.accepted.min(MAX_LEARNING_OBSERVATIONS);
            mode.rejected = mode.rejected.min(MAX_LEARNING_OBSERVATIONS);
            mode.partial = mode.partial.min(MAX_LEARNING_OBSERVATIONS);
        }
        state.learned_mode = best_learned_mode(&state.modes);
        Ok(state)
    }
}

fn valid_learned_mode(mode: &str) -> bool {
    !mode.is_empty()
        && mode.len() <= 64
        && mode
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
}

fn best_learned_mode(modes: &BTreeMap<String, ModeLearningState>) -> Option<String> {
    modes
        .iter()
        .filter_map(|(mode, state)| {
            let total = state
                .accepted
                .saturating_add(state.rejected)
                .saturating_add(state.partial);
            (total > 0).then(|| {
                let score = state
                    .accepted
                    .saturating_mul(1_000)
                    .saturating_add(state.partial.saturating_mul(500))
                    / total;
                (mode, score, total)
            })
        })
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| right.0.cmp(left.0))
        })
        .map(|(mode, _, _)| mode.clone())
}

fn bounded_increment(value: &mut u32) {
    *value = value.saturating_add(1).min(MAX_LEARNING_OBSERVATIONS);
}

#[derive(Debug, Clone)]
pub struct AutopilotInput {
    pub retrieval: RetrievalContext,
    /// Validated task-envelope time used for deterministic retention checks.
    pub evaluation_time: Option<DateTime<FixedOffset>>,
    pub task_class: TaskClass,
    pub entitled_to_adaptive: bool,
    pub confidence_milli: u16,
    pub configured_mode: Option<String>,
    pub default_mode: String,
    /// A non-overridable security or organisation policy decision.
    pub security_forced_mode: Option<String>,
    pub overrides: UserOverrides,
    pub policy: ContextPolicy,
    pub kernel_mode: KernelMode,
    pub economics: AutopilotEconomics,
    pub learning: AdaptiveLearningState,
    pub available_providers: Vec<String>,
    /// Providers proven local; every unclassified provider is treated as remote.
    pub local_providers: BTreeSet<String>,
    pub cached_preloads: BTreeSet<String>,
    pub preload_budget: PreloadBudget,
    /// The scope's promoted read-strategy policy, if a runtime promoted one.
    pub context_policy: Option<ScopeContextPolicy>,
}

/// A promoted policy (`context_store::policy_store`) as planning sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeContextPolicy {
    pub policy: lean_ctx_protocol::context_policy_evidence::ContextPolicyV1,
    /// Use the policy as the learned mode; otherwise it is only recorded.
    pub apply: bool,
}

impl ScopeContextPolicy {
    /// The active policy of `scope` and the user's apply switch.
    pub(crate) fn load(scope: &str) -> Option<Self> {
        let policy = crate::core::context_store::policy_store::load(scope)
            .active?
            .policy;
        let apply = crate::core::config::Config::load_global()
            .intelligence_runtime
            .context_policy_apply;
        Some(Self { policy, apply })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPolicy {
    pub mode: String,
    pub explicit_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalPolicy {
    pub semantic_search: bool,
    pub graph_search: bool,
    pub max_candidates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryPolicy {
    pub recall: bool,
    pub consolidate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub allow_provider_routing: bool,
    pub proxy_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSurfacePolicy {
    pub expose_mcp_tools: bool,
    pub telemetry_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPolicy {
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionReason {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreloadAction {
    pub provider_id: String,
    pub action: String,
    pub estimated_tokens: usize,
    pub expected_net_value_micros: i64,
    pub low_priority: bool,
    pub cancellable: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct PreloadCancellation(Arc<AtomicBool>);

impl PreloadCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait PreloadExecutor {
    type Error;

    fn preload_low_priority(
        &mut self,
        action: &PreloadAction,
        cancellation: &PreloadCancellation,
    ) -> Result<(), Self::Error>;
}

/// Execute the bounded low-priority batch while honoring cancellation between calls.
pub fn execute_preloads<E: PreloadExecutor>(
    actions: &[PreloadAction],
    cancellation: &PreloadCancellation,
    executor: &mut E,
) -> Vec<Result<(), E::Error>> {
    let mut results = Vec::with_capacity(actions.len().min(MAX_PRELOAD_ITEMS));
    let mut used_tokens = 0usize;
    for action in actions.iter().take(MAX_PRELOAD_ITEMS) {
        if cancellation.is_cancelled() {
            break;
        }
        if action.estimated_tokens > MAX_PRELOAD_TOKENS_PER_ITEM
            || action.estimated_tokens > MAX_PRELOAD_TOKENS.saturating_sub(used_tokens)
        {
            break;
        }
        used_tokens = used_tokens.saturating_add(action.estimated_tokens);
        results.push(executor.preload_low_priority(action, cancellation));
    }
    results
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakEvenDecision {
    pub baseline_cost_micros: u64,
    pub candidate_total_micros: u64,
    pub expected_value_micros: u64,
    pub net_value_micros: i64,
    pub positive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowComparison {
    /// Legacy wire flag: the comparison itself is never an execution authority.
    /// The plan IDs below identify which evaluated plan is actually executable.
    #[serde(default)]
    pub authoritative: bool,
    /// The baseline is evaluated for comparison, never dispatched by this decision.
    #[serde(rename = "baseline_plan_id", alias = "counterfactual_plan_id")]
    pub counterfactual_plan_id: String,
    pub baseline_mode: String,
    /// The adaptive candidate is the single plan returned for execution.
    #[serde(rename = "candidate_plan_id", alias = "authoritative_plan_id")]
    pub authoritative_plan_id: String,
    pub candidate_mode: String,
    pub expected_net_value_micros: i64,
    pub observed: Option<ShadowOutcomeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowOutcomeMetrics {
    pub baseline_cost_micros: u64,
    pub candidate_cost_micros: u64,
    pub baseline_quality_milli: u16,
    pub candidate_quality_milli: u16,
    pub preload_hits: u32,
    pub preload_misses: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowOutcomeEvidence {
    pub evidence_id: String,
    pub decision_id: String,
    pub candidate_plan_id: String,
    pub candidate_mode: String,
    pub metrics: ShadowOutcomeMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowObservationError {
    MissingComparison,
    AlreadyRecorded,
    InvalidQuality,
    TooManyPreloadObservations,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyObservation {
    pub object_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct AutopilotDecision {
    pub decision_id: String,
    pub tier: PlannerTier,
    pub context_plan: ContextPlan,
    pub task_class: TaskClass,
    pub context_need: ContextNeed,
    pub strategy: PlanningStrategy,
    pub read_policy: ReadPolicy,
    pub retrieval_policy: RetrievalPolicy,
    pub memory_policy: MemoryPolicy,
    pub routing_policy: RoutingPolicy,
    pub tool_surface: ToolSurfacePolicy,
    pub checkpoint_policy: CheckpointPolicy,
    pub confidence_milli: u16,
    pub reasons: Vec<DecisionReason>,
    pub preloads: Vec<PreloadAction>,
    pub economics: BreakEvenDecision,
    pub shadow: Option<ShadowComparison>,
    pub policy_observations: Vec<PolicyObservation>,
}

/// Immutable handoff from Context Autopilot to the execution lifecycle.
///
/// The wire projection is derived from the executable kernel plan exactly once.
/// Keeping both fields private prevents a caller from replacing that plan after
/// the task and projection digest have been bound.
#[derive(Debug, Clone)]
pub struct TaskAutopilotDecision {
    decision: AutopilotDecision,
    projection: ContextPlanProjectionV1,
}

impl TaskAutopilotDecision {
    /// Train the executed mode once, only from a validated, exactly matching protocol.
    /// Unknown outcomes and Community decisions never train adaptive state.
    pub fn observe_protocol_outcome(
        &self,
        protocol: &ValidatedExecutionProtocolV1<'_>,
        learning: &mut AdaptiveLearningState,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            protocol.context_plan() == &self.projection,
            "learning outcome does not bind the authoritative context projection"
        );
        self.validate_execution_plan(protocol.execution_plan())?;
        anyhow::ensure!(
            protocol
                .execution_plan()
                .extensions
                .get(AUTOPILOT_DECISION_REF)
                .and_then(serde_json::Value::as_str)
                == Some(self.decision.decision_id.as_str()),
            "learning outcome does not bind the executed autopilot decision and mode"
        );
        protocol.validate_learning_handoff(self)?;
        if self.decision.tier != PlannerTier::AdaptivePro {
            return Ok(false);
        }
        let outcome = match protocol.outcome().accepted {
            AcceptanceState::Accepted => ReceiptOutcome::Accepted,
            AcceptanceState::Rejected => ReceiptOutcome::Rejected,
            AcceptanceState::Unknown => return Ok(false),
        };
        let receipt_id = protocol.receipt_id();
        if learning.processed_receipts.contains(receipt_id)
            || learning.processed_receipts.len() >= MAX_LEARNING_OBSERVATIONS as usize
        {
            return Ok(false);
        }
        if !learning.observe_mode_outcome(&self.decision.read_policy.mode, outcome) {
            return Ok(false);
        }
        learning.processed_receipts.insert(receipt_id.to_owned());
        Ok(true)
    }

    pub fn decision(&self) -> &AutopilotDecision {
        &self.decision
    }

    pub fn context_projection(&self) -> &ContextPlanProjectionV1 {
        &self.projection
    }

    /// Bind internally derived source metadata without replacing the semantic plan.
    /// Consuming the handoff prevents changes through an existing execution borrow.
    pub(crate) fn bind_source_metadata(
        mut self,
        lineage: Option<serde_json::Value>,
        evaluation_time: Option<lean_ctx_protocol::UtcTimestamp>,
    ) -> anyhow::Result<Self> {
        let epoch = evaluation_time.map(|time| {
            serde_json::json!({
                "schema_version": 1, "evaluation_time": time.as_str(),
            })
        });
        let mut changed = false;
        for (key, value) in [
            ("source_lineage_v1", lineage),
            ("context_plan_evaluation_v1", epoch),
        ] {
            if let Some(value) = value {
                anyhow::ensure!(
                    self.projection.extensions.get(key).is_none(),
                    "source metadata already bound"
                );
                self.projection
                    .extensions
                    .insert(key, value)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                changed = true;
            }
        }
        if changed {
            self.projection.projection_digest = Some(
                self.projection
                    .compute_projection_digest()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?,
            );
        }
        self.projection
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(self)
    }

    /// Bind the context policy before the execution plan is hashed and receipted.
    pub fn bind_execution_plan(
        &self,
        mut plan: ExecutionPlanV1,
    ) -> anyhow::Result<ExecutionPlanV1> {
        self.validate_execution_plan(&plan)?;
        if let Some(existing) = plan.extensions.get(AUTOPILOT_DECISION_REF) {
            anyhow::ensure!(
                existing.as_str() == Some(self.decision.decision_id.as_str()),
                "execution plan already belongs to another autopilot decision"
            );
        } else {
            plan.extensions
                .insert(
                    AUTOPILOT_DECISION_REF.to_owned(),
                    serde_json::Value::String(self.decision.decision_id.clone()),
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        plan.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(plan)
    }

    /// Reject a lifecycle plan belonging to a different task or context decision.
    pub fn validate_execution_plan(&self, plan: &ExecutionPlanV1) -> anyhow::Result<()> {
        plan.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            plan.task_id == self.projection.task_id,
            "context task mismatch"
        );
        crate::core::engine_interface::planning::context_binding::validate_projection(
            plan,
            &self.projection,
        )
        .map_err(anyhow::Error::msg)?;
        crate::core::engine_interface::planning::context_binding::validate_handoff(
            plan,
            &self.canonical_bytes()?,
        )
        .map_err(anyhow::Error::msg)?;
        Ok(())
    }

    /// Include task lineage and the canonical wire digest in the handoff bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&(
            &self.projection,
            canonical_decision(&self.decision, Some(&self.decision.decision_id)),
        ))
    }
}

#[derive(Serialize)]
struct CanonicalContextPlan<'a> {
    plan_id: &'a str,
    intent: &'a str,
    budget: &'a PlanBudget,
    selected: &'a [PlanEntry],
    excluded: &'a [ExcludedEntry],
    deferred: &'a [DeferredEntry],
    provider_stats: BTreeMap<&'a str, &'a ProviderStat>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    origins: &'a BTreeMap<String, Vec<ContextOriginV1>>,
}

#[derive(Serialize)]
struct CanonicalDecision<'a> {
    decision_id: Option<&'a str>,
    tier: &'a PlannerTier,
    context_plan: CanonicalContextPlan<'a>,
    task_class: &'a TaskClass,
    context_need: &'a ContextNeed,
    strategy: &'a PlanningStrategy,
    read_policy: &'a ReadPolicy,
    retrieval_policy: &'a RetrievalPolicy,
    memory_policy: &'a MemoryPolicy,
    routing_policy: &'a RoutingPolicy,
    tool_surface: &'a ToolSurfacePolicy,
    checkpoint_policy: &'a CheckpointPolicy,
    confidence_milli: u16,
    reasons: &'a [DecisionReason],
    preloads: &'a [PreloadAction],
    economics: &'a BreakEvenDecision,
    shadow: &'a Option<ShadowComparison>,
    policy_observations: &'a [PolicyObservation],
}

impl AutopilotDecision {
    /// Bind only the authoritative semantic plan; the baseline stays counterfactual.
    pub fn bind_task(self, task_id: TaskId) -> anyhow::Result<TaskAutopilotDecision> {
        anyhow::ensure!(
            self.decision_id == decision_id(&self),
            "autopilot decision changed after its identity was issued"
        );
        if let Some(shadow) = &self.shadow {
            anyhow::ensure!(
                !shadow.authoritative && shadow.authoritative_plan_id == self.context_plan.plan_id,
                "autopilot shadow comparison disagrees with the authoritative plan"
            );
        }
        let mut projection = project_context_plan(task_id, &self.context_plan)?;
        if let Some(selection) = self.reasons.iter().find(|reason| {
            matches!(
                reason.code.as_str(),
                "private_context_budget_relevance" | "private_context_unavailable"
            )
        }) {
            projection
                .extensions
                .insert(
                    "adaptive_context_v1",
                    serde_json::json!({
                            "schema_version": 1, "decision_id": self.decision_id,
                            "status": selection.code, "explanation": selection.detail,
                    "context_prepared": true, "model_transferred": false,
                    "learned_personalization": false,
                    "outcome_learning_eligible": self.tier == PlannerTier::AdaptivePro
                        }),
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            projection.projection_digest = Some(
                projection
                    .compute_projection_digest()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?,
            );
            projection
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        Ok(TaskAutopilotDecision {
            decision: self,
            projection,
        })
    }

    /// Stable representation for receipts, audit logs, and determinism tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&canonical_decision(self, Some(&self.decision_id)))
    }

    /// Attach caller-reported comparison diagnostics, without granting learning authority.
    /// Runtime training exclusively consumes a validated protocol on the task handoff.
    pub fn record_shadow_outcome(
        &mut self,
        metrics: ShadowOutcomeMetrics,
    ) -> Result<String, ShadowObservationError> {
        let shadow = self
            .shadow
            .as_ref()
            .ok_or(ShadowObservationError::MissingComparison)?;
        if shadow.observed.is_some() {
            return Err(ShadowObservationError::AlreadyRecorded);
        }
        if metrics.baseline_quality_milli > 1_000 || metrics.candidate_quality_milli > 1_000 {
            return Err(ShadowObservationError::InvalidQuality);
        }
        if metrics.preload_hits.saturating_add(metrics.preload_misses) > MAX_LEARNING_OBSERVATIONS {
            return Err(ShadowObservationError::TooManyPreloadObservations);
        }

        let originating_decision_id = self.decision_id.clone();
        let candidate_plan_id = shadow.authoritative_plan_id.clone();
        let candidate_mode = shadow.candidate_mode.clone();
        let material = serde_json::to_vec(&(
            &originating_decision_id,
            &candidate_plan_id,
            &candidate_mode,
            &metrics,
        ))
        .expect("bounded shadow evidence serializes");
        let evidence_id = format!("shadow_{}", &blake3::hash(&material).to_hex()[..16]);
        let evidence = ShadowOutcomeEvidence {
            evidence_id: evidence_id.clone(),
            decision_id: originating_decision_id,
            candidate_plan_id,
            candidate_mode: candidate_mode.clone(),
            metrics,
        };
        self.shadow
            .as_mut()
            .expect("comparison was validated above")
            .observed = Some(evidence);
        self.decision_id = decision_id(self);
        Ok(evidence_id)
    }
}

fn canonical_decision<'a>(
    decision: &'a AutopilotDecision,
    decision_id: Option<&'a str>,
) -> CanonicalDecision<'a> {
    CanonicalDecision {
        decision_id,
        tier: &decision.tier,
        context_plan: CanonicalContextPlan {
            plan_id: &decision.context_plan.plan_id,
            intent: &decision.context_plan.intent,
            budget: &decision.context_plan.budget,
            selected: &decision.context_plan.selected,
            excluded: &decision.context_plan.excluded,
            deferred: &decision.context_plan.deferred,
            provider_stats: decision
                .context_plan
                .provider_stats
                .iter()
                .map(|(provider, stats)| (provider.as_str(), stats))
                .collect(),
            origins: &decision.context_plan.origins,
        },
        task_class: &decision.task_class,
        context_need: &decision.context_need,
        strategy: &decision.strategy,
        read_policy: &decision.read_policy,
        retrieval_policy: &decision.retrieval_policy,
        memory_policy: &decision.memory_policy,
        routing_policy: &decision.routing_policy,
        tool_surface: &decision.tool_surface,
        checkpoint_policy: &decision.checkpoint_policy,
        confidence_milli: decision.confidence_milli,
        reasons: &decision.reasons,
        preloads: &decision.preloads,
        economics: &decision.economics,
        shadow: &decision.shadow,
        policy_observations: &decision.policy_observations,
    }
}

pub struct CommunityContextPlanner {
    kernel: ContextKernel,
}

impl CommunityContextPlanner {
    pub fn new(kernel: ContextKernel) -> Self {
        Self { kernel }
    }

    fn plan(&self, input: &AutopilotInput) -> Result<AutopilotDecision, KernelPlanError> {
        let enforced = read_kernel_plan(&self.kernel, input)?;
        let (policy_mode, policy_reason) = context_policy_mode(input, &enforced.plan);
        let mode = resolve_mode(input, policy_mode);
        let explicit_override = has_explicit_mode(input);
        let economics = break_even(&input.economics);
        let mut reasons = vec![reason(
            "community_reference",
            "deterministic Community planner selected",
        )];
        if explicit_override {
            reasons.push(reason("explicit_override", "explicit read policy applied"));
        }
        reasons.extend(policy_reason);
        if let Some(selection) = enforced.selection {
            reasons.push(reason(selection, if selection == "private_context_budget_relevance" {
                "Installed private runtime scored admitted context for query relevance and budget; no learned quality claim"
            } else {
                "Private context selection unavailable; authorized public context retained"
            }));
        }
        let mut result = decision(
            PlannerTier::Community,
            PlanningStrategy::Deterministic,
            enforced.plan,
            input,
            mode,
            explicit_override,
            reasons,
            Vec::new(),
            economics,
            None,
            enforced.observations,
        );
        // Keep the Community/Deterministic tier: AdaptivePro also enables the
        // learned-mode outcome handoff. Context-only scoring grants none of it.
        if enforced.selection == Some("private_context_budget_relevance") {
            result
                .reasons
                .retain(|reason| reason.code != "community_reference");
            result.decision_id = decision_id(&result);
        }
        Ok(result)
    }
}

pub struct AdaptiveContextPlanner {
    kernel: ContextKernel,
}

impl AdaptiveContextPlanner {
    pub fn new(kernel: ContextKernel) -> Self {
        Self { kernel }
    }

    fn plan(
        &self,
        input: &AutopilotInput,
        baseline: &AutopilotDecision,
        bandit: Option<&mut ProviderBandit>,
    ) -> Result<AutopilotDecision, KernelPlanError> {
        let enforced = read_kernel_plan(&self.kernel, input)?;
        let (policy_mode, policy_reason) = context_policy_mode(input, &enforced.plan);
        let mode = resolve_mode(
            input,
            policy_mode.or_else(|| input.learning.learned_mode.clone()),
        );
        let economics = break_even(&input.economics);
        let preloads = bandit.map_or_else(Vec::new, |bandit| planned_preloads(input, bandit));
        let mut reasons = vec![
            reason("adaptive_entitlement", "Adaptive Pro planner selected"),
            reason(
                "positive_expected_value",
                "candidate clears break-even gate",
            ),
        ];
        reasons.extend(policy_reason);
        let shadow = Some(ShadowComparison {
            authoritative: false,
            counterfactual_plan_id: baseline.context_plan.plan_id.clone(),
            baseline_mode: baseline.read_policy.mode.clone(),
            authoritative_plan_id: enforced.plan.plan_id.clone(),
            candidate_mode: mode.clone(),
            expected_net_value_micros: economics.net_value_micros,
            observed: None,
        });
        Ok(decision(
            PlannerTier::AdaptivePro,
            PlanningStrategy::Adaptive,
            enforced.plan,
            input,
            mode,
            has_explicit_mode(input),
            reasons,
            preloads,
            economics,
            shadow,
            enforced.observations,
        ))
    }
}

/// Selects exactly one planner and returns an inspectable, bounded decision.
pub struct AutopilotController {
    community: CommunityContextPlanner,
    adaptive: AdaptiveContextPlanner,
    confidence_threshold_milli: u16,
}

impl AutopilotController {
    /// Plan for an admitted task and return its immutable lifecycle handoff.
    pub fn plan_for_task(
        &self,
        task_id: TaskId,
        input: &AutopilotInput,
        bandit: Option<&mut ProviderBandit>,
    ) -> anyhow::Result<TaskAutopilotDecision> {
        let decision = self.plan(input, bandit)?;
        decision.bind_task(task_id)
    }

    /// Build Community from fixed reference weights and Pro from one learned snapshot.
    pub fn for_project(project_root: &str) -> Self {
        let community_kernel = ContextKernel::with_field(
            super::providers::default_providers(project_root),
            ContextField::with_weights(FieldWeights::default()),
        )
        .with_private_selection();
        let adaptive_kernel = ContextKernel::with_field(
            super::providers::default_providers(project_root),
            ContextField::with_weights(crate::core::context_field::active_weights()),
        );
        Self::with_kernels(community_kernel, adaptive_kernel)
    }

    pub fn with_kernels(community: ContextKernel, adaptive: ContextKernel) -> Self {
        Self {
            community: CommunityContextPlanner::new(community),
            adaptive: AdaptiveContextPlanner::new(adaptive),
            confidence_threshold_milli: DEFAULT_CONFIDENCE_THRESHOLD_MILLI,
        }
    }

    #[must_use]
    pub fn with_confidence_threshold(mut self, threshold_milli: u16) -> Self {
        self.confidence_threshold_milli = threshold_milli.min(1_000);
        self
    }

    pub fn plan(
        &self,
        input: &AutopilotInput,
        bandit: Option<&mut ProviderBandit>,
    ) -> Result<AutopilotDecision, KernelPlanError> {
        let baseline = self.community.plan(input)?;
        if baseline
            .reasons
            .iter()
            .any(|reason| reason.code == "private_context_budget_relevance")
        {
            return Ok(baseline);
        }
        if !input.entitled_to_adaptive {
            return Ok(with_fallback_reason(
                baseline,
                "not_entitled",
                "Community fallback: no Pro entitlement",
            ));
        }
        if input.confidence_milli < self.confidence_threshold_milli {
            return Ok(with_fallback_reason(
                baseline,
                "low_confidence",
                "Community fallback: adaptive confidence below threshold",
            ));
        }
        if !input.economics.is_positive() {
            return Ok(with_fallback_reason(
                baseline,
                "break_even_failed",
                "Community fallback: adaptive expected value is not positive",
            ));
        }
        self.adaptive.plan(input, &baseline, bandit)
    }
}

fn with_fallback_reason(
    mut decision: AutopilotDecision,
    code: &str,
    detail: &str,
) -> AutopilotDecision {
    decision.strategy = PlanningStrategy::SafeFallback;
    decision.reasons.push(reason(code, detail));
    decision.decision_id = decision_id(&decision);
    decision
}

fn resolve_mode(input: &AutopilotInput, learned: Option<String>) -> String {
    if let Some(mode) = &input.security_forced_mode {
        return mode.clone();
    }
    let explicit = if input.overrides.no_compression {
        Some("full".to_owned())
    } else {
        input.overrides.read_mode.clone()
    };
    resolve_mode_precedence(
        explicit,
        input.configured_mode.clone(),
        learned,
        &input.default_mode,
    )
}

/// The promoted policy's strategy for this plan's workload. It enters the
/// precedence as the learned mode only when the user switched it to apply,
/// so security, explicit and configured modes still win; otherwise it is
/// recorded next to the decision and changes nothing.
fn context_policy_mode(
    input: &AutopilotInput,
    plan: &ContextPlan,
) -> (Option<String>, Option<DecisionReason>) {
    let Some(scope) = &input.context_policy else {
        return (None, None);
    };
    let workload = learning_store::workload_of(input.task_class, plan);
    let Some(strategy) = scope.policy.strategy_for(&workload) else {
        return (None, None);
    };
    let mode = strategy.as_str().to_owned();
    let version = scope.policy.version;
    let outranked = has_explicit_mode(input) || input.configured_mode.is_some();
    if scope.apply && !outranked {
        let detail = format!("promoted policy v{version} applied: {mode}");
        (Some(mode), Some(reason("context_policy_applied", &detail)))
    } else {
        let detail = format!("promoted policy v{version} recommends {mode}; not applied");
        (None, Some(reason("context_policy_shadow", &detail)))
    }
}

fn has_explicit_mode(input: &AutopilotInput) -> bool {
    input.security_forced_mode.is_some()
        || input.overrides.no_compression
        || input.overrides.read_mode.is_some()
}

fn planned_preloads(input: &AutopilotInput, bandit: &mut ProviderBandit) -> Vec<PreloadAction> {
    if !input.learning.predictive_preload_enabled()
        || input.preload_budget.max_items == 0
        || input.preload_budget.tokens_per_item == 0
        || input.preload_budget.tokens_per_item > MAX_PRELOAD_TOKENS_PER_ITEM
    {
        return Vec::new();
    }
    let tokens_per_item = input.preload_budget.tokens_per_item;
    let max_tokens = input.preload_budget.max_tokens.min(MAX_PRELOAD_TOKENS);
    let token_bound = max_tokens / tokens_per_item;
    let max_items = input
        .preload_budget
        .max_items
        .min(MAX_PRELOAD_ITEMS)
        .min(token_bound);
    let predictions = active_inference::predict_preloads(
        input
            .retrieval
            .task
            .as_deref()
            .unwrap_or(&input.retrieval.query),
        &input.available_providers,
        bandit,
        max_items,
    );
    predictions
        .into_iter()
        .filter_map(|prediction| {
            let key = format!("{}:{}", prediction.provider_id, prediction.action);
            if input.cached_preloads.contains(&key) {
                return None;
            }
            let remote = !input.local_providers.contains(&prediction.provider_id);
            if remote
                && (!input.preload_budget.allow_remote_queries
                    || !input.preload_budget.remote_query_consent)
            {
                return None;
            }
            let expected_value = (prediction.confidence.clamp(0.0, 1.0)
                * input.preload_budget.value_per_hit_micros as f64)
                .round() as u64;
            let net =
                i128::from(expected_value) - i128::from(input.preload_budget.cost_per_item_micros);
            if net <= 0 {
                return None;
            }
            Some(PreloadAction {
                provider_id: prediction.provider_id,
                action: prediction.action,
                estimated_tokens: tokens_per_item,
                expected_net_value_micros: i64::try_from(net.min(i128::from(i64::MAX)))
                    .expect("bounded preload net value fits i64"),
                low_priority: true,
                cancellable: true,
                reason: prediction.reason,
            })
        })
        .collect()
}

struct EnforcedPlan {
    plan: ContextPlan,
    observations: Vec<PolicyObservation>,
    selection: Option<&'static str>,
}

fn read_kernel_plan(
    kernel: &ContextKernel,
    input: &AutopilotInput,
) -> Result<EnforcedPlan, KernelPlanError> {
    let (plan, blocked, selection) = kernel.plan_with_selection_at(
        &input.retrieval,
        &input.policy,
        input.evaluation_time.as_ref(),
    )?;
    Ok(EnforcedPlan {
        plan,
        selection,
        observations: blocked
            .into_iter()
            .map(|entry| PolicyObservation {
                object_id: entry.object_id,
                reason: entry.reason,
            })
            .collect(),
    })
}

fn break_even(economics: &AutopilotEconomics) -> BreakEvenDecision {
    BreakEvenDecision {
        baseline_cost_micros: economics.baseline_cost_micros,
        candidate_total_micros: economics.candidate_total_micros(),
        expected_value_micros: economics.expected_value_micros(),
        net_value_micros: economics.net_value_micros(),
        positive: economics.is_positive(),
    }
}

#[allow(clippy::too_many_arguments)]
fn decision(
    tier: PlannerTier,
    strategy: PlanningStrategy,
    context_plan: ContextPlan,
    input: &AutopilotInput,
    mode: String,
    explicit_override: bool,
    reasons: Vec<DecisionReason>,
    preloads: Vec<PreloadAction>,
    economics: BreakEvenDecision,
    shadow: Option<ShadowComparison>,
    policy_observations: Vec<PolicyObservation>,
) -> AutopilotDecision {
    let context_need = context_need(input.task_class);
    let adaptive = tier == PlannerTier::AdaptivePro;
    let mut value = AutopilotDecision {
        decision_id: String::new(),
        tier,
        context_plan,
        task_class: input.task_class,
        context_need,
        strategy,
        read_policy: ReadPolicy {
            mode,
            explicit_override,
        },
        retrieval_policy: RetrievalPolicy {
            semantic_search: adaptive && context_need == ContextNeed::Deep,
            graph_search: adaptive && context_need != ContextNeed::Minimal,
            max_candidates: input.retrieval.max_candidates,
        },
        memory_policy: MemoryPolicy {
            recall: adaptive && input.learning.accepted > 0,
            consolidate: adaptive && input.learning.accepted > input.learning.rejected,
        },
        routing_policy: RoutingPolicy {
            allow_provider_routing: adaptive && !input.overrides.no_routing,
            proxy_only: !input.overrides.no_routing && !input.economics.is_positive(),
        },
        tool_surface: ToolSurfacePolicy {
            expose_mcp_tools: input.economics.is_positive(),
            telemetry_enabled: !input.overrides.no_telemetry,
        },
        checkpoint_policy: CheckpointPolicy {
            create: adaptive && context_need == ContextNeed::Deep,
        },
        confidence_milli: input.confidence_milli.min(1_000),
        reasons,
        preloads,
        economics,
        shadow,
        policy_observations,
    };
    value.decision_id = decision_id(&value);
    value
}

fn decision_id(decision: &AutopilotDecision) -> String {
    let material = serde_json::to_vec(&canonical_decision(decision, None))
        .expect("canonical autopilot decision serializes");
    format!("autopilot_{}", &blake3::hash(&material).to_hex()[..16])
}

fn context_need(task_class: TaskClass) -> ContextNeed {
    match task_class {
        TaskClass::Documentation => ContextNeed::Minimal,
        TaskClass::Refactor | TaskClass::TestAddition => ContextNeed::Balanced,
        TaskClass::BugFix | TaskClass::Investigation => ContextNeed::Deep,
    }
}

fn reason(code: &str, detail: &str) -> DecisionReason {
    DecisionReason {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
#[path = "autopilot_tests.rs"]
mod tests;
