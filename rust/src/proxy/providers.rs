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
//! Shape ≠ identity: the proxy understands three wire dialects (the shapes) and
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
    http::{HeaderMap, HeaderValue, Request, StatusCode},
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

    if provider.api_key_env.is_some() {
        inject_gateway_credential(&provider, req.headers_mut())?;
    }

    match provider.shape {
        WireShape::Anthropic => {
            forward::forward_request(
                State(state),
                req,
                &provider.base_url,
                "/v1/messages",
                super::anthropic::compress_request_body,
                "Anthropic",
                &[],
            )
            .await
        }
        WireShape::OpenAi => {
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
    }
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(shape: WireShape, api_key_env: Option<&str>) -> ResolvedProvider {
        ResolvedProvider {
            id: "test".into(),
            shape,
            base_url: "https://example.invalid".into(),
            api_key_env: api_key_env.map(str::to_string),
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
