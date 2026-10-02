//! Upstream HTTP send and client-facing response construction.

use axum::{
    body::Body,
    http::{StatusCode, request::Parts},
    response::Response,
};

use super::headers::{is_forwarded_response_header, should_forward_request_header};
use crate::proxy::ProxyState;
use crate::proxy::codec::{is_retryable_status, retry_backoff};

#[cfg(feature = "shape-xlat")]
use super::xlat::xlat_response_bytes;

pub(crate) async fn send_upstream(
    state: &ProxyState,
    parts: &Parts,
    url: &str,
    body: Vec<u8>,
    provider_label: &str,
    preserve_content_encoding: bool,
) -> Result<reqwest::Response, StatusCode> {
    let send_once = |body: Vec<u8>| {
        let mut req = state.client.request(parts.method.clone(), url);
        for (key, value) in &parts.headers {
            let k = key.as_str().to_lowercase();
            if should_forward_request_header(&k, preserve_content_encoding) {
                req = req.header(key.clone(), value.clone());
            }
        }
        req.body(body).send()
    };

    // First attempt. The request body is fully buffered, and no response byte
    // has reached the client yet — retrying here is always safe for the
    // client connection; the status filter keeps it safe semantically.
    let first = send_once(body.clone()).await;
    let retry_reason = match &first {
        Ok(resp) if is_retryable_status(resp.status()) => {
            format!("status {}", resp.status().as_u16())
        }
        Err(e) if e.is_connect() || e.is_timeout() => format!("connect error: {e}"),
        Ok(resp) => {
            let _ = resp; // healthy (or non-retryable) response — pass through
            return first.map_err(|_| StatusCode::BAD_GATEWAY);
        }
        Err(e) => {
            tracing::error!("lean-ctx proxy: {provider_label} upstream error: {e}");
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    tracing::warn!("lean-ctx proxy: {provider_label} upstream {retry_reason} — retrying once");
    retry_backoff().await;
    match send_once(body).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            // Second failure: surface the ORIGINAL outcome when it was an HTTP
            // response (its status/headers are more honest than our 502).
            tracing::error!("lean-ctx proxy: {provider_label} retry failed: {e}");
            first.map_err(|_| StatusCode::BAD_GATEWAY)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_response(
    response: reqwest::Response,
    extra_stream_types: &[&str],
    usage_provider: crate::proxy::usage::Provider,
    url_model: Option<String>,
    cohort: crate::proxy::holdout::Cohorts,
    wire: Option<Box<crate::proxy::usage::WireContext>>,
    xlat: bool,
    cache: Option<&crate::proxy::ocla_cache_bridge::OclaCacheBridge>,
    model: Option<&str>,
    cache_prompt_hash: &str,
) -> Result<Response, StatusCode> {
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);
    let resp_headers = response.headers().clone();

    // Gateway-billed USD from response headers (#1189): LiteLLM's standard
    // header plus the operator-configured one. Body-reported costs (OpenRouter
    // usage.cost) beat this inside the scanner.
    let extra_cost_header = crate::core::config::Config::load()
        .proxy
        .cost_response_header();
    let header_cost = crate::proxy::usage_accounting::cost_from_headers(
        &resp_headers,
        extra_cost_header.as_deref(),
    );

    let is_sse = crate::proxy::bedrock::response_is_sse(&resp_headers);
    let is_stream = is_sse
        || resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| extra_stream_types.iter().any(|t| ct.contains(t)));

    if is_stream {
        let scanner = crate::proxy::usage::Scanner::new(usage_provider, url_model)
            .with_cohort(cohort)
            .with_wire_context(wire)
            .with_header_cost(header_cost);
        let inner = Box::pin(response.bytes_stream());
        let body = crate::proxy::bedrock::build_stream_body(
            inner,
            scanner,
            is_sse,
            extra_stream_types.contains(&"application/vnd.amazon.eventstream"),
            xlat,
        );
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
    let ocla_context = wire
        .as_ref()
        .and_then(|wire| wire.ocla_request_context())
        .cloned();
    let mut scanner = crate::proxy::usage::Scanner::new(usage_provider, url_model)
        .with_cohort(cohort)
        .with_wire_context(wire)
        .with_header_cost(header_cost);
    scanner.feed_body(&resp_bytes);
    let measured_output_tokens = if let Some(usage) = scanner.finalize() {
        let output_tokens = usage.output_tokens;
        crate::proxy::usage_meter::record(&usage);
        output_tokens
    } else {
        u64::try_from(resp_bytes.len().saturating_add(3)).unwrap_or(u64::MAX) / 4
    };

    if let Some(context) = ocla_context {
        let request = crate::core::ocla::types::ResponseOptimizationRequest {
            context,
            response_ref: format!("blake3:{}", blake3::hash(&resp_bytes).to_hex()),
            original_tokens: measured_output_tokens,
            target_tokens: measured_output_tokens,
        };
        if let Err(error) = crate::core::ocla::OclaRegistry::global()
            .response_optimizer
            .optimize_response(request)
            .await
        {
            tracing::warn!("lean-ctx response optimizer unavailable: {error:?}");
        }
    }

    let resp_bytes =
        if let Some(shaped) = crate::proxy::shaping_hook::shape_response_if_enabled(&resp_bytes) {
            let actual_output_tokens = measured_output_tokens
                .saturating_sub(u64::try_from(shaped.tokens_saved).unwrap_or(u64::MAX));
            crate::core::solution_tracker::record_output_tokens(
                measured_output_tokens,
                actual_output_tokens,
            );
            tracing::debug!(tokens_saved = shaped.tokens_saved, "response shaped");
            shaped.bytes
        } else {
            resp_bytes.to_vec()
        };

    let resp_bytes = if xlat {
        xlat_response_bytes(&resp_bytes, status)
    } else {
        resp_bytes
    };
    if let (Some(cache), Some(model)) = (cache, model) {
        cache.record_response(
            model,
            cache_prompt_hash,
            0.0,
            0,
            status,
            &resp_bytes,
            measured_output_tokens,
        );
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

/// Streaming body, translated back to Anthropic SSE when `xlat` is set.
#[cfg(feature = "shape-xlat")]
pub(crate) fn xlat_stream_body<S>(teed: S, xlat: bool) -> Body
where
    S: futures::Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    if xlat {
        Body::from_stream(crate::proxy::shape_xlat::to_anthropic_stream(teed))
    } else {
        Body::from_stream(teed)
    }
}

#[cfg(not(feature = "shape-xlat"))]
pub(crate) fn xlat_stream_body<S>(teed: S, _xlat: bool) -> Body
where
    S: futures::Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    Body::from_stream(teed)
}

#[cfg(not(feature = "shape-xlat"))]
fn xlat_response_bytes(resp_bytes: &[u8], _status: StatusCode) -> Vec<u8> {
    resp_bytes.to_vec()
}
