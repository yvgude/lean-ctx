// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    AgentId, ContextSessionPhaseV1, ContextSessionRecoveryStateV1, RunId, SessionId, TraceId,
};
use serde_json::json;

const CHECKPOINT_ID: &str = "11111111-2222-4333-8444-555555555555";
const PARENT_ID: &str = "22222222-3333-4444-8555-666666666666";
const WORKSPACE_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// Canonical JSON produced by [`fixture`]; any drift is a wire break.
const GOLDEN_JSON: &str = r#"{"carrier":{"carrier_schema_id":"leanctx.context-checkpoint/v2","envelope_digest":"sha256:8888888888888888888888888888888888888888888888888888888888888888","logical_state_schema_id":"leanctx.workspace.state/v1","state_digest":"sha256:7777777777777777777777777777777777777777777777777777777777777777","workspace_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"},"created_at":"2026-09-06T12:00:00Z","engine_version":"4.0.0","identity":{"branch_id":"main","checkpoint_id":"11111111-2222-4333-8444-555555555555","device_id":"device-a","device_sequence":7,"parent_checkpoint_id":"22222222-3333-4444-8555-666666666666"},"lineage":{"context_ir_digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","evidence_refs":["evidence:alpha","evidence:beta"],"gotcha_refs":["gotcha:alpha"],"hosted_index_digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","knowledge_refs":["knowledge:alpha"],"plan_id":"plan-a","project_id":"project-a","receipt_ids":["receipt-a","receipt-b"],"snapshot_refs":["snapshot:alpha"],"task_id":"task-a","workspace_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"},"live_state":{"decisions":[{"decision_id":"decision-a","evidence_refs":["evidence:alpha"],"rationale":"Prevents replay across projects","statement":"Bind lineage into the checkpoint digest","status":"accepted"},{"decision_id":"decision-b","evidence_refs":[],"rationale":"Fail closed rather than filter later","statement":"Reject machine-local material at construction","status":"proposed"}],"files":[{"content_digest":"sha256:3333333333333333333333333333333333333333333333333333333333333333","role":"modified","source_id":"source-a"},{"content_digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","role":"reference","source_id":"source-b"}],"findings":["Carrier verifier pins seven logical-state keys"],"handoff_summary":"Typed domain complete; projections remain downstream","next_steps":["Run the deterministic gates","Write the report"],"package_pins":[{"package_digest":"sha256:6666666666666666666666666666666666666666666666666666666666666666","package_id":"package-a","version":"1.2.3"}],"policy_pins":[{"policy_digest":"sha256:5555555555555555555555555555555555555555555555555555555555555555","policy_id":"policy-a"}],"profile_id":"profile-a","progress":{"completed_steps":3,"confidence_milliunits":820,"summary":"Domain types landed; gates pending","total_steps":8},"schema_version":1,"task":{"plan_id":"plan-a","status":"in_progress","task_id":"task-a","title":"Converge the checkpoint domain"}},"schema_version":1}"#;

/// Domain-separated digests over [`GOLDEN_JSON`] and its parts.
///
/// These were reproduced by an independent SHA-256 implementation over the
/// same domain prefixes, so they pin the wire form and not just this code.
const GOLDEN_CHECKPOINT_DIGEST: &str =
    "sha256:c4678a9e1d053b6db66f6c75e5a40bb3bea2b085a2d3345b56180d819b9e9dec";
const GOLDEN_IDENTITY_DIGEST: &str =
    "sha256:c333698fa48bcf4056498c7029967732265ff7e2431bb7dc2a17b072fc94b122";
const GOLDEN_LINEAGE_DIGEST: &str =
    "sha256:c81bf99b0589aaceb301a0fe004adc7334447955bef852170ad670ab93cc56a9";
const GOLDEN_LIVE_STATE_DIGEST: &str =
    "sha256:b3d743e997c4f2527b5a2f370f45b8c88fa71ebaf784a6222e0db5b330cd0e55";
const GOLDEN_SIGNING_DIGEST: &str =
    "sha256:4c4d755f301ca9054f112642552fc301eed801cc616898db14c9edab95556edc";

fn digest_of(seed: &str) -> Sha256Digest {
    Sha256Digest::new(format!("sha256:{}", seed.repeat(64 / seed.len()))).expect("valid digest")
}

fn text(value: &str) -> ContextCheckpointTextV1 {
    ContextCheckpointTextV1::new(value).expect("valid text")
}

fn reference(value: &str) -> ProtocolReference {
    ProtocolReference::new(value).expect("valid reference")
}

fn identity() -> ContextCheckpointIdentityV1 {
    ContextCheckpointIdentityV1::try_new(
        ContextCheckpointIdV1::new(CHECKPOINT_ID).expect("valid checkpoint id"),
        Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent id")),
        Some(ContextCheckpointDeviceIdV1::new("device-a").expect("valid parent device")),
        Some(ContextCheckpointBranchIdV1::new("main").expect("valid parent branch")),
        Some(6),
        ContextCheckpointBranchIdV1::new("main").expect("valid branch"),
        ContextCheckpointDeviceIdV1::new("device-a").expect("valid device"),
        7,
    )
    .expect("valid identity")
}

fn lineage() -> ContextCheckpointLineageV1 {
    ContextCheckpointLineageV1 {
        project_id: ProjectId::new("project-a").expect("valid project"),
        workspace_id: WorkspaceId::new(WORKSPACE_ID).expect("valid workspace"),
        tenant_id: TenantId::new("tenant-a").expect("valid tenant"),
        task_id: TaskId::new("task-a").expect("valid task"),
        plan_id: Some(PlanId::new("plan-a").expect("valid plan")),
        receipt_ids: vec![
            ReceiptId::new("receipt-a").expect("valid receipt"),
            ReceiptId::new("receipt-b").expect("valid receipt"),
        ],
        context_ir_digest: Some(digest_of("1")),
        hosted_index_digest: Some(digest_of("2")),
        evidence_refs: vec![reference("evidence:alpha"), reference("evidence:beta")],
        knowledge_refs: vec![reference("knowledge:alpha")],
        gotcha_refs: vec![reference("gotcha:alpha")],
        snapshot_refs: vec![reference("snapshot:alpha")],
    }
}

fn live_state() -> ContextCheckpointLiveStateV1 {
    ContextCheckpointLiveStateV1 {
        schema_version: ContextCheckpointLiveStateV1::SCHEMA_VERSION,
        task: ContextCheckpointTaskV1 {
            task_id: TaskId::new("task-a").expect("valid task"),
            title: text("Converge the checkpoint domain"),
            status: ContextCheckpointTaskStatusV1::InProgress,
            plan_id: Some(PlanId::new("plan-a").expect("valid plan")),
        },
        progress: ContextCheckpointProgressV1 {
            completed_steps: 3,
            total_steps: 8,
            confidence_milliunits: 820,
            summary: text("Domain types landed; gates pending"),
        },
        decisions: vec![
            ContextCheckpointDecisionV1 {
                decision_id: DecisionId::new("decision-a").expect("valid decision"),
                statement: text("Bind lineage into the checkpoint digest"),
                rationale: text("Prevents replay across projects"),
                status: ContextCheckpointDecisionStatusV1::Accepted,
                evidence_refs: vec![reference("evidence:alpha")],
            },
            ContextCheckpointDecisionV1 {
                decision_id: DecisionId::new("decision-b").expect("valid decision"),
                statement: text("Reject machine-local material at construction"),
                rationale: text("Fail closed rather than filter later"),
                status: ContextCheckpointDecisionStatusV1::Proposed,
                evidence_refs: Vec::new(),
            },
        ],
        findings: vec![text("Carrier verifier pins seven logical-state keys")],
        next_steps: vec![
            text("Run the deterministic gates"),
            text("Write the report"),
        ],
        handoff_summary: text("Typed domain complete; projections remain downstream"),
        files: vec![
            ContextCheckpointFileV1 {
                source_id: SourceId::new("source-a").expect("valid source"),
                role: ContextCheckpointFileRoleV1::Modified,
                content_digest: digest_of("3"),
            },
            ContextCheckpointFileV1 {
                source_id: SourceId::new("source-b").expect("valid source"),
                role: ContextCheckpointFileRoleV1::Reference,
                content_digest: digest_of("4"),
            },
        ],
        profile_id: Some(ProfileId::new("profile-a").expect("valid profile")),
        policy_pins: vec![ContextCheckpointPolicyPinV1 {
            policy_id: PolicyId::new("policy-a").expect("valid policy"),
            policy_digest: digest_of("5"),
        }],
        package_pins: vec![ContextCheckpointPackagePinV1 {
            package_id: PackageId::new("package-a").expect("valid package"),
            version: SemanticVersion::new("1.2.3").expect("valid version"),
            package_digest: digest_of("6"),
        }],
        session_state: None,
        learning_state: None,
    }
}

fn carrier() -> ContextCheckpointCarrierBindingV1 {
    ContextCheckpointCarrierBindingV1 {
        carrier_schema_id: CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID.to_owned(),
        logical_state_schema_id: CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID.to_owned(),
        workspace_id: WorkspaceId::new(WORKSPACE_ID).expect("valid workspace"),
        state_digest: digest_of("7"),
        envelope_digest: digest_of("8"),
    }
}

fn session_state() -> ContextCheckpointSessionStateV1 {
    ContextCheckpointSessionStateV1::try_new(
        SessionIdentityV1 {
            session_id: SessionId::new("session-a").expect("valid session"),
            task_id: TaskId::new("task-a").expect("valid task"),
            run_id: RunId::new("run-a").expect("valid run"),
            trace_id: TraceId::new("trace-a").expect("valid trace"),
            parent_task_id: None,
            agent_id: AgentId::new("agent-a").expect("valid agent"),
            project_id: ProjectId::new("project-a").expect("valid project"),
            workspace_id: Some(WorkspaceId::new(WORKSPACE_ID).expect("valid workspace")),
            tenant_id: Some(TenantId::new("tenant-a").expect("valid tenant")),
            project_root_ref: reference("project:root"),
            project_revision_ref: Some(reference("revision:abc123")),
            created_at: UtcTimestamp::new("2026-09-06T11:00:00Z").expect("valid timestamp"),
        },
        ContextSessionStateV1 {
            phase: ContextSessionPhaseV1::Executing,
            revision: 3,
            next_event_sequence: 4,
            active_plan_id: Some(PlanId::new("plan-a").expect("valid plan")),
            receipt_id: None,
            last_checkpoint_digest: Some(digest_of("a")),
            recovery_state: ContextSessionRecoveryStateV1::Resumable,
            abort_reason: None,
        },
    )
    .expect("valid session state")
}

fn learning_state() -> ContextCheckpointLearningStateV1 {
    ContextCheckpointLearningStateV1::try_new(reference("learning:alpha"), digest_of("9"), 2)
        .expect("valid learning state")
}

fn encryption_metadata() -> ContextCheckpointEncryptionMetadataV1 {
    ContextCheckpointEncryptionMetadataV1::try_new(
        ContextCheckpointEncryptionAlgorithmV1::Aes256Gcm,
        reference("kms:key-a"),
        digest_of("0"),
    )
    .expect("valid encryption metadata")
}

fn fixture() -> ContextCheckpointV1 {
    ContextCheckpointV1::try_new(
        identity(),
        lineage(),
        live_state(),
        Some(carrier()),
        SemanticVersion::new("4.0.0").expect("valid engine version"),
        UtcTimestamp::new("2026-09-06T12:00:00Z").expect("valid timestamp"),
    )
    .expect("fixture must be valid")
}

fn rich_fixture() -> ContextCheckpointV1 {
    fixture()
        .with_portable_state(
            Some(session_state()),
            Some(learning_state()),
            Some(encryption_metadata()),
        )
        .expect("rich fixture must be valid")
}

fn canonical_value(checkpoint: &ContextCheckpointV1) -> Value {
    serde_json::from_slice(&checkpoint.canonical_bytes().expect("canonical bytes"))
        .expect("canonical JSON")
}

fn decode(value: &Value) -> Result<ContextCheckpointV1, ValidationError> {
    let bytes = serde_json::to_vec(&sort_json(value.clone())).expect("encode");
    ContextCheckpointV1::from_canonical_bytes(&bytes)
}

include!("tests_core.rs");
include!("tests_extended.rs");
