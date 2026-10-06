//! Compatibility adapter from MCP events to the canonical execution lifecycle.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::OnceLock,
};

use crate::core::execution_lifecycle::{
    CompletionObservation, ExecutionLifecycle, ProductEntitlements, RuntimeContext, ToolRequest,
    ToolSurface,
};

#[cfg(test)]
use crate::core::triage::TriageEngine;

pub use crate::core::execution_lifecycle::TaskContext;

#[derive(Debug)]
/// Bridges MCP tool lifecycle events into decision-loop accounting.
pub struct DecisionLoopRuntime {
    lifecycle: ExecutionLifecycle,
}

impl DecisionLoopRuntime {
    pub fn get_or_init() -> &'static Self {
        static RUNTIME: OnceLock<DecisionLoopRuntime> = OnceLock::new();
        RUNTIME.get_or_init(|| Self {
            lifecycle: ExecutionLifecycle::global().clone(),
        })
    }

    /// Returns the most recently triaged profile for a session.
    pub fn profile_for_session(
        &self,
        session_id: &str,
    ) -> Option<crate::core::triage::profile::TaskProfileLocal> {
        self.lifecycle.profile_for_session(session_id)
    }

    pub fn on_tool_start(
        &self,
        tool_name: &str,
        query: Option<&str>,
        session_id: &str,
        agent_id: &str,
    ) -> TaskContext {
        catch_unwind(AssertUnwindSafe(|| {
            self.lifecycle.begin(
                ToolRequest {
                    tool_name: tool_name.to_owned(),
                    query: query.map(str::to_owned),
                    session_id: session_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    surface: ToolSurface::Mcp,
                    idempotency_key: None,
                },
                RuntimeContext::default(),
                ProductEntitlements::default(),
            )
        }))
        .unwrap_or_else(|_| {
            self.lifecycle.begin(
                ToolRequest {
                    tool_name: tool_name.to_owned(),
                    query: None,
                    session_id: session_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    surface: ToolSurface::Mcp,
                    idempotency_key: None,
                },
                RuntimeContext::default(),
                ProductEntitlements::default(),
            )
        })
    }

    pub fn on_tool_end(
        &self,
        ctx: &TaskContext,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        success: bool,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        if ctx.outcome().is_some() {
            return None;
        }
        catch_unwind(AssertUnwindSafe(|| {
            self.lifecycle
                .complete_once(
                    ctx,
                    CompletionObservation::tool_result(input_tokens, output_tokens, model, success),
                )
                .and_then(|outcome| outcome.assessment)
        }))
        .ok()
        .flatten()
    }

    /// Records a completed tool and schedules an accepted Shadow Mode sample.
    ///
    /// Shadow work is detached from the MCP response path: inability to spawn
    /// or persist a comparison must never affect the completed tool call.
    pub fn on_tool_end_with_shadow(
        &self,
        ctx: &TaskContext,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        success: bool,
        shadow_auto_record: bool,
        shadow_tokens: Option<(u64, u64)>,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        if ctx.outcome().is_some() {
            return None;
        }
        catch_unwind(AssertUnwindSafe(|| {
            let mut observation =
                CompletionObservation::tool_result(input_tokens, output_tokens, model, success);
            observation.shadow_auto_record = shadow_auto_record;
            observation.shadow_tokens = shadow_tokens;
            self.lifecycle
                .complete_once(ctx, observation)
                .and_then(|outcome| outcome.assessment)
        }))
        .ok()
        .flatten()
    }

    #[cfg(test)]
    pub(crate) fn with_triage(triage: TriageEngine) -> Self {
        Self {
            lifecycle: ExecutionLifecycle::with_triage(triage),
        }
    }

    #[cfg(test)]
    pub(crate) fn assessment_for(
        &self,
        task_id: &str,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        self.lifecycle.assessment_for(task_id)
    }

    #[cfg(test)]
    pub(crate) fn outcome_for(
        &self,
        task_id: &str,
    ) -> Option<crate::core::execution_lifecycle::LifecycleOutcome> {
        self.lifecycle.outcome_for(task_id)
    }
}
