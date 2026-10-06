// SPDX-License-Identifier: Apache-2.0

//! External planning adapter; selection and policy stay in the canonical kernel.

#[path = "context_plan/materialize.rs"]
pub(crate) mod materialize_impl;
pub(crate) mod sources;

#[cfg(test)]
#[path = "context_plan/source_handoff_tests.rs"]
mod source_handoff_tests;

use std::path::Path;

use chrono::{DateTime, FixedOffset};

use lean_ctx_protocol::{
    EngineContextPlanRequestV1, EngineContextPlanResponseV1,
    EngineContextSourceMaterializationRequestV1, EngineContextSourceMaterializationResponseV1,
    SemanticVersion,
};

use super::{
    ENGINE_INTERFACE_VERSION, ENGINE_TRANSPORT_VERSION, EngineTransportError, bind_transport_root,
};
use crate::core::context_field::TokenBudget;
use crate::core::context_kernel::{
    autopilot::{AutopilotController, TaskAutopilotDecision},
    bridge::reference_input,
    enforce::KernelMode,
    types::RetrievalContext,
};

/// Replan and materialize an explicit source plan using the canonical planner.
pub(crate) fn materialize(
    root: &Path,
    request: &EngineContextSourceMaterializationRequestV1,
) -> Result<EngineContextSourceMaterializationResponseV1, &'static str> {
    materialize_impl::materialize_source_plan(root, request)
}

/// Preserve the actual private kernel decision alongside materialized context.
pub(crate) fn materialize_sources_with_decision(
    root: &Path,
    request: &EngineContextSourceMaterializationRequestV1,
) -> Result<
    (
        EngineContextSourceMaterializationResponseV1,
        TaskAutopilotDecision,
    ),
    &'static str,
> {
    materialize_impl::materialize_source_plan_with_decision(root, request)
}

/// Local operator authority only: no tenant authorization is inferred from task labels.
pub(crate) fn plan(
    root: &Path,
    request: EngineContextPlanRequestV1,
) -> Result<EngineContextPlanResponseV1, &'static str> {
    response(&plan_with_controller(root, request, None)?)
}

fn plan_with_controller(
    root: &Path,
    request: EngineContextPlanRequestV1,
    controller: Option<AutopilotController>,
) -> Result<TaskAutopilotDecision, &'static str> {
    plan_with_controller_at(
        root,
        request,
        controller,
        Some(chrono::Utc::now().fixed_offset()),
    )
}

/// Run the canonical planner at an explicit identity time, never an admission grant.
///
/// The time is only an input to the kernel's retention-aware plan identity;
/// callers must separately perform fresh admission checks before materializing
/// a replayed plan.
pub(crate) fn plan_with_controller_at(
    root: &Path,
    request: EngineContextPlanRequestV1,
    controller: Option<AutopilotController>,
    evaluation_time: Option<DateTime<FixedOffset>>,
) -> Result<TaskAutopilotDecision, &'static str> {
    request.validate_payload().map_err(|_| "invalid_request")?;
    let root = bind_transport_root(root).map_err(EngineTransportError::code)?;
    let root = root.to_str().ok_or("unsafe_root")?;
    let mut input = reference_input(
        RetrievalContext {
            task: Some(request.query.clone()),
            query: request.query,
            project_root: root.to_owned(),
            budget: TokenBudget {
                total: request.budget_tokens as usize,
                used: 0,
            },
            max_candidates: usize::from(request.max_candidates),
        },
        // The external surface cannot select shadow mode to relax host policy.
        KernelMode::Enforce,
    )
    .map_err(|_| "context_policy_unavailable")?;
    input.evaluation_time = evaluation_time;
    controller
        .unwrap_or_else(|| AutopilotController::for_project(root))
        .plan_for_task(request.task_id, &input, None)
        .map_err(|_| "context_planning_failed")
}

fn response(decision: &TaskAutopilotDecision) -> Result<EngineContextPlanResponseV1, &'static str> {
    Ok(EngineContextPlanResponseV1 {
        schema_version: 1,
        transport_version: ENGINE_TRANSPORT_VERSION,
        engine_interface_version: SemanticVersion::new(ENGINE_INTERFACE_VERSION)
            .map_err(|_| "internal")?,
        plan: decision.context_projection().clone(),
    })
}
