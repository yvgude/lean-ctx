// SPDX-License-Identifier: Apache-2.0

//! Context Snapshot materializer.

use super::{
    CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION, ContextCheckpointContextSnapshotInputsV1,
    ContextCheckpointLegacyErrorV1, ContextCheckpointLegacyFieldV1,
    ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyRefusalReasonV1,
    ContextCheckpointLegacyRequestV1, ContextCheckpointProjectionInputV1,
    ContextCheckpointProjectionResultV1, ContextCheckpointV1, ContextSnapshotV1, GitAnchorV1,
    SNAPSHOT_MAX_SESSION_LIST, SnapshotLedgerV1, SnapshotLineageV1, SnapshotProjectV1,
    SnapshotRoiV1, SnapshotSessionV1, compression_rate, encode_owner, payload_from_value,
    progress_percent, refusal, require_capacity, residual_losses,
};

pub(super) fn materialize_context_snapshot(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    inputs: &ContextCheckpointContextSnapshotInputsV1,
    plan: &ContextCheckpointProjectionResultV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    if !checkpoint.lineage.receipt_ids.is_empty()
        || checkpoint.lineage.context_ir_digest.is_some()
        || checkpoint.lineage.hosted_index_digest.is_some()
        || !checkpoint.lineage.evidence_refs.is_empty()
        || !checkpoint.lineage.knowledge_refs.is_empty()
        || !checkpoint.lineage.gotcha_refs.is_empty()
        || !checkpoint.lineage.snapshot_refs.is_empty()
    {
        return refusal(
            request,
            ContextCheckpointLegacyRefusalReasonV1::TargetOwnerDependency,
            "crate::core::context_snapshot::{builder,digest,publish}",
            vec![
                ContextCheckpointProjectionInputV1::ContextSnapshotLineage,
                ContextCheckpointProjectionInputV1::ContextSnapshotLedger,
            ],
        );
    }
    require_capacity(
        checkpoint.live_state.decisions.len(),
        SNAPSHOT_MAX_SESSION_LIST,
        ContextCheckpointLegacyFieldV1::Decisions,
    )?;
    let session = Some(SnapshotSessionV1 {
        session_id: checkpoint
            .live_state
            .session_state
            .as_ref()
            .map(|value| value.identity.session_id.as_str().to_owned()),
        task: Some(checkpoint.live_state.task.title.as_str().to_owned()),
        decisions: checkpoint
            .live_state
            .decisions
            .iter()
            .map(|decision| decision.statement.as_str().to_owned())
            .collect(),
        files_touched: Vec::new(),
        progress_pct: Some(progress_percent(checkpoint)),
    });
    let mut snapshot = ContextSnapshotV1 {
        schema_version: CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
        snapshot_id: String::new(),
        parent_id: inputs
            .parent_snapshot_id
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        created_at: inputs.created_at.as_str().to_owned(),
        lean_ctx_version: inputs.lean_ctx_version.as_str().to_owned(),
        git: GitAnchorV1 {
            commit: inputs
                .git
                .commit
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            branch: inputs
                .git
                .branch
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            dirty: inputs.git.dirty,
        },
        project: SnapshotProjectV1 {
            root_hash: inputs
                .project
                .project_root_hash
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            identity_hash: inputs
                .project
                .project_identity_hash
                .as_ref()
                .map(|value| value.as_str().to_owned()),
        },
        roi: SnapshotRoiV1 {
            input_tokens: inputs.roi.input_tokens,
            output_tokens: inputs.roi.output_tokens,
            tokens_saved: inputs.roi.tokens_saved,
            compression_rate: compression_rate(&inputs.roi),
        },
        lineage: SnapshotLineageV1 {
            items_recorded: inputs.ledger_totals.lineage_items_recorded,
            items: Vec::new(),
        },
        ledger: SnapshotLedgerV1 {
            window_size: inputs.ledger_totals.window_size as usize,
            total_tokens_sent: inputs.ledger_totals.total_tokens_sent as usize,
            total_tokens_saved: inputs.ledger_totals.total_tokens_saved as usize,
            items: Vec::new(),
        },
        session,
        signature: None,
    };
    snapshot.snapshot_id = crate::core::context_snapshot::digest::finalize_id(&mut snapshot)
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    let value = serde_json::to_value(&snapshot)
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    let canonical_json = encode_owner(&value, |json| {
        let parsed: ContextSnapshotV1 =
            serde_json::from_str(json).map_err(|_| "snapshot parse failed".to_owned())?;
        if parsed.schema_version != CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION {
            return Err("snapshot version mismatch".to_owned());
        }
        let id = crate::core::context_snapshot::digest::compute_id(&parsed)?;
        if id != parsed.snapshot_id {
            return Err("snapshot id mismatch".to_owned());
        }
        Ok(())
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
