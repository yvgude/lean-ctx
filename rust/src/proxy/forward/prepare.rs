//! Request-body preparation: parse, translate, compress.

use axum::http::{StatusCode, request::Parts};
use std::borrow::Cow;

use crate::proxy::codec::{
    RequestBodyEncoding, decode_gzip_bounded, decode_zstd_bounded, encode_gzip, encode_zstd,
    request_body_encoding,
};

use super::max_body_bytes;

#[cfg(feature = "shape-xlat")]
use super::xlat::translated_openai_body;

/// Requested model for the policy gate (enterprise#25): from the JSON body
/// (Anthropic/OpenAI dialects) or the URL path (Gemini). Encrypted-passthrough
/// or unparseable bodies yield `None` — the ceiling governs what the gateway
/// can see; budgets (identity-keyed) still apply to every request.
pub(crate) fn requested_model_of(parts: &Parts, body_bytes: &[u8]) -> Option<String> {
    if let Some(m) = crate::proxy::usage::gemini_model_from_path(parts.uri.path()) {
        return Some(m);
    }
    let decoded: Cow<'_, [u8]> = match request_body_encoding(parts) {
        RequestBodyEncoding::Identity => Cow::Borrowed(body_bytes),
        RequestBodyEncoding::Gzip => {
            Cow::Owned(decode_gzip_bounded(body_bytes, max_body_bytes()).ok()?)
        }
        RequestBodyEncoding::Zstd => {
            Cow::Owned(decode_zstd_bounded(body_bytes, max_body_bytes()).ok()?)
        }
        RequestBodyEncoding::Passthrough => return None,
    };
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

/// Builds the request-side [`WireContext`](crate::proxy::usage::WireContext) stamped
/// onto this turn's usage record: identity tags (inserted as a request
/// extension by the auth guard, enterprise#11), the per-request compression
/// saving, the pre-compression token estimate (baseline input, enterprise#18)
/// and whether the serving upstream is local.
pub(crate) fn wire_context(
    parts: &Parts,
    provider_label: &str,
    upstream_base: &str,
    tokens_saved: u64,
    original_size: usize,
    lineage: Option<crate::core::ocla::OclaRequestContext>,
) -> Box<crate::proxy::usage::WireContext> {
    #[cfg(feature = "enterprise")]
    let tags = parts
        .extensions
        .get::<crate::proxy::gateway_identity::GatewayTags>()
        .cloned()
        .unwrap_or_default();
    #[cfg(not(feature = "enterprise"))]
    let tags = crate::proxy::usage::WireContext::default();
    // Registry routes attribute usage to the provider identity ("foundry",
    // "local"), not the wire-shape label ("OpenAI") — shape ≠ identity. The
    // entry's resolved local flag rides along (shadow-rate billing for
    // non-loopback local endpoints, e.g. host.docker.internal).
    let registry = parts
        .extensions
        .get::<crate::proxy::providers::RegistryProviderId>();
    let provider = registry.map_or(provider_label, |r| r.id.as_str());
    let is_local = registry.map_or_else(
        || crate::proxy::codec::upstream_is_local(upstream_base),
        |r| r.local,
    );
    Box::new(crate::proxy::usage::WireContext {
        provider: provider.to_string(),
        person: tags.person,
        team: tags.team,
        project: tags.project,
        saved_tokens: tokens_saved,
        // bytes/4 — the same estimation basis the proxy stats use throughout.
        uncompressed_input_tokens: original_size as u64 / 4,
        is_local,
        routed_from: None,    // populated by the routing hook (wave 3)
        counterfactual: None, // populated after the probe spawn (#701)
        lineage,
    })
}

/// Output-savings arm (#895) for a request body, or `None` when no holdout is
/// active. Keyed per provider; OpenAI's Chat vs Responses bodies are
/// distinguished by the request path so each uses the matching cohort key.
pub(crate) fn cohort_arm(
    parsed: &serde_json::Value,
    provider_label: &str,
    default_path: &str,
) -> Option<crate::proxy::holdout::Arm> {
    let holdout = crate::core::config::Config::load()
        .proxy
        .output_holdout_fraction();
    if holdout <= 0.0 {
        return None;
    }
    let key = match provider_label {
        "Anthropic" => crate::proxy::holdout::anthropic_key(parsed),
        "OpenAI" | "ChatGPT" => {
            if default_path.contains("responses") {
                crate::proxy::holdout::openai_responses_key(parsed)
            } else {
                crate::proxy::holdout::openai_chat_key(parsed)
            }
        }
        _ => crate::proxy::holdout::google_key(parsed),
    };
    Some(crate::proxy::holdout::assign(&key, holdout))
}

pub(crate) struct PreparedRequestBody {
    pub(crate) body: Vec<u8>,
    pub(crate) parsed: Option<serde_json::Value>,
    pub(crate) original_size: usize,
    pub(crate) compressed_size: usize,
    pub(crate) compression_candidate: bool,
    pub(crate) preserve_content_encoding: bool,
    pub(crate) content_dedup_tokens_saved: usize,
    /// Routing decision applied to the body (enterprise#13); `None` = passthrough.
    pub(crate) route: Option<crate::proxy::routing::RouteDecision>,
    /// Input-compression holdout arm (#1905); `None` when that holdout is off
    /// or the body could not be parsed.
    pub(crate) compression_arm: Option<crate::proxy::holdout::Arm>,
}

pub(crate) fn prepare_request_body(
    parts: &Parts,
    body_bytes: &[u8],
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    route_hook: impl FnOnce(&mut serde_json::Value) -> Option<crate::proxy::routing::RouteDecision>,
    default_upstream_base: &str,
    openai_shape: bool,
    compression_arm: Option<crate::proxy::holdout::Arm>,
) -> Result<PreparedRequestBody, StatusCode> {
    let encoding = request_body_encoding(parts);
    let decoded = match encoding {
        RequestBodyEncoding::Identity => Cow::Borrowed(body_bytes),
        RequestBodyEncoding::Gzip => Cow::Owned(decode_gzip_bounded(body_bytes, max_body_bytes())?),
        RequestBodyEncoding::Zstd => Cow::Owned(decode_zstd_bounded(body_bytes, max_body_bytes())?),
        RequestBodyEncoding::Passthrough => {
            return Ok(PreparedRequestBody {
                body: body_bytes.to_vec(),
                parsed: None,
                original_size: body_bytes.len(),
                compressed_size: body_bytes.len(),
                compression_candidate: false,
                preserve_content_encoding: true,
                content_dedup_tokens_saved: 0,
                route: None,
                compression_arm,
            });
        }
    };

    // #1905: the control arm of the input-compression holdout skips every
    // compression stage below — conversation shaping, agent compaction and the
    // provider compressor — so it is forwarded as sent.
    // Every return carries the arm: the comparison is by assignment, so a body
    // that could not be compressed still counts in the arm it was assigned to.
    let compression_control = compression_arm == Some(crate::proxy::holdout::Arm::Control);

    let decoded = if !compression_control
        && let Some((compressed_body, _tokens_saved, _summarized, _dropped)) =
            crate::proxy::shaping_hook::compress_conversation_if_enabled(&decoded)
    {
        Cow::Owned(compressed_body)
    } else {
        decoded
    };

    let Some(mut parsed) = serde_json::from_slice::<serde_json::Value>(&decoded).ok() else {
        return Ok(PreparedRequestBody {
            body: body_bytes.to_vec(),
            parsed: None,
            original_size: body_bytes.len(),
            compressed_size: body_bytes.len(),
            compression_candidate: false,
            preserve_content_encoding: encoding != RequestBodyEncoding::Identity,
            content_dedup_tokens_saved: 0,
            route: None,
            compression_arm,
        });
    };
    // #1570 P1: agent-requested compaction runs FIRST, on the raw client
    // bytes — later mutations must never shift its fingerprint-pinned
    // boundary. The determinism guard reverts it wholesale like every other
    // mutation; its savings ride the content-level counter (zeroed on revert
    // together with it). Repeated tool output is deduplicated later, inside
    // the request only, by the compression pipeline (#1980).
    let content_dedup_tokens_saved = if compression_control {
        0
    } else {
        crate::proxy::agent_compact::apply(&mut parsed)
    };

    // Router runs on the freshly parsed body, before compression: the model
    // swap lands in the same single serialization as the compression pass.
    let mut route = route_hook(&mut parsed);

    // Measured cost opt-in (#1179): when the *effective* upstream (post-
    // routing) is OpenRouter and the body speaks the OpenAI shape, ask for the
    // billed charge in the final usage payload. Other upstreams never see the
    // non-standard `usage` field (api.openai.com rejects unknown params).
    let effective_upstream = route
        .as_ref()
        .and_then(|r| r.upstream_base.as_deref())
        .unwrap_or(default_upstream_base);
    let wants_billed_cost =
        crate::proxy::usage_accounting::upstream_is_openrouter(effective_upstream)
            && crate::core::config::Config::load()
                .proxy
                .meters_openai_usage();
    let xlat_route = route.as_ref().is_some_and(|r| r.xlat);
    // `usage.include` is a Chat-Completions-only parameter: gate on the shape
    // AND the call path so a Responses-API body never carries it.
    let chat_completions_call = openai_shape
        && parts
            .uri
            .path()
            .trim_end_matches('/')
            .ends_with("/chat/completions");
    if wants_billed_cost && chat_completions_call && !xlat_route {
        crate::proxy::usage_accounting::inject_usage_include(&mut parsed);
    }

    let original_size = decoded.len();
    // The provider compressors read the holdout arm through this scope; it ends
    // with the synchronous compression pass below.
    let control_scope = crate::proxy::holdout::enter_compression_control(compression_control);
    // Cross-shape route (enterprise#16): translate Messages→Chat-Completions
    // and compress with the target shape's compressor. An untranslatable body
    // fails open — the route is cancelled and the request forwards natively.
    let (logical_body, _, compressed_size) =
        if let Some(mut openai_body) = translated_openai_body(route.as_ref(), &parsed) {
            if wants_billed_cost {
                crate::proxy::usage_accounting::inject_usage_include(&mut openai_body);
            }
            crate::proxy::openai::compress_request_body(openai_body, original_size)
        } else {
            if route.as_ref().is_some_and(|r| r.xlat) {
                let decision = route.take().expect("checked is_some");
                tracing::warn!(
                    "lean-ctx proxy: request not translatable to OpenAI shape — \
                 cancelling route to '{}', forwarding natively",
                    decision.provider_id.as_deref().unwrap_or("?")
                );
                parsed["model"] = serde_json::Value::String(decision.routed_from);
            }
            compress_body(parsed.clone(), original_size)
        };
    drop(control_scope);
    let final_parsed = serde_json::from_slice(&logical_body).ok();
    let body = encode_request_body(parts, logical_body)?;

    Ok(PreparedRequestBody {
        body,
        parsed: final_parsed,
        original_size,
        compressed_size,
        compression_candidate: true,
        preserve_content_encoding: encoding != RequestBodyEncoding::Identity,
        content_dedup_tokens_saved,
        route,
        compression_arm,
    })
}

/// Encodes a rewritten logical JSON body with the caller's `Content-Encoding`,
/// which is forwarded upstream unchanged. Only decodable encodings ever yield a
/// parsed body to rewrite, so an opaque one here is an internal error.
pub(crate) fn encode_request_body(parts: &Parts, logical: Vec<u8>) -> Result<Vec<u8>, StatusCode> {
    match request_body_encoding(parts) {
        RequestBodyEncoding::Identity => Ok(logical),
        RequestBodyEncoding::Gzip => encode_gzip(&logical),
        RequestBodyEncoding::Zstd => encode_zstd(&logical),
        RequestBodyEncoding::Passthrough => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[cfg(not(feature = "shape-xlat"))]
pub(crate) fn translated_openai_body(
    _route: Option<&crate::proxy::routing::RouteDecision>,
    _parsed: &serde_json::Value,
) -> Option<serde_json::Value> {
    None
}
