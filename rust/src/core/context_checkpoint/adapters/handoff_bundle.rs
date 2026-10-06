// SPDX-License-Identifier: Apache-2.0

//! Handoff Transfer Bundle materializer.

use super::{
    ArtifactsExcerptV1, ContextCheckpointHandoffBundleInputsV1, ContextCheckpointLegacyErrorV1,
    ContextCheckpointLegacyFieldV1, ContextCheckpointLegacyMaterializationV1,
    ContextCheckpointLegacyRefusalReasonV1, ContextCheckpointLegacyRequestV1,
    ContextCheckpointProjectionInputV1, ContextCheckpointProjectionResultV1, ContextCheckpointV1,
    HANDOFF_LEDGER_MAX_DECISIONS, HANDOFF_LEDGER_MAX_LIST, HANDOFF_LEDGER_V1_SCHEMA_VERSION,
    HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION, HandoffLedgerV1, HandoffProjectIdentityV1,
    HandoffSessionExcerpt, HandoffTransferBundleV1, KnowledgeExcerpt, ToolCallsSummary,
    chrono_timestamp, encode_owner, payload_from_value, refusal, require_capacity, residual_losses,
};

pub(super) fn materialize_handoff_bundle(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    inputs: &ContextCheckpointHandoffBundleInputsV1,
    plan: &ContextCheckpointProjectionResultV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    let Some(session_state) = checkpoint.live_state.session_state.as_ref() else {
        return refusal(
            request,
            ContextCheckpointLegacyRefusalReasonV1::MissingSessionState,
            "lean_ctx_protocol::ContextCheckpointSessionStateV1",
            vec![ContextCheckpointProjectionInputV1::HandoffBundleLedger],
        );
    };
    require_capacity(
        checkpoint.live_state.decisions.len(),
        HANDOFF_LEDGER_MAX_DECISIONS,
        ContextCheckpointLegacyFieldV1::Decisions,
    )?;
    require_capacity(
        checkpoint.live_state.findings.len(),
        HANDOFF_LEDGER_MAX_LIST,
        ContextCheckpointLegacyFieldV1::Findings,
    )?;
    require_capacity(
        checkpoint.live_state.next_steps.len(),
        HANDOFF_LEDGER_MAX_LIST,
        ContextCheckpointLegacyFieldV1::NextSteps,
    )?;
    let mut ledger = HandoffLedgerV1 {
        schema_version: HANDOFF_LEDGER_V1_SCHEMA_VERSION,
        created_at: inputs.ledger_created_at.as_str().to_owned(),
        content_md5: String::new(),
        manifest_md5: inputs.manifest_digest.as_str().to_owned(),
        project_root: None,
        agent_id: inputs
            .agent_id
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        client_name: inputs
            .client_name
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        workflow: None,
        session_snapshot: String::new(),
        session: HandoffSessionExcerpt {
            id: session_state.identity.session_id.as_str().to_owned(),
            task: Some(checkpoint.live_state.task.title.as_str().to_owned()),
            decisions: checkpoint
                .live_state
                .decisions
                .iter()
                .map(|decision| decision.statement.as_str().to_owned())
                .collect(),
            findings: checkpoint
                .live_state
                .findings
                .iter()
                .map(|finding| finding.as_str().to_owned())
                .collect(),
            next_steps: checkpoint
                .live_state
                .next_steps
                .iter()
                .map(|step| step.as_str().to_owned())
                .collect(),
        },
        tool_calls: ToolCallsSummary::default(),
        evidence_keys: checkpoint
            .lineage
            .evidence_refs
            .iter()
            .map(|reference| reference.as_str().to_owned())
            .collect(),
        knowledge: KnowledgeExcerpt::default(),
        curated_refs: Vec::new(),
        active_overlays: Vec::new(),
    };
    ledger.content_md5 = crate::core::handoff_ledger::compute_content_md5_for_ledger(&ledger);
    let bundle = HandoffTransferBundleV1 {
        schema_version: HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
        exported_at: chrono_timestamp(
            &inputs.exported_at,
            ContextCheckpointLegacyFieldV1::Payload,
        )?,
        privacy: "redacted".to_owned(),
        project: HandoffProjectIdentityV1 {
            project_root_hash: inputs
                .project
                .project_root_hash
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            project_identity_hash: inputs
                .project
                .project_identity_hash
                .as_ref()
                .map(|value| value.as_str().to_owned()),
        },
        ledger,
        artifacts: ArtifactsExcerptV1::default(),
        signature: None,
        signer_public_key: None,
        signer_agent_id: None,
    };
    let value = serde_json::to_value(&bundle)
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    let canonical_json = encode_owner(&value, |json| {
        crate::core::handoff_transfer_bundle::parse_bundle_v1(json).map(|_| ())
    })?;
    let payload = payload_from_value(
        checkpoint,
        request.target,
        request.target_schema_version,
        canonical_json,
        residual_losses(plan),
    )?;
    Ok(ContextCheckpointLegacyMaterializationV1::Materialized(
        payload,
    ))
}
