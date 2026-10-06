// SPDX-License-Identifier: Apache-2.0

//! Local Engine planning transport. A plan is not admission or execution evidence.

use serde::{Deserialize, Serialize};

use crate::{ContextPlanProjectionV1, SemanticVersion, TaskId, ValidationError};

/// Bound the request before parsing, independently of the selected context budget.
pub const MAX_ENGINE_CONTEXT_PLAN_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_ENGINE_CONTEXT_PLAN_QUERY_BYTES: usize = 16 * 1024;
pub const MAX_ENGINE_CONTEXT_PLAN_TOKENS: u32 = 1_048_576;
pub const MAX_ENGINE_CONTEXT_PLAN_CANDIDATES: u16 = 256;

/// Ask the canonical kernel for a reference plan under the host's local policy.
/// The task ID is correlation only, never an identity or authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextPlanRequestV1 {
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub task_id: TaskId,
    pub query: String,
    pub budget_tokens: u32,
    pub max_candidates: u16,
}

impl EngineContextPlanRequestV1 {
    /// Validate payload bounds; the receiving transport must also admit versions.
    pub fn validate_payload(&self) -> Result<(), ValidationError> {
        if self.query.trim().is_empty()
            || self.query.len() > MAX_ENGINE_CONTEXT_PLAN_QUERY_BYTES
            || self.query.contains('\0')
            || !(1..=MAX_ENGINE_CONTEXT_PLAN_TOKENS).contains(&self.budget_tokens)
            || !(1..=MAX_ENGINE_CONTEXT_PLAN_CANDIDATES).contains(&self.max_candidates)
        {
            return Err(ValidationError::new("invalid Engine context-plan bounds"));
        }
        Ok(())
    }
}

/// Existing canonical projection, not a second planner or a fabricated receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextPlanResponseV1 {
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub plan: ContextPlanProjectionV1,
}
