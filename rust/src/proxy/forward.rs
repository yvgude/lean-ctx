use axum::{
    body::Body,
    extract::State,
    http::{request::Parts, Request, StatusCode},
    response::Response,
};

use super::ProxyState;

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub type CompressFn = fn(&[u8]) -> (Vec<u8>, usize, usize);

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
