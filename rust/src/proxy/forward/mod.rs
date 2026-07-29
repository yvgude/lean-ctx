//! Shared upstream forward path for OpenAI-compatible providers.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};

use super::ProxyState;
use super::connector::schedule_provider_connector;
use super::intent::classify_and_store_proxy_intent;

#[cfg(feature = "shape-xlat")]
mod xlat;

mod headers;
mod prepare;
pub mod trace_id;
mod transport;

#[cfg(test)]
mod tests;

#[allow(unused_imports)] // re-exported for proxy::* and tests
pub(super) use headers::{
    ALLOWED_REQUEST_HEADERS, FORWARDED_HEADERS, is_allowed_request_header,
    is_forwarded_response_header,
};
pub(super) use transport::xlat_stream_body;

// Unit tests import these via `use super::*`.
#[cfg(test)]
#[allow(unused_imports)]
use super::codec::{
    RequestBodyEncoding, decode_gzip_bounded, encode_gzip, encode_zstd, is_retryable_status,
    request_body_encoding,
};
#[cfg(test)]
#[allow(unused_imports)]
use axum::http::request::Parts;
#[cfg(test)]
#[allow(unused_imports)]
use headers::should_forward_request_header;
#[cfg(test)]
#[allow(unused_imports)]
pub(super) use prepare::{cohort_arm, prepare_request_body, wire_context};

const HEADROOM_COMPRESSED_HEADER: &str = "x-headroom-compressed";
const OCLA_BUDGET_SCOPE_HEADER: &str = "x-ocla-budget-scope";
const ESTIMATED_CHARS_PER_TOKEN: u64 = 4;

/// Check whether an incoming request was already compressed by Headroom.
pub(super) fn is_headroom_compressed(parts: &axum::http::request::Parts) -> bool {
    parts
        .headers
        .get(HEADROOM_COMPRESSED_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Default request-body ceiling (MiB). A large-codebase refactor with several
/// big files in context easily exceeds the old 10 MiB cap, which surfaced to the
/// agent as a hard `400` mid-task. Raised and made configurable via
/// `LEAN_CTX_PROXY_MAX_BODY_MB`.
const DEFAULT_MAX_BODY_MB: usize = 64;

pub(super) fn max_body_bytes() -> usize {
    std::env::var("LEAN_CTX_PROXY_MAX_BODY_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_MAX_BODY_MB)
        .saturating_mul(1024 * 1024)
}

fn apply_ocla_budget_admission(
    parts: &axum::http::request::Parts,
    estimated_bytes: usize,
) -> Result<(), StatusCode> {
    let Some(scope) = parts
        .headers
        .get(OCLA_BUDGET_SCOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let estimated_tokens = (estimated_bytes as u64).saturating_add(ESTIMATED_CHARS_PER_TOKEN - 1)
        / ESTIMATED_CHARS_PER_TOKEN;
    crate::core::ocla::wire_api::admit_budgeted_request(scope, estimated_tokens, 0.0)
        .map_err(|_| StatusCode::PAYMENT_REQUIRED)
}

pub async fn forward_request(
    State(state): State<ProxyState>,
    req: Request<Body>,
    upstream_base: &str,
    default_path: &str,
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    provider_label: &str,
    extra_stream_types: &[&str],
) -> Result<Response, StatusCode> {
    let (mut parts, body) = req.into_parts();
    let trace_id = trace_id::extract_or_generate_trace_id(&parts.headers);
    let body_limit = super::bedrock::request_body_limit(&parts).unwrap_or_else(max_body_bytes);
    let body_bytes = axum::body::to_bytes(body, body_limit)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let mut lineage = super::lineage::from_trusted_request(&parts, &body_bytes);
    if let Some(context) = lineage.as_mut() {
        context.trace_id.clone_from(&trace_id);
    }

    // Org-policy gate (enterprise#25): under a signed + trusted + enforced org
    // policy, refuse models outside the ceiling and requests over a hard
    // budget — before any routing/compression work. No policy → no-op.
    let gate_rules = super::policy_gate::active_rules();
    if let Some(rules) = &gate_rules {
        let tags = parts
            .extensions
            .get::<super::gateway_identity::GatewayTags>()
            .cloned()
            .unwrap_or_default();
        let requested_model = prepare::requested_model_of(&parts, &body_bytes);
        if let Err(refusal) = super::policy_gate::enforce(rules, requested_model.as_deref(), &tags)
        {
            tracing::warn!(
                "lean-ctx gateway: org policy refused request ({refusal:?}) \
                 person={:?} project={:?}",
                tags.person,
                tags.project
            );
            let mut response = super::policy_gate::refusal_response(&refusal, provider_label);
            trace_id::inject_trace_id(&mut response, &trace_id);
            return Ok(response);
        }
    }
    // Active router (enterprise#13): may rewrite `model` in the parsed body
    // (before compression, so exactly one serialization) and re-target the
    // upstream within the same wire shape. Fail-open: any miss routes nothing.
    // An org policy may exempt specific projects from downgrades (#25).
    let routing_rules = crate::core::config::Config::load().proxy.routing.clone();
    let downgrade_forbidden = gate_rules.as_ref().is_some_and(|rules| {
        let project = parts
            .extensions
            .get::<super::gateway_identity::GatewayTags>()
            .and_then(|t| t.project.clone());
        super::policy_gate::downgrade_forbidden(rules, project.as_deref())
    });
    let route_upstreams =
        (routing_rules.is_active() && !downgrade_forbidden).then(|| state.upstream_snapshot());
    // Cross-shape translation (enterprise#16) only exists for the exact
    // messages-create call — count_tokens/batches subpaths have no OpenAI
    // equivalent and must stay within-shape.
    let xlat_ok = cfg!(feature = "shape-xlat")
        && provider_label == "Anthropic"
        && parts
            .uri
            .path()
            .trim_end_matches('/')
            .ends_with("/v1/messages");
    let route_hook = |parsed: &mut serde_json::Value| {
        route_upstreams.as_ref().and_then(|up| {
            super::routing::route_request(parsed, provider_label, up, &routing_rules, xlat_ok)
        })
    };
    if is_headroom_compressed(&parts) {
        super::anthropic::set_headroom_request(true);
        super::prefix_cache_stats::record_headroom_compat();
    }
    let prepared = prepare::prepare_request_body(
        &parts,
        &body_bytes,
        compress_body,
        route_hook,
        upstream_base,
        provider_label == "OpenAI",
    )?;
    apply_ocla_budget_admission(&parts, prepared.body.len())?;
    let original_size = prepared.original_size;
    let compressed_size = prepared.compressed_size;
    let compression_candidate = prepared.compression_candidate;
    let preserve_content_encoding = prepared.preserve_content_encoding;
    let route = prepared.route;
    let parsed = prepared.parsed;
    let _intent_classification =
        classify_and_store_proxy_intent(&mut parts, parsed.as_ref(), lineage.as_ref(), &body_bytes);
    // Apply the routing decision to the wire: re-target the upstream and — for
    // registry providers holding their own key — swap the credential headers.
    let upstream_base = route
        .as_ref()
        .and_then(|r| r.upstream_base.as_deref())
        .unwrap_or(upstream_base);
    if let Some(provider) = route.as_ref().and_then(|r| r.credential.as_ref()) {
        super::providers::inject_gateway_credential(provider, &mut parts.headers)?;
    }
    schedule_provider_connector(&parts, lineage.as_ref(), route.as_ref(), provider_label);
    if let Some(ref parsed) = parsed {
        let provider = match provider_label {
            "Anthropic" | "Bedrock" => super::introspect::Provider::Anthropic,
            "OpenAI" | "ChatGPT" => super::introspect::Provider::OpenAi,
            _ => super::introspect::Provider::Gemini,
        };
        let breakdown = super::introspect::analyze_request(parsed, provider);
        state.introspect.record(breakdown);
    }
    // #895 Track B: assign output-savings holdout from the same pristine parsed
    // body that each provider's compressor receives. Only when active.
    let cohort = parsed
        .as_ref()
        .and_then(|p| prepare::cohort_arm(p, provider_label, default_path));
    if compression_candidate {
        // Shape label drives compression/routing; stats identity may differ —
        // Grok registry routes speak OpenAI shape but meter under "Grok".
        let registry_id = parts
            .extensions
            .get::<super::providers::RegistryProviderId>()
            .map(|r| r.id.as_str());
        let stats_label = super::providers::stats_label(registry_id, provider_label);
        state
            .stats
            .record_provider_request(stats_label, original_size, compressed_size);
    }

    let tokens_saved = original_size.saturating_sub(compressed_size) as u64 / 4;
    super::metrics::record_request(tokens_saved, compressed_size as u64);

    // Context Kernel: record identity, coverage, ETPAO for this request.
    {
        let proxy_headers: Vec<(String, String)> = parts
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let kernel_data = crate::core::context_kernel::proxy_bridge::ProxyRequestData {
            headers: proxy_headers,
            input_tokens: original_size / 4,
            output_tokens: 0,
            tokens_saved: tokens_saved as usize,
            model: parsed
                .as_ref()
                .and_then(|v| v.get("model"))
                .and_then(|m| m.as_str())
                .map(String::from),
            provider: Some(provider_label.to_owned()),
            request_count: 1,
            ..Default::default()
        };

        // Evidence pipeline: proxy data → envelope → normalizer → receipt chain.
        let kernel_result =
            crate::core::context_kernel::proxy_bridge::process_proxy_request(&kernel_data);
        crate::core::context_kernel::envelope_wiring::process_proxy_evidence(
            &kernel_data,
            &kernel_result,
        );
    }

    let model = parsed
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.as_str());
    let cache_prompt_hash = super::ocla_cache_bridge::prompt_hash(&body_bytes);
    if let (Some(cache), Some(model)) = (&state.ocla_cache, model)
        && let Some(cached) = cache.try_cache_hit(model, &cache_prompt_hash, 0.0, 0)
    {
        if let Some(route_decision) = &route {
            crate::proxy::routing_feedback::global_feedback().record_outcome_for_decision(
                &route_decision.decision_id,
                None,
                tokens_saved,
                0,
            );
        }
        let mut response = Response::builder()
            .status(cached.status)
            .body(Body::from(cached.body))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        trace_id::inject_trace_id(&mut response, &trace_id);
        return Ok(response);
    }
    super::cost::record(
        model,
        tokens_saved,
        original_size as u64,
        compressed_size as u64,
    );

    // Cross-shape route (enterprise#16): the body now speaks OpenAI Chat
    // Completions — address the matching endpoint instead of the caller's
    // `/v1/messages` path, and scan the response with the OpenAI parser.
    let xlat = route.as_ref().is_some_and(|r| r.xlat);
    let upstream_url = if xlat {
        format!("{upstream_base}/v1/chat/completions")
    } else {
        crate::proxy::codec::build_upstream_url(&parts, upstream_base, default_path)
    };

    let counterfactual = if provider_label == "Anthropic" && !xlat {
        super::counterfactual::maybe_spawn_probe(
            &state.client,
            &parts,
            upstream_base,
            parsed.as_ref(),
            route.as_ref().map(|r| r.routed_from.as_str()),
            compressed_size < original_size,
        )
    } else {
        None
    };

    let forwarded_body = super::bedrock::finalize_request(
        provider_label,
        &mut parts,
        &body_bytes,
        prepared.body,
        body_limit,
        &upstream_url,
    )?;

    if let Some(ref pre) = parsed {
        let cfg_replay = crate::core::config::Config::load();
        if matches!(
            cfg_replay.proxy.resolved_proxy_mode(),
            crate::core::config::ProxyMode::Cache
        ) {
            let system_val = pre.get("system");
            if let Some(msgs) = pre.get("messages").and_then(|m| m.as_array()) {
                let conv_id = super::prefix_replay::conversation_id(system_val, msgs);
                super::prefix_replay::record_forwarded(
                    conv_id,
                    forwarded_body.clone(),
                    msgs,
                    msgs.len(),
                );
            }
        }
    }

    let response = transport::send_upstream(
        &state,
        &parts,
        &upstream_url,
        forwarded_body,
        provider_label,
        preserve_content_encoding,
    )
    .await?;

    if let Some(route_decision) = &route {
        crate::proxy::routing_feedback::global_feedback().record_outcome_for_decision(
            &route_decision.decision_id,
            None,
            tokens_saved,
            0,
        );
    }

    // Measured usage: read the real model + billed tokens from the response.
    // Gemini puts the model in the URL path, not the request/response body.
    // Translated requests get OpenAI-shape responses regardless of the label.
    let usage_provider = if xlat {
        super::usage::Provider::OpenAi
    } else {
        super::usage::Provider::from_label(provider_label)
    };
    let url_model = if usage_provider == super::usage::Provider::Gemini {
        super::usage::gemini_model_from_path(parts.uri.path())
    } else {
        None
    };

    // Gateway context (enterprise#11/#17/#18): identity tags from the auth
    // guard + wire savings + baseline inputs, stamped onto the usage record.
    // A routed request is attributed to the provider actually serving it, and
    // carries the originally requested model as routed_from (enterprise#13).
    let mut wire = prepare::wire_context(
        &parts,
        provider_label,
        upstream_base,
        tokens_saved,
        original_size,
        lineage,
    );
    if let Some(route) = &route {
        wire.routed_from = Some(route.routed_from.clone());
        if let Some(id) = &route.provider_id {
            wire.provider = id.clone();
        }
        // Registry route targets carry their own local-inference flag
        // (shadow-rate billing); built-in targets keep the URL heuristic.
        if let Some(local) = route.local {
            wire.is_local = local;
        }
    }
    wire.counterfactual = counterfactual;
    let wire = Some(wire);
    let mut response = transport::build_response(
        response,
        extra_stream_types,
        usage_provider,
        url_model,
        cohort,
        wire,
        xlat,
        state.ocla_cache.as_deref(),
        model,
        &cache_prompt_hash,
    )
    .await?;
    trace_id::inject_trace_id(&mut response, &trace_id);
    Ok(response)
}
