use super::admission::trusted_destination_binding;
use super::*;
use crate::core::context_admission::egress::EgressTarget;
use crate::core::execution_lifecycle::{
    ExecutionDriver, ExecutionLifecycle, ProductEntitlements, RuntimeContext, StageDisposition,
    TaskContext, ToolRequest, ToolSurface,
};
use crate::core::ocla::registry::with_test_registry;
use crate::core::ocla::traits::{IntentClassifier, OclaService};
use crate::core::ocla::types::{
    IntentDecision, IntentRequest, OclaCapability, OclaCapabilityKind, OclaResult,
};
use crate::proxy::intent::ProxyIntentClassification;
use axum::body::to_bytes;
use axum::http::header::CONTENT_TYPE;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// #1774: the scope-error annotation must add lean-ctx's diagnosis without
/// discarding a byte of the upstream text, and must leave every other 401 alone.
#[tokio::test]
async fn scope_401_is_annotated_without_losing_the_upstream_message() {
    let body = serde_json::json!({
        "error": {
            "message": "You have insufficient permissions for this operation. Missing scopes: api.responses.write.",
            "type": "invalid_request_error"
        }
    });
    let response = Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("response");

    let annotated = annotate_openai_scope_401(response).await;
    assert_eq!(annotated.status(), StatusCode::UNAUTHORIZED);
    let bytes = to_bytes(annotated.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");

    assert!(
        text.contains("Missing scopes: api.responses.write"),
        "the upstream message must survive verbatim, got: {text}"
    );
    assert!(
        text.contains("codex-chatgpt on"),
        "the annotation must name the actual remedy, got: {text}"
    );
    assert!(
        text.contains("lean_ctx_hint"),
        "the hint must also be machine-readable, got: {text}"
    );
}

#[tokio::test]
async fn unrelated_401_passes_through_untouched() {
    let original = br#"{"error":{"message":"Incorrect API key provided."}}"#.to_vec();
    let response = Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::from(original.clone()))
        .expect("response");

    let passed = annotate_openai_scope_401(response).await;
    let bytes = to_bytes(passed.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(
        bytes.to_vec(),
        original,
        "a 401 that is not the scope error must not be rewritten"
    );
}

#[tokio::test]
async fn non_json_scope_401_still_gets_the_hint_appended() {
    let response = Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::from("insufficient permissions, see docs"))
        .expect("response");

    let annotated = annotate_openai_scope_401(response).await;
    let bytes = to_bytes(annotated.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    assert!(
        text.starts_with("insufficient permissions, see docs"),
        "the original body must come first, got: {text}"
    );
    assert!(
        text.contains("codex-chatgpt on"),
        "the hint must be appended, got: {text}"
    );
}

struct SpyIntentClassifier(Arc<AtomicUsize>);

fn proxy_test_state(upstream: &str) -> ProxyState {
    let (_, upstreams) = tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
        anthropic: upstream.to_owned(),
        openai: upstream.to_owned(),
        chatgpt: upstream.to_owned(),
        gemini: upstream.to_owned(),
        providers: Vec::new(),
    }));
    ProxyState {
        client: crate::proxy::upstream_http_client_builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap(),
        port: 0,
        stats: Arc::new(crate::proxy::ProxyStats::default()),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    }
}

fn proxy_test_context() -> TaskContext {
    ExecutionLifecycle::default().begin(
        ToolRequest {
            tool_name: "proxy_forward".into(),
            query: None,
            session_id: "admission-test".into(),
            agent_id: "OpenAI".into(),
            surface: ToolSurface::Proxy,
            idempotency_key: None,
        },
        RuntimeContext::default(),
        ProductEntitlements::default(),
    )
}

type TestCompressor = fn(serde_json::Value, usize) -> (Vec<u8>, usize, usize);

fn proxy_test_driver<'a>(
    state: ProxyState,
    body: &serde_json::Value,
    upstream: &'a str,
) -> ProxyDriver<'a, TestCompressor> {
    ProxyDriver {
        state: Some(state),
        request: Some(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        ),
        admitted: None,
        prepared_request: None,
        upstream_base: upstream,
        default_path: "/v1/chat/completions",
        compress_body: Some(|value, original_size| {
            let body = serde_json::to_vec(&value).unwrap();
            let size = body.len();
            (body, original_size, size)
        }),
        provider_label: "OpenAI",
        extra_stream_types: Vec::new(),
        trace_id: "admission-test".into(),
    }
}

fn cache_mode_request(
    label: &str,
    isolation: &crate::core::data_dir::IsolatedDataDir,
) -> serde_json::Value {
    let mut config = crate::core::config::Config::default();
    config.proxy.proxy_mode = Some("cache".into());
    assert!(
        crate::core::config::Config::path()
            .unwrap()
            .starts_with(isolation.path())
    );
    config.save().unwrap();
    assert_eq!(
        crate::core::config::Config::load()
            .proxy
            .resolved_proxy_mode(),
        crate::core::config::ProxyMode::Cache
    );
    serde_json::json!({"model": "gpt-5", "messages": [{"role": "user", "content": label}]})
}

async fn run_proxy_test_driver(
    driver: ProxyDriver<'_, TestCompressor>,
) -> (TaskContext, ProxyDispatchResult) {
    let lifecycle = ExecutionLifecycle::default();
    let request = ToolRequest {
        tool_name: "proxy_forward".into(),
        query: None,
        session_id: "admission-full-run".into(),
        agent_id: "OpenAI".into(),
        surface: ToolSurface::Proxy,
        idempotency_key: Some("capture-admission-context".into()),
    };
    // Reuse the public idempotency contract to inspect the actual run's shared
    // context; no synthetic stage entries or extra production test hooks.
    let context = lifecycle.begin(
        request.clone(),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let result = lifecycle
        .run(
            request,
            RuntimeContext::default(),
            ProductEntitlements::default(),
            driver,
        )
        .await
        .unwrap();
    assert_eq!(
        context.completed_stages(),
        crate::core::execution_lifecycle::LIFECYCLE_STAGE_ORDER
    );
    assert_eq!(
        context.outcome().unwrap().accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert!(context.outcome().unwrap().assessment.is_none());
    (context, result)
}

fn recorded_stage(
    context: &TaskContext,
    stage: crate::core::execution_lifecycle::LifecycleStage,
) -> StageDisposition {
    context
        .stage_executions()
        .into_iter()
        .find(|entry| entry.stage == stage)
        .unwrap()
        .disposition
}

#[tokio::test]
async fn failed_send_keeps_attempt_ledger_without_advancing_prefix() {
    let isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let body = cache_mode_request("failed-send-prefix-regression", &isolation);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let state = proxy_test_state(&upstream);
    let stats = state.stats.clone();
    let mut driver = proxy_test_driver(state, &body, &upstream);
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let PreparedProxyRequest::Upstream(outbound) = driver.prepared_request.as_ref().unwrap() else {
        panic!("upstream prepared")
    };
    let (id, _, originals, _) = outbound.prepared.prefix_replay.as_ref().unwrap();
    let id = *id;
    let mut extended = originals.clone();
    extended.push(serde_json::json!({"role": "user", "content": "next"}));
    assert!(crate::proxy::prefix_replay::detect_append_only(id, &extended).is_none());
    let (primitive, disposition) = driver.dispatch_primitive(&context).await.unwrap();
    assert_eq!(disposition, StageDisposition::Applied);
    let (processed, _) = driver
        .reversible_post_process(&context, primitive)
        .await
        .unwrap();
    assert_eq!(processed.result.error, Some(StatusCode::BAD_GATEWAY));
    assert!(!processed.prepared.as_ref().unwrap().upstream_send_succeeded);
    driver.record_ledger(&context, &processed).await.unwrap();
    assert_eq!(stats.requests_total.load(Ordering::Relaxed), 1);
    assert!(crate::proxy::prefix_replay::detect_append_only(id, &extended).is_none());
}

#[tokio::test]
async fn successful_send_advances_exact_prefix_even_if_response_body_fails() {
    let isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let body = cache_mode_request("successful-send-decode-error-prefix-regression", &isolation);
    let (upstream, server) = upstream_response_with_wire(
        b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{}",
    )
    .await;
    let state = proxy_test_state(&upstream);
    let stats = state.stats.clone();
    let mut driver = proxy_test_driver(state, &body, &upstream);
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let PreparedProxyRequest::Upstream(outbound) = driver.prepared_request.as_ref().unwrap() else {
        panic!("upstream prepared")
    };
    let (id, bytes, originals, _) = outbound.prepared.prefix_replay.as_ref().unwrap();
    let id = *id;
    let expected_bytes = bytes.clone();
    assert_eq!(expected_bytes, outbound.forwarded_body.bytes());
    let mut extended = originals.clone();
    extended.push(serde_json::json!({"role": "user", "content": "next"}));
    assert_eq!(stats.requests_total.load(Ordering::Relaxed), 0);
    assert!(
        !server.is_finished(),
        "preparation must not send the request"
    );
    let (primitive, disposition) = driver.dispatch_primitive(&context).await.unwrap();
    assert_eq!(disposition, StageDisposition::Applied);
    let ProxyPrimitive::Upstream { prepared, .. } = &primitive else {
        panic!("upstream dispatched")
    };
    assert!(prepared.upstream_send_succeeded);
    let (processed, _) = driver
        .reversible_post_process(&context, primitive)
        .await
        .unwrap();
    assert_eq!(processed.result.error, Some(StatusCode::BAD_GATEWAY));
    assert!(crate::proxy::prefix_replay::detect_append_only(id, &extended).is_none());
    driver.record_ledger(&context, &processed).await.unwrap();
    let delta = crate::proxy::prefix_replay::detect_append_only(id, &extended).unwrap();
    assert_eq!(delta.prefix_bytes, expected_bytes);
    assert_eq!(stats.requests_total.load(Ordering::Relaxed), 1);
    assert_eq!(server.await.unwrap()["model"], "gpt-5");
}

#[tokio::test]
async fn response_cache_hit_skips_dispatch() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!({"model": "gpt-5", "messages": [{"role": "user", "content": "cache-hit-no-dispatch"}]});
    let cache = Arc::new(crate::proxy::ocla_cache_bridge::OclaCacheBridge::new(
        Arc::new(crate::core::ocla::response_cache::ResponseCache::new(
            4,
            std::time::Duration::from_mins(1),
        )),
    ));
    cache.record_response(
        "gpt-5",
        &crate::proxy::ocla_cache_bridge::prompt_hash(&serde_json::to_vec(&body).unwrap()),
        0.0,
        0,
        StatusCode::OK,
        b"cached",
        1,
    );
    let mut state = proxy_test_state(&upstream);
    state.ocla_cache = Some(cache);
    let driver = proxy_test_driver(state, &body, &upstream);
    let (context, result) = run_proxy_test_driver(driver).await;
    assert_eq!(
        recorded_stage(
            &context,
            crate::core::execution_lifecycle::LifecycleStage::DispatchPrimitive
        ),
        StageDisposition::Skipped("response cache hit")
    );
    assert!(result.error.is_none());
    let response = result.response.lock().unwrap().take().unwrap();
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        "cached"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[cfg(feature = "enterprise")]
#[tokio::test]
#[serial_test::serial(policy_gate_ledger)]
async fn policy_refusal_skips_context_preparation_and_dispatch() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    struct ClearRules;
    impl Drop for ClearRules {
        fn drop(&mut self) {
            crate::proxy::policy_gate::test_clear_rules();
        }
    }
    let _rules = ClearRules;
    crate::proxy::policy_gate::test_set_rules(Some(crate::proxy::policy_gate::GateRules {
        allowed_models: vec!["permitted-model".into()],
        model_ceiling_groups: Vec::new(),
        forbid_downgrade_for: Vec::new(),
        max_cost_usd_per_person_per_day: None,
        max_cost_usd_per_project_per_month: None,
        max_requests_per_minute_per_person: None,
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!({"model": "forbidden-model", "messages": []});
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    driver.compress_body = Some(|_, _| panic!("policy must run before compressor"));
    let (context, result) = run_proxy_test_driver(driver).await;
    assert_eq!(
        recorded_stage(
            &context,
            crate::core::execution_lifecycle::LifecycleStage::ApplySecurityBoundaries
        ),
        StageDisposition::Applied
    );
    for stage in [
        crate::core::execution_lifecycle::LifecycleStage::GatherContextStrategy,
        crate::core::execution_lifecycle::LifecycleStage::DispatchPrimitive,
    ] {
        assert_eq!(
            recorded_stage(&context, stage),
            StageDisposition::Skipped("org policy refused request")
        );
    }
    assert!(result.error.is_none());
    assert!(
        result
            .response
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .status()
            .is_client_error()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn transport_failure_reaches_ledger_and_preserves_original_error() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let (_sender, upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: upstream.clone(),
            openai: upstream.clone(),
            chatgpt: upstream.clone(),
            gemini: upstream.clone(),
            providers: Vec::new(),
        }));
    let stats = Arc::new(crate::proxy::ProxyStats::default());
    let state = ProxyState {
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap(),
        port: 0,
        stats: stats.clone(),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    };
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "model": "gpt-5",
                "messages": [{"role": "user", "content": "inspect ".repeat(100)}]
            }))
            .unwrap(),
        ))
        .unwrap();
    let captured = Arc::new(std::sync::Mutex::new(None));
    let capture = captured.clone();
    let result = forward_request(
        State(state),
        request,
        &upstream,
        "/v1/chat/completions",
        move |value, original_size| {
            *capture.lock().unwrap() = crate::core::task_spine::TaskSpine::current();
            (serde_json::to_vec(&value).unwrap(), original_size, 0)
        },
        "OpenAI",
        &[],
    )
    .await;
    assert_eq!(result.unwrap_err(), StatusCode::BAD_GATEWAY);
    assert_eq!(stats.requests_total.load(Ordering::Relaxed), 1);
    let task = captured.lock().unwrap().clone().unwrap();
    let outcome = crate::core::execution_lifecycle::ExecutionLifecycle::global()
        .outcome_for(task.task_id.as_str())
        .unwrap();
    assert_eq!(
        outcome.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert!(outcome.assessment.is_none());
}

impl OclaService for SpyIntentClassifier {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::IntentClassifier)
    }
}

impl IntentClassifier for SpyIntentClassifier {
    fn classify_intent(&self, request: IntentRequest) -> OclaResult<IntentDecision> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(IntentDecision {
            intent: request
                .candidate_intents
                .into_iter()
                .next()
                .unwrap_or_default(),
            confidence_milli: 1000,
            rationale_ref: None,
        })
    }
}

fn parts_for(uri: &str) -> Parts {
    Request::builder().uri(uri).body(()).unwrap().into_parts().0
}

fn add_test_marker(mut value: serde_json::Value, original_size: usize) -> (Vec<u8>, usize, usize) {
    value["lean_ctx_touched"] = serde_json::Value::Bool(true);
    let out = serde_json::to_vec(&value).unwrap();
    let compressed_size = out.len();
    (out, original_size, compressed_size)
}

/// A gateway refusal is a typed answer (P5): 400, the reason code in a
/// response header and in the provider-shaped JSON error — never a bare
/// status, and never anything from the request.
async fn assert_typed_refusal(result: Result<Response, StatusCode>, code: &str) {
    assert_typed_refusal_status(result, StatusCode::BAD_REQUEST, code).await;
}

async fn assert_typed_refusal_status(
    result: Result<Response, StatusCode>,
    status: StatusCode,
    code: &str,
) {
    let response = result.expect("a gateway refusal is a typed response, not a bare status");
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[crate::proxy::admitted::REFUSAL_HEADER],
        "context_gateway"
    );
    assert_eq!(
        response.headers()[crate::proxy::admitted::REASON_CODES_HEADER],
        code
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("refusal body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON refusal body");
    assert_eq!(body["lean_ctx"]["reason_codes"], serde_json::json!([code]));
    assert_eq!(body["error"]["code"], "context_gateway_refused");
}

async fn upstream_response() -> (String, tokio::task::JoinHandle<serde_json::Value>) {
    upstream_response_with_wire(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").await
}

async fn upstream_response_with_wire(
    wire: &'static [u8],
) -> (String, tokio::task::JoinHandle<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let header = std::str::from_utf8(&request[..header_end]).unwrap();
        let content_length = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        stream.write_all(wire).await.unwrap();
        serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap()
    });
    (format!("http://{address}"), server)
}

/// G6 end to end: what the upstream model provider receives is the admitted
/// request — the credential is masked on the wire, the rest of the request
/// arrives intact, and the request's receipt is stored under the proxy key.
#[tokio::test]
async fn upstream_receives_only_admitted_content() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": format!("deploy with {key} today")}]
    });
    let (upstream, server) = upstream_response_with_wire(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
    )
    .await;
    let state = proxy_test_state(&upstream);
    let mut driver = proxy_test_driver(state, &body, &upstream);
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let _ = driver.dispatch_primitive(&context).await.unwrap();

    let received = server.await.unwrap().to_string();
    assert!(
        !received.contains(key),
        "the raw credential left the machine: {received}"
    );
    assert!(
        received.contains("deploy with"),
        "the rest of the turn arrives: {received}"
    );
    assert!(received.contains("today"), "{received}");

    let stored = crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        1,
    );
    let receipt = &stored
        .first()
        .expect("a proxy receipt")
        .1
        .as_ref()
        .expect("verified")
        .receipt;
    assert!(receipt.security.redactions >= 1);
    assert!(
        receipt.final_context.is_some(),
        "the forwarded bytes are bound"
    );
}

#[cfg(feature = "enterprise")]
#[tokio::test(flavor = "current_thread")]
async fn invalid_org_policy_refuses_forwarding_without_upstream_connection() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::proxy::policy_gate::test_policy_error();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let (_sender, upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: upstream.clone(),
            openai: upstream.clone(),
            chatgpt: upstream.clone(),
            gemini: upstream.clone(),
            providers: Vec::new(),
        }));
    let state = ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(crate::proxy::ProxyStats::default()),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    };
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"model":"test","messages":[]}"#))
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        forward_request(
            State(state),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("policy rejection must not wait on upstream");
    assert_eq!(result.unwrap_err(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept(),)
            .await
            .is_err(),
        "invalid policy must not open an upstream connection"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(cache_telemetry)]
async fn required_receipt_failure_never_connects_to_http_upstream() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("governed config");
    let data = crate::core::data_dir::lean_ctx_data_dir().expect("data directory");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::write(data.join("gateway"), b"blocked test store").expect("store obstruction");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let upstream = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("receipt refusal must not wait for upstream");
    assert_typed_refusal_status(
        result,
        StatusCode::SERVICE_UNAVAILABLE,
        "receipt.unavailable",
    )
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(cache_telemetry)]
async fn destination_allowlist_denial_never_connects_to_direct_registry_upstream() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        r#"
            [context_gateway]
            enabled = false

            [[context_gateway.allowed_destinations]]
            provider = "approved-provider"
            locality = "remote"
            organization_managed = false
            account_ref = "production-account"
            region = "eu-west-1"
        "#,
    )
    .expect("destination allow-list config");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let upstream = format!("http://{}", listener.local_addr().expect("address"));
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request");
    request
        .extensions_mut()
        .insert(super::super::providers::RegistryProviderId {
            id: "unapproved-provider".into(),
            local: false,
            account_ref: None,
            region: Some("eu-west-1".into()),
            gateway_managed_credential: false,
        });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("destination denial must not wait for upstream");
    assert_typed_refusal(result, "destination.constraint_unsatisfied").await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "an unapproved registry destination must be denied before dispatch"
    );

    // Even a matching registry and gateway-held credential do not establish
    // the configured account label when the caller supplies an OpenAI scope.
    let mut spoofed_scope = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .header("openai-project", "caller-selected-scope")
        .body(Body::from(
            r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("scoped request");
    spoofed_scope
        .extensions_mut()
        .insert(super::super::providers::RegistryProviderId {
            id: "approved-provider".into(),
            local: false,
            account_ref: Some("production-account".into()),
            region: Some("eu-west-1".into()),
            gateway_managed_credential: true,
        });
    let scoped_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        forward_request(
            State(proxy_test_state(&upstream)),
            spoofed_scope,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("unbound account scope must fail before upstream dispatch");
    assert_typed_refusal(scoped_result, "destination.constraint_unsatisfied").await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "caller-selected account scope must not reach an upstream under the configured account label"
    );
}

#[tokio::test]
#[ignore = "determinism guard reverts unstable modifications"]
async fn forward_request_applies_pre_optimization_and_reports_its_result() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = upstream_response().await;
    let old_message = "a".repeat(400);
    let mut messages = (0..11)
        .map(|_| serde_json::json!({"role": "assistant", "content": old_message}))
        .collect::<Vec<_>>();
    messages.push(serde_json::json!({
        "role": "user",
        "content": "Please fix this broken endpoint"
    }));
    let body = serde_json::json!({"model": "gpt-5", "messages": messages});
    let original_tokens = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["content"].as_str().unwrap().chars().count())
        .sum::<usize>()
        .div_ceil(4);
    let expected_pruned = (2 * (400_usize - 200)).div_ceil(4);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let (upstreams, state_upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: upstream.clone(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
    let _upstreams = upstreams;
    let state = ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(crate::proxy::ProxyStats::default()),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams: state_upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    };

    let response = forward_request(
        State(state),
        request,
        &upstream,
        "/v1/chat/completions",
        add_test_marker,
        "OpenAI",
        &[],
    )
    .await
    .unwrap();

    assert_eq!(
        response.headers()["x-leanctx-tokens-pruned"],
        expected_pruned.to_string()
    );
    assert_eq!(response.headers()["x-leanctx-task-class"], "coding_fix");
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        "{}"
    );
    let forwarded = server.await.unwrap();
    assert_eq!(
        forwarded["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["content"].as_str().unwrap().chars().count() == 200)
            .count(),
        2
    );
    assert_eq!(
        crate::proxy::value_gate_proxy::session_metrics().total_original_tokens,
        u64::try_from(original_tokens).unwrap()
    );
}

#[test]
fn ocla_budget_admission_rejects_over_limit_scope() {
    let mut parts = parts_for("/v1/chat/completions");
    parts.headers.insert(
        OCLA_BUDGET_SCOPE_HEADER,
        axum::http::HeaderValue::from_static("user:budget-forward-test"),
    );
    let scope = crate::core::ocla::budget::BudgetScope::User("budget-forward-test".into());
    let limit = crate::core::ocla::budget::BudgetLimit {
        scope,
        max_tokens_per_day: 2,
        max_usd_per_day: 1.0,
    };
    crate::core::ocla::wire_api::set_test_budget_limit(limit);

    let err = apply_ocla_budget_admission(&parts, 12).unwrap_err();

    assert_eq!(err, StatusCode::PAYMENT_REQUIRED);
}

#[test]
fn proxy_cycle_invokes_and_stores_intent_classification() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = crate::core::ocla::OclaRegistry::with_builtins();
    registry.intent_classifier = Arc::new(SpyIntentClassifier(calls.clone()));
    let _guard = with_test_registry(registry);

    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "explain caching"}]
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let mut parts = parts_for("/v1/chat/completions");
    let classification =
        classify_and_store_proxy_intent(&mut parts, Some(&body), None, &body_bytes)
            .expect("builtin proxy classification should succeed");

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        classification._decision.intent,
        "model=gpt-5; message=explain caching"
    );
    assert!(
        parts
            .extensions
            .get::<ProxyIntentClassification>()
            .is_some()
    );
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
        assert!(
            crate::proxy::codec::upstream_is_local(local),
            "{local} must count as local"
        );
    }
    for remote in [
        "https://api.anthropic.com",
        "https://acme.services.ai.azure.com/openai",
        "https://localhost.evil.example.com", // subdomain trick ≠ local
    ] {
        assert!(
            !crate::proxy::codec::upstream_is_local(remote),
            "{remote} must not be local"
        );
    }
}

#[test]
#[cfg(feature = "enterprise")]
fn wire_context_carries_identity_tags_and_baseline() {
    let mut parts = parts_for("/v1/messages");
    parts
        .extensions
        .insert(super::super::gateway_identity::GatewayTags {
            person: Some("yves".into()),
            team: Some("platform".into()),
            project: Some("billing".into()),
        });
    let wire = wire_context(
        &parts,
        "Anthropic",
        "https://api.anthropic.com",
        750,
        4000,
        None,
    );
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
            account_ref: None,
            region: None,
            gateway_managed_credential: false,
        });
    let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:11434", 0, 400, None);
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
            account_ref: None,
            region: None,
            gateway_managed_credential: false,
        });
    let wire = wire_context(
        &parts,
        "OpenAI",
        "http://host.docker.internal:11434",
        0,
        400,
        None,
    );
    assert!(wire.is_local, "declared local flag must win");

    let mut parts = parts_for("/v1/chat/completions");
    parts
        .extensions
        .insert(super::super::providers::RegistryProviderId {
            id: "tunnel".into(),
            local: false,
            account_ref: None,
            region: None,
            gateway_managed_credential: false,
        });
    let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:9999", 0, 400, None);
    assert!(!wire.is_local, "declared non-local flag must win");
}

#[test]
fn trusted_destination_binding_uses_routed_provider_and_declared_metadata() {
    let parts = parts_for("/v1/chat/completions");
    let route = super::super::routing::RouteDecision {
        model: "gpt-5".into(),
        routed_from: "team/fast".into(),
        provider_id: Some("foundry-prod".into()),
        upstream_base: Some("https://foundry.example".into()),
        credential: None,
        local: Some(false),
        account_ref: Some("finance-prod".into()),
        region: Some("eu-west-1".into()),
        gateway_managed_credential: true,
        xlat: false,
    };
    let binding = trusted_destination_binding(&parts, Some(&route));
    let destination = crate::core::context_admission::egress::destination_with_binding(
        &EgressTarget {
            provider: "OpenAI",
            model: Some("gpt-5"),
            upstream_base: "http://127.0.0.1:9999",
        },
        binding,
    );
    assert_eq!(destination.provider.as_str(), "foundry-prod");
    assert_eq!(
        destination.locality,
        lean_ctx_protocol::context_gateway::DestinationLocalityV1::Remote
    );
    assert_eq!(
        destination
            .account_ref
            .as_ref()
            .map(lean_ctx_protocol::ProtocolReference::as_str),
        Some("finance-prod")
    );
    assert_eq!(
        destination
            .region
            .as_ref()
            .map(lean_ctx_protocol::ProtocolReference::as_str),
        Some("eu-west-1")
    );

    // A model alias that has no provider identity keeps the shape label and URL
    // behavior for backwards-compatible built-in routing.
    let builtin = super::super::routing::RouteDecision {
        provider_id: None,
        local: None,
        account_ref: None,
        region: None,
        gateway_managed_credential: false,
        ..route
    };
    let binding = trusted_destination_binding(&parts, Some(&builtin));
    assert_eq!(binding.provider_id, None);
    assert_eq!(binding.local, None);
}

#[test]
fn model_only_alias_preserves_direct_registry_binding_but_builtin_target_replaces_it() {
    let mut parts = parts_for("/v1/chat/completions");
    parts
        .extensions
        .insert(super::super::providers::RegistryProviderId {
            id: "foundry-prod".into(),
            local: false,
            account_ref: Some("finance-prod".into()),
            region: Some("eu-west-1".into()),
            gateway_managed_credential: true,
        });
    let model_only = super::super::routing::RouteDecision {
        model: "gpt-5".into(),
        routed_from: "team/fast".into(),
        provider_id: None,
        upstream_base: None,
        credential: None,
        local: None,
        account_ref: None,
        region: None,
        gateway_managed_credential: false,
        xlat: false,
    };

    let retained = trusted_destination_binding(&parts, Some(&model_only));
    assert_eq!(retained.provider_id, Some("foundry-prod"));
    assert_eq!(retained.local, Some(false));
    assert_eq!(retained.account_ref, Some("finance-prod"));
    assert_eq!(retained.region, Some("eu-west-1"));

    let explicit_builtin = super::super::routing::RouteDecision {
        upstream_base: Some("https://api.openai.com".into()),
        ..model_only
    };
    let replaced = trusted_destination_binding(&parts, Some(&explicit_builtin));
    assert_eq!(replaced.provider_id, None);
    assert_eq!(replaced.local, None);
    assert_eq!(replaced.account_ref, None);
    assert_eq!(replaced.region, None);
}

#[test]
fn direct_registry_and_bedrock_bindings_ignore_caller_scope_headers() {
    let mut parts = parts_for("/v1/chat/completions");
    parts
        .extensions
        .insert(super::super::providers::RegistryProviderId {
            id: "bedrock-prod".into(),
            local: false,
            account_ref: Some("aws-account-prod".into()),
            region: Some("us-east-1".into()),
            gateway_managed_credential: true,
        });
    parts
        .extensions
        .insert(super::super::bedrock::SigningContext {
            region: "us-west-2".into(),
        });
    parts
        .headers
        .insert("openai-project", HeaderValue::from_static("caller-scope"));

    let binding = trusted_destination_binding(&parts, None);
    let destination = crate::core::context_admission::egress::destination_with_binding(
        &EgressTarget {
            provider: "Bedrock",
            model: Some("model-x"),
            upstream_base: "https://bedrock-runtime.us-east-1.amazonaws.com",
        },
        binding,
    );
    assert_eq!(destination.provider.as_str(), "bedrock-prod");
    assert_eq!(
        destination.locality,
        lean_ctx_protocol::context_gateway::DestinationLocalityV1::Remote
    );
    assert_eq!(
        destination.account_ref, None,
        "caller scope must not inherit the configured account label"
    );
    assert_eq!(
        destination
            .region
            .as_ref()
            .map(lean_ctx_protocol::ProtocolReference::as_str),
        Some("us-west-2")
    );
}

#[test]
fn caller_key_query_cannot_inherit_a_configured_account_label() {
    let mut parts = parts_for("/v1/models?alt=sse&%6Bey=caller-key&key=duplicate");
    parts
        .extensions
        .insert(super::super::providers::RegistryProviderId {
            id: "gemini-prod".into(),
            local: false,
            account_ref: Some("configured-gemini-account".into()),
            region: Some("europe-west4".into()),
            gateway_managed_credential: true,
        });

    let binding = trusted_destination_binding(&parts, None);

    assert_eq!(binding.provider_id, Some("gemini-prod"));
    assert_eq!(binding.local, Some(false));
    assert_eq!(binding.region, Some("europe-west4"));
    assert_eq!(
        binding.account_ref, None,
        "caller query credentials must not be attributed to the configured account"
    );
}

#[test]
fn sanitized_gemini_query_is_the_query_used_for_upstream_url_and_binding() {
    let mut parts = parts_for("/v1/models?alt=sse&%6Bey=caller-key&key=duplicate");
    parts
        .extensions
        .insert(super::super::providers::RegistryProviderId {
            id: "gemini-prod".into(),
            local: false,
            account_ref: Some("configured-gemini-account".into()),
            region: Some("europe-west4".into()),
            gateway_managed_credential: true,
        });
    assert_eq!(
        trusted_destination_binding(&parts, None).account_ref,
        None,
        "raw caller query must suppress account attribution before sanitization"
    );

    super::super::providers::strip_gemini_api_key_query(&mut parts.uri)
        .expect("Gemini query can be sanitized");
    let upstream_url =
        crate::proxy::codec::build_upstream_url(&parts, "https://gemini.example", "/v1/models");

    assert_eq!(upstream_url, "https://gemini.example/v1/models?alt=sse");
    assert_eq!(
        trusted_destination_binding(&parts, None).account_ref,
        Some("configured-gemini-account"),
        "the gateway credential is the only remaining key selector"
    );
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
    let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:11434", 0, 400, None);
    assert_eq!(wire.person, None);
    assert_eq!(wire.project, None);
    assert_eq!(wire.uncompressed_input_tokens, 100);
    assert!(wire.is_local);
}

#[test]
fn wire_context_carries_managed_lineage_without_forwarding_control_headers() {
    let parts = Request::builder().body(()).unwrap().into_parts().0;
    let lineage = crate::core::ocla::OclaRequestContext {
        request_id: "req-1".into(),
        session_id: "session-1".into(),
        agent_id: "agent-1".into(),
        content_ref: "blake3:abc".into(),
        tenant_id: None,
        trace_id: "tr-unit".into(),
        task_id: None,
        parent_task_id: None,
    };
    let wire = wire_context(
        &parts,
        "OpenAI",
        "https://api.openai.com",
        0,
        4,
        Some(lineage.clone()),
    );
    assert_eq!(wire.ocla_request_context(), Some(&lineage));
    for header in [
        super::super::lineage::REQUEST_ID_HEADER,
        super::super::lineage::SESSION_ID_HEADER,
        super::super::lineage::AGENT_ID_HEADER,
    ] {
        assert!(!is_allowed_request_header(header));
        assert!(!should_forward_request_header(header, false));
    }
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

    let prepared = prepare_request_body(
        &parts,
        &encoded,
        add_test_marker,
        |_| None,
        "https://api.openai.com",
        false,
        None,
    )
    .unwrap();
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

    let prepared = prepare_request_body(
        &parts,
        &encoded,
        add_test_marker,
        |_| None,
        "https://api.openai.com",
        false,
        None,
    )
    .unwrap();
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
fn openrouter_chat_requests_opt_into_billed_cost() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let body = serde_json::json!({"model": "deepseek/deepseek-v4-flash", "messages": []});
    let json = serde_json::to_vec(&body).unwrap();
    let parts = parts_for("/v1/chat/completions");

    let prepared = prepare_request_body(
        &parts,
        &json,
        add_test_marker,
        |_| None,
        "https://openrouter.ai/api",
        true,
        None,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&prepared.body).unwrap();
    assert_eq!(
        parsed["usage"]["include"], true,
        "OpenRouter chat requests must ask for the billed cost (#1179)"
    );
}

#[test]
fn non_openrouter_upstreams_never_carry_the_usage_opt_in() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let body = serde_json::json!({"model": "gpt-5.5", "messages": []});
    let json = serde_json::to_vec(&body).unwrap();
    let parts = parts_for("/v1/chat/completions");

    let prepared = prepare_request_body(
        &parts,
        &json,
        add_test_marker,
        |_| None,
        "https://api.openai.com",
        true,
        None,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&prepared.body).unwrap();
    assert!(
        parsed.get("usage").is_none(),
        "api.openai.com rejects unknown top-level params — no injection"
    );
}

#[test]
fn responses_api_bodies_never_carry_the_usage_opt_in() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let body = serde_json::json!({"model": "gpt-5.5", "input": []});
    let json = serde_json::to_vec(&body).unwrap();
    let parts = parts_for("/v1/responses");

    let prepared = prepare_request_body(
        &parts,
        &json,
        add_test_marker,
        |_| None,
        "https://openrouter.ai/api",
        true,
        None,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&prepared.body).unwrap();
    assert!(
        parsed.get("usage").is_none(),
        "`usage.include` is Chat-Completions-only — Responses bodies stay clean"
    );
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
        "https://api.openai.com",
        false,
        None,
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
        "https://api.openai.com",
        false,
        None,
    )
    .unwrap();

    assert_eq!(request_body_encoding(&parts), RequestBodyEncoding::Identity);
    assert_eq!(prepared.body, body);
    assert!(prepared.parsed.is_none());
    assert!(!prepared.compression_candidate);
    assert!(!prepared.preserve_content_encoding);
}

static COMPRESSOR_INPUT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn example_access_key() -> &'static str {
    concat!("AK", "IAIOSFODNN7EXAMPLE")
}

/// Records what the compression stage was handed, then tries to put the
/// caller's protected value back into the request.
fn reintroducing_compressor(
    mut value: serde_json::Value,
    original_size: usize,
) -> (Vec<u8>, usize, usize) {
    *COMPRESSOR_INPUT.lock().unwrap() = Some(value.to_string());
    value["metadata"] =
        serde_json::json!({ "note": format!("rotate {} later", example_access_key()) });
    let out = serde_json::to_vec(&value).unwrap();
    let compressed_size = out.len();
    (out, original_size, compressed_size)
}

/// G-02: admission decides on the caller's body first, so compression only
/// ever sees admitted content, and the final admission masks whatever a
/// transformation brings back before the bytes are bound and sent.
#[tokio::test]
async fn compression_sees_only_admitted_content_and_cannot_put_a_value_back() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let key = example_access_key();
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": format!("deploy with {key} today")}]
    });
    let (upstream, server) = upstream_response_with_wire(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
    )
    .await;
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    driver.compress_body = Some(reintroducing_compressor);
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let PreparedProxyRequest::Upstream(outbound) = driver.prepared_request.as_ref().unwrap() else {
        panic!("upstream prepared")
    };
    let bound = outbound.forwarded_body.bytes().to_vec();
    let _ = driver.dispatch_primitive(&context).await.unwrap();

    let seen = COMPRESSOR_INPUT
        .lock()
        .unwrap()
        .take()
        .expect("the compressor ran");
    assert!(
        !seen.contains(key),
        "compression was handed the caller's unadmitted value: {seen}"
    );
    assert!(seen.contains("deploy with"), "{seen}");

    let received = server.await.unwrap();
    let wire = received.to_string();
    assert!(
        !wire.contains(key),
        "a transformation put the value back on the wire: {wire}"
    );
    assert!(
        wire.contains("rotate"),
        "the rest of the note arrives: {wire}"
    );
    assert_eq!(
        received,
        serde_json::from_slice::<serde_json::Value>(&bound).unwrap(),
        "the upstream receives exactly the bytes the receipt was bound to"
    );
}

fn project_policy(body: &str) -> crate::core::policy::ResolvedPolicy {
    crate::core::policy::load(&format!(
        "name = \"proxy-egress-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{body}"
    ))
    .unwrap()
}

/// The policy the proxy runs under holds for proxied bodies as it does for
/// every other carrier: its redaction rules rewrite what leaves.
#[tokio::test]
async fn project_policy_redaction_applies_to_proxied_bodies() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(project_policy(
        "[redaction]\ncustomer = \"K-[0-9]{6}\"\n",
    )));
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "invoice for K-482193 is due"}]
    });
    let (upstream, server) = upstream_response().await;
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let _ = driver.dispatch_primitive(&context).await.unwrap();

    let received = server.await.unwrap().to_string();
    assert!(
        !received.contains("K-482193"),
        "the project rule was not applied: {received}"
    );
    assert!(received.contains("[REDACTED:customer]"), "{received}");
    assert!(received.contains("invoice for"), "{received}");
}

/// A body the policy cannot inspect has no provable admission: under an
/// active project policy it is refused with its reason and never dispatched,
/// even in developer mode.
#[tokio::test]
async fn uninspectable_body_under_a_project_policy_is_refused_without_a_connection() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(project_policy("")));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(vec![0xff_u8, 0xfe, 0x00, 0x01, 0x80]))
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("a refusal must not wait for upstream");
    assert_typed_refusal(result, "policy.content_withheld").await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "a refused body must not open an upstream connection"
    );
}

/// The proxy mode of a test that compares forwarded bytes, stated by the test
/// instead of inherited from whichever test set the variable before it: in
/// `cache` mode a request the gateway admits unchanged is forwarded byte for
/// byte. (The default `token` mode appends the verbosity steer to the last
/// user turn — `egress_lifecycle::token_mode_changes_only_the_last_user_turn`.)
/// Take it after the data-directory guard, which holds the environment lock;
/// the previous value is restored on drop.
fn proxy_mode(mode: &str) -> crate::setup::EnvVarGuard {
    crate::setup::EnvVarGuard::set("LEAN_CTX_PROXY_MODE", mode)
}

fn write_gateway_config(text: &str) {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(config.join("config.toml"), text).expect("gateway config");
}

fn identity_compression(
    value: &serde_json::Value,
    original_size: usize,
) -> (Vec<u8>, usize, usize) {
    let out = serde_json::to_vec(value).unwrap();
    let compressed_size = out.len();
    (out, original_size, compressed_size)
}

/// Runs between the caller admission and the final one, like any transformation.
const COMPRESSOR_WHILE_POLICY_TIGHTENS: TestCompressor = |value, original_size| {
    write_gateway_config("[context_gateway]\nmode = \"governed\"\n");
    identity_compression(&value, original_size)
};

const COMPRESSOR_WHILE_CONFIG_BREAKS: TestCompressor = |value, original_size| {
    write_gateway_config("[context_gateway\nmode = ");
    identity_compression(&value, original_size)
};

/// An admission is only valid under the policy it was decided under. When the
/// global policy changes — or stops being loadable — after the caller's body
/// was admitted, an untouched body is not sent on the strength of the old
/// decision: the request is refused with the typed reason, without any
/// upstream connection.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn policy_change_after_admission_refuses_an_untouched_body_before_dispatch() {
    for (compressor, status, code) in [
        (
            COMPRESSOR_WHILE_POLICY_TIGHTENS,
            StatusCode::BAD_REQUEST,
            "policy.changed",
        ),
        (
            COMPRESSOR_WHILE_CONFIG_BREAKS,
            StatusCode::SERVICE_UNAVAILABLE,
            "gateway.config_unloadable",
        ),
    ] {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#,
            ))
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            forward_request(
                State(proxy_test_state(&upstream)),
                request,
                &upstream,
                "/v1/chat/completions",
                compressor,
                "OpenAI",
                &[],
            ),
        )
        .await
        .expect("a refusal must not wait for upstream");
        assert_typed_refusal_status(result, status, code).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "{code}: bytes admitted under the old policy must not reach the upstream"
        );
    }
}

/// #1905: the compression holdout's control arm is the uncompressed baseline.
/// The same request is compressed in the treatment arm and forwarded with its
/// content unchanged in the control arm, and both carry their arm to the meter.
#[test]
fn compression_holdout_control_arm_is_forwarded_uncompressed() {
    use crate::proxy::holdout::Arm;
    let _iso = crate::core::data_dir::isolated_data_dir();
    // The treatment arm is compressed without a recovery copy: pin the mode.
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    // Output shaping is the other, independent holdout; keep it out of the way
    // so the body comparison is about input compression alone.
    crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
    crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
    crate::core::config::Config::update_global(|c| c.proxy.verbosity_steer = Some(false)).unwrap();
    let log = (0..90).fold(String::new(), |mut log, i| {
        use std::fmt::Write as _;
        let _ = writeln!(
            log,
            "INFO  processing item {i}: ok, latency={i}ms, queue depth normal"
        );
        log
    });
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "f1", "name": "forge_shell", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "f1", "content": log}]}
        ]
    });
    let json = serde_json::to_vec(&body).unwrap();
    let parts = parts_for("/v1/messages");
    let prepare = |arm| {
        prepare_request_body(
            &parts,
            &json,
            crate::proxy::anthropic::compress_request_body,
            |_| None,
            "https://api.anthropic.com",
            false,
            Some(arm),
        )
        .unwrap()
    };

    let treatment = prepare(Arm::Treatment);
    assert!(
        treatment.compressed_size < treatment.original_size,
        "the treatment arm must be compressed"
    );

    let control = prepare(Arm::Control);
    assert_eq!(control.compressed_size, control.original_size);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&control.body).unwrap(),
        body,
        "the control arm must reach the upstream uncompressed"
    );
}

#[path = "tests_header_relay.rs"]
mod header_relay;

#[path = "tests_egress_lifecycle.rs"]
mod egress_lifecycle;

#[path = "tests_receipt.rs"]
mod receipt;
