//! Universal provider routes — the request-path half of the
//! universal-provider-framework (enterprise#7).
//!
//! `/providers/{id}/...` forwards to the `[[proxy.providers]]` registry entry
//! with that id, speaking the entry's declared [`WireShape`]. A new
//! OpenAI/Anthropic/Gemini-compatible endpoint (Azure AI Foundry, OpenRouter,
//! Groq, vLLM, a corporate gateway…) is therefore pure configuration — no code
//! change, no rebuild:
//!
//! ```toml
//! [[proxy.providers]]
//! id = "foundry"
//! shape = "openai"
//! base_url = "https://my-resource.services.ai.azure.com"
//! api_key_env = "FOUNDRY_API_KEY"   # optional: gateway-held credential
//! ```
//!
//! Shape ≠ identity: the proxy understands four wire dialects (the shapes) and
//! any number of provider identities map onto them. Compression, introspection
//! and usage metering all run exactly as they do for the built-in routes of the
//! same shape.
//!
//! When `api_key_env` is set, the gateway holds the upstream credential and the
//! caller authenticates with the lean-ctx Bearer token only: every incoming
//! credential header is stripped and replaced by the configured key (the caller
//! never needs — or sees — the provider key). Without `api_key_env` the
//! caller's own credentials are forwarded verbatim, exactly like the built-ins.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, Uri},
    response::Response,
};

use super::{ProxyState, forward};
use crate::core::config::{ResolvedProvider, WireShape};

/// Request extension carrying the registry identity of the serving provider.
///
/// Shape ≠ identity (module docs): the forward path only knows the wire shape
/// ("OpenAI"), but usage metering must attribute to the provider *identity*
/// ("foundry", "local") — otherwise every OpenAI-shaped registry entry shows
/// up as "OpenAI" in `usage_events` and the admin breakdown (enterprise#20).
/// `local` carries the entry's resolved local-inference flag so shadow-rate
/// billing works for non-loopback local endpoints too (host.docker.internal).
#[derive(Debug, Clone)]
pub(super) struct RegistryProviderId {
    pub id: String,
    pub local: bool,
    pub account_ref: Option<String>,
    pub region: Option<String>,
    pub gateway_managed_credential: bool,
}

/// Registry ids that share the Grok / xAI dual-rail (subscription `grok-chat`,
/// API-key `xai`). OpenAI wire shape; separate `proxy status` bucket.
pub(super) fn is_grok_provider_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "grok-chat" | "xai" | "grok"
    )
}

/// Registry ids that share the Command Code gateway rail (`commandcode`).
/// Separate `proxy status` bucket so traffic does not fold into the wire-shape
/// line.
pub(super) fn is_commandcode_provider_id(id: &str) -> bool {
    matches!(
        id.trim().to_ascii_lowercase().as_str(),
        "commandcode" | "command-code"
    )
}

/// Per-upstream stats label for a registry route.
///
/// Wire shape stays `OpenAI`/`Anthropic`/… for compression; identity for
/// `ProxyStats` may differ (Grok must not fold into the OpenAI line).
pub(super) fn stats_label<'a>(registry_id: Option<&str>, shape_label: &'a str) -> &'a str {
    match registry_id {
        Some(id) if is_grok_provider_id(id) => "Grok",
        Some(id) if is_commandcode_provider_id(id) => "CommandCode",
        _ => shape_label,
    }
}

/// True when an OpenAI-shaped registry request should use the Responses
/// compressor (`input` / `function_call_output`) rather than Chat Completions.
pub(super) fn is_openai_responses_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == "/responses"
        || path == "/v1/responses"
        || path.starts_with("/responses/")
        || path.starts_with("/v1/responses/")
}

pub async fn handler(
    State(state): State<ProxyState>,
    Path((id, rest)): Path<(String, String)>,
    mut req: Request<Body>,
) -> Result<Response, StatusCode> {
    let Some(provider) = state.upstream_snapshot().provider_by_id(&id).cloned() else {
        tracing::warn!("lean-ctx proxy: unknown registry provider '{id}' (404)");
        return Err(StatusCode::NOT_FOUND);
    };
    req.extensions_mut().insert(RegistryProviderId {
        id: provider.id.clone(),
        local: provider.local,
        account_ref: provider.account_ref.clone(),
        region: provider.region.clone(),
        gateway_managed_credential: provider.api_key_env.is_some()
            || provider.shape == WireShape::Bedrock,
    });

    // Strip the `/providers/{id}` prefix so the upstream sees the bare provider
    // path: `/providers/foundry/v1/chat/completions` → `/v1/chat/completions`.
    let path = format!("/{rest}");
    let uri = match req.uri().query() {
        Some(q) => format!("{path}?{q}").parse::<axum::http::Uri>(),
        None => path.parse::<axum::http::Uri>(),
    }
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    *req.uri_mut() = uri;

    if provider.shape == WireShape::Bedrock {
        super::bedrock::validate_invoke_request(&req)?;
        super::bedrock::attach_signing_context(&provider, &mut req)?;
    } else if provider.api_key_env.is_some() {
        if provider.shape == WireShape::Gemini {
            strip_gemini_api_key_query(req.uri_mut())?;
        }
        inject_gateway_credential(&provider, req.headers_mut())?;
    }

    match provider.shape {
        WireShape::Anthropic => {
            forward::forward_request(
                State(state),
                req,
                &provider.base_url,
                "/v1/messages",
                super::bedrock::passthrough_request_body,
                "Anthropic",
                &[],
            )
            .await
        }
        WireShape::OpenAi => {
            // Built-in OpenAI routes pick Chat Completions vs Responses by path
            // (`/v1/chat/completions` vs `/v1/responses`). Registry providers
            // must do the same: Grok CLI hits `/providers/grok-chat/v1/responses`
            // with `function_call_output` in `input`, which the Chat compressor
            // ignores (it only rewrites `messages`). Path already has the
            // `/providers/{id}` prefix stripped above.
            if is_openai_responses_path(req.uri().path()) {
                forward::forward_request(
                    State(state),
                    req,
                    &provider.base_url,
                    "/v1/responses",
                    super::openai_responses::compress_request_body,
                    "OpenAI",
                    &[],
                )
                .await
            } else {
                forward::forward_request(
                    State(state),
                    req,
                    &provider.base_url,
                    "/v1/chat/completions",
                    super::openai::compress_request_body,
                    "OpenAI",
                    &[],
                )
                .await
            }
        }
        WireShape::Gemini => {
            // Gemini carries the model in the URL path, not the body (#840).
            let model = super::usage::gemini_model_from_path(req.uri().path());
            forward::forward_request(
                State(state),
                req,
                &provider.base_url,
                "/",
                move |body, size| {
                    super::google::compress_request_body(body, size, model.as_deref())
                },
                "Gemini",
                &["application/x-ndjson"],
            )
            .await
        }
        WireShape::Bedrock => {
            forward::forward_request(
                State(state),
                req,
                &provider.base_url,
                "/",
                super::bedrock::passthrough_request_body,
                "Bedrock",
                &["application/vnd.amazon.eventstream"],
            )
            .await
        }
    }
}

fn query_component_name(component: &str) -> Result<Option<String>, StatusCode> {
    if component.is_empty() {
        return Ok(None);
    }
    let url = reqwest::Url::parse(&format!("http://leanctx.invalid/?{component}"))
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(url.query_pairs().next().map(|(name, _)| name.into_owned()))
}

fn is_api_key_query_component(component: &str) -> Result<bool, StatusCode> {
    Ok(query_component_name(component)?.is_some_and(|name| name.eq_ignore_ascii_case("key")))
}

/// Gemini accepts API credentials through `?key=` as well as `x-goog-api-key`.
/// For gateway-managed credentials, discard every decoded `key` query parameter
/// so a caller cannot override the configured credential at the upstream.
pub(super) fn strip_gemini_api_key_query(uri: &mut Uri) -> Result<(), StatusCode> {
    let Some(query) = uri.query() else {
        return Ok(());
    };

    let mut removed_key = false;
    let mut retained = Vec::new();
    for component in query.split('&') {
        if is_api_key_query_component(component)? {
            removed_key = true;
        } else {
            retained.push(component);
        }
    }
    if !removed_key {
        return Ok(());
    }

    let mut path_and_query = uri.path().to_string();
    if retained.iter().any(|component| !component.is_empty()) {
        path_and_query.push('?');
        path_and_query.push_str(&retained.join("&"));
    }
    *uri = path_and_query
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(())
}

/// Any caller-supplied `key` parameter is a possible upstream credential or
/// account selector. Treat undecodable query components conservatively when
/// deciding whether a configured account label can be trusted.
pub(super) fn has_untrusted_key_query(uri: &Uri) -> bool {
    uri.query().is_some_and(|query| {
        query.split('&').any(|component| {
            query_component_name(component).map_or(true, |name| {
                name.is_some_and(|name| name.eq_ignore_ascii_case("key"))
            })
        })
    })
}

/// Replace every caller credential header with the gateway-held key from the
/// entry's `api_key_env`, in the header dialect of the provider's shape. A
/// configured-but-missing env var is a deployment error and must surface
/// loudly (502), never silently forward the caller's lean-ctx token upstream.
pub(super) fn inject_gateway_credential(
    provider: &ResolvedProvider,
    headers: &mut HeaderMap,
) -> Result<(), StatusCode> {
    let env_name = provider
        .api_key_env
        .as_deref()
        .expect("caller checked api_key_env");
    let key = std::env::var(env_name)
        .ok()
        .filter(|k| !k.trim().is_empty());
    let Some(key) = key else {
        tracing::error!(
            "lean-ctx proxy: provider '{}' configures api_key_env='{env_name}' but the \
             variable is unset/empty — cannot authenticate upstream (502)",
            provider.id
        );
        return Err(StatusCode::BAD_GATEWAY);
    };

    // The caller authenticated against the gateway (Bearer token); none of its
    // credential headers may leak upstream.
    for h in ["authorization", "x-api-key", "api-key", "x-goog-api-key"] {
        headers.remove(h);
    }

    let value = |v: String| {
        HeaderValue::from_str(&v).map_err(|_| {
            tracing::error!(
                "lean-ctx proxy: provider '{}' key from {env_name} contains invalid header bytes",
                provider.id
            );
            StatusCode::BAD_GATEWAY
        })
    };
    match provider.shape {
        WireShape::Anthropic => {
            headers.insert("x-api-key", value(key)?);
        }
        WireShape::OpenAi => {
            // Bearer for OpenAI-compatible endpoints; `api-key` additionally
            // covers Azure deployments that only read that header.
            headers.insert("api-key", value(key.clone())?);
            headers.insert("authorization", value(format!("Bearer {key}"))?);
        }
        WireShape::Gemini => {
            headers.insert("x-goog-api-key", value(key)?);
        }
        WireShape::Bedrock => unreachable!("Bedrock uses SigV4, not api_key_env"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_label_maps_grok_rails_to_grok_bucket() {
        assert_eq!(stats_label(Some("grok-chat"), "OpenAI"), "Grok");
        assert_eq!(stats_label(Some("xai"), "OpenAI"), "Grok");
        assert_eq!(stats_label(Some("GROK"), "OpenAI"), "Grok");
        assert_eq!(stats_label(Some("foundry"), "OpenAI"), "OpenAI");
        assert_eq!(stats_label(None, "OpenAI"), "OpenAI");
        assert_eq!(stats_label(Some("local"), "OpenAI"), "OpenAI");
    }

    #[test]
    fn is_grok_provider_id_accepts_dual_rail_ids() {
        assert!(is_grok_provider_id("grok-chat"));
        assert!(is_grok_provider_id("xai"));
        assert!(is_grok_provider_id(" Grok "));
        assert!(!is_grok_provider_id("openai"));
        assert!(!is_grok_provider_id("foundry"));
    }

    #[test]
    fn stats_label_maps_commandcode_rail_to_commandcode_bucket() {
        assert_eq!(stats_label(Some("commandcode"), "OpenAI"), "CommandCode");
        assert_eq!(stats_label(Some("command-code"), "OpenAI"), "CommandCode");
        assert_eq!(stats_label(Some(" CommandCode "), "OpenAI"), "CommandCode");
    }

    #[test]
    fn is_commandcode_provider_id_accepts_rail_ids() {
        assert!(is_commandcode_provider_id("commandcode"));
        assert!(is_commandcode_provider_id("command-code"));
        assert!(is_commandcode_provider_id(" COMMANDCODE "));
        assert!(!is_commandcode_provider_id("openai"));
        assert!(!is_commandcode_provider_id("grok"));
    }

    #[test]
    fn is_openai_responses_path_detects_responses_api() {
        assert!(is_openai_responses_path("/v1/responses"));
        assert!(is_openai_responses_path("/v1/responses/"));
        assert!(is_openai_responses_path("/responses"));
        assert!(is_openai_responses_path(
            "/v1/responses/resp_123/input_items"
        ));
        assert!(!is_openai_responses_path("/v1/chat/completions"));
        assert!(!is_openai_responses_path("/v1/models"));
        assert!(!is_openai_responses_path("/v1/responsesx"));
    }

    fn provider(shape: WireShape, api_key_env: Option<&str>) -> ResolvedProvider {
        ResolvedProvider {
            id: "test".into(),
            shape,
            base_url: "https://example.invalid".into(),
            api_key_env: api_key_env.map(str::to_string),
            aws_region: None,
            account_ref: None,
            region: None,
            local: false,
        }
    }

    #[test]
    fn injection_replaces_caller_credentials_per_shape() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LC_TEST_PROVIDER_KEY", "sk-upstream");

        for (shape, expect_header, expect_value) in [
            (WireShape::Anthropic, "x-api-key", "sk-upstream"),
            (WireShape::OpenAi, "authorization", "Bearer sk-upstream"),
            (WireShape::Gemini, "x-goog-api-key", "sk-upstream"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("authorization", "Bearer lean-ctx-token".parse().unwrap());
            headers.insert("x-api-key", "caller-key".parse().unwrap());
            inject_gateway_credential(&provider(shape, Some("LC_TEST_PROVIDER_KEY")), &mut headers)
                .expect("key present");

            assert_eq!(
                headers.get(expect_header).unwrap().to_str().unwrap(),
                expect_value,
                "{shape:?} must carry the gateway key in its native header"
            );
            // The caller's gateway token must never leak upstream.
            let leaked = headers
                .iter()
                .any(|(_, v)| v.to_str().is_ok_and(|v| v.contains("lean-ctx-token")));
            assert!(!leaked, "caller bearer token leaked upstream for {shape:?}");
            if shape != WireShape::Anthropic {
                assert!(
                    headers.get("x-api-key").is_none(),
                    "stale caller x-api-key must be stripped for {shape:?}"
                );
            }
        }
        crate::test_env::remove_var("LC_TEST_PROVIDER_KEY");
    }

    #[test]
    fn gemini_gateway_credential_strips_decoded_and_duplicate_key_query_parameters() {
        let mut uri: Uri = "/v1/models?alt=sse&%6Bey=caller-key&key=second-key&pretty=1"
            .parse()
            .expect("valid request URI");

        strip_gemini_api_key_query(&mut uri).expect("query is valid");

        assert_eq!(uri.to_string(), "/v1/models?alt=sse&pretty=1");
        assert!(!has_untrusted_key_query(&uri));
    }

    #[test]
    fn gemini_gateway_query_sanitizer_preserves_unrelated_query_parameters() {
        let mut uri: Uri = "/v1/models?alt=sse&pretty=1"
            .parse()
            .expect("valid request URI");

        strip_gemini_api_key_query(&mut uri).expect("query is valid");

        assert_eq!(uri.to_string(), "/v1/models?alt=sse&pretty=1");
        assert!(!has_untrusted_key_query(&uri));
    }

    #[test]
    fn openai_shape_also_sets_azure_api_key_header() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LC_TEST_PROVIDER_KEY2", "fk-123");
        let mut headers = HeaderMap::new();
        inject_gateway_credential(
            &provider(WireShape::OpenAi, Some("LC_TEST_PROVIDER_KEY2")),
            &mut headers,
        )
        .unwrap();
        assert_eq!(headers.get("api-key").unwrap(), "fk-123");
        crate::test_env::remove_var("LC_TEST_PROVIDER_KEY2");
    }

    #[test]
    fn missing_key_env_is_a_loud_bad_gateway() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LC_TEST_PROVIDER_KEY_MISSING");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer lean-ctx-token".parse().unwrap());
        let err = inject_gateway_credential(
            &provider(WireShape::OpenAi, Some("LC_TEST_PROVIDER_KEY_MISSING")),
            &mut headers,
        )
        .unwrap_err();
        assert_eq!(err, StatusCode::BAD_GATEWAY);
    }
}
