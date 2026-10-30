// SPDX-License-Identifier: Apache-2.0

//! Ephemeral explicit-source provider; never persists into project/global stores.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use lean_ctx_protocol::context_gateway::ClassificationV1;
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
    ensure_project_root_matches_scope(root)?;
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
    pub(super) admitted_sources: Vec<AdmittedSourceBody>,
}

/// Private, freshly admitted bodies for the selected source bindings. The
/// descriptor still identifies the caller's original bytes; the retained
/// candidate holds the current policy-approved representation for delivery.
pub(super) struct AdmittedSourceBody {
    pub(super) descriptor: EngineContextSourceDescriptorV1,
    pub(super) candidate: ContextObjectV1,
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
    ensure_project_root_matches_scope(root)?;
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
    ensure_project_root_matches_scope(root)?;
    let project_root = root.to_str().ok_or("unsafe_root")?;
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        plan_at_in_view(root, request, evaluation_time, include_marker)
    })
    .map_err(|_| "source_policy_view_unavailable")?
}

fn plan_at_in_view(
    root: &Path,
    request: EngineContextSourcePlanRequestV1,
    evaluation_time: DateTime<chrono::FixedOffset>,
    include_marker: bool,
) -> Result<SourcePlanningHandoff, &'static str> {
    let now = Utc::now();
    let max_sensitivity = current_policy(root)?.max_sensitivity;
    let mut candidates = request
        .sources
        .iter()
        .map(|source| candidate(source, now, &max_sensitivity))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    // Bound enumeration deterministically, never by untrusted input order.
    candidates.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let admitted_candidate_by_ref = candidates
        .iter()
        .map(|candidate| (candidate.id.0.clone(), candidate.clone()))
        .collect::<HashMap<_, _>>();
    let provider = ExplicitSourceProvider(candidates);
    let controller = AutopilotController::with_kernels(
        ContextKernel::with_field(
            vec![Box::new(provider.clone())],
            ContextField::with_weights(FieldWeights::default()),
        )
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
    let source_bindings = request
        .sources
        .iter()
        .filter(|source| {
            initial_result.plan.selections.iter().any(|selection| {
                selection.source_ref == source.descriptor.object_ref.as_str()
                    && selection.disposition == ContextDispositionV1::Selected
            })
        })
        .map(|source| source.descriptor.clone())
        .collect::<Vec<_>>();
    let admitted_sources = source_bindings
        .iter()
        .map(|descriptor| {
            let candidate = admitted_candidate_by_ref
                .get(descriptor.object_ref.as_str())
                .ok_or("source_plan_changed")?;
            Ok(AdmittedSourceBody {
                descriptor: descriptor.clone(),
                candidate: candidate.clone(),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let admission_metadata = source_admission_metadata(&admitted_sources)?;
    // Two binds in the single bind's key order, so each bound has its own
    // typed failure: an oversized lineage keeps the contract's
    // `source_lineage_too_large` (docs/contracts/engine-context-sources-v1.md),
    // never the newer admission-metadata code.
    let decision = decision
        .bind_source_metadata(lineage, epoch, None)
        .map_err(|_| "source_lineage_too_large")?
        .bind_source_metadata(None, None, Some(admission_metadata))
        .map_err(|_| "source_plan_metadata_too_large")?;
    let result = super::response(&decision)?;
    // Reload immediately before returning: the replay identity is not a
    // concurrency lock, so a policy change must win over its older snapshot.
    ensure_current_admission(
        root,
        &request.sources,
        &source_bindings,
        &admitted_sources,
        result.plan.budget_tokens,
    )?;
    let response =
        EngineContextSourcePlanResponseV1::new(result, source_bindings).map_err(|_| "internal")?;
    // No presentation is recorded here: selection alone is not yet a
    // delivery under the project's delivery policy. The revalidation that
    // closes a materialization records it (`revalidate_admitted_sources`).
    Ok(SourcePlanningHandoff {
        decision,
        response,
        admitted_sources,
    })
}

fn current_policy(root: &Path) -> Result<ContextPolicy, &'static str> {
    let root = root.to_str().ok_or("unsafe_root")?;
    ContextPolicy::from_config(root).map_err(|_| "context_policy_unavailable")
}

/// The kernel context policy (sensitivity ceiling, budget cap, retention) in
/// a stable serialized form — one part of the policy identity a presented
/// source is admitted under. `None`: the policy cannot be loaded.
pub(crate) fn kernel_policy_identity(root: &Path) -> Option<String> {
    serde_json::to_string(&current_policy(root).ok()?).ok()
}

/// The bindings of the selected, admitted caller-supplied sources as they
/// hold now. `None` — no lineage at all — when one of them is restricted
/// (owner decision E3: a restricted source is in no reusable lineage) or the
/// policy identity cannot be resolved; an empty selection is `None` too.
pub(super) fn presented_bindings(
    root: &Path,
    admitted_sources: &[AdmittedSourceBody],
) -> Option<Vec<crate::core::archive::presented::PresentedBinding>> {
    if admitted_sources.is_empty() {
        return None;
    }
    admitted_sources
        .iter()
        .map(|source| {
            if source.candidate.sensitivity == SensitivityLevel::Restricted {
                return None;
            }
            crate::core::archive::presented::PresentedBinding::admitted_now(
                root,
                source.descriptor.object_ref.as_str(),
                source.descriptor.source_id.as_str(),
                source.descriptor.content_digest.as_str(),
            )
        })
        .collect()
}

/// Record "presented and admitted now" for every admitted caller-supplied
/// source that is not restricted. Best effort: a presentation that cannot be
/// recorded leaves the binding without authority, never with a wrong one.
fn record_presentations(root: &Path, admitted_sources: &[AdmittedSourceBody]) {
    for source in admitted_sources {
        if source.candidate.sensitivity == SensitivityLevel::Restricted {
            continue;
        }
        if let Some(binding) = crate::core::archive::presented::PresentedBinding::admitted_now(
            root,
            source.descriptor.object_ref.as_str(),
            source.descriptor.source_id.as_str(),
            source.descriptor.content_digest.as_str(),
        ) {
            crate::core::archive::presented::record_presentation(&binding);
        }
    }
}

fn ensure_project_root_matches_scope(root: &Path) -> Result<(), &'static str> {
    let request_root = crate::core::policy::runtime::detached_project_root(root)
        .map_err(|_| "source_policy_view_unavailable")?;
    if request_root != root {
        let supplied = root
            .canonicalize()
            .map_err(|_| "source_policy_view_unavailable")?;
        let requested = request_root
            .canonicalize()
            .map_err(|_| "source_policy_view_unavailable")?;
        if supplied != requested {
            return Err("source_policy_view_mismatch");
        }
    }
    Ok(())
}

fn source_admission_metadata(
    sources: &[AdmittedSourceBody],
) -> Result<serde_json::Value, &'static str> {
    let mut records = sources
        .iter()
        .map(|source| {
            let content = source
                .candidate
                .content
                .as_deref()
                .ok_or("source_plan_changed")?;
            let admitted_digest = super::super::sha256_digest(content.as_bytes())
                .map_err(|_| "source_plan_digest_failed")?;
            Ok(serde_json::json!({
                "object_ref": source.descriptor.object_ref.as_str(),
                "source_id": source.descriptor.source_id.as_str(),
                "original_digest": source.descriptor.content_digest.as_str(),
                "admitted_digest": admitted_digest.as_str(),
                "effective_classification": classification_name(&source.candidate.sensitivity),
            }))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    records.sort_by(|left, right| {
        left["object_ref"]
            .as_str()
            .cmp(&right["object_ref"].as_str())
    });
    Ok(serde_json::json!({
        "schema_version": 1,
        "sources": records,
    }))
}

fn classification_name(sensitivity: &SensitivityLevel) -> &'static str {
    match sensitivity {
        SensitivityLevel::Public => "public",
        SensitivityLevel::Internal => "internal",
        SensitivityLevel::Confidential => "confidential",
        SensitivityLevel::Restricted => "restricted",
    }
}

pub(super) fn ensure_current_admission(
    root: &Path,
    sources: &[EngineContextSourceV1],
    bindings: &[EngineContextSourceDescriptorV1],
    admitted_sources: &[AdmittedSourceBody],
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
        sources
            .iter()
            .find(|source| source.descriptor == *binding)
            .ok_or("source_plan_changed")?;
        let object = admitted_sources
            .iter()
            .find(|source| source.descriptor == *binding)
            .map(|source| &source.candidate)
            .ok_or("source_plan_changed")?;
        if policy
            .candidate_violation_at(&object, Some(&evaluation_time))
            .is_some()
        {
            return Err("source_plan_changed");
        }
    }
    Ok(())
}

pub(super) fn revalidate_admitted_sources(
    root: &Path,
    sources: &[EngineContextSourceV1],
    bindings: &[EngineContextSourceDescriptorV1],
    admitted_sources: &[AdmittedSourceBody],
    budget_tokens: u64,
) -> Result<(), &'static str> {
    ensure_project_root_matches_scope(root)?;
    let project_root = root.to_str().ok_or("unsafe_root")?;
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        let policy = current_policy(root)?;
        if let Some(cap) = policy.budget_cap_tokens
            && budget_tokens > u64::try_from(cap).map_err(|_| "context_policy_unavailable")?
        {
            return Err("source_plan_changed");
        }
        let now = Utc::now();
        let evaluation_time = now.fixed_offset();
        let mut revalidated = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let original = sources
                .iter()
                .find(|source| source.descriptor == *binding)
                .ok_or("source_plan_changed")?;
            let retained = admitted_sources
                .iter()
                .find(|source| source.descriptor == *binding)
                .ok_or("source_plan_changed")?;
            let current =
                candidate(original, now, &policy.max_sensitivity)?.ok_or("source_plan_changed")?;
            if current.content != retained.candidate.content
                || current.sensitivity != retained.candidate.sensitivity
                || policy
                    .candidate_violation_at(&current, Some(&evaluation_time))
                    .is_some()
            {
                return Err("source_plan_changed");
            }
            revalidated.push(AdmittedSourceBody {
                descriptor: binding.clone(),
                candidate: current,
            });
        }
        // Every binding was presented again in this request and admitted now.
        record_presentations(root, &revalidated);
        Ok(())
    })
    .map_err(|_| "source_policy_view_unavailable")?
}

/// `Ok(None)`: the source is rejected BEFORE it becomes a candidate and leaves
/// no selection entry (contract: "rejected pre-candidate inputs have no
/// selection entry").
///
/// That includes a source whose body carries a classification marking the
/// gateway DETECTS, above both what its descriptor declares and what the
/// kernel policy admits (`max_sensitivity`). Its descriptor misstates the
/// body, so the input is rejected like any other invalid manifest — and a
/// canonical exclusion entry would project the SHA-256 of content that is
/// more sensitive than this plan may carry. A source whose DECLARED
/// classification exceeds the policy is unchanged: it stays a candidate and
/// the kernel records its canonical policy exclusion.
fn candidate(
    source: &EngineContextSourceV1,
    now: DateTime<Utc>,
    max_sensitivity: &SensitivityLevel,
) -> Result<Option<ContextObjectV1>, &'static str> {
    let descriptor = &source.descriptor;
    if descriptor.permission != EngineContextSourcePermissionV1::Permitted {
        return Ok(None);
    }
    let declared_classification = match descriptor.classification.as_ref() {
        Some(DataClassification::Public) => ClassificationV1::Public,
        Some(DataClassification::Internal) => ClassificationV1::Internal,
        Some(DataClassification::Confidential) => ClassificationV1::Confidential,
        Some(DataClassification::Restricted) => ClassificationV1::Restricted,
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
    let admission = match crate::core::context_admission::admit_context_source(&source.content) {
        Ok(admission) => admission,
        // Withheld sources do not enter selection. The admission layer records
        // the content-free decision; the plan never echoes the rejected body.
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(_) => return Err("source_admission_unavailable"),
    };
    let admitted_maximum = match max_sensitivity {
        SensitivityLevel::Public => ClassificationV1::Public,
        SensitivityLevel::Internal => ClassificationV1::Internal,
        SensitivityLevel::Confidential => ClassificationV1::Confidential,
        SensitivityLevel::Restricted => ClassificationV1::Restricted,
    };
    // What an unmarked object is classified as in the configured mode: only a
    // classification above it comes from a marking detected in the body.
    let unmarked = crate::core::config::Config::load_arc()
        .context_gateway
        .mode
        .unclassified();
    let detected = admission.classification;
    if detected > unmarked && detected > declared_classification && detected > admitted_maximum {
        return Ok(None);
    }
    let classification = declared_classification.join(admission.classification);
    let sensitivity = match classification {
        ClassificationV1::Public => SensitivityLevel::Public,
        ClassificationV1::Internal => SensitivityLevel::Internal,
        ClassificationV1::Confidential => SensitivityLevel::Confidential,
        ClassificationV1::Restricted => SensitivityLevel::Restricted,
    };
    let tokens = crate::core::tokens::count_tokens(&admission.text).max(1);
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
        content: Some(admission.text),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn source(content: &str, declared: &str) -> EngineContextSourceV1 {
        let digest = super::super::super::sha256_digest(content.as_bytes()).expect("digest");
        serde_json::from_value(serde_json::json!({
            "descriptor": {
                "object_ref": "marked", "source_id": "source-marked",
                "source_type": "filesystem", "content_digest": digest.as_str(),
                "revision": "revision-one", "owner": "operator",
                "observed_at": null, "valid_until": null,
                "classification": declared, "permission": "permitted"
            },
            "content": content
        }))
        .expect("source fixture")
    }

    /// Both branches of the pre-candidate decision. Before, a source whose
    /// body carries a marking above its descriptor became a candidate, and its
    /// canonical exclusion projected the digest of the restricted body.
    #[test]
    fn a_detected_classification_above_declaration_and_policy_is_no_candidate() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(
            dir.join("config.toml"),
            "[context_gateway]\nclassification = \"warn\"\n",
        )
        .expect("config");
        let now = Utc::now();
        let marked = "*** TOP SECRET *** invoice ledger plans";

        // Detected restricted, declared public, policy admits internal:
        // rejected before candidacy — no selection entry, no digest.
        let laundered = candidate(&source(marked, "Public"), now, &SensitivityLevel::Internal)
            .expect("no transport error");
        assert!(laundered.is_none(), "old code returned a candidate");

        // The policy admits restricted content: it stays a candidate, with
        // the DETECTED sensitivity, never the declared one.
        let admitted = candidate(
            &source(marked, "Public"),
            now,
            &SensitivityLevel::Restricted,
        )
        .expect("no transport error")
        .expect("candidate");
        assert_eq!(admitted.sensitivity, SensitivityLevel::Restricted);

        // Declared too high, nothing detected: unchanged — a candidate, so the
        // kernel records its canonical policy exclusion with its metadata.
        let declared = candidate(
            &source("invoice ledger plans", "Restricted"),
            now,
            &SensitivityLevel::Internal,
        )
        .expect("no transport error")
        .expect("candidate for the canonical exclusion");
        assert_eq!(declared.sensitivity, SensitivityLevel::Restricted);

        // Unmarked and public: an ordinary candidate with its body unchanged.
        let plain = candidate(
            &source("invoice ledger plans", "Public"),
            now,
            &SensitivityLevel::Internal,
        )
        .expect("no transport error")
        .expect("candidate");
        assert_eq!(plain.sensitivity, SensitivityLevel::Public);
        assert_eq!(plain.content.as_deref(), Some("invoice ledger plans"));
    }
}
