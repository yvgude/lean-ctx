//! Upstream HTTP send and client-facing response construction.

use axum::{
    body::Body,
    http::{StatusCode, request::Parts},
    response::Response,
};

use super::headers::{is_forwarded_response_header, should_forward_request_header};
use crate::proxy::ProxyState;
use crate::proxy::admitted::{
    AdmittedBody, GatewayRefusal, RequestHead, SendAttempt, transport_failure,
};
use crate::proxy::codec::{is_retryable_status, retry_backoff};
use futures::StreamExt as _;

#[cfg(feature = "shape-xlat")]
use super::xlat::xlat_response_bytes;

/// Why no upstream response is handed back.
#[derive(Debug)]
pub(crate) enum UpstreamFailure {
    /// Transport failure, reported as `status`. `observed`: every attempt
    /// that was sent has its observation on record (AP10-C5).
    Status { status: StatusCode, observed: bool },
    /// The gateway did not (or no longer) let the body leave.
    Refused(GatewayRefusal),
}

/// The proxy's own answer `status` to a request one of whose sent attempts
/// is not on record: empty, with [`EGRESS_EVIDENCE_HEADER`].
pub(crate) fn unobserved_failure(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response.headers_mut().insert(
        EGRESS_EVIDENCE_HEADER,
        axum::http::HeaderValue::from_static(OBSERVATION_UNAVAILABLE),
    );
    response
}

/// Response header on a reply whose sent attempts are not all on record: an
/// attempt left, but its transport observation could not be saved. The
/// stored lifecycle alone cannot show that gap — a missing attempt 2 reads
/// like a request that was never retried — so the reply states it.
pub(crate) const EGRESS_EVIDENCE_HEADER: &str = "x-leanctx-egress-evidence";
pub(crate) const OBSERVATION_UNAVAILABLE: &str = "receipt.observation_unavailable";

/// The upstream response a dispatch hands back.
#[derive(Debug)]
pub(crate) struct Dispatched {
    pub(crate) response: reqwest::Response,
    /// Every attempt that was sent has its observation on record. `false`
    /// (AP10-C5) is carried to the reply as [`EGRESS_EVIDENCE_HEADER`].
    pub(crate) observed: bool,
}

/// The head a forwarded request is sent with: the caller's method and the
/// allow-listed caller headers, as they are when the body is bound. It is
/// bound with the body, so nothing added to `parts` afterwards leaves.
pub(crate) fn forwarded_head(parts: &Parts, preserve_content_encoding: bool) -> RequestHead {
    RequestHead::new(
        parts.method.clone(),
        parts
            .headers
            .iter()
            .filter(|(name, _)| {
                should_forward_request_header(
                    &name.as_str().to_lowercase(),
                    preserve_content_encoding,
                )
            })
            .map(|(name, value)| (name.clone(), value.clone())),
    )
}

/// Dispatch an admitted body. The body type is the guarantee: only gateway
/// admission creates an [`AdmittedBody`], and it carries the bytes, the bound
/// upstream URL and head, the receipt every attempt is recorded against and
/// the authority each attempt — the retry after its backoff included — must
/// still find current before anything is sent.
pub(crate) async fn send_upstream(
    state: &ProxyState,
    body: &AdmittedBody,
    provider_label: &str,
) -> Result<Dispatched, UpstreamFailure> {
    // The retry uses the same admitted bytes and destination. A timeout does
    // not establish whether the peer processed the first attempt; preserve
    // each attempt separately instead of claiming exactly-once delivery.
    let SendAttempt {
        response: first,
        recorded,
    } = body
        .send(&state.client, 1, |request| request)
        .await
        .map_err(UpstreamFailure::Refused)?;
    let dispatched = |response: Result<reqwest::Response, reqwest::Error>, observed: bool| {
        response
            .map(|response| Dispatched { response, observed })
            .map_err(|_| UpstreamFailure::Status {
                status: StatusCode::BAD_GATEWAY,
                observed,
            })
    };
    if !recorded {
        return dispatched(first, false);
    }
    let retry_reason = match &first {
        Ok(resp) if is_retryable_status(resp.status()) => {
            format!("status {}", resp.status().as_u16())
        }
        Err(e) if e.is_connect() || e.is_timeout() => transport_failure(e).to_owned(),
        Ok(resp) => {
            let _ = resp; // healthy (or non-retryable) response — pass through
            return dispatched(first, true);
        }
        Err(e) => {
            tracing::error!(
                "lean-ctx proxy: {provider_label} upstream error: {}",
                transport_failure(e)
            );
            return dispatched(first, true);
        }
    };

    tracing::warn!("lean-ctx proxy: {provider_label} upstream {retry_reason} — retrying once");
    retry_backoff().await;
    // The policy may have changed during the backoff: the retry proves its
    // authority again and is refused, not sent, when it is no longer current.
    // Whatever is returned, attempt 2 was sent: an observation of it that was
    // not saved travels with the reply (AP10-C5), not only into a log line.
    let SendAttempt {
        response: second,
        recorded,
    } = body
        .send(&state.client, 2, |request| request)
        .await
        .map_err(UpstreamFailure::Refused)?;
    match second {
        Ok(response) => Ok(Dispatched {
            response,
            observed: recorded,
        }),
        Err(e) => {
            // Second failure: surface the ORIGINAL outcome when it was an HTTP
            // response (its status/headers are more honest than our 502).
            tracing::error!(
                "lean-ctx proxy: {provider_label} retry failed: {}",
                transport_failure(&e)
            );
            dispatched(first, recorded)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_response(
    response: reqwest::Response,
    upstream_started: Option<std::time::Instant>,
    extra_stream_types: &[&str],
    usage_provider: crate::proxy::usage::Provider,
    url_model: Option<String>,
    cohort: crate::proxy::holdout::Cohorts,
    wire: Option<Box<crate::proxy::usage::WireContext>>,
    xlat: bool,
    cache: Option<&crate::proxy::ocla_cache_bridge::OclaCacheBridge>,
    model: Option<&str>,
    cache_prompt_hash: &str,
    mut sealed: Option<crate::proxy::sealed_origin::Recorder>,
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
            .with_response_clock(upstream_started, status.as_u16())
            .with_cohort(cohort)
            .with_wire_context(wire)
            .with_header_cost(header_cost);
        // The finished sealed items of the stream are recorded as they pass;
        // the bytes themselves are relayed untouched.
        let framed = resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("application/vnd.amazon.eventstream"));
        let inner = Box::pin(response.bytes_stream().map(move |chunk| {
            if let (Some(recorder), Ok(bytes)) = (sealed.as_mut(), &chunk) {
                if framed {
                    recorder.feed_eventstream(bytes);
                } else {
                    recorder.feed(bytes);
                }
            }
            chunk
        }));
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

    if let Some(recorder) = sealed.as_mut() {
        recorder.observe_body(&resp_bytes);
    }

    // Non-streaming: the whole body is one JSON object carrying `usage`.
    let ocla_context = wire
        .as_ref()
        .and_then(|wire| wire.ocla_request_context())
        .cloned();
    let mut scanner = crate::proxy::usage::Scanner::new(usage_provider, url_model)
        .with_response_clock(upstream_started, status.as_u16())
        .with_cohort(cohort)
        .with_wire_context(wire)
        .with_header_cost(header_cost);
    scanner.feed_body(&resp_bytes);
    let measured_output_tokens =
        if let Some(usage) = scanner.finalize_at(crate::proxy::usage::UsageBoundary::BodyEnd) {
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
