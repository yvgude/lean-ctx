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

fn max_body_bytes() -> usize {
    std::env::var("LEAN_CTX_PROXY_MAX_BODY_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_MAX_BODY_MB)
        .saturating_mul(1024 * 1024)
}

/// Receives the already-parsed JSON value, avoiding a redundant
/// `serde_json::from_slice` on every request. Returns the serialized (possibly
/// compressed) body, original size, and compressed size.
pub type CompressFn = fn(serde_json::Value, usize) -> (Vec<u8>, usize, usize);

pub async fn forward_request(
    State(state): State<ProxyState>,
    req: Request<Body>,
    upstream_base: &str,
    default_path: &str,
    compress_body: CompressFn,
    provider_label: &str,
    extra_stream_types: &[&str],
) -> Result<Response, StatusCode> {
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, max_body_bytes())
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    state.stats.record_request();

    let original_size = body_bytes.len();

    // Parse once; the parsed value is shared between introspection, cost
    // attribution, and compression — eliminating the redundant re-parse that
    // each compress_body function previously performed internally.
    let parsed = serde_json::from_slice::<serde_json::Value>(&body_bytes).ok();
    if let Some(ref parsed) = parsed {
        let provider = match provider_label {
            "Anthropic" => super::introspect::Provider::Anthropic,
            "OpenAI" => super::introspect::Provider::OpenAi,
            _ => super::introspect::Provider::Gemini,
        };
        let breakdown = super::introspect::analyze_request(parsed, provider);
        state.introspect.record(breakdown);
    }

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

    build_response(response, extra_stream_types, usage_provider, url_model).await
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
const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "content-type",
    "accept",
    "user-agent",
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "openai-organization",
    "openai-project",
    "openai-beta",
    "x-goog-api-key",
    "x-goog-api-client",
];

async fn send_upstream(
    state: &ProxyState,
    parts: &Parts,
    url: &str,
    body: Vec<u8>,
    provider_label: &str,
) -> Result<reqwest::Response, StatusCode> {
    let mut req = state.client.request(parts.method.clone(), url);

    for (key, value) in &parts.headers {
        let k = key.as_str().to_lowercase();
        if ALLOWED_REQUEST_HEADERS.contains(&k.as_str()) {
            req = req.header(key.clone(), value.clone());
        }
    }

    req.body(body).send().await.map_err(|e| {
        tracing::error!("lean-ctx proxy: {provider_label} upstream error: {e}");
        StatusCode::BAD_GATEWAY
    })
}

const FORWARDED_HEADERS: &[&str] = &[
    "content-type",
    "content-encoding",
    "x-request-id",
    "openai-organization",
    "openai-processing-ms",
    "openai-version",
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

async fn build_response(
    response: reqwest::Response,
    extra_stream_types: &[&str],
    usage_provider: super::usage::Provider,
    url_model: Option<String>,
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
        let scanner = super::usage::Scanner::new(usage_provider, url_model);
        let inner = Box::pin(response.bytes_stream());
        let body = Body::from_stream(super::usage::tee_stream(inner, scanner));
        let mut resp = Response::builder().status(status);
        for (k, v) in &resp_headers {
            let ks = k.as_str().to_lowercase();
            if FORWARDED_HEADERS.contains(&ks.as_str()) {
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
    let mut scanner = super::usage::Scanner::new(usage_provider, url_model);
    scanner.feed_body(&resp_bytes);
    if let Some(usage) = scanner.finalize() {
        super::usage_meter::record(&usage);
    }

    let mut resp = Response::builder().status(status);
    for (k, v) in &resp_headers {
        let ks = k.as_str().to_lowercase();
        if FORWARDED_HEADERS.contains(&ks.as_str()) {
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
}
