// SPDX-License-Identifier: Apache-2.0
//! Fixtures shared by the delegation tests and the composition root's tests.

use super::ContextCapsuleV1;
use crate::core::context_capsule::{
    CONTEXT_CAPSULE_SCHEMA_VERSION, CapsuleSensitivityV1, ContextCapsuleBudgetV1,
    ContextCapsuleChainV1,
};

/// A signed-shape capsule for graph `g1`, child node `child`.
pub fn capsule() -> ContextCapsuleV1 {
    ContextCapsuleV1 {
        schema_version: CONTEXT_CAPSULE_SCHEMA_VERSION,
        capsule_id: "capsule:child".into(),
        request_id: "request:1".into(),
        session_id: "session:1".into(),
        agent_id: "owner".into(),
        intent_ref: "intent:test".into(),
        task_ref: "task:child".into(),
        expected_outcome_ref: "outcome:tests-pass".into(),
        acceptance_criteria_refs: Vec::new(),
        references: Vec::new(),
        finding_refs: Vec::new(),
        decision_refs: Vec::new(),
        uncertainty_refs: Vec::new(),
        negative_result_refs: Vec::new(),
        source_ref: "source:test".into(),
        policy_ref: "policy:workspace".into(),
        contract_ref: "contract:v1".into(),
        freshness_ref: "freshness:1".into(),
        sensitivity: CapsuleSensitivityV1::Internal,
        allowed_agent_ids: vec!["worker".into()],
        budget: ContextCapsuleBudgetV1 {
            tokens_used: 0,
            tokens_remaining: 100,
            cost_micros_used: 0,
            cost_micros_remaining: 50,
            latency_ms_used: 0,
            latency_ms_remaining: 200,
        },
        chain: ContextCapsuleChainV1 {
            chain_id: "graph:g1".into(),
            parent_capsule_ref: Some("capsule:root".into()),
            owner_agent_id: "owner".into(),
            attribution_ref: "attribution:1".into(),
            hop: 1,
        },
        quality_signal_refs: Vec::new(),
        recovery_refs: Vec::new(),
        delta_from: None,
    }
}
