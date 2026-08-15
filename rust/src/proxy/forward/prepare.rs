//! Request-body preparation: parse, translate, compress.

use axum::http::{StatusCode, request::Parts};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::proxy::codec::{
    RequestBodyEncoding, decode_gzip_bounded, decode_zstd_bounded, encode_gzip, encode_zstd,
    request_body_encoding,
};
use crate::proxy::dedup::{ContentAddressedDedup, ToolResultCache};

use super::max_body_bytes;

/// One proxy process serves one agent session, so its tool-result cache is safe
/// to share across forwarded requests.
static DEDUP_CACHE: OnceLock<Arc<ToolResultCache>> = OnceLock::new();

fn dedup_cache() -> &'static Arc<ToolResultCache> {
    DEDUP_CACHE.get_or_init(|| Arc::new(ToolResultCache::new()))
}

fn content_dedup_cache() -> &'static std::sync::Mutex<ContentAddressedDedup> {
    static CACHE: OnceLock<std::sync::Mutex<ContentAddressedDedup>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(ContentAddressedDedup::new()))
}

struct ToolResultToCache {
    tool_name: String,
    content: String,
}

/// Replace result blocks already sent during an earlier request, returning the
/// misses to cache after the request's normal compression pass completes.
fn deduplicate_tool_results(
    parsed: &mut serde_json::Value,
    cache: &ToolResultCache,
) -> (Vec<ToolResultToCache>, usize) {
    let Some(messages) = parsed.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return (Vec::new(), 0);
    };

    let tool_names = messages
        .iter()
        .flat_map(|message| {
            message
                .get("content")
                .and_then(|content| content.as_array())
                .into_iter()
                .flatten()
        })
        .filter(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("tool_use"))
        .filter_map(|block| {
            Some((
                block.get("id")?.as_str()?.to_owned(),
                block.get("name")?.as_str()?.to_owned(),
            ))
        })
        .collect::<HashMap<_, _>>();
    let mut misses = Vec::new();
    let mut tokens_saved = 0;

    for message in messages {
        let Some(blocks) = message
            .get_mut("content")
            .and_then(|content| content.as_array_mut())
        else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(|kind| kind.as_str()) != Some("tool_result") {
                continue;
            }
            let Some(content) = block.get("content").and_then(|content| content.as_str()) else {
                continue;
            };
            let tool_name = block
                .get("tool_use_id")
                .and_then(|id| id.as_str())
                .and_then(|id| tool_names.get(id))
                .cloned()
                .unwrap_or_else(|| "tool_result".to_owned());

            if let Some(hit) = cache.check(&tool_name, content) {
                tokens_saved += hit.tokens_saved;
                block["content"] = serde_json::Value::String(hit.stub);
            } else {
                misses.push(ToolResultToCache {
                    tool_name,
                    content: content.to_owned(),
                });
            }
        }
    }
    (misses, tokens_saved)
}

fn cache_tool_results(cache: &ToolResultCache, results: Vec<ToolResultToCache>) {
    for result in results {
        let token_count = result.content.len().saturating_add(3) / 4;
        cache.insert(&result.tool_name, &result.content, token_count, None);
    }
}

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
}

pub(crate) fn prepare_request_body(
    parts: &Parts,
    body_bytes: &[u8],
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    route_hook: impl FnOnce(&mut serde_json::Value) -> Option<crate::proxy::routing::RouteDecision>,
    default_upstream_base: &str,
    openai_shape: bool,
) -> Result<PreparedRequestBody, StatusCode> {
    let cache = dedup_cache();
    cache.advance_turn();
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
            });
        }
    };

    let decoded = if let Some((compressed_body, _tokens_saved, _summarized, _dropped)) =
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
        });
    };
    let (tool_results_to_cache, dedup_tokens_saved) = deduplicate_tool_results(&mut parsed, cache);
    let content_dedup_tokens_saved = parsed
        .get_mut("messages")
        .and_then(|messages| messages.as_array_mut())
        .map(|messages| {
            content_dedup_cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .dedup_messages(messages)
                .tokens_saved
        })
        .unwrap_or_default();

    if dedup_tokens_saved > 0 {
        tracing::debug!(dedup_tokens_saved, "deduplicated proxy tool results");
    }

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
    cache_tool_results(cache, tool_results_to_cache);
    let final_parsed = serde_json::from_slice(&logical_body).ok();
    let body = match encoding {
        RequestBodyEncoding::Identity => logical_body,
        RequestBodyEncoding::Gzip => encode_gzip(&logical_body)?,
        RequestBodyEncoding::Zstd => encode_zstd(&logical_body)?,
        RequestBodyEncoding::Passthrough => unreachable!("passthrough returned above"),
    };

    Ok(PreparedRequestBody {
        body,
        parsed: final_parsed,
        original_size,
        compressed_size,
        compression_candidate: true,
        preserve_content_encoding: encoding != RequestBodyEncoding::Identity,
        content_dedup_tokens_saved,
        route,
    })
}

#[cfg(not(feature = "shape-xlat"))]
pub(crate) fn translated_openai_body(
    _route: Option<&crate::proxy::routing::RouteDecision>,
    _parsed: &serde_json::Value,
) -> Option<serde_json::Value> {
    None
}

#[cfg(test)]
mod tests {
    use super::{ToolResultCache, cache_tool_results, deduplicate_tool_results};
    use serde_json::json;

    fn tool_result_body(content: &str) -> serde_json::Value {
        json!({
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "ctx_shell",
                        "input": {}
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": content
                    }]
                }
            ]
        })
    }

    #[test]
    fn repeated_tool_result_on_second_request_returns_stub() {
        let cache = ToolResultCache::new();
        let content = "cargo test completed successfully";

        cache.advance_turn();
        let (first_misses, first_saved) =
            deduplicate_tool_results(&mut tool_result_body(content), &cache);
        assert_eq!(first_saved, 0);
        cache_tool_results(&cache, first_misses);

        cache.advance_turn();
        let mut repeated = tool_result_body(content);
        let (_second_misses, second_saved) = deduplicate_tool_results(&mut repeated, &cache);
        let stub = repeated["messages"][1]["content"][0]["content"]
            .as_str()
            .expect("dedup stub content");
        assert!(stub.contains("unchanged since turn 1"));
        assert!(stub.contains("cargo test completed successfully"));
        assert!(second_saved > 0);
    }

    #[test]
    fn different_tool_results_are_not_deduplicated() {
        let cache = ToolResultCache::new();
        let first = "first result";
        let second = "different result";

        cache.advance_turn();
        let (misses, _) = deduplicate_tool_results(&mut tool_result_body(first), &cache);
        cache_tool_results(&cache, misses);

        cache.advance_turn();
        let mut different = tool_result_body(second);
        let (misses, saved) = deduplicate_tool_results(&mut different, &cache);
        assert_eq!(saved, 0);
        assert_eq!(misses.len(), 1);
        assert_eq!(
            different["messages"][1]["content"][0]["content"],
            json!(second)
        );
    }
}
