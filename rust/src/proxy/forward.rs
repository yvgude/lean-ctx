use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, request::Parts},
    response::Response,
};

use super::ProxyState;

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

/// Transforms the already-parsed JSON request body (parsed once upstream, so the
/// compressor never re-parses) into the serialized — possibly compressed — body,
/// its original size, and its compressed size. A plain `fn` from the static
/// providers or a closure that captures request-derived context (e.g. Gemini's
/// path-encoded model) both satisfy this bound.
pub async fn forward_request(
    State(state): State<ProxyState>,
    req: Request<Body>,
    upstream_base: &str,
    default_path: &str,
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    provider_label: &str,
    extra_stream_types: &[&str],
) -> Result<Response, StatusCode> {
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, max_body_bytes())
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    state.stats.record_request();

    let original_size = body_bytes.len();
    let preserve_content_encoding = has_non_identity_content_encoding(&parts);

    // Parse once; the parsed value is shared between introspection, cost
    // attribution, and compression — eliminating the redundant re-parse that
    // each compress_body function previously performed internally.
    let parsed = if preserve_content_encoding {
        None
    } else {
        serde_json::from_slice::<serde_json::Value>(&body_bytes).ok()
    };
    if let Some(ref parsed) = parsed {
        let provider = match provider_label {
            "Anthropic" => super::introspect::Provider::Anthropic,
            "OpenAI" => super::introspect::Provider::OpenAi,
            _ => super::introspect::Provider::Gemini,
        };
        let breakdown = super::introspect::analyze_request(parsed, provider);
        state.introspect.record(breakdown);
    }

    // #895 Track B: recompute the output-savings arm from the same pristine body
    // each provider's compress_body keys on, so the metered arm matches the arm
    // that decided output-shaping. Only when a holdout is active (fraction > 0).
    let cohort = parsed
        .as_ref()
        .and_then(|p| cohort_arm(p, provider_label, default_path));

    let (compressed_body, _, compressed_size) = if let Some(value) = parsed.clone() {
        compress_body(value, original_size)
    } else {
        (body_bytes.to_vec(), original_size, original_size)
    };

    if compressed_size < original_size {
        state
            .stats
            .record_compression(original_size, compressed_size);
    }

    let tokens_saved = original_size.saturating_sub(compressed_size) as u64 / 4;
    super::metrics::record_request(tokens_saved, compressed_size as u64);

    let model = parsed
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.as_str());
    super::cost::record(
        model,
        tokens_saved,
        original_size as u64,
        compressed_size as u64,
    );

    let upstream_url = build_upstream_url(&parts, upstream_base, default_path);
    let response = send_upstream(
        &state,
        &parts,
        &upstream_url,
        compressed_body,
        provider_label,
        preserve_content_encoding,
    )
    .await?;

    // Measured usage: read the real model + billed tokens from the response.
    // Gemini puts the model in the URL path, not the request/response body.
    let usage_provider = super::usage::Provider::from_label(provider_label);
    let url_model = if usage_provider == super::usage::Provider::Gemini {
        super::usage::gemini_model_from_path(parts.uri.path())
    } else {
        None
    };

    build_response(
        response,
        extra_stream_types,
        usage_provider,
        url_model,
        cohort,
    )
    .await
}

/// Output-savings arm (#895) for a request body, or `None` when no holdout is
/// active. Keyed per provider; OpenAI's Chat vs Responses bodies are
/// distinguished by the request path so each uses the matching cohort key.
fn cohort_arm(
    parsed: &serde_json::Value,
    provider_label: &str,
    default_path: &str,
) -> Option<super::holdout::Arm> {
    let holdout = crate::core::config::Config::load()
        .proxy
        .output_holdout_fraction();
    if holdout <= 0.0 {
        return None;
    }
    let key = match provider_label {
        "Anthropic" => super::holdout::anthropic_key(parsed),
        "OpenAI" => {
            if default_path.contains("responses") {
                super::holdout::openai_responses_key(parsed)
            } else {
                super::holdout::openai_chat_key(parsed)
            }
        }
        _ => super::holdout::google_key(parsed),
    };
    Some(super::holdout::assign(&key, holdout))
}

fn build_upstream_url(parts: &Parts, base: &str, default_path: &str) -> String {
    format!(
        "{base}{}",
        parts
            .uri
            .path_and_query()
            .map_or(default_path, axum::http::uri::PathAndQuery::as_str)
    )
}

/// Request headers forwarded verbatim to the upstream provider. Anything not
/// listed here is stripped before the request leaves the loopback proxy.
///
/// `openai-project` (and `openai-organization`) must be forwarded: OpenCode and
/// the OpenAI SDK send the project scope via this header for project-scoped API
/// keys when calling the Responses API (`/responses`). Dropping it makes OpenAI
/// reject the request with `Missing scopes: api.responses.write` (#366).
pub(super) const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "content-type",
    "accept",
    "user-agent",
    "originator",
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "openai-organization",
    "openai-project",
    "openai-beta",
    "chatgpt-account-id",
    "x-openai-fedramp",
    "x-openai-internal-codex-residency",
    "x-openai-internal-codex-responses-lite",
    "x-openai-product-sku",
    "oai-product-sku",
    "x-oai-attestation",
    "x-client-request-id",
    "x-codex-beta-features",
    "x-codex-installation-id",
    "x-codex-parent-thread-id",
    "x-openai-subagent",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-window-id",
    "x-openai-memgen-request",
    "x-responsesapi-include-timing-metrics",
    "mcp-session-id",
    "last-event-id",
    "cache-control",
    "x-goog-api-key",
    "x-goog-api-client",
];

pub(super) fn is_allowed_request_header(name: &str) -> bool {
    ALLOWED_REQUEST_HEADERS.contains(&name)
}

fn should_forward_request_header(name: &str, preserve_content_encoding: bool) -> bool {
    is_allowed_request_header(name)
        || (preserve_content_encoding && name.eq_ignore_ascii_case("content-encoding"))
}

fn has_non_identity_content_encoding(parts: &Parts) -> bool {
    parts
        .headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("identity")
        })
}

async fn send_upstream(
    state: &ProxyState,
    parts: &Parts,
    url: &str,
    body: Vec<u8>,
    provider_label: &str,
    preserve_content_encoding: bool,
) -> Result<reqwest::Response, StatusCode> {
    let mut req = state.client.request(parts.method.clone(), url);

    for (key, value) in &parts.headers {
        let k = key.as_str().to_lowercase();
        if should_forward_request_header(&k, preserve_content_encoding) {
            req = req.header(key.clone(), value.clone());
        }
    }

    req.body(body).send().await.map_err(|e| {
        tracing::error!("lean-ctx proxy: {provider_label} upstream error: {e}");
        StatusCode::BAD_GATEWAY
    })
}

pub(super) const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "mcp-session-id",
    "x-request-id",
    "x-oai-request-id",
    "cf-ray",
    "x-openai-authorization-error",
    "x-error-json",
    "openai-organization",
    "openai-model",
    "openai-processing-ms",
    "openai-version",
    "x-models-etag",
    "x-reasoning-included",
    "anthropic-ratelimit-requests-limit",
    "anthropic-ratelimit-requests-remaining",
    "anthropic-ratelimit-tokens-limit",
    "anthropic-ratelimit-tokens-remaining",
    "retry-after",
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "cache-control",
];

pub(super) fn is_forwarded_response_header(name: &str) -> bool {
    FORWARDED_HEADERS.contains(&name)
        || name.starts_with("x-codex-")
        || name.starts_with("x-ratelimit-")
}

async fn build_response(
    response: reqwest::Response,
    extra_stream_types: &[&str],
    usage_provider: super::usage::Provider,
    url_model: Option<String>,
    cohort: Option<super::holdout::Arm>,
) -> Result<Response, StatusCode> {
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);
    let resp_headers = response.headers().clone();

    let is_stream = resp_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.contains("text/event-stream") || extra_stream_types.iter().any(|t| ct.contains(t))
        });

    if is_stream {
        // Tee the stream through a usage Scanner: each chunk is forwarded
        // byte-for-byte while the real model + billed tokens are extracted from
        // the final event and recorded when the stream ends.
        let scanner = super::usage::Scanner::new(usage_provider, url_model).with_cohort(cohort);
        let inner = Box::pin(response.bytes_stream());
        let body = Body::from_stream(super::usage::tee_stream(inner, scanner));
        let mut resp = Response::builder().status(status);
        for (k, v) in &resp_headers {
            let ks = k.as_str().to_lowercase();
            if is_forwarded_response_header(&ks) {
                resp = resp.header(k, v);
            }
        }
        return resp
            .body(body)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let resp_bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    // Non-streaming: the whole body is one JSON object carrying `usage`.
    let mut scanner = super::usage::Scanner::new(usage_provider, url_model).with_cohort(cohort);
    scanner.feed_body(&resp_bytes);
    if let Some(usage) = scanner.finalize() {
        super::usage_meter::record(&usage);
    }

    let mut resp = Response::builder().status(status);
    for (k, v) in &resp_headers {
        let ks = k.as_str().to_lowercase();
        if is_forwarded_response_header(&ks) {
            resp = resp.header(k, v);
        }
    }
    resp.body(Body::from(resp_bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts_for(uri: &str) -> Parts {
        Request::builder().uri(uri).body(()).unwrap().into_parts().0
    }

    #[test]
    fn encoded_request_bodies_skip_json_rewrite_and_preserve_encoding() {
        let parts = Request::builder()
            .uri("/backend-api/codex/responses")
            .header(axum::http::header::CONTENT_ENCODING, "zstd")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        assert!(has_non_identity_content_encoding(&parts));
        assert!(should_forward_request_header("content-encoding", true));
        assert!(!should_forward_request_header("content-encoding", false));
    }

    #[test]
    fn identity_content_encoding_can_be_rewritten_as_json() {
        let parts = Request::builder()
            .uri("/v1/responses")
            .header(axum::http::header::CONTENT_ENCODING, "identity")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        assert!(!has_non_identity_content_encoding(&parts));
    }

    #[test]
    fn upstream_url_preserves_subpath() {
        let base = "https://api.anthropic.com";
        let parts = parts_for("/v1/messages/count_tokens");
        assert_eq!(
            build_upstream_url(&parts, base, "/v1/messages"),
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
    }

    #[test]
    fn upstream_url_preserves_batches_subpath() {
        let base = "https://api.anthropic.com";
        let parts = parts_for("/v1/messages/batches/batch_123/results");
        assert_eq!(
            build_upstream_url(&parts, base, "/v1/messages"),
            "https://api.anthropic.com/v1/messages/batches/batch_123/results"
        );
    }

    #[test]
    fn upstream_url_exact_path() {
        let base = "https://api.anthropic.com";
        let parts = parts_for("/v1/messages");
        assert_eq!(
            build_upstream_url(&parts, base, "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn upstream_url_preserves_query_params() {
        let base = "https://api.anthropic.com";
        let parts = parts_for("/v1/messages/count_tokens?model=claude-4");
        assert_eq!(
            build_upstream_url(&parts, base, "/v1/messages"),
            "https://api.anthropic.com/v1/messages/count_tokens?model=claude-4"
        );
    }

    #[test]
    fn forwards_openai_project_and_auth_headers() {
        // #366: project-scoped OpenAI keys carry the scope via `OpenAI-Project`.
        // It must be forwarded upstream, otherwise the Responses API rejects the
        // call with `Missing scopes: api.responses.write`.
        for required in ["authorization", "openai-project", "openai-organization"] {
            assert!(
                ALLOWED_REQUEST_HEADERS.contains(&required),
                "request header `{required}` must be forwarded upstream"
            );
        }
    }

    #[test]
    fn forwards_chatgpt_codex_oauth_headers() {
        for required in [
            "authorization",
            "chatgpt-account-id",
            "x-openai-fedramp",
            "x-openai-internal-codex-residency",
            "x-openai-product-sku",
            "oai-product-sku",
            "x-client-request-id",
            "x-codex-installation-id",
            "x-codex-turn-metadata",
            "x-openai-subagent",
            "x-codex-turn-state",
            "originator",
        ] {
            assert!(
                is_allowed_request_header(required),
                "request header `{required}` must be forwarded upstream"
            );
        }
    }

    #[test]
    fn forwards_streamable_http_mcp_headers() {
        for required in ["mcp-session-id", "last-event-id"] {
            assert!(
                ALLOWED_REQUEST_HEADERS.contains(&required),
                "request header `{required}` must be forwarded upstream"
            );
        }
        assert!(
            is_forwarded_response_header("mcp-session-id"),
            "MCP session id response header must be forwarded downstream"
        );
    }

    #[test]
    fn forwards_codex_state_response_headers() {
        for required in [
            "x-codex-turn-state",
            "x-codex-primary-used-percent",
            "openai-model",
            "x-models-etag",
            "x-reasoning-included",
            "x-oai-request-id",
            "cf-ray",
            "x-openai-authorization-error",
            "x-error-json",
        ] {
            assert!(
                is_forwarded_response_header(required),
                "response header `{required}` must be forwarded downstream"
            );
        }
    }
}
