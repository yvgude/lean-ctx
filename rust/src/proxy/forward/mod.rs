//! Shared upstream forward path for OpenAI-compatible providers.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, request::Parts},
    response::Response,
};

use super::ProxyState;
use super::connector::schedule_provider_connector;
use super::intent::classify_and_store_proxy_intent;
use crate::core::context_admission::egress::{self, EgressBody, EgressOutcome, EgressTarget};

#[cfg(feature = "shape-xlat")]
mod xlat;

mod driver;
pub(crate) mod enterprise_headers;
mod headers;
mod prepare;
pub mod trace_id;
mod transport;

#[cfg(test)]
mod tests;

#[allow(unused_imports)] // re-exported for proxy::* and tests
pub(super) use headers::{
    ALLOWED_REQUEST_HEADERS, FORWARDED_HEADERS, is_allowed_request_header,
    is_forwarded_response_header,
};
pub(super) use transport::xlat_stream_body;

// Unit tests import these via `use super::*`.
#[cfg(test)]
#[allow(unused_imports)]
use super::codec::{
    RequestBodyEncoding, decode_gzip_bounded, encode_gzip, encode_zstd, is_retryable_status,
    request_body_encoding,
};
#[cfg(test)]
#[allow(unused_imports)]
use headers::should_forward_request_header;
#[cfg(test)]
#[allow(unused_imports)]
pub(super) use prepare::{cohort_arm, prepare_request_body, wire_context};

pub(crate) mod pipeline;
pub use crate::core::config::PipelineConfig;
pub use pipeline::{CompressionPipeline, PipelineReport, StageReport};

const HEADROOM_COMPRESSED_HEADER: &str = "x-headroom-compressed";
const OCLA_BUDGET_SCOPE_HEADER: &str = "x-ocla-budget-scope";
const ESTIMATED_CHARS_PER_TOKEN: u64 = 4;

/// Final egress control (G6): admit the body that is about to leave. A
/// parsed body is admitted string by string and re-encoded only when the
/// gateway changed something, so an unchanged request keeps its exact bytes
/// (#1912); a body the proxy cannot parse is handled as an opaque payload.
fn admit_egress(
    parts: &Parts,
    body: Vec<u8>,
    parsed: Option<&serde_json::Value>,
    provider: &str,
    upstream_base: &str,
) -> Result<(Vec<u8>, EgressOutcome), StatusCode> {
    let target = EgressTarget {
        provider,
        model: parsed
            .and_then(|doc| doc.get("model"))
            .and_then(serde_json::Value::as_str),
        upstream_base,
    };
    let outcome = match parsed {
        Some(doc) => egress::admit_request(doc, &target),
        None => egress::admit_opaque(body.len(), &target),
    };
    let body = match &outcome.body {
        EgressBody::Rewritten(doc) => {
            let logical = serde_json::to_vec(doc).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            prepare::encode_request_body(parts, logical)?
        }
        EgressBody::Unchanged | EgressBody::Refused(_) => body,
    };
    Ok((body, outcome))
}

/// Check whether an incoming request was already compressed by Headroom.
pub(super) fn is_headroom_compressed(parts: &axum::http::request::Parts) -> bool {
    parts
        .headers
        .get(HEADROOM_COMPRESSED_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Default request-body ceiling (MiB). A large-codebase refactor with several
/// big files in context easily exceeds the old 10 MiB cap, which surfaced to the
/// agent as a hard `400` mid-task. Raised and made configurable via
/// `LEAN_CTX_PROXY_MAX_BODY_MB`.
const DEFAULT_MAX_BODY_MB: usize = 64;

#[derive(Clone)]
struct ProxyDispatchResult {
    error: Option<StatusCode>,
    response: std::sync::Arc<std::sync::Mutex<Option<Response>>>,
    economics: crate::core::execution_lifecycle::ProxyEconomicsObservation,
}

struct ProxyContextIntent {
    kernel_data: crate::core::context_kernel::proxy_bridge::ProxyRequestData,
    #[cfg(feature = "enterprise")]
    causal_session_id: String,
    #[cfg(feature = "enterprise")]
    causal_request: Option<serde_json::Value>,
    #[cfg(feature = "enterprise")]
    causal_turn_provided: u64,
}

#[derive(Clone, Copy)]
enum ProxyTerminalKind {
    CacheHit,
}

struct ProxyPrepared {
    state: ProxyState,
    extra_stream_types: Vec<String>,
    usage_provider: crate::proxy::usage::Provider,
    url_model: Option<String>,
    cohort: crate::proxy::holdout::Cohorts,
    wire: Option<Box<crate::proxy::usage::WireContext>>,
    xlat: bool,
    model: Option<String>,
    cache_prompt_hash: String,
    cache_alignment_score: u8,
    headroom_compatible: bool,
    determinism_audit: super::determinism_guard::DeterminismProof,
    determinism_proof: super::determinism_guard::DeterminismProof,
    pipeline_report: Option<PipelineReport>,
    tokens_pruned: usize,
    original_tokens: usize,
    task_class: String,
    content_dedup_tokens_saved: usize,
    original_size: usize,
    compressed_size: usize,
    compression_candidate: bool,
    tokens_saved: u64,
    route: Option<crate::proxy::routing::RouteDecision>,
    stats_label: String,
    introspect: Option<(serde_json::Value, super::introspect::Provider)>,
    prefix_replay: Option<(u64, Vec<u8>, Vec<serde_json::Value>, usize)>,
    // A response confirms the send, independently of later response decoding.
    upstream_send_succeeded: bool,
    upstream_started: Option<std::time::Instant>,
    context_ir: Option<ProxyContextIntent>,
    terminal: Option<ProxyTerminalKind>,
}

struct AdmittedProxyRequest {
    parts: Parts,
    raw_body_bytes: axum::body::Bytes,
    body_limit: usize,
    #[cfg(feature = "enterprise")]
    gate_rules: Option<super::policy_gate::GateRules>,
}

struct ProxyOutbound {
    parts: Parts,
    upstream_url: String,
    upstream_base: String,
    forwarded_body: Vec<u8>,
    preserve_content_encoding: bool,
    prepared: ProxyPrepared,
}

enum PreparedProxyRequest {
    Upstream(Box<ProxyOutbound>),
    Terminal(Box<ProxyPrimitive>),
}

enum ProxyPrimitive {
    Upstream {
        response: Result<reqwest::Response, StatusCode>,
        prepared: ProxyPrepared,
    },
    Cache {
        response: Response,
        prepared: ProxyPrepared,
    },
    Policy {
        response: Response,
        original_tokens: usize,
        trace_id: String,
    },
}

struct ProxyProcessed {
    result: ProxyDispatchResult,
    prepared: Option<ProxyPrepared>,
    skip_reason: Option<&'static str>,
}

struct ProxyDriver<'a, F> {
    state: Option<ProxyState>,
    request: Option<Request<Body>>,
    admitted: Option<AdmittedProxyRequest>,
    prepared_request: Option<PreparedProxyRequest>,
    upstream_base: &'a str,
    default_path: &'a str,
    compress_body: Option<F>,
    provider_label: &'a str,
    extra_stream_types: Vec<String>,
    trace_id: String,
}

fn proxy_dispatch_result(
    response: Response,
    tokens_pruned: usize,
    original_tokens: usize,
    task_class: &str,
) -> ProxyDispatchResult {
    ProxyDispatchResult {
        error: None,
        response: std::sync::Arc::new(std::sync::Mutex::new(Some(response))),
        economics: crate::core::execution_lifecycle::ProxyEconomicsObservation {
            tokens_pruned,
            original_tokens,
            task_class: task_class.to_owned(),
        },
    }
}

pub(super) fn max_body_bytes() -> usize {
    std::env::var("LEAN_CTX_PROXY_MAX_BODY_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_MAX_BODY_MB)
        .saturating_mul(1024 * 1024)
}

/// Appends lean-ctx's diagnosis to an upstream `401 … insufficient permissions`
/// body (#1774), preserving the original text byte for byte.
///
/// Any other 401 — a revoked key, a genuine organization-permission problem —
/// passes through untouched: guessing at those would be worse than staying quiet.
async fn annotate_openai_scope_401(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, max_body_bytes()).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if !text.contains("api.responses.write") && !text.contains("insufficient permissions") {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let hint = "lean-ctx: this looks like a ChatGPT subscription token sent to the OpenAI \
                platform API. api.openai.com accepts API keys only, so it reports a missing \
                scope rather than the real cause. Codex with a ChatGPT login belongs on the \
                subscription rail: run `lean-ctx proxy codex-chatgpt on` with the proxy \
                running. If you meant to use an API key, set OPENAI_API_KEY.";
    let annotated = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(mut doc) => {
            if let Some(error) = doc.get_mut("error").and_then(|e| e.as_object_mut()) {
                let message = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if let Some(message) = message {
                    error.insert(
                        "message".to_string(),
                        serde_json::Value::String(format!("{message}\n\n{hint}")),
                    );
                }
                error.insert(
                    "lean_ctx_hint".to_string(),
                    serde_json::Value::String(hint.to_string()),
                );
            }
            serde_json::to_vec(&doc).unwrap_or_else(|_| bytes.to_vec())
        }
        Err(_) => format!("{text}\n\n{hint}").into_bytes(),
    };
    // The body length changed. `build_response` never forwards content-length,
    // but drop it defensively so no stale value can survive.
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(annotated))
}

fn apply_ocla_budget_admission(
    parts: &axum::http::request::Parts,
    estimated_bytes: usize,
) -> Result<(), StatusCode> {
    let Some(scope) = parts
        .headers
        .get(OCLA_BUDGET_SCOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let estimated_tokens = (estimated_bytes as u64).saturating_add(ESTIMATED_CHARS_PER_TOKEN - 1)
        / ESTIMATED_CHARS_PER_TOKEN;
    crate::core::ocla::wire_api::admit_budgeted_request(scope, estimated_tokens, 0.0)
        .map_err(|_| StatusCode::PAYMENT_REQUIRED)
}

#[allow(clippy::if_not_else)]
pub async fn forward_request(
    State(state): State<ProxyState>,
    req: Request<Body>,
    upstream_base: &str,
    default_path: &str,
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize) + Send,
    provider_label: &str,
    extra_stream_types: &[&str],
) -> Result<Response, StatusCode> {
    let trace_id = trace_id::extract_or_generate_trace_id(req.headers());
    let (parts, body) = req.into_parts();
    let body_limit = super::bedrock::request_body_limit(&parts).unwrap_or_else(max_body_bytes);
    let body_bytes = axum::body::to_bytes(body, body_limit)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let request_scope = format!(
        "proxy:{provider_label}:{}:{}:{}:{trace_id}",
        parts.method,
        parts.uri,
        blake3::hash(&body_bytes).to_hex()
    );
    let req = Request::from_parts(parts, Body::from(body_bytes));
    let lifecycle = crate::core::execution_lifecycle::ExecutionLifecycle::global();
    let result = lifecycle
        .run(
            crate::core::execution_lifecycle::ToolRequest {
                tool_name: "proxy_forward".to_owned(),
                query: None,
                session_id: trace_id.clone(),
                agent_id: provider_label.to_owned(),
                surface: crate::core::execution_lifecycle::ToolSurface::Proxy,
                idempotency_key: Some(request_scope),
            },
            crate::core::execution_lifecycle::RuntimeContext {
                client_name: Some(provider_label.to_owned()),
                project_root: None,
            },
            crate::core::execution_lifecycle::ProductEntitlements::default(),
            ProxyDriver {
                state: Some(state),
                request: Some(req),
                admitted: None,
                prepared_request: None,
                upstream_base,
                default_path,
                compress_body: Some(compress_body),
                provider_label,
                extra_stream_types: extra_stream_types
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                trace_id,
            },
        )
        .await;

    let completed = match result {
        Ok(completed) => completed,
        Err(crate::core::execution_lifecycle::LifecycleRunError::Dispatch(status)) => {
            return Err(status);
        }
        Err(
            crate::core::execution_lifecycle::LifecycleRunError::Aborted(_)
            | crate::core::execution_lifecycle::LifecycleRunError::ReplayTypeMismatch,
        ) => {
            return Err(StatusCode::CONFLICT);
        }
    };
    if let Some(status) = completed.error {
        return Err(status);
    }
    let mut response = completed
        .response
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .ok_or(StatusCode::CONFLICT)?;
    let value_metrics = super::value_gate_proxy::session_metrics();
    let compression_ratio = super::value_gate_proxy::compression_ratio();
    if let Ok(value) = HeaderValue::from_str(&format!("{compression_ratio:.4}")) {
        response
            .headers_mut()
            .insert("x-leanctx-compression-ratio", value);
    }
    if let Some(session_cpao_micros) = value_metrics.session_cpao_micros
        && let Ok(value) = HeaderValue::from_str(&session_cpao_micros.to_string())
    {
        response
            .headers_mut()
            .insert("x-leanctx-cpao-micros", value);
    }
    Ok(response)
}

#[allow(clippy::if_not_else)]
async fn prepare_upstream_request(
    State(state): State<ProxyState>,
    admitted: AdmittedProxyRequest,
    upstream_base: &str,
    default_path: &str,
    compress_body: impl FnOnce(serde_json::Value, usize) -> (Vec<u8>, usize, usize),
    provider_label: &str,
    extra_stream_types: &[&str],
    trace_id: String,
) -> Result<PreparedProxyRequest, StatusCode> {
    let AdmittedProxyRequest {
        mut parts,
        raw_body_bytes,
        body_limit,
        #[cfg(feature = "enterprise")]
        gate_rules,
    } = admitted;
    let original_parsed =
        super::determinism_guard::parse_request_body(&raw_body_bytes, &parts, body_limit);
    let original_messages = original_parsed
        .as_ref()
        .map(super::determinism_guard::cache_relevant_messages);
    // #1905: decide the input-compression holdout arm once, on the caller's
    // pristine body. The control arm skips pre-optimization here and every
    // compression stage in `prepare_request_body` and the pipeline below.
    let compression_arm = original_parsed
        .as_ref()
        .and_then(super::holdout::compression_holdout_arm);
    let compression_control = compression_arm == Some(super::holdout::Arm::Control);
    let (mut body_bytes, mut pre_optimize_result) =
        serde_json::from_slice::<serde_json::Value>(&raw_body_bytes)
            .ok()
            .filter(|_| !compression_control)
            .and_then(|mut parsed_body| {
                let result = crate::proxy::pre_optimize::pre_optimize(&mut parsed_body)?;
                #[cfg(feature = "enterprise")]
                crate::proxy::reasoning_budget::apply_reasoning_budget_with_config(
                    &mut parsed_body,
                    &result.task_class,
                    &result.complexity,
                    &crate::core::config::Config::load().proxy.reasoning_budget,
                );
                let serialized = serde_json::to_vec(&parsed_body).ok()?;
                Some((serialized.into(), Some(result)))
            })
            .unwrap_or((raw_body_bytes.clone(), None));
    // Determinism telemetry: report a warning score to the caller, but keep the
    // request byte-for-byte intact. Scanning the raw body also makes this work
    // consistently for every provider shape handled by the shared forwarder.
    let cache_alignment_score = crate::proxy::cache_aligner::detect_volatile_content(
        std::str::from_utf8(&body_bytes).unwrap_or_default(),
    )
    .alignment_score;
    let mut lineage = super::lineage::from_trusted_request(&parts, &raw_body_bytes);
    if let Some(context) = lineage.as_mut() {
        context.trace_id.clone_from(&trace_id);
    }

    // Operator aliases: may rewrite `model` in the parsed body (before
    // compression, so exactly one serialization) and re-target the upstream.
    // Fail-open: any miss routes nothing. An org policy may exempt specific
    // projects from alias rewrites that change the model (#25).
    let routing_rules = crate::core::config::Config::load().proxy.routing.clone();
    #[cfg(feature = "enterprise")]
    let downgrade_forbidden = gate_rules.as_ref().is_some_and(|rules| {
        let project = parts
            .extensions
            .get::<super::gateway_identity::GatewayTags>()
            .and_then(|t| t.project.clone());
        super::policy_gate::downgrade_forbidden(rules, project.as_deref())
    });
    #[cfg(not(feature = "enterprise"))]
    let downgrade_forbidden = false;
    let route_upstreams =
        (routing_rules.is_active() && !downgrade_forbidden).then(|| state.upstream_snapshot());
    // Cross-shape translation (enterprise#16) only exists for the exact
    // messages-create call — count_tokens/batches subpaths have no OpenAI
    // equivalent and must stay within-shape.
    let xlat_ok = cfg!(feature = "shape-xlat")
        && provider_label == "Anthropic"
        && parts
            .uri
            .path()
            .trim_end_matches('/')
            .ends_with("/v1/messages");
    let route_hook = |parsed: &mut serde_json::Value| {
        route_upstreams.as_ref().and_then(|up| {
            super::routing::route_request(parsed, provider_label, up, &routing_rules, xlat_ok)
        })
    };
    let headroom_compatible = is_headroom_compressed(&parts);
    if headroom_compatible {
        super::anthropic::set_headroom_request(true);
    }
    let mut prepared = prepare::prepare_request_body(
        &parts,
        &body_bytes,
        compress_body,
        route_hook,
        upstream_base,
        provider_label == "OpenAI",
        compression_arm,
    )?;
    let guard = super::determinism_guard::DeterminismGuard::new(&trace_id);
    let mut determinism_proof = original_messages.as_deref().map_or_else(
        || guard.verify(&[], &[]),
        |before| {
            let after = prepared
                .parsed
                .as_ref()
                .map(super::determinism_guard::cache_relevant_messages)
                .unwrap_or_default();
            guard.verify(before, &after)
        },
    );
    let determinism_audit = determinism_proof.clone();
    let guard_reverted = !determinism_proof.is_stable;
    if guard_reverted {
        tracing::warn!(
            request_id = %determinism_proof.request_id,
            frozen_bytes = determinism_proof.frozen_bytes,
            modification_start_byte = determinism_proof.modification_start_byte,
            "lean-ctx determinism violation: reverting request modifications"
        );
        // Safe mode is deliberately fail-closed for cache safety: resend exactly
        // what the caller supplied instead of allowing a cache-busting rewrite.
        prepared.body = raw_body_bytes.to_vec();
        prepared.parsed = original_parsed.clone();
        prepared.original_size = raw_body_bytes.len();
        prepared.compressed_size = raw_body_bytes.len();
        prepared.compression_candidate = false;
        prepared.content_dedup_tokens_saved = 0;
        prepared.route = None;
        body_bytes = raw_body_bytes.clone();
        pre_optimize_result = None;
        determinism_proof = original_messages.as_deref().map_or_else(
            || guard.verify(&[], &[]),
            |before| guard.verify(before, before),
        );
    }
    if !super::outbound_model_policy::allows_outbound(prepared.parsed.as_ref()) {
        return Err(StatusCode::FORBIDDEN);
    }
    apply_ocla_budget_admission(&parts, prepared.body.len())?;
    let original_size = prepared.original_size;
    let mut compressed_size = prepared.compressed_size;
    let preserve_content_encoding = prepared.preserve_content_encoding;

    let mut pipeline_report = None;
    let mut pipeline_changed = false;
    // #1912: a guard revert promised the caller's exact bytes — no pipeline
    // stage and no effort injection may run on top of it. #1905: neither may
    // the input-compression control arm, which is forwarded uncompressed.
    if !guard_reverted
        && !compression_control
        && let Some(messages) = prepared
            .parsed
            .as_mut()
            .and_then(|body| body.get_mut("messages"))
            .and_then(serde_json::Value::as_array_mut)
    {
        let messages_before_pipeline = messages.clone();
        let active_user_before_pipeline = messages
            .iter()
            .rposition(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("user")
            })
            .map(|index| (index, messages[index].clone()));
        let pipeline_config = crate::core::config::Config::load().proxy.pipeline.clone();
        // Knowledge routing is advisory: it runs after pre-triage but before
        // compression, has a hard latency budget, and any miss leaves the
        // existing pipeline untouched.
        let context_advice = if pipeline_config.enable_knowledge_routing {
            let query = messages
                .iter()
                .rev()
                .find(|message| {
                    message.get("role").and_then(serde_json::Value::as_str) == Some("user")
                })
                .and_then(|message| message.get("content"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_default();
            let routing_task_id = trace_id.clone();

            if query.is_empty() {
                None
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(5),
                    tokio::task::spawn_blocking(move || {
                        crate::core::knowledge_router::KnowledgeRouter {
                            manifests: Vec::new(),
                            resolvers: vec![std::sync::Arc::new(
                                crate::core::knowledge_router::PatternReferenceResolver,
                            )],
                        }
                        .context_advice(
                            &routing_task_id,
                            &query,
                            &crate::core::task_spine::TaskProfileLocal::default(),
                            &crate::core::knowledge_router::builtin_manifests(),
                            None,
                        )
                    }),
                )
                .await
                {
                    Ok(Ok(advice)) if !advice.is_empty() => Some(advice),
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "knowledge routing task failed open");
                        None
                    }
                    Err(_) => {
                        tracing::debug!(
                            "knowledge routing exceeded 5ms; continuing without advice"
                        );
                        None
                    }
                }
            }
        } else {
            None
        };
        if let Ok(report) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            CompressionPipeline::run_with_context_advice(
                messages,
                &pipeline_config,
                context_advice.as_ref(),
            )
        })) {
            pipeline_changed = *messages != messages_before_pipeline;
            pipeline_report = Some(report);
        } else {
            *messages = messages_before_pipeline;
            tracing::warn!("compression pipeline failed; continuing with prepared request");
        }
        if let Some((index, active_user)) = active_user_before_pipeline {
            if let Some(current) = messages.get_mut(index) {
                *current = active_user;
            }
        }
    }

    if let (Some(report), Some(parsed_body)) = (pipeline_report.as_ref(), prepared.parsed.as_mut())
    {
        let effort_changed = report.apply_effort_budget(parsed_body);
        if pipeline_changed || effort_changed {
            let serialized =
                serde_json::to_vec(parsed_body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            // The upstream still receives the caller's Content-Encoding, so a
            // rewritten gzip/zstd body must be re-encoded, not sent as plain JSON.
            prepared.body = prepare::encode_request_body(&parts, serialized)?;
            prepared.compression_candidate = true;
        }
    }

    // #1912: when nothing changed the request's meaning — every rewrite was a
    // no-op or reverted — forward the caller's bytes untouched. A re-serialized
    // body reorders keys and reformats numbers, which busts provider prompt
    // caches keyed on the exact prefix bytes.
    if prepared.parsed.is_some() && prepared.parsed == original_parsed {
        prepared.body = raw_body_bytes.to_vec();
        compressed_size = original_size;
    }

    let compression_candidate = prepared.compression_candidate;
    // Agent compaction (prepare) plus the pipeline's in-request tool-output
    // dedup (#1980); a reverted pipeline reports zero for its stage.
    let content_dedup_tokens_saved = prepared.content_dedup_tokens_saved
        + pipeline_report
            .as_ref()
            .map_or(0, PipelineReport::dedup_tokens_saved);
    let route = prepared.route;
    let parsed = prepared.parsed;
    let intent_classification =
        classify_and_store_proxy_intent(&mut parts, parsed.as_ref(), lineage.as_ref(), &body_bytes);
    // Apply the routing decision to the wire: re-target the upstream and — for
    // registry providers holding their own key — swap the credential headers.
    let upstream_base = route
        .as_ref()
        .and_then(|r| r.upstream_base.as_deref())
        .unwrap_or(upstream_base);
    if let Some(provider) = route.as_ref().and_then(|r| r.credential.as_ref()) {
        super::providers::inject_gateway_credential(provider, &mut parts.headers)?;
    }
    schedule_provider_connector(&parts, lineage.as_ref(), route.as_ref(), provider_label);
    // #895 Track B: assign output savings from the pristine parsed body; #1905
    // assigns input compression before rewrites so both arms survive the pipeline.
    let cohort = super::holdout::Cohorts {
        output: parsed
            .as_ref()
            .and_then(|p| prepare::cohort_arm(p, provider_label, default_path)),
        compression: compression_arm,
    };
    // Shape label drives compression/routing; stats identity may differ —
    // Grok registry routes speak OpenAI shape but meter under "Grok".
    let registry_id = parts
        .extensions
        .get::<super::providers::RegistryProviderId>()
        .map(|r| r.id.as_str());
    let stats_label = super::providers::stats_label(registry_id, provider_label).to_owned();

    let tokens_saved = original_size.saturating_sub(compressed_size) as u64 / 4;
    #[cfg(feature = "enterprise")]
    let causal_session_id = lineage
        .as_ref()
        .map_or_else(|| trace_id.clone(), |context| context.session_id.clone());
    #[cfg(feature = "enterprise")]
    let causal_turn_provided = super::value_gate_proxy::session_metrics()
        .request_count
        .saturating_add(1);
    let context_ir = {
        let proxy_headers: Vec<(String, String)> = parts
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let kernel_data = crate::core::context_kernel::proxy_bridge::ProxyRequestData {
            headers: proxy_headers,
            input_tokens: original_size / 4,
            output_tokens: 0,
            tokens_saved: tokens_saved as usize,
            model: parsed
                .as_ref()
                .and_then(|v| v.get("model"))
                .and_then(|m| m.as_str())
                .map(String::from),
            provider: Some(provider_label.to_owned()),
            request_count: 1,
            ..Default::default()
        };
        Some(ProxyContextIntent {
            kernel_data,
            #[cfg(feature = "enterprise")]
            causal_session_id,
            #[cfg(feature = "enterprise")]
            causal_request: parsed.clone(),
            #[cfg(feature = "enterprise")]
            causal_turn_provided,
        })
    };

    let model = parsed
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.as_str())
        .map(str::to_owned);
    let cache_prompt_hash = super::ocla_cache_bridge::prompt_hash(&body_bytes);
    if let (Some(cache), Some(model)) = (&state.ocla_cache, model.clone())
        && let Some(cached) = cache.try_cache_hit(&model, &cache_prompt_hash, 0.0, 0)
    {
        let response = Response::builder()
            .status(cached.status)
            .body(Body::from(cached.body))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let introspect = parsed.as_ref().map(|parsed| {
            let provider = match provider_label {
                "Anthropic" | "Bedrock" => super::introspect::Provider::Anthropic,
                "OpenAI" | "ChatGPT" => super::introspect::Provider::OpenAi,
                _ => super::introspect::Provider::Gemini,
            };
            (parsed.clone(), provider)
        });
        let prepared = ProxyPrepared {
            state,
            extra_stream_types: extra_stream_types
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            usage_provider: super::usage::Provider::from_label(provider_label),
            url_model: None,
            cohort,
            wire: None,
            xlat: route.as_ref().is_some_and(|decision| decision.xlat),
            model: Some(model),
            cache_prompt_hash,
            cache_alignment_score,
            headroom_compatible,
            determinism_audit,
            determinism_proof,
            pipeline_report,
            tokens_pruned: tokens_saved as usize,
            original_tokens: original_size / 4,
            task_class: pre_optimize_result.as_ref().map_or_else(
                || {
                    intent_classification.as_ref().map_or_else(
                        || "unknown".to_owned(),
                        |classification| classification._decision.intent.clone(),
                    )
                },
                |result| result.task_class.clone(),
            ),
            content_dedup_tokens_saved,
            original_size,
            compressed_size,
            compression_candidate,
            tokens_saved,
            route,
            stats_label,
            introspect,
            prefix_replay: None,
            upstream_send_succeeded: false,
            upstream_started: None,
            context_ir,
            terminal: Some(ProxyTerminalKind::CacheHit),
        };
        return Ok(PreparedProxyRequest::Terminal(Box::new(
            ProxyPrimitive::Cache { response, prepared },
        )));
    }

    // Cross-shape route (enterprise#16): the body now speaks OpenAI Chat
    // Completions — address the matching endpoint instead of the caller's
    // `/v1/messages` path, and scan the response with the OpenAI parser.
    let xlat = route.as_ref().is_some_and(|r| r.xlat);
    let upstream_url = if xlat {
        format!("{upstream_base}/v1/chat/completions")
    } else {
        crate::proxy::codec::build_upstream_url(&parts, upstream_base, default_path)
    };

    // G6: final egress control, on exactly the body that leaves — after
    // compression, routing, translation and every cache-safety revert.
    let (request_body, egress) = admit_egress(
        &parts,
        prepared.body,
        parsed.as_ref(),
        provider_label,
        upstream_base,
    )?;
    let egress_agent = lineage.as_ref().map(|context| context.agent_id.clone());
    if let EgressBody::Refused(refusal) = &egress.body {
        tracing::warn!("lean-ctx proxy: {refusal} (see `lean-ctx inspect --proxy`)");
        egress::finish_off_runtime(egress, None, original_size, egress_agent).await;
        return Err(StatusCode::FORBIDDEN);
    }

    let forwarded_body = super::bedrock::finalize_request(
        provider_label,
        &mut parts,
        &body_bytes,
        request_body,
        body_limit,
        &upstream_url,
    )?;
    egress::finish_off_runtime(
        egress,
        Some(forwarded_body.clone()),
        original_size,
        egress_agent,
    )
    .await;

    let prefix_replay = if let Some(ref pre) = parsed {
        let cfg_replay = crate::core::config::Config::load();
        if matches!(
            cfg_replay.proxy.resolved_proxy_mode(),
            crate::core::config::ProxyMode::Cache
        ) {
            let system_val = pre.get("system");
            if let Some(msgs) = pre.get("messages").and_then(|m| m.as_array()) {
                let conv_id = super::prefix_replay::conversation_id(system_val, msgs);
                Some((conv_id, forwarded_body.clone(), msgs.clone(), msgs.len()))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Enterprise Suite: inject x-leanctx-* metadata headers before dispatch.
    {
        let enterprise_cfg = crate::core::config::Config::load().enterprise.clone();
        if enterprise_cfg.should_inject_headers() {
            let agent_id = lineage.as_ref().map(|l| l.agent_id.clone());
            let session_id = lineage.as_ref().map(|l| l.session_id.clone());
            let task_class = intent_classification
                .as_ref()
                .map(|ic| ic._decision.intent.clone());
            let meta = enterprise_headers::RuntimeMetadata {
                original_tokens: original_size / 4,
                compressed_tokens: compressed_size / 4,
                agent_id,
                session_id,
                task_class,
            };
            enterprise_headers::inject(&mut parts, &enterprise_cfg, &meta);
        }
    }
    // Measured usage: read the real model + billed tokens from the response.
    // Gemini puts the model in the URL path, not the request/response body.
    // Translated requests get OpenAI-shape responses regardless of the label.
    let usage_provider = if xlat {
        super::usage::Provider::OpenAi
    } else {
        super::usage::Provider::from_label(provider_label)
    };
    let url_model = if usage_provider == super::usage::Provider::Gemini {
        super::usage::gemini_model_from_path(parts.uri.path())
    } else {
        None
    };

    // Gateway context (enterprise#11/#17/#18): identity tags from the auth
    // guard + wire savings + baseline inputs, stamped onto the usage record.
    // A routed request is attributed to the provider actually serving it, and
    // carries the originally requested model as routed_from (enterprise#13).
    let mut wire = prepare::wire_context(
        &parts,
        provider_label,
        upstream_base,
        tokens_saved,
        original_size,
        lineage,
    );
    if let Some(route) = &route {
        wire.routed_from = Some(route.routed_from.clone());
        if let Some(id) = &route.provider_id {
            wire.provider.clone_from(id);
        }
        // Registry route targets carry their own local-inference flag
        // (shadow-rate billing); built-in targets keep the URL heuristic.
        if let Some(local) = route.local {
            wire.is_local = local;
        }
    }
    let wire = Some(wire);
    let introspect = parsed.as_ref().map(|parsed| {
        let provider = match provider_label {
            "Anthropic" | "Bedrock" => super::introspect::Provider::Anthropic,
            "OpenAI" | "ChatGPT" => super::introspect::Provider::OpenAi,
            _ => super::introspect::Provider::Gemini,
        };
        (parsed.clone(), provider)
    });
    let (tokens_pruned, original_tokens, task_class) = pre_optimize_result.as_ref().map_or_else(
        || {
            let task_class = intent_classification
                .as_ref()
                .map_or("unknown", |classification| {
                    classification._decision.intent.as_str()
                });
            (
                tokens_saved as usize,
                original_size / 4,
                task_class.to_owned(),
            )
        },
        |result| {
            (
                result.tokens_pruned,
                result.original_token_estimate,
                result.task_class.clone(),
            )
        },
    );
    let upstream_base = upstream_base.to_owned();
    let prepared = ProxyPrepared {
        state,
        extra_stream_types: extra_stream_types
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        usage_provider,
        url_model,
        cohort,
        wire,
        xlat,
        model,
        cache_prompt_hash,
        cache_alignment_score,
        headroom_compatible,
        determinism_audit,
        determinism_proof,
        pipeline_report,
        tokens_pruned,
        original_tokens,
        task_class,
        content_dedup_tokens_saved,
        original_size,
        compressed_size,
        compression_candidate,
        tokens_saved,
        route,
        stats_label,
        introspect,
        prefix_replay,
        upstream_send_succeeded: false,
        upstream_started: None,
        context_ir,
        terminal: None,
    };
    Ok(PreparedProxyRequest::Upstream(Box::new(ProxyOutbound {
        parts,
        upstream_url,
        upstream_base,
        forwarded_body,
        preserve_content_encoding,
        prepared,
    })))
}

fn apply_response_headers(response: &mut Response, prepared: &ProxyPrepared) {
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&prepared.tokens_pruned.to_string()) {
        headers.insert("x-leanctx-tokens-pruned", value);
    }
    if let Ok(value) = HeaderValue::from_str(&prepared.content_dedup_tokens_saved.to_string()) {
        headers.insert("x-leanctx-dedup-savings", value);
    }
    if let Ok(value) = HeaderValue::from_str(&prepared.task_class) {
        headers.insert("x-leanctx-task-class", value);
    }
    if let Some(rank) = super::leaderboard::rank_header_if_due()
        && let Ok(value) = HeaderValue::from_str(&rank)
    {
        headers.insert("x-leanctx-rank", value);
    }
    if let Ok(value) = HeaderValue::from_str(&prepared.cache_alignment_score.to_string()) {
        headers.insert("x-leanctx-cache-alignment", value);
    }
    if let Some(report) = prepared.pipeline_report.as_ref() {
        report.apply_response_headers(headers);
    }
    if let Some((prepared_body, _)) = prepared.introspect.as_ref() {
        let messages = prepared_body
            .get("messages")
            .or_else(|| prepared_body.get("input"))
            .and_then(serde_json::Value::as_array);
        if let Some(messages) = messages {
            // Same session-stable score the effort stage injects (#1912).
            let task = crate::proxy::effort_routing::TaskComplexity::from_score(
                crate::proxy::effort_routing::score_session_complexity(messages),
            );
            if let Ok(value) = HeaderValue::from_str(&task.score.to_string()) {
                headers.insert("x-leanctx-complexity", value);
            }
            if let Ok(value) = HeaderValue::from_str(&task.budget_tokens.to_string()) {
                headers.insert("x-leanctx-effort-budget", value);
            }
        }
    }
    super::determinism_guard::apply_response_headers(response, &prepared.determinism_proof);
    trace_id::inject_trace_id(response, &prepared.determinism_proof.request_id);
}
