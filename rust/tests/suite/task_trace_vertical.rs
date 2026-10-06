// SPDX-License-Identifier: Apache-2.0
//! Vertical integration test: task_id appears consistently across the
//! identity layer and OCLA request context, proving Phase 0 exit criterion.

use lean_ctx::core::context_kernel::identity::TaskContext;
use lean_ctx_ocla::types::OclaRequestContext;

#[test]
fn task_context_carries_all_identity_fields() {
    let task = TaskContext {
        task_id: "task-vertical-known".to_owned(),
        trace_id: "trace-vertical-known".to_owned(),
        parent_task_id: None,
        session_id: Some("session-vertical".to_owned()),
        agent_id: Some("agent-vertical".to_owned()),
        project_id: Some("project-vertical".to_owned()),
    };

    assert_eq!(task.task_id, "task-vertical-known");
    assert_eq!(task.trace_id, "trace-vertical-known");
    assert!(task.parent_task_id.is_none());
}

#[test]
fn ocla_request_context_carries_task_id() {
    let mut ctx = OclaRequestContext::new(
        "request-vertical".to_owned(),
        "session-vertical".to_owned(),
        "agent-vertical".to_owned(),
        "file:src/lib.rs".to_owned(),
        None,
        Some("trace-vertical-known".to_owned()),
    );
    ctx.task_id = Some("task-vertical-known".to_owned());
    ctx.parent_task_id = Some("task-parent".to_owned());

    assert_eq!(ctx.task_id.as_deref(), Some("task-vertical-known"));
    assert_eq!(ctx.parent_task_id.as_deref(), Some("task-parent"));
    assert_eq!(ctx.session_id, "session-vertical");
    assert_eq!(ctx.agent_id, "agent-vertical");
}

#[test]
fn task_id_survives_serialization_roundtrip() {
    let mut ctx = OclaRequestContext::new(
        "request-serde".to_owned(),
        "session-serde".to_owned(),
        "agent-serde".to_owned(),
        "file:mod.rs".to_owned(),
        None,
        Some("trace-serde".to_owned()),
    );
    ctx.task_id = Some("task-serde-check".to_owned());

    let json = serde_json::to_string(&ctx).expect("serialize");
    let deserialized: OclaRequestContext = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.task_id, ctx.task_id);
    assert_eq!(deserialized.session_id, ctx.session_id);
}

#[test]
fn task_context_new_root_generates_ids() {
    let root = TaskContext::new_root();
    assert!(!root.task_id.is_empty());
    assert!(!root.trace_id.is_empty());
    assert!(root.parent_task_id.is_none());
}
