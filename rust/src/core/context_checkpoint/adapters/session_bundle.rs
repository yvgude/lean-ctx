// SPDX-License-Identifier: Apache-2.0

//! Session Bundle materializer.

use super::{
    CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION, CcpSessionBundleV1, ContextCheckpointLegacyErrorV1,
    ContextCheckpointLegacyFieldV1, ContextCheckpointLegacyMaterializationV1,
    ContextCheckpointLegacyRefusalReasonV1, ContextCheckpointLegacyRequestV1,
    ContextCheckpointProjectionInputV1, ContextCheckpointProjectionResultV1,
    ContextCheckpointSessionBundleInputsV1, ContextCheckpointV1, Decision, Finding, ProgressEntry,
    SESSION_BUNDLE_MAX_NEXT_STEPS, SESSION_BUNDLE_MAX_PAYLOAD_BYTES, SessionExcerptV1,
    SessionPolicyIdentityV1, SessionProjectIdentityV1, SessionStats, TaskInfo, chrono_timestamp,
    encode_owner, payload_from_value, progress_percent, refusal, require_arity, require_capacity,
    residual_losses,
};

pub(super) fn materialize_session_bundle(
    checkpoint: &ContextCheckpointV1,
    request: &ContextCheckpointLegacyRequestV1,
    inputs: &ContextCheckpointSessionBundleInputsV1,
    plan: &ContextCheckpointProjectionResultV1,
) -> Result<ContextCheckpointLegacyMaterializationV1, ContextCheckpointLegacyErrorV1> {
    let Some(session_state) = checkpoint.live_state.session_state.as_ref() else {
        return refusal(
            request,
            ContextCheckpointLegacyRefusalReasonV1::MissingSessionState,
            "lean_ctx_protocol::ContextCheckpointSessionStateV1",
            vec![ContextCheckpointProjectionInputV1::SessionBundleSessionExcerpt],
        );
    };

    require_arity(
        inputs.decision_observed_at.len(),
        checkpoint.live_state.decisions.len(),
        ContextCheckpointLegacyFieldV1::DecisionObservedAt,
    )?;
    require_arity(
        inputs.finding_observed_at.len(),
        checkpoint.live_state.findings.len(),
        ContextCheckpointLegacyFieldV1::FindingObservedAt,
    )?;
    require_capacity(
        checkpoint.live_state.next_steps.len(),
        SESSION_BUNDLE_MAX_NEXT_STEPS,
        ContextCheckpointLegacyFieldV1::NextSteps,
    )?;
    if let Some(profile_id) = checkpoint.live_state.profile_id.as_ref()
        && inputs.profile.name.as_str() != profile_id.as_str()
    {
        return Err(ContextCheckpointLegacyErrorV1::IdentityMismatch {
            field: ContextCheckpointLegacyFieldV1::SessionId,
        });
    }

    let started_at = chrono_timestamp(
        &session_state.identity.created_at,
        ContextCheckpointLegacyFieldV1::SessionId,
    )?;
    let updated_at = chrono_timestamp(
        &checkpoint.updated_at,
        ContextCheckpointLegacyFieldV1::SessionId,
    )?;
    let exported_at =
        chrono_timestamp(&inputs.exported_at, ContextCheckpointLegacyFieldV1::Payload)?;
    let task = Some(TaskInfo {
        description: checkpoint.live_state.task.title.as_str().to_owned(),
        intent: None,
        progress_pct: Some(progress_percent(checkpoint)),
    });
    let findings = checkpoint
        .live_state
        .findings
        .iter()
        .zip(&inputs.finding_observed_at)
        .map(|(finding, observed_at)| {
            Ok(Finding {
                file: None,
                line: None,
                summary: finding.as_str().to_owned(),
                timestamp: chrono_timestamp(
                    observed_at,
                    ContextCheckpointLegacyFieldV1::FindingObservedAt,
                )?,
            })
        })
        .collect::<Result<Vec<_>, ContextCheckpointLegacyErrorV1>>()?;
    let decisions = checkpoint
        .live_state
        .decisions
        .iter()
        .zip(&inputs.decision_observed_at)
        .map(|(decision, observed_at)| {
            Ok(Decision {
                summary: decision.statement.as_str().to_owned(),
                rationale: Some(decision.rationale.as_str().to_owned()),
                timestamp: chrono_timestamp(
                    observed_at,
                    ContextCheckpointLegacyFieldV1::DecisionObservedAt,
                )?,
            })
        })
        .collect::<Result<Vec<_>, ContextCheckpointLegacyErrorV1>>()?;
    let progress = vec![ProgressEntry {
        action: "checkpoint_progress".to_owned(),
        detail: Some(checkpoint.live_state.progress.summary.as_str().to_owned()),
        timestamp: updated_at,
    }];
    let stats = SessionStats {
        total_tool_calls: inputs.stats.total_tool_calls,
        total_tokens_saved: inputs.stats.total_tokens_saved,
        total_tokens_input: inputs.stats.total_tokens_input,
        cache_hits: inputs.stats.cache_hits,
        files_read: inputs.stats.files_read,
        commands_run: inputs.stats.commands_run,
        intents_inferred: inputs.stats.intents_inferred,
        intents_explicit: inputs.stats.intents_explicit,
        unsaved_changes: inputs.stats.unsaved_changes,
        // Security counts are provable events of the session that observed
        // them (#1856). The checkpoint schema carries no such evidence, so a
        // restored session starts at zero instead of inheriting unproven counts.
        security: Default::default(),
    };
    let compression_level = inputs
        .compression_level
        .as_ref()
        .map_or_else(String::new, |value| value.as_str().to_owned());

    let bundle = CcpSessionBundleV1 {
        schema_version: CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
        exported_at,
        project: SessionProjectIdentityV1 {
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
        role: SessionPolicyIdentityV1 {
            name: inputs.role.name.as_str().to_owned(),
            policy_md5: inputs.role.policy_digest.as_str().to_owned(),
        },
        profile: SessionPolicyIdentityV1 {
            name: inputs.profile.name.as_str().to_owned(),
            policy_md5: inputs.profile.policy_digest.as_str().to_owned(),
        },
        session: SessionExcerptV1 {
            id: session_state.identity.session_id.as_str().to_owned(),
            version: u32::try_from(session_state.state.revision).map_err(|_| {
                ContextCheckpointLegacyErrorV1::InvalidInputs {
                    field: ContextCheckpointLegacyFieldV1::SessionRevision,
                }
            })?,
            started_at,
            updated_at,
            project_root: None,
            shell_cwd: None,
            task,
            findings,
            decisions,
            files_touched: Vec::new(),
            test_results: None,
            progress,
            next_steps: checkpoint
                .live_state
                .next_steps
                .iter()
                .map(|step| step.as_str().to_owned())
                .collect(),
            evidence: Vec::new(),
            stats,
            terse_mode: inputs.terse_mode,
            compression_level,
        },
    };
    let value = serde_json::to_value(&bundle)
        .map_err(|_| ContextCheckpointLegacyErrorV1::LegacyOwnerRejected)?;
    let canonical_json = encode_owner(&value, |json| {
        crate::core::ccp_session_bundle::parse_bundle_v1(json).map(|_| ())
    })?;
    if canonical_json.len() > SESSION_BUNDLE_MAX_PAYLOAD_BYTES {
        return Err(ContextCheckpointLegacyErrorV1::LegacyCapacityExceeded {
            field: ContextCheckpointLegacyFieldV1::Payload,
            limit: SESSION_BUNDLE_MAX_PAYLOAD_BYTES as u64,
            actual: canonical_json.len() as u64,
        });
    }
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
