// SPDX-License-Identifier: Apache-2.0

//! Ephemeral explicit-source provider; never persists into project/global stores.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use lean_ctx_protocol::{
    ContextDispositionV1, DataClassification, EngineContextSourceDescriptorV1,
    EngineContextSourcePermissionV1, EngineContextSourcePlanRequestV1,
    EngineContextSourcePlanResponseV1, EngineContextSourceV1, UtcTimestamp,
};

use crate::core::{
    context_field::{ContextField, ContextItemId, FieldWeights, Provenance, ViewCosts, ViewKind},
    context_kernel::{
        autopilot::{AutopilotController, TaskAutopilotDecision},
        orchestrator::ContextKernel,
        policy::ContextPolicy,
        types::{
            CandidateProvider, ContextObjectKind, ContextObjectV1, Freshness, RetrievalContext,
            SensitivityLevel, SideEffectPolicy,
        },
    },
};

#[derive(Clone)]
struct ExplicitSourceProvider(Vec<ContextObjectV1>);

impl CandidateProvider for ExplicitSourceProvider {
    fn provider_id(&self) -> &'static str {
        "engine.explicit-sources.v1"
    }

    fn candidates(&self, ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
        self.0.iter().take(ctx.max_candidates).cloned().collect()
    }

    fn side_effect_policy(&self) -> SideEffectPolicy {
        SideEffectPolicy::ReadOnly
    }
}

pub(crate) fn plan(
    root: &Path,
    request: EngineContextSourcePlanRequestV1,
) -> Result<EngineContextSourcePlanResponseV1, &'static str> {
    request
        .validate_payload()
        .map_err(|_| "invalid_source_manifest")?;
    let policy = current_policy(root)?;
    let (evaluation_time, include_marker) = if policy.retention_days.is_some() {
        (
            Utc::now()
                .with_nanosecond(0)
                .ok_or("source_plan_epoch_invalid")?
                .fixed_offset(),
            true,
        )
    } else {
        (Utc::now().fixed_offset(), false)
    };
    Ok(plan_at(root, request, evaluation_time, include_marker)?.response)
}

/// Private same-process handoff; no deserialization from a caller's wire plan.
pub(super) struct SourcePlanningHandoff {
    pub(super) decision: TaskAutopilotDecision,
    pub(super) response: EngineContextSourcePlanResponseV1,
}

/// Replan a materialization request at the plan's retained identity time.
///
/// The identity time is only used to reconstruct the original retention-aware
/// digest. Fresh source and policy admission is performed independently below.
pub(super) fn plan_for_materialization(
    root: &Path,
    request: EngineContextSourcePlanRequestV1,
    evaluation_time: Option<DateTime<chrono::FixedOffset>>,
) -> Result<SourcePlanningHandoff, &'static str> {
    request
        .validate_payload()
        .map_err(|_| "invalid_source_manifest")?;
    let policy = current_policy(root)?;
    let include_marker = policy.retention_days.is_some();
    let evaluation_time = if include_marker {
        let evaluation_time = evaluation_time.ok_or("source_plan_epoch_missing")?;
        if evaluation_time > Utc::now().fixed_offset() {
            return Err("source_plan_epoch_invalid");
        }
        evaluation_time
    } else {
        // Retention is the only policy that contributes the epoch to identity;
        // ignore an optional echo for old no-retention plans.
        Utc::now().fixed_offset()
    };
    plan_at(root, request, evaluation_time, include_marker)
}

fn plan_at(
    root: &Path,
    request: EngineContextSourcePlanRequestV1,
    evaluation_time: DateTime<chrono::FixedOffset>,
    include_marker: bool,
) -> Result<SourcePlanningHandoff, &'static str> {
    request
        .validate_payload()
        .map_err(|_| "invalid_source_manifest")?;
    let now = Utc::now();
    let mut candidates = request
        .sources
        .iter()
        .map(|source| candidate(source, now))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    // Bound enumeration deterministically, never by untrusted input order.
    candidates.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let provider = ExplicitSourceProvider(candidates);
    let controller = AutopilotController::with_kernels(
        ContextKernel::with_field(
            vec![Box::new(provider.clone())],
            ContextField::with_weights(FieldWeights::default()),
        )
        .with_private_selection()
        .with_relevance_floor(),
        ContextKernel::with_field(
            vec![Box::new(provider)],
            ContextField::with_weights(FieldWeights::default()),
        )
        .with_relevance_floor(),
    );
    let decision = super::plan_with_controller_at(
        root,
        request.planning,
        Some(controller),
        Some(evaluation_time),
    )?;
    let initial_result = super::response(&decision)?;
    let epoch = if include_marker {
        Some(
            UtcTimestamp::new(evaluation_time.to_rfc3339_opts(SecondsFormat::Secs, true))
                .map_err(|_| "source_plan_epoch_invalid")?,
        )
    } else {
        None
    };
    // Only kernel-admitted origins may contribute equivalent-source metadata.
    // Hash equality alone cannot resurrect a denied or unenumerated source.
    let mut by_digest = BTreeMap::<&str, Vec<&EngineContextSourceDescriptorV1>>::new();
    for source in &request.sources {
        let descriptor = &source.descriptor;
        if let Some([origin]) = decision
            .decision()
            .context_plan
            .origins
            .get(descriptor.object_ref.as_str())
            .map(Vec::as_slice)
            && origin.admitted
            && origin.source == descriptor.source_id.as_str()
            && origin.content_ref == descriptor.content_digest.as_str()
        {
            by_digest
                .entry(descriptor.content_digest.as_str())
                .or_default()
                .push(descriptor);
        }
    }
    let mut groups = Vec::new();
    for selection in &initial_result.plan.selections {
        if selection.disposition != ContextDispositionV1::Selected {
            continue;
        }
        if let Some(equivalent) = selection
            .sha256_digest
            .as_deref()
            .and_then(|digest| by_digest.get_mut(digest))
            && equivalent.len() > 1
        {
            equivalent.sort_by(|left, right| left.object_ref.cmp(&right.object_ref));
            groups.push(serde_json::json!({
                "selected_ref": selection.source_ref,
                "equivalent_sources": equivalent,
            }));
        }
    }
    let lineage = (!groups.is_empty()).then(|| {
        serde_json::json!({
            "schema_version": 1, "groups": groups,
        })
    });
    let decision = decision
        .bind_source_metadata(lineage, epoch)
        .map_err(|_| "source_lineage_too_large")?;
    let result = super::response(&decision)?;
    let source_bindings = request
        .sources
        .iter()
        .filter(|source| {
            result.plan.selections.iter().any(|selection| {
                selection.source_ref == source.descriptor.object_ref.as_str()
                    && selection.disposition == ContextDispositionV1::Selected
            })
        })
        .map(|source| source.descriptor.clone())
        .collect::<Vec<_>>();
    // Reload immediately before returning: the replay identity is not a
    // concurrency lock, so a policy change must win over its older snapshot.
    ensure_current_admission(
        root,
        &request.sources,
        &source_bindings,
        result.plan.budget_tokens,
    )?;
    let response =
        EngineContextSourcePlanResponseV1::new(result, source_bindings).map_err(|_| "internal")?;
    Ok(SourcePlanningHandoff { decision, response })
}

fn current_policy(root: &Path) -> Result<ContextPolicy, &'static str> {
    let root = root.to_str().ok_or("unsafe_root")?;
    ContextPolicy::from_config(root).map_err(|_| "context_policy_unavailable")
}

pub(super) fn ensure_current_admission(
    root: &Path,
    sources: &[EngineContextSourceV1],
    bindings: &[EngineContextSourceDescriptorV1],
    budget_tokens: u64,
) -> Result<(), &'static str> {
    let policy = current_policy(root)?;
    if let Some(cap) = policy.budget_cap_tokens
        && budget_tokens > u64::try_from(cap).map_err(|_| "context_policy_unavailable")?
    {
        return Err("source_plan_changed");
    }
    let now = Utc::now();
    let evaluation_time = now.fixed_offset();
    for binding in bindings {
        let source = sources
            .iter()
            .find(|source| source.descriptor == *binding)
            .ok_or("source_plan_changed")?;
        let object = candidate(source, now)?.ok_or("source_plan_changed")?;
        if policy
            .candidate_violation_at(&object, Some(&evaluation_time))
            .is_some()
        {
            return Err("source_plan_changed");
        }
    }
    Ok(())
}

fn candidate(
    source: &EngineContextSourceV1,
    now: DateTime<Utc>,
) -> Result<Option<ContextObjectV1>, &'static str> {
    let descriptor = &source.descriptor;
    if descriptor.permission != EngineContextSourcePermissionV1::Permitted {
        return Ok(None);
    }
    let sensitivity = match descriptor.classification.as_ref() {
        Some(DataClassification::Public) => SensitivityLevel::Public,
        Some(DataClassification::Internal) => SensitivityLevel::Internal,
        Some(DataClassification::Confidential) => SensitivityLevel::Confidential,
        Some(DataClassification::Restricted) => SensitivityLevel::Restricted,
        None => return Ok(None),
    };
    for (timestamp, expiry) in [
        (&descriptor.observed_at, false),
        (&descriptor.valid_until, true),
    ] {
        if let Some(timestamp) = timestamp {
            let time = DateTime::parse_from_rfc3339(timestamp.as_str())
                .map_err(|_| "invalid_source_manifest")?;
            if (expiry && time <= now) || (!expiry && time > now) {
                return Ok(None);
            }
        }
    }
    let tokens = crate::core::tokens::count_tokens(&source.content).max(1);
    // Only full content is materialized: never advertise imaginary cheap views.
    let mut view_costs = ViewCosts::new();
    view_costs.set(ViewKind::Full, tokens);
    let observed = descriptor
        .observed_at
        .as_ref()
        .map(|time| time.as_str().to_owned());
    let mut metadata = HashMap::new();
    metadata.insert(
        "source_binding".to_owned(),
        serde_json::to_string(descriptor).map_err(|_| "invalid_source_manifest")?,
    );
    Ok(Some(ContextObjectV1 {
        id: ContextItemId(descriptor.object_ref.as_str().to_owned()),
        kind: ContextObjectKind::SearchChunk,
        source: descriptor.source_id.as_str().to_owned(),
        content_ref: descriptor.content_digest.as_str().to_owned(),
        title: descriptor.object_ref.as_str().to_owned(),
        content: Some(source.content.clone()),
        freshness: Freshness {
            created_at: observed.clone().unwrap_or_default(),
            ttl_secs: None,
            stale: observed.is_none(),
        },
        // No verified authority/reliability is inferred from producer metadata.
        confidence: 0.0,
        sensitivity,
        token_estimate: tokens,
        view_costs,
        provenance: Provenance {
            tool: Some("engine.explicit-sources.v1".to_owned()),
            timestamp: observed,
            ..Provenance::default()
        },
        semantic_fingerprint: None,
        metadata,
    }))
}
