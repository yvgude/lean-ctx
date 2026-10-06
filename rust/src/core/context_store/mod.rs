// SPDX-License-Identifier: Apache-2.0
//! The Context Store: one access contract over the stores that already hold
//! LeanCTX state. It is a logical layer, not another database.
//!
//! | State | Authoritative store |
//! |---|---|
//! | decisions, plans, invocations, outcomes | execution ledger (`core::execution_ledger`) |
//! | what reached the model, under which policy | Decision Receipts (`core::context_admission::receipt_store`) |
//! | learned planner state | `context_kernel::autopilot::learning_store` |
//! | knowledge | `core::knowledge` (`KnowledgeStore`) |
//! | relations | `core::property_graph` |
//! | per-task runtime signals (bounce, expand, edit failure) | `task_signals` |
//! | promoted read-strategy policies | `policy_store` |
//!
//! Search, graph and vector indexes are derived from these and rebuildable.
//! Modules here read and join; they never become a second copy of that state.
//! `lean-ctx engine context-lineage` and `context-policy-evidence` expose the
//! joins read-only (docs/contracts/engine-context-store-v1.md).

use lean_ctx_protocol::{ProjectId, TenantId};

pub(crate) mod lineage;
pub(crate) mod policy_store;
pub(crate) mod task_signals;

/// The tenant/project scope a task belongs to, as one opaque key. Task-keyed
/// state (receipt indexes, learning) is always looked up under this scope, so
/// equal task ids in different projects never meet.
pub(crate) fn task_scope(tenant: Option<&TenantId>, project: &ProjectId) -> String {
    serde_json::to_string(&(tenant, project)).expect("identifiers serialize")
}
