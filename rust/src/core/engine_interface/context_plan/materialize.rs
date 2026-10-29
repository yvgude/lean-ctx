// SPDX-License-Identifier: Apache-2.0

//! Bind a previously returned explicit-source plan to one immutable context
//! snapshot.  Selection remains owned by `sources::plan`; this module only
//! rejoins validated bodies, applies existing delivery policy, and measures
//! the resulting bytes.

use std::{fmt::Write as _, path::Path};

use chrono::DateTime;
use lean_ctx_protocol::{
    EngineContextSourceDescriptorV1, EngineContextSourceMaterializationRequestV1,
    EngineContextSourceMaterializationResponseV1, EngineContextSourcePlanResponseV1,
    MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES, Sha256Digest,
};

use super::sources;

// Materialization is a bounded context read, so existing ctx_read allow/deny
// rules continue to govern it without introducing a second policy vocabulary.
const MATERIALIZATION_POLICY_NAME: &str = "ctx_read";

/// Replan and materialize the exact selected source bodies without creating a
/// cache, changing selection, or truncating the rendered context.
pub(crate) fn materialize_source_plan(
    root: &Path,
    request: &EngineContextSourceMaterializationRequestV1,
) -> Result<EngineContextSourceMaterializationResponseV1, &'static str> {
    Ok(materialize_source_plan_with_decision(root, request)?.0)
}

pub(super) fn materialize_source_plan_with_decision(
    root: &Path,
    request: &EngineContextSourceMaterializationRequestV1,
) -> Result<
    (
        EngineContextSourceMaterializationResponseV1,
        super::TaskAutopilotDecision,
    ),
    &'static str,
> {
    request
        .validate_payload()
        .map_err(|_| "invalid_source_materialization_request")?;
    let expected_binding_digest = request.expected_binding_digest.clone();
    let source_plan = request.source_plan.clone();
    let evaluation_time = request
        .planning_evaluation_time
        .as_ref()
        .map(|value| {
            DateTime::parse_from_rfc3339(value.as_str()).map_err(|_| "source_plan_epoch_invalid")
        })
        .transpose()?;
    let handoff = sources::plan_for_materialization(root, source_plan.clone(), evaluation_time)?;
    let plan = handoff.response;
    plan.validate_binding()
        .map_err(|_| "source_plan_binding_invalid")?;
    if plan.binding_digest != expected_binding_digest {
        return Err("source_plan_changed");
    }

    // The replay handoff carries bodies admitted against current Gateway and
    // project policy; never rejoin the unadmitted wire payload here.
    let selected = selected_source_bodies(&plan, &handoff.admitted_sources)?;
    let joined = render_selected_sources(&selected)?;
    let (content, policy_budget) = apply_delivery_policies(root, &joined)?;
    let effective_budget = policy_budget.map_or(plan.result.plan.budget_tokens, |limit| {
        plan.result.plan.budget_tokens.min(limit)
    });
    let materialized_token_count = enforce_rendered_budget(&content, effective_budget)?;
    if content.len() > MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES {
        return Err("source_materialization_too_large");
    }
    let materialized_digest = sha256_digest(content.as_bytes())?;
    let response = EngineContextSourceMaterializationResponseV1 {
        schema_version: plan.result.schema_version,
        transport_version: plan.result.transport_version,
        engine_interface_version: plan.result.engine_interface_version.clone(),
        plan,
        materialized_digest,
        materialized_token_count,
        content,
    };
    response
        .validate()
        .map_err(|_| "source_materialization_invalid")?;
    sources::revalidate_admitted_sources(
        root,
        &source_plan.sources,
        &response.plan.source_bindings,
        &handoff.admitted_sources,
        response.plan.result.plan.budget_tokens,
    )?;
    // Release point of prepared context: register the exact delivered text
    // with the caller-supplied sources it was joined from. Nothing is
    // recorded unless every one of them is bound, presented and admitted now
    // (a restricted source leaves the whole block without lineage).
    record_delivery(root, &response.content, &handoff.admitted_sources);
    Ok((response, handoff.decision))
}

fn record_delivery(root: &Path, delivered: &str, admitted_sources: &[sources::AdmittedSourceBody]) {
    use crate::core::archive::delivered;
    let Some(bindings) = sources::presented_bindings(root, admitted_sources) else {
        return;
    };
    let Some(project_root) = root.to_str() else {
        return;
    };
    let lineage = delivered::Lineage::of_presented(bindings);
    let _ = crate::core::policy::runtime::with_project_source_view(project_root, || {
        delivered::record_lineage(&delivered::digest(delivered), &lineage)
    });
}

/// Register a text the Engine DERIVED from a materialized context (the
/// compressed view of source execution) with the lineage of that context:
/// the derivation adds no source. Nothing is recorded when the materialized
/// text has no lineage on record or a source of it no longer holds.
pub(crate) fn record_derived_delivery(root: &Path, materialized: &str, derived: &str) {
    use crate::core::archive::delivered;
    let Some(project_root) = root.to_str() else {
        return;
    };
    let _ = crate::core::policy::runtime::with_project_source_view(project_root, || {
        delivered::lineage_of(&delivered::digest(materialized))
            .is_some_and(|lineage| delivered::record_lineage(&delivered::digest(derived), &lineage))
    });
}

fn selected_source_bodies<'a>(
    plan: &EngineContextSourcePlanResponseV1,
    sources: &'a [sources::AdmittedSourceBody],
) -> Result<Vec<(&'a EngineContextSourceDescriptorV1, &'a str)>, &'static str> {
    plan.source_bindings
        .iter()
        .map(|binding| {
            let source = sources
                .iter()
                .find(|source| source.descriptor == *binding)
                .ok_or("source_plan_body_missing")?;
            Ok((
                &source.descriptor,
                source
                    .candidate
                    .content
                    .as_deref()
                    .ok_or("source_plan_body_missing")?,
            ))
        })
        .collect()
}

/// Join selected full bodies in the response's canonical object-reference
/// order.  Framing is intentionally plain and deterministic; it is not a
/// second selection or source-policy algorithm.
fn render_selected_sources(
    selected: &[(&EngineContextSourceDescriptorV1, &str)],
) -> Result<String, &'static str> {
    let mut output = String::new();
    for (index, (descriptor, content)) in selected.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        writeln!(&mut output, "## leanctx-source-v1")
            .map_err(|_| "source_materialization_render_failed")?;
        writeln!(
            &mut output,
            "object_ref: {}",
            descriptor.object_ref.as_str()
        )
        .map_err(|_| "source_materialization_render_failed")?;
        writeln!(&mut output, "source_id: {}", descriptor.source_id.as_str())
            .map_err(|_| "source_materialization_render_failed")?;
        writeln!(
            &mut output,
            "content_digest: {}",
            descriptor.content_digest.as_str()
        )
        .map_err(|_| "source_materialization_render_failed")?;
        output.push('\n');
        output.push_str(content);
        if !content.ends_with('\n') {
            output.push('\n');
        }
        if output.len() > MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES {
            return Err("source_materialization_too_large");
        }
    }
    Ok(output)
}

fn enforce_rendered_budget(content: &str, budget_tokens: u64) -> Result<u64, &'static str> {
    let token_count = u64::try_from(crate::core::tokens::count_tokens(content))
        .map_err(|_| "source_materialization_budget_unavailable")?;
    if token_count > budget_tokens {
        return Err("source_materialization_budget_exceeded");
    }
    Ok(token_count)
}

/// Reuse the existing delivery chokepoints; policy rewrites are reflected in
/// the returned materialized digest, while blocked/warning outcomes fail closed.
fn apply_delivery_policies(
    root: &Path,
    input: &str,
) -> Result<(String, Option<u64>), &'static str> {
    // Blocking detectors must see the original content, before another
    // redactor or sensitivity transform can erase a blocking signal.
    let mut text = input.to_owned();
    let config = crate::core::config::Config::load();

    let project_policy = crate::core::policy::runtime::for_project(root)
        .map_err(|_| "source_materialization_policy_unavailable")?;
    if let Some(active) = project_policy {
        if !active.tool_allowed(MATERIALIZATION_POLICY_NAME) {
            return Err("source_materialization_policy_rejected");
        }
        let policy_budget = active.resolved.max_context_tokens.map(u64::from);
        let outcome = crate::core::policy::content::evaluate_text(&text, &active);
        crate::server::policy_guard::audit_filter(
            MATERIALIZATION_POLICY_NAME,
            &outcome.audit,
            outcome.blocked,
        );
        if outcome.blocked || !outcome.warnings.is_empty() {
            return Err("source_materialization_policy_rejected");
        }
        text = crate::core::sensitivity::enforce_text(
            outcome.text,
            None,
            &config.sensitivity_effective(),
        )
        .into_text();
        let delivered = crate::core::redaction::redact_text_if_enabled(&text);
        // The last step before the result exists: the budget above was read
        // from the policy and the text passed it, but neither is delivered
        // under a policy whose signed horizon has passed since.
        if !active.within_horizon() {
            return Err("source_materialization_policy_rejected");
        }
        return Ok((delivered, policy_budget));
    }
    text = crate::core::sensitivity::enforce_text(text, None, &config.sensitivity_effective())
        .into_text();
    Ok((crate::core::redaction::redact_text_if_enabled(&text), None))
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, &'static str> {
    super::super::sha256_digest(bytes).map_err(|_| "source_materialization_digest_failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        DataClassification, EngineContextSourcePermissionV1, EngineContextSourceTypeV1,
        ProtocolReference, SourceId,
    };

    fn descriptor(
        object_ref: &str,
        source_id: &str,
        content: &str,
    ) -> EngineContextSourceDescriptorV1 {
        let digest = sha256_digest(content.as_bytes()).expect("content digest");
        EngineContextSourceDescriptorV1 {
            object_ref: ProtocolReference::new(object_ref).expect("object reference"),
            source_id: SourceId::new(source_id).expect("source ID"),
            source_type: EngineContextSourceTypeV1::Other,
            content_digest: digest,
            revision: None,
            owner: None,
            observed_at: None,
            valid_until: None,
            classification: Some(DataClassification::Public),
            permission: EngineContextSourcePermissionV1::Permitted,
        }
    }

    fn admitted_source(
        descriptor: EngineContextSourceDescriptorV1,
        content: &str,
    ) -> sources::AdmittedSourceBody {
        let tokens = crate::core::tokens::count_tokens(content).max(1);
        let mut view_costs = crate::core::context_field::ViewCosts::new();
        view_costs.set(crate::core::context_field::ViewKind::Full, tokens);
        sources::AdmittedSourceBody {
            candidate: crate::core::context_kernel::types::ContextObjectV1 {
                id: crate::core::context_field::ContextItemId(
                    descriptor.object_ref.as_str().to_owned(),
                ),
                kind: crate::core::context_kernel::types::ContextObjectKind::SearchChunk,
                source: descriptor.source_id.as_str().to_owned(),
                content_ref: descriptor.content_digest.as_str().to_owned(),
                title: descriptor.object_ref.as_str().to_owned(),
                content: Some(content.to_owned()),
                freshness: crate::core::context_kernel::types::Freshness {
                    created_at: String::new(),
                    ttl_secs: None,
                    stale: true,
                },
                confidence: 0.0,
                sensitivity: crate::core::context_kernel::types::SensitivityLevel::Public,
                token_estimate: tokens,
                view_costs,
                provenance: crate::core::context_field::Provenance::default(),
                semantic_fingerprint: None,
                metadata: std::collections::HashMap::new(),
            },
            descriptor,
        }
    }

    #[test]
    fn selected_sources_render_in_canonical_binding_order_without_body_loss() {
        let z_content = "z body\nwith two lines";
        let a_content = "a body";
        let z = descriptor("z-ref", "z-source", z_content);
        let a = descriptor("a-ref", "a-source", a_content);
        let sources = [
            admitted_source(z.clone(), z_content),
            admitted_source(a.clone(), a_content),
        ];
        let selected = selected_source_bodies(
            &EngineContextSourcePlanResponseV1 {
                result: serde_json::from_value(serde_json::json!({
                    "schema_version":1,"transport_version":1,
                    "engine_interface_version":"1.0.0",
                    "plan":{"schema_version":1,"context_plan_id":"plan-1",
                      "task_id":"task-1","projection_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
                      "budget_tokens":100,"selections":[],"provider_stats":{},"policy_decision_refs":[],"evidence":[]}
                }))
                .expect("test plan"),
                source_bindings: vec![a.clone(), z.clone()],
                binding_digest: Sha256Digest::new(
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                )
                .expect("test binding digest"),
            },
            &sources,
        )
        .expect("matching source bodies");
        let rendered = render_selected_sources(&selected).expect("rendered context");
        assert!(
            rendered.find("a body").expect("a body") < rendered.find("z body").expect("z body")
        );
        assert!(rendered.contains("z body\nwith two lines"));
    }

    #[test]
    fn rendered_budget_rejects_instead_of_truncating() {
        let content = "one two three";
        let actual = crate::core::tokens::count_tokens(content) as u64;
        assert!(actual > 1);
        assert_eq!(
            enforce_rendered_budget(content, actual).expect("exact budget"),
            actual
        );
        assert_eq!(
            enforce_rendered_budget(content, actual - 1),
            Err("source_materialization_budget_exceeded")
        );
        assert_eq!(content, "one two three");
    }
}
