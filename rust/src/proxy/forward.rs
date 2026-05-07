use axum::{
    body::Body,
    extract::State,
    http::{request::Parts, Request, StatusCode},
    response::Response,
};

use super::ProxyState;

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub type CompressFn = fn(&[u8]) -> (Vec<u8>, usize, usize);

pub(crate) fn upstream_from_env_or_config(
    var_name: &str,
    config_value: Option<&str>,
    default: &str,
) -> String {
    std::env::var(var_name)
        .ok()
        .and_then(|value| normalize_upstream(&value))
        .or_else(|| config_value.and_then(normalize_upstream))
        .unwrap_or_else(|| default.trim_end_matches('/').to_string())
}

fn normalize_upstream(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

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
    let body_bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    state.stats.record_request();

    let (compressed_body, original_size, compressed_size) = compress_body(&body_bytes);

    if compressed_size < original_size {
        state
            .stats
            .record_compression(original_size, compressed_size);
    }

    let upstream_url = build_upstream_url(&parts, upstream_base, default_path);
    let response = send_upstream(
        &state,
        &parts,
        &upstream_url,
        compressed_body,
        provider_label,
    )
    .await?;

    build_response(response, extra_stream_types).await
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
        if k == "host" || k == "content-length" || k == "transfer-encoding" {
            continue;
        }
        req = req.header(key.clone(), value.clone());
    }

    req.body(body).send().await.map_err(|e| {
        tracing::error!("lean-ctx proxy: {provider_label} upstream error: {e}");
        StatusCode::BAD_GATEWAY
    })
}

async fn build_response(
    response: reqwest::Response,
    extra_stream_types: &[&str],
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
        let stream = response.bytes_stream();
        let body = Body::from_stream(stream);
        let mut resp = Response::builder().status(status);
        for (k, v) in &resp_headers {
            resp = resp.header(k, v);
        }
        return resp
            .body(body)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let resp_bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let mut resp = Response::builder().status(status);
    for (k, v) in &resp_headers {
        let ks = k.as_str().to_lowercase();
        if ks == "transfer-encoding" || ks == "content-length" {
            continue;
        }
        resp = resp.header(k, v);
    }
    resp.body(Body::from(resp_bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::{normalize_upstream, upstream_from_env_or_config};

    #[test]
    fn normalizes_configured_upstream() {
        assert_eq!(
            normalize_upstream(" https://example.test/api/code/ "),
            Some("https://example.test/api/code".to_string())
        );
        assert_eq!(normalize_upstream("   "), None);
    }

    #[test]
    fn upstream_from_env_or_config_uses_default_when_unset_or_empty() {
        let var_name = "LEAN_CTX_TEST_UPSTREAM_FROM_ENV";
        std::env::remove_var(var_name);
        assert_eq!(
            upstream_from_env_or_config(var_name, None, "https://api.example.test/"),
            "https://api.example.test"
        );

        std::env::set_var(var_name, "   ");
        assert_eq!(
            upstream_from_env_or_config(var_name, None, "https://api.example.test/"),
            "https://api.example.test"
        );
        std::env::remove_var(var_name);
    }

    #[test]
    fn upstream_from_env_prefers_configured_value() {
        let var_name = "LEAN_CTX_TEST_CONFIGURED_UPSTREAM";
        std::env::set_var(var_name, "https://gateway.example.test/api/code/");
        assert_eq!(
            upstream_from_env_or_config(var_name, None, "https://api.example.test"),
            "https://gateway.example.test/api/code"
        );
        std::env::remove_var(var_name);
    }

    #[test]
    fn upstream_from_env_or_config_prefers_env_then_config_then_default() {
        let _guard = crate::core::data_dir::test_env_lock();
        let var_name = "LEAN_CTX_TEST_CONFIG_PRECEDENCE_UPSTREAM";

        std::env::set_var(var_name, "https://env.example.test/");
        assert_eq!(
            upstream_from_env_or_config(
                var_name,
                Some("https://config.example.test/"),
                "https://default.example.test/"
            ),
            "https://env.example.test"
        );

        std::env::remove_var(var_name);
        assert_eq!(
            upstream_from_env_or_config(
                var_name,
                Some("https://config.example.test/"),
                "https://default.example.test/"
            ),
            "https://config.example.test"
        );

        assert_eq!(
            upstream_from_env_or_config(var_name, Some("   "), "https://default.example.test/"),
            "https://default.example.test"
        );
    }
}
