// SPDX-License-Identifier: Apache-2.0

//! Adapter from the context kernel's semantic plan to the public V1 wire projection.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use lean_ctx_protocol::{
    ContextDispositionV1, ContextPlanId, ContextPlanProjectionV1, ContextProviderStatsV1,
    ContextReasonCodeV1, ContextSelectionV1, TaskId,
};

use super::types::ContextPlanV1;

/// Project the kernel-owned plan without duplicating selection or budgeting semantics.
pub fn project_context_plan(
    task_id: TaskId,
    plan: &ContextPlanV1,
) -> Result<ContextPlanProjectionV1> {
    let mut selections =
        Vec::with_capacity(plan.selected.len() + plan.excluded.len() + plan.deferred.len());
    for entry in &plan.selected {
        selections.push(ContextSelectionV1 {
            source_ref: entry.object_id.clone(),
            provider: entry.provider.clone(),
            disposition: ContextDispositionV1::Selected,
            token_count: u64::try_from(entry.tokens)
                .map_err(|_| anyhow!("selected token count does not fit u64"))?,
            sha256_digest: bound_content_digest(plan, &entry.object_id, &entry.provider),
            // Legacy PlanEntry.reason is display content, not selection metadata.
            // Never let source text leak into evidence or choose its reason code.
            reason_codes: vec![ContextReasonCodeV1::Relevant],
            reason_detail: Some("selected by canonical context compiler".to_owned()),
        });
    }
    for entry in &plan.excluded {
        selections.push(ContextSelectionV1 {
            source_ref: entry.object_id.clone(),
            provider: entry.provider.clone(),
            disposition: ContextDispositionV1::Excluded,
            token_count: 0,
            sha256_digest: bound_content_digest(plan, &entry.object_id, &entry.provider),
            reason_codes: vec![reason_code(
                &entry.reason,
                ContextReasonCodeV1::LowerUtility,
            )],
            reason_detail: Some(entry.reason.clone()),
        });
    }
    for entry in &plan.deferred {
        selections.push(ContextSelectionV1 {
            source_ref: entry.object_id.clone(),
            provider: entry.provider.clone(),
            disposition: ContextDispositionV1::Deferred,
            token_count: 0,
            sha256_digest: bound_content_digest(plan, &entry.object_id, &entry.provider),
            reason_codes: vec![reason_code(
                &entry.reason,
                ContextReasonCodeV1::DeferredForLater,
            )],
            reason_detail: Some(entry.reason.clone()),
        });
    }
    selections.sort_by(|left, right| {
        disposition_rank(left.disposition)
            .cmp(&disposition_rank(right.disposition))
            .then_with(|| left.source_ref.cmp(&right.source_ref))
            .then_with(|| left.provider.cmp(&right.provider))
    });

    let provider_stats = plan
        .provider_stats
        .iter()
        .map(|(provider, stats)| {
            Ok((
                provider.clone(),
                ContextProviderStatsV1 {
                    candidates_offered: u64::try_from(stats.candidates_offered)
                        .map_err(|_| anyhow!("provider candidates_offered does not fit u64"))?,
                    candidates_selected: u64::try_from(stats.candidates_selected)
                        .map_err(|_| anyhow!("provider candidates_selected does not fit u64"))?,
                    tokens_used: u64::try_from(stats.tokens_used)
                        .map_err(|_| anyhow!("provider tokens_used does not fit u64"))?,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    let mut projection = ContextPlanProjectionV1 {
        schema_version: 1,
        context_plan_id: ContextPlanId::try_from(plan.plan_id.clone())
            .map_err(|error| anyhow!(error.to_string()))?,
        task_id,
        projection_digest: None,
        budget_tokens: u64::try_from(plan.budget.total_tokens)
            .map_err(|_| anyhow!("context budget does not fit u64"))?,
        selections,
        provider_stats,
        policy_decision_refs: Vec::new(),
        evidence: Vec::new(),
        extensions: Default::default(),
    };
    projection.projection_digest = Some(
        projection
            .compute_projection_digest()
            .map_err(|error| anyhow!(error.to_string()))?,
    );
    projection
        .validate()
        .map_err(|error| anyhow!(error.to_string()))?;
    Ok(projection)
}

/// Prefer kernel-owned content identity; an opaque object alias is not content.
fn bound_content_digest(plan: &ContextPlanV1, object_id: &str, source: &str) -> Option<String> {
    match plan.origins.get(object_id) {
        Some(origins) => match origins.as_slice() {
            [origin] if origin.source == source => content_digest(&origin.content_ref),
            _ => None,
        },
        // Historical manually assembled plans lack origin bindings. Preserve their
        // compatibility projection; never use this fallback for a bound candidate.
        None => content_digest(object_id),
    }
}

/// Parse an explicitly supplied content address.
fn content_digest(source_ref: &str) -> Option<String> {
    let digest = source_ref.strip_prefix("sha256:").unwrap_or(source_ref);
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("sha256:{digest}"))
}

fn disposition_rank(disposition: ContextDispositionV1) -> u8 {
    match disposition {
        ContextDispositionV1::Selected => 0,
        ContextDispositionV1::Excluded => 1,
        ContextDispositionV1::Deferred => 2,
    }
}

fn reason_code(reason: &str, fallback: ContextReasonCodeV1) -> ContextReasonCodeV1 {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("budget") {
        ContextReasonCodeV1::BudgetExceeded
    } else if reason.contains("policy") || reason.contains("sensitive") {
        ContextReasonCodeV1::PolicyExcluded
    } else if reason.contains("cache") {
        ContextReasonCodeV1::CacheHit
    } else if reason.contains("required") {
        ContextReasonCodeV1::Required
    } else {
        fallback
    }
}
