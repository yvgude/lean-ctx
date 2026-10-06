// SPDX-License-Identifier: Apache-2.0

use lean_ctx_protocol::{
    ContextCheckpointBranchIdV1, ContextCheckpointDeviceIdV1, ContextCheckpointIdV1,
    ContextCheckpointIdentityV1, ContextCheckpointLineageV1, ContextCheckpointLiveStateV1,
    ContextCheckpointProgressV1, ContextCheckpointTaskStatusV1, ContextCheckpointTaskV1,
    ContextCheckpointTextV1, ContextCheckpointV1, PlanId, SemanticVersion, TaskEnvelopeV1,
    TenantId, UtcTimestamp, WorkspaceId,
};

pub(super) fn checkpoint(task: &TaskEnvelopeV1, plan_id: &PlanId) -> ContextCheckpointV1 {
    let task_id = task.task_id.clone();
    let project_id = task.project_id.clone();
    ContextCheckpointV1::try_new(
        ContextCheckpointIdentityV1::try_new(
            ContextCheckpointIdV1::new("11111111-2222-4333-8444-555555555555")
                .expect("checkpoint id"),
            None,
            None,
            None,
            None,
            ContextCheckpointBranchIdV1::new("main").expect("branch"),
            ContextCheckpointDeviceIdV1::new("device-a").expect("device"),
            1,
        )
        .expect("identity"),
        ContextCheckpointLineageV1 {
            project_id,
            workspace_id: WorkspaceId::new("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                .expect("workspace"),
            tenant_id: TenantId::new("tenant-1").expect("tenant"),
            task_id: task_id.clone(),
            plan_id: Some(plan_id.clone()),
            receipt_ids: Vec::new(),
            context_ir_digest: None,
            hosted_index_digest: None,
            evidence_refs: Vec::new(),
            knowledge_refs: Vec::new(),
            gotcha_refs: Vec::new(),
            snapshot_refs: Vec::new(),
        },
        ContextCheckpointLiveStateV1 {
            schema_version: 1,
            task: ContextCheckpointTaskV1 {
                task_id,
                title: ContextCheckpointTextV1::new("continue task").expect("title"),
                status: ContextCheckpointTaskStatusV1::InProgress,
                plan_id: Some(plan_id.clone()),
            },
            progress: ContextCheckpointProgressV1 {
                completed_steps: 0,
                total_steps: 1,
                confidence_milliunits: 500,
                summary: ContextCheckpointTextV1::new("ready").expect("summary"),
            },
            decisions: Vec::new(),
            findings: Vec::new(),
            next_steps: vec![ContextCheckpointTextV1::new("execute").expect("step")],
            handoff_summary: ContextCheckpointTextV1::new("continue").expect("handoff"),
            files: Vec::new(),
            profile_id: None,
            policy_pins: Vec::new(),
            package_pins: Vec::new(),
            session_state: None,
            learning_state: None,
        },
        None,
        SemanticVersion::new("4.0.0").expect("version"),
        UtcTimestamp::new("2026-08-23T12:00:00Z").expect("timestamp"),
    )
    .expect("checkpoint")
}
