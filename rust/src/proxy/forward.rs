use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, request::Parts},
    response::Response,
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use std::borrow::Cow;
use std::io::{Read, Write};

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
    let (mut parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, max_body_bytes())
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    // Active router (enterprise#13): may rewrite `model` in the parsed body
    // (before compression, so exactly one serialization) and re-target the
    // upstream within the same wire shape. Fail-open: any miss routes nothing.
    let routing_rules = crate::core::config::Config::load().proxy.routing.clone();
    let route_upstreams = routing_rules.is_active().then(|| state.upstream_snapshot());
    let route_hook = |parsed: &mut serde_json::Value| {
        route_upstreams.as_ref().and_then(|up| {
            super::routing::route_request(parsed, provider_label, up, &routing_rules)
        })
    };

    let prepared = prepare_request_body(&parts, &body_bytes, compress_body, route_hook)?;
    let original_size = prepared.original_size;
    let compressed_size = prepared.compressed_size;
    let compression_candidate = prepared.compression_candidate;
    let preserve_content_encoding = prepared.preserve_content_encoding;
    let route = prepared.route;
    let parsed = prepared.parsed;

    // Apply the routing decision to the wire: re-target the upstream and — for
    // registry providers holding their own key — swap the credential headers.
    let upstream_base = route
        .as_ref()
        .and_then(|r| r.upstream_base.as_deref())
        .unwrap_or(upstream_base);
    if let Some(provider) = route.as_ref().and_then(|r| r.credential.as_ref()) {
        super::providers::inject_gateway_credential(provider, &mut parts.headers)?;
    }
    if let Some(ref parsed) = parsed {
        let provider = match provider_label {
            "Anthropic" => super::introspect::Provider::Anthropic,
            "OpenAI" | "ChatGPT" => super::introspect::Provider::OpenAi,
            _ => super::introspect::Provider::Gemini,
        };
        let breakdown = super::introspect::analyze_request(parsed, provider);
        state.introspect.record(breakdown);
    }

    // #895 Track B: assign output-savings holdout from the same pristine parsed
    // body that each provider's compressor receives. Only when active.
    let cohort = parsed
        .as_ref()
        .and_then(|p| cohort_arm(p, provider_label, default_path));

    if compression_candidate {
        state
            .stats
            .record_provider_request(provider_label, original_size, compressed_size);
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
        prepared.body,
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

    // Gateway context (enterprise#11/#17/#18): identity tags from the auth
    // guard + wire savings + baseline inputs, stamped onto the usage record.
    // A routed request is attributed to the provider actually serving it, and
    // carries the originally requested model as routed_from (enterprise#13).
    let mut wire = wire_context(
        &parts,
        provider_label,
        upstream_base,
        tokens_saved,
        original_size,
    );
    if let Some(route) = &route {
        wire.routed_from = Some(route.routed_from.clone());
        if let Some(id) = &route.provider_id {
            wire.provider = id.clone();
        }
        // Registry route targets carry their own local-inference flag
        // (shadow-rate billing); built-in targets keep the URL heuristic.
        if let Some(local) = route.local {
            wire.is_local = local;
        }
    }
    let wire = Some(wire);

    build_response(
        response,
        extra_stream_types,
        usage_provider,
        url_model,
        cohort,
        wire,
    )
    .await
}

/// Builds the request-side [`WireContext`](super::usage::WireContext) stamped
/// onto this turn's usage record: identity tags (inserted as a request
/// extension by the auth guard, enterprise#11), the per-request compression
/// saving, the pre-compression token estimate (baseline input, enterprise#18)
/// and whether the serving upstream is local.
fn wire_context(
    parts: &Parts,
    provider_label: &str,
    upstream_base: &str,
    tokens_saved: u64,
    original_size: usize,
) -> Box<super::usage::WireContext> {
    let tags = parts
        .extensions
        .get::<super::gateway_identity::GatewayTags>()
        .cloned()
        .unwrap_or_default();
    // Registry routes attribute usage to the provider identity ("foundry",
    // "local"), not the wire-shape label ("OpenAI") — shape ≠ identity. The
    // entry's resolved local flag rides along (shadow-rate billing for
    // non-loopback local endpoints, e.g. host.docker.internal).
    let registry = parts
        .extensions
        .get::<super::providers::RegistryProviderId>();
    let provider = registry.map_or(provider_label, |r| r.id.as_str());
    let is_local = registry.map_or_else(|| upstream_is_local(upstream_base), |r| r.local);
    Box::new(super::usage::WireContext {
        provider: provider.to_string(),
        person: tags.person,
        team: tags.team,
        project: tags.project,
        saved_tokens: tokens_saved,
        // bytes/4 — the same estimation basis the proxy stats use throughout.
        uncompressed_input_tokens: original_size as u64 / 4,
        is_local,
        routed_from: None, // populated by the routing hook (wave 3)
    })
}

/// True when the upstream base URL points at a loopback/local endpoint (an
/// Ollama/vLLM-style local model): billed with the transparent
/// `local_shadow_rate` instead of provider list prices (enterprise#15/#18).
fn upstream_is_local(upstream_base: &str) -> bool {
    let rest = upstream_base
        .strip_prefix("https://")
        .or_else(|| upstream_base.strip_prefix("http://"))
        .unwrap_or(upstream_base);
    let host_port = rest.split(['/', '?']).next().unwrap_or(rest);
    // Split off the port; bracketed IPv6 hosts keep their brackets.
    let host = if let Some(b) = host_port.strip_prefix('[') {
        b.split(']').next().unwrap_or(b)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
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
        "OpenAI" | "ChatGPT" => {
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

struct PreparedRequestBody {
    body: Vec<u8>,
    parsed: Option<serde_json::Value>,
    original_size: usize,
    compressed_size: usize,
    compression_candidate: bool,
    preserve_content_encoding: bool,
    /// Routing decision applied to the body (enterprise#13); `None` = passthrough.
    route: Option<super::routing::RouteDecision>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestBodyEncoding {
    Identity,
    Gzip,
    Zstd,
    Passthrough,
}

fn prepare_request_body(
    parts: &Parts,
    body_bytes: &[u8],
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    route_hook: impl FnOnce(&mut serde_json::Value) -> Option<super::routing::RouteDecision>,
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
                route: None,
            });
        }
    };

    let Some(mut parsed) = serde_json::from_slice::<serde_json::Value>(&decoded).ok() else {
        return Ok(PreparedRequestBody {
            body: body_bytes.to_vec(),
            parsed: None,
            original_size: body_bytes.len(),
            compressed_size: body_bytes.len(),
            compression_candidate: false,
            preserve_content_encoding: encoding != RequestBodyEncoding::Identity,
            route: None,
        });
    };

    // Router runs on the freshly parsed body, before compression: the model
    // swap lands in the same single serialization as the compression pass.
    let route = route_hook(&mut parsed);

    let original_size = decoded.len();
    let (logical_body, _, compressed_size) = compress_body(parsed.clone(), original_size);
    let body = match encoding {
        RequestBodyEncoding::Identity => logical_body,
        RequestBodyEncoding::Gzip => encode_gzip(&logical_body)?,
        RequestBodyEncoding::Zstd => encode_zstd(&logical_body)?,
        RequestBodyEncoding::Passthrough => unreachable!("passthrough returned above"),
    };

    Ok(PreparedRequestBody {
        body,
        parsed: Some(parsed),
        original_size,
        compressed_size,
        compression_candidate: true,
        preserve_content_encoding: encoding != RequestBodyEncoding::Identity,
        route,
    })
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
    // Azure OpenAI / AI Foundry credential header (universal providers, #7).
    "api-key",
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

fn request_body_encoding(parts: &Parts) -> RequestBodyEncoding {
    let Some(value) = parts
        .headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return RequestBodyEncoding::Identity;
    };

    let encodings = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.eq_ignore_ascii_case("identity"))
        .collect::<Vec<_>>();
    match encodings.as_slice() {
        [] => RequestBodyEncoding::Identity,
        [encoding] if encoding.eq_ignore_ascii_case("gzip") => RequestBodyEncoding::Gzip,
        [encoding] if encoding.eq_ignore_ascii_case("zstd") => RequestBodyEncoding::Zstd,
        _ => RequestBodyEncoding::Passthrough,
    }
}

fn decode_zstd_bounded(data: &[u8], max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    let decoder = zstd::Decoder::new(data).map_err(|e| {
        tracing::warn!("lean-ctx proxy: invalid zstd request body: {e}");
        StatusCode::BAD_REQUEST
    })?;
    read_bounded(decoder, max_bytes).inspect_err(|e| {
        tracing::warn!("lean-ctx proxy: zstd request decode failed: {e}");
    })
}

fn encode_zstd(data: &[u8]) -> Result<Vec<u8>, StatusCode> {
    zstd::encode_all(data, 3).map_err(|e| {
        tracing::error!("lean-ctx proxy: zstd request encode failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

fn decode_gzip_bounded(data: &[u8], max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    read_bounded(GzDecoder::new(data), max_bytes).inspect_err(|e| {
        tracing::warn!("lean-ctx proxy: gzip request decode failed: {e}");
    })
}

fn encode_gzip(data: &[u8]) -> Result<Vec<u8>, StatusCode> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(|e| {
        tracing::error!("lean-ctx proxy: gzip request encode failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    encoder.finish().map_err(|e| {
        tracing::error!("lean-ctx proxy: gzip request encode failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

fn read_bounded<R: Read>(reader: R, max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    let mut limited = reader.take(max_bytes as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if out.len() > max_bytes {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(out)
}

/// Statuses safe to retry once (enterprise#51): the upstream explicitly did
/// NOT process the request (429 rejected, 502/503 gateway/unavailable). 500 and
/// 504 are excluded — the model may have already consumed/billed the call.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503)
}

/// Short jittered backoff before the single retry: enough for a load balancer
/// to fail over or a rate-limit window to move, never long enough to stack up
/// under load (fail-open rule — the client's own retry logic stays primary).
async fn retry_backoff() {
    let mut buf = [0u8; 2];
    let jitter_ms =
        getrandom::fill(&mut buf).map_or(100, |()| u64::from(u16::from_le_bytes(buf)) % 200);
    tokio::time::sleep(std::time::Duration::from_millis(150 + jitter_ms)).await;
}

async fn send_upstream(
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
    wire: Option<Box<super::usage::WireContext>>,
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
        let scanner = super::usage::Scanner::new(usage_provider, url_model)
            .with_cohort(cohort)
            .with_wire_context(wire);
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
    let mut scanner = super::usage::Scanner::new(usage_provider, url_model)
        .with_cohort(cohort)
        .with_wire_context(wire);
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

    fn add_test_marker(
        mut value: serde_json::Value,
        original_size: usize,
    ) -> (Vec<u8>, usize, usize) {
        value["lean_ctx_touched"] = serde_json::Value::Bool(true);
        let out = serde_json::to_vec(&value).unwrap();
        let compressed_size = out.len();
        (out, original_size, compressed_size)
    }

    // --- enterprise#11/#18: wire context (identity + baseline inputs) ---

    #[test]
    fn upstream_is_local_detects_loopback_hosts() {
        for local in [
            "http://127.0.0.1:11434",
            "http://localhost:8080/v1",
            "http://[::1]:9999",
            "http://0.0.0.0:4000",
        ] {
            assert!(upstream_is_local(local), "{local} must count as local");
        }
        for remote in [
            "https://api.anthropic.com",
            "https://acme.services.ai.azure.com/openai",
            "https://localhost.evil.example.com", // subdomain trick ≠ local
        ] {
            assert!(!upstream_is_local(remote), "{remote} must not be local");
        }
    }

    #[test]
    fn wire_context_carries_identity_tags_and_baseline() {
        let mut parts = parts_for("/v1/messages");
        parts
            .extensions
            .insert(super::super::gateway_identity::GatewayTags {
                person: Some("yves".into()),
                team: Some("platform".into()),
                project: Some("billing".into()),
            });
        let wire = wire_context(&parts, "Anthropic", "https://api.anthropic.com", 750, 4000);
        assert_eq!(wire.provider, "Anthropic");
        assert_eq!(wire.person.as_deref(), Some("yves"));
        assert_eq!(wire.team.as_deref(), Some("platform"));
        assert_eq!(wire.project.as_deref(), Some("billing"));
        assert_eq!(wire.saved_tokens, 750);
        // bytes/4 estimate, same basis as the proxy stats (enterprise#18).
        assert_eq!(wire.uncompressed_input_tokens, 1000);
        assert!(!wire.is_local);
        assert_eq!(wire.routed_from, None);
    }

    #[test]
    fn wire_context_prefers_registry_provider_id_over_shape_label() {
        // /providers/local/... speaks the OpenAI shape but must meter as
        // "local" — the admin breakdown groups by provider identity (#20).
        let mut parts = parts_for("/v1/chat/completions");
        parts
            .extensions
            .insert(super::super::providers::RegistryProviderId {
                id: "local".into(),
                local: false,
            });
        let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:11434", 0, 400);
        assert_eq!(wire.provider, "local");
    }

    #[test]
    fn wire_context_registry_local_flag_beats_url_heuristic() {
        // The containerized gateway reaches host Ollama via
        // host.docker.internal — not loopback, but declared local = true must
        // book the shadow rate (enterprise#15/#18). And the inverse: a
        // loopback-tunneled cloud endpoint declared local = false must not.
        let mut parts = parts_for("/v1/chat/completions");
        parts
            .extensions
            .insert(super::super::providers::RegistryProviderId {
                id: "local".into(),
                local: true,
            });
        let wire = wire_context(
            &parts,
            "OpenAI",
            "http://host.docker.internal:11434",
            0,
            400,
        );
        assert!(wire.is_local, "declared local flag must win");

        let mut parts = parts_for("/v1/chat/completions");
        parts
            .extensions
            .insert(super::super::providers::RegistryProviderId {
                id: "tunnel".into(),
                local: false,
            });
        let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:9999", 0, 400);
        assert!(!wire.is_local, "declared non-local flag must win");
    }

    // --- enterprise#51: fail-open single retry ---

    #[test]
    fn retry_covers_exactly_not_processed_statuses() {
        // Retryable: the upstream explicitly did not process the request.
        for code in [429_u16, 502, 503] {
            assert!(
                is_retryable_status(reqwest::StatusCode::from_u16(code).unwrap()),
                "{code} must be retryable"
            );
        }
        // Not retryable: success, client errors, and "may have processed".
        for code in [200_u16, 400, 401, 404, 500, 504] {
            assert!(
                !is_retryable_status(reqwest::StatusCode::from_u16(code).unwrap()),
                "{code} must NOT be retryable"
            );
        }
    }

    #[test]
    fn wire_context_without_tags_still_carries_baseline() {
        // Local solo mode: no identity, but savings + baseline are still real.
        let parts = parts_for("/v1/chat/completions");
        let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:11434", 0, 400);
        assert_eq!(wire.person, None);
        assert_eq!(wire.project, None);
        assert_eq!(wire.uncompressed_input_tokens, 100);
        assert!(wire.is_local);
    }

    #[test]
    fn zstd_request_bodies_are_rewritten_and_reencoded() {
        let body = serde_json::json!({"model": "gpt-5", "input": []});
        let json = serde_json::to_vec(&body).unwrap();
        let encoded = encode_zstd(&json).unwrap();
        let parts = Request::builder()
            .uri("/backend-api/codex/responses")
            .header(axum::http::header::CONTENT_ENCODING, "zstd")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let prepared = prepare_request_body(&parts, &encoded, add_test_marker, |_| None).unwrap();
        assert_eq!(request_body_encoding(&parts), RequestBodyEncoding::Zstd);
        assert_eq!(prepared.original_size, json.len());
        assert!(prepared.compression_candidate);
        assert!(prepared.preserve_content_encoding);
        assert!(should_forward_request_header("content-encoding", true));
        assert!(!should_forward_request_header("content-encoding", false));

        let decoded = zstd::decode_all(prepared.body.as_slice()).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(parsed["lean_ctx_touched"], true);
        assert_eq!(parsed["model"], "gpt-5");
    }

    #[test]
    fn gzip_request_bodies_are_rewritten_and_reencoded() {
        let body = serde_json::json!({"model": "gpt-5", "input": []});
        let json = serde_json::to_vec(&body).unwrap();
        let encoded = encode_gzip(&json).unwrap();
        let parts = Request::builder()
            .uri("/backend-api/codex/responses")
            .header(axum::http::header::CONTENT_ENCODING, "gzip")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        let prepared = prepare_request_body(&parts, &encoded, add_test_marker, |_| None).unwrap();
        assert_eq!(request_body_encoding(&parts), RequestBodyEncoding::Gzip);
        assert_eq!(prepared.original_size, json.len());
        assert!(prepared.compression_candidate);
        assert!(prepared.preserve_content_encoding);

        let decoded = decode_gzip_bounded(&prepared.body, max_body_bytes()).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(parsed["lean_ctx_touched"], true);
        assert_eq!(parsed["model"], "gpt-5");
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

        assert_eq!(request_body_encoding(&parts), RequestBodyEncoding::Identity);
    }

    #[test]
    fn unknown_encoded_request_bodies_stay_passthrough() {
        let parts = Request::builder()
            .uri("/v1/responses")
            .header(axum::http::header::CONTENT_ENCODING, "br")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let body = b"not-json";

        let prepared = prepare_request_body(
            &parts,
            body,
            |_, _| panic!("unknown encodings must not be JSON-rewritten"),
            |_| None,
        )
        .unwrap();

        assert_eq!(
            request_body_encoding(&parts),
            RequestBodyEncoding::Passthrough
        );
        assert_eq!(prepared.body, body);
        assert!(prepared.parsed.is_none());
        assert!(!prepared.compression_candidate);
        assert!(prepared.preserve_content_encoding);
    }

    #[test]
    fn invalid_json_request_bodies_are_not_compression_candidates() {
        let parts = Request::builder()
            .uri("/v1/responses")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let body = b"not-json";

        let prepared = prepare_request_body(
            &parts,
            body,
            |_, _| panic!("invalid JSON must not enter the compression pipeline"),
            |_| None,
        )
        .unwrap();

        assert_eq!(request_body_encoding(&parts), RequestBodyEncoding::Identity);
        assert_eq!(prepared.body, body);
        assert!(prepared.parsed.is_none());
        assert!(!prepared.compression_candidate);
        assert!(!prepared.preserve_content_encoding);
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

    #[test]
    fn chatgpt_responses_use_openai_responses_holdout_key() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.output_holdout = Some(1.0);
        })
        .unwrap();

        let body = serde_json::json!({
            "model": "gpt-5",
            "input": "same conversation",
        });

        assert_eq!(
            cohort_arm(&body, "ChatGPT", "/backend-api/codex/responses"),
            cohort_arm(&body, "OpenAI", "/v1/responses")
        );
    }
}
