// SPDX-License-Identifier: Apache-2.0

//! Task-owned planning snapshot transported explicitly across async/blocking dispatch.

#[cfg(test)]
mod tests;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use chrono::{DateTime, FixedOffset};
use lean_ctx_protocol::{TaskEnvelopeV1, TaskId};

use super::{KernelEnrichment, enrichment_from_decision, reference_input};
use crate::core::context_kernel::{
    autopilot::{AutopilotController, TaskAutopilotDecision},
    enforce::{KernelMode, resolve_mode},
    types::RetrievalContext,
};
use crate::core::outcome::contracts::TaskClass;

/// No mutable plan view escapes this task-bound snapshot.
#[derive(Debug)]
pub(crate) struct PreparedKernelContext {
    query: String,
    project_root: String,
    budget: usize,
    mode: KernelMode,
    decision: TaskAutopilotDecision,
    delivered: AtomicBool,
}

impl PreparedKernelContext {
    // Compatibility constructor is now used only by internal test fixtures.
    #[cfg(test)]
    pub(crate) fn plan(
        task_id: TaskId,
        query: String,
        project_root: String,
        budget: usize,
        task_class: TaskClass,
        explicit_mode: Option<String>,
    ) -> anyhow::Result<Self> {
        Self::plan_with_evaluation_time(
            task_id,
            query,
            project_root,
            budget,
            task_class,
            explicit_mode,
            None,
            None,
        )
    }

    /// Plan from the admitted task envelope's validated creation time.
    pub(crate) fn plan_for_envelope(
        envelope: TaskEnvelopeV1,
        query: String,
        project_root: String,
        budget: usize,
        task_class: TaskClass,
        explicit_mode: Option<String>,
    ) -> anyhow::Result<Self> {
        let evaluation_time = DateTime::parse_from_rfc3339(&envelope.created_at)
            .map_err(|_| anyhow::anyhow!("invalid task evaluation timestamp"))?;
        // The scope the task's outcomes are learned under, and so its policy.
        let scope = crate::core::context_store::task_scope(
            envelope.tenant_id.as_ref(),
            &envelope.project_id,
        );
        Self::plan_with_evaluation_time(
            envelope.task_id,
            query,
            project_root,
            budget,
            task_class,
            explicit_mode,
            Some(evaluation_time),
            Some(&scope),
        )
    }

    fn plan_with_evaluation_time(
        task_id: TaskId,
        query: String,
        project_root: String,
        budget: usize,
        task_class: TaskClass,
        explicit_mode: Option<String>,
        evaluation_time: Option<DateTime<FixedOffset>>,
        scope: Option<&str>,
    ) -> anyhow::Result<Self> {
        let budget = budget.min(150);
        anyhow::ensure!(budget > 0, "kernel context budget is disabled");
        let mode = resolve_mode(&project_root);
        let mut input = reference_input(
            RetrievalContext {
                query: query.clone(),
                task: Some(query.clone()),
                project_root: project_root.clone(),
                budget: crate::core::context_field::TokenBudget {
                    total: budget,
                    used: 0,
                },
                max_candidates: 20,
            },
            mode,
        )?;
        input.task_class = task_class;
        input.overrides.read_mode = explicit_mode;
        input.evaluation_time = evaluation_time;
        input.context_policy = scope.and_then(super::super::autopilot::ScopeContextPolicy::load);
        let decision =
            AutopilotController::for_project(&project_root).plan_for_task(task_id, &input, None)?;
        Ok(Self {
            query,
            project_root,
            budget,
            mode,
            decision,
            delivered: AtomicBool::new(false),
        })
    }

    pub(crate) fn decision(&self) -> &TaskAutopilotDecision {
        &self.decision
    }

    pub(super) fn enrich(
        &self,
        query: &str,
        project_root: &str,
        budget: usize,
    ) -> Option<KernelEnrichment> {
        // Inputs changing after admission never trigger a second planner or
        // reuse context from another task/workspace/budget.
        if query != self.query
            || project_root != self.project_root
            || budget.min(150) != self.budget
        {
            return None;
        }
        let enrichment =
            enrichment_from_decision(self.decision.decision().clone(), self.budget, self.mode)?;
        // One supplement budget per task, including batch-read consumers.
        self.delivered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(enrichment)
    }
}

/// Legacy callers remain supported; a failed admitted plan suppresses replanning.
#[derive(Debug, Clone, Default)]
pub(crate) enum KernelPlanningHandoff {
    #[default]
    Legacy,
    Suppressed,
    Prepared(Arc<PreparedKernelContext>),
}

tokio::task_local! {
    pub(crate) static KERNEL_PLANNING_HANDOFF: KernelPlanningHandoff;
}

pub(crate) fn current_handoff() -> KernelPlanningHandoff {
    KERNEL_PLANNING_HANDOFF
        .try_with(Clone::clone)
        .unwrap_or_default()
}

pub(crate) fn planned_project_root() -> Option<String> {
    match current_handoff() {
        KernelPlanningHandoff::Prepared(prepared) => Some(prepared.project_root.clone()),
        _ => None,
    }
}

/// Compose caches include the exact planning decision, not just task text/root.
pub(crate) fn compose_cache_task(query: &str) -> String {
    let identity = match current_handoff() {
        KernelPlanningHandoff::Legacy => None,
        KernelPlanningHandoff::Suppressed => Some("suppressed".to_owned()),
        KernelPlanningHandoff::Prepared(prepared) => {
            Some(prepared.decision.decision().decision_id.clone())
        }
    };
    serde_json::to_string(&("kernel-context-v1", query, identity)).expect("strings serialize")
}
