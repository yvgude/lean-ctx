// SPDX-License-Identifier: Apache-2.0

//! Reference-bound supplemental context; never a primary-output budget override.

use lean_ctx_protocol::{
    ContextBudgetPolicyV1, ContextPlanProjectionV1, ExecutionPlanV1, Sha256Digest, TaskEnvelopeV1,
};
use serde::{Deserialize, Serialize};

use crate::core::context_kernel::bridge::runtime;

const EXTENSION: &str = "native_context_handoff_v1";
const DECISION_REF: &str = "context_autopilot_decision_ref";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeContextBinding {
    schema_version: u16,
    budget_scope: BudgetScope,
    task_digest: Sha256Digest,
    handoff_digest: Sha256Digest,
    decision_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BudgetScope {
    Supplement,
}

impl NativeContextBinding {
    pub(super) fn capture(task: &TaskEnvelopeV1) -> Result<Option<Self>, String> {
        let runtime::KernelPlanningHandoff::Prepared(context) = runtime::current_handoff() else {
            return Ok(None);
        };
        if context.decision().context_projection().task_id != task.task_id {
            return Err("native context handoff belongs to another task".into());
        }
        let bytes = context
            .decision()
            .canonical_bytes()
            .map_err(|error| error.to_string())?;
        let handoff_digest = super::super::sha256_digest(&bytes)?;
        drop(super::super::persist_engine_artifact_content(
            "execution/evidence",
            handoff_digest.hex(),
            "json",
            &bytes,
        )?);
        Ok(Some(Self {
            schema_version: 1,
            budget_scope: BudgetScope::Supplement,
            task_digest: super::super::sha256_digest(
                &task.canonical_bytes().map_err(|error| error.to_string())?,
            )?,
            handoff_digest,
            decision_id: context.decision().decision().decision_id.clone(),
        }))
    }

    pub(super) fn from_plan(plan: &ExecutionPlanV1) -> Result<Option<Self>, String> {
        plan.extensions
            .get(EXTENSION)
            .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()))
            .transpose()
    }

    pub(super) fn validate_task(&self, task: &TaskEnvelopeV1) -> Result<(), String> {
        if self.task_digest
            != super::super::sha256_digest(
                &task.canonical_bytes().map_err(|error| error.to_string())?,
            )?
        {
            return Err("native context handoff task snapshot mismatch".into());
        }
        Ok(())
    }

    fn projection(&self) -> Result<ContextPlanProjectionV1, String> {
        if self.schema_version != 1 || self.decision_id.is_empty() {
            return Err("invalid native context binding".into());
        }
        let bytes = super::super::artifact_store::read_content(
            "execution/evidence",
            self.handoff_digest.hex(),
            "json",
        )?;
        if super::super::sha256_digest(&bytes)? != self.handoff_digest {
            return Err("native context handoff content digest mismatch".into());
        }
        let (projection, decision): (ContextPlanProjectionV1, serde_json::Value) =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        projection.validate().map_err(|error| error.to_string())?;
        if decision
            .get("decision_id")
            .and_then(serde_json::Value::as_str)
            != Some(self.decision_id.as_str())
        {
            return Err("native context decision identity mismatch".into());
        }
        Ok(projection)
    }

    pub(super) fn apply(&self, plan: &mut ExecutionPlanV1) -> Result<(), String> {
        let projection = self.projection()?;
        if projection.task_id != plan.task_id {
            return Err("native context projection task mismatch".into());
        }
        plan.context_plan_id = Some(projection.context_plan_id);
        plan.extensions
            .insert(
                EXTENSION,
                serde_json::to_value(self).map_err(|e| e.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        plan.extensions
            .insert(DECISION_REF, self.decision_id.clone().into())
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn validate_plan(&self, plan: &ExecutionPlanV1) -> Result<ContextPlanProjectionV1, String> {
        let projection = self.projection()?;
        if plan.provider != super::manifest().provider
            || plan.model != "local-native"
            || plan.capability_ids.len() != 1
            || plan.capability_ids[0].as_str() != super::CAPABILITY_ID
            || plan.context_budget_tokens != 0
            || !matches!(
                plan.context_budget_policy,
                Some(ContextBudgetPolicyV1::NoTokenLimit {})
            )
            || projection.task_id != plan.task_id
            || plan.context_plan_id.as_ref() != Some(&projection.context_plan_id)
            || plan
                .extensions
                .get(DECISION_REF)
                .and_then(serde_json::Value::as_str)
                != Some(self.decision_id.as_str())
        {
            return Err("native supplemental context binding mismatch".into());
        }
        Ok(projection)
    }
}

pub(crate) fn validate_projection(
    plan: &ExecutionPlanV1,
    projection: &ContextPlanProjectionV1,
) -> Result<(), String> {
    if let Some(binding) = NativeContextBinding::from_plan(plan)? {
        if &binding.validate_plan(plan)? != projection {
            return Err("native supplemental context projection mismatch".into());
        }
    } else if plan.context_plan_id.as_ref() != Some(&projection.context_plan_id)
        || plan.context_budget_tokens != projection.budget_tokens
    {
        return Err("execution plan does not bind the context projection".into());
    }
    Ok(())
}

pub(crate) fn validate_handoff(plan: &ExecutionPlanV1, bytes: &[u8]) -> Result<(), String> {
    if let Some(binding) = NativeContextBinding::from_plan(plan)?
        && binding.handoff_digest != super::super::sha256_digest(bytes)?
    {
        return Err("native plan binds a different complete context handoff".into());
    }
    Ok(())
}

pub(crate) fn validate_task_binding(
    plan: &ExecutionPlanV1,
    task: &TaskEnvelopeV1,
) -> Result<(), String> {
    if let Some(binding) = NativeContextBinding::from_plan(plan)? {
        binding.validate_task(task)?;
    }
    Ok(())
}
