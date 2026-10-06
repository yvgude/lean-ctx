use super::*;
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
        client: reqwest::Client::builder()
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
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
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
    assert_eq!(expected_bytes, outbound.forwarded_body);
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
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
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
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
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

#[tokio::test]
#[ignore = "determinism guard reverts unstable modifications"]
async fn forward_request_applies_pre_optimization_and_reports_its_result() {
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
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
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
        });
    let wire = wire_context(&parts, "OpenAI", "http://127.0.0.1:9999", 0, 400, None);
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

#[path = "tests_header_relay.rs"]
mod header_relay;
/// #1905: the compression holdout's control arm is the uncompressed baseline.
/// The same request is compressed in the treatment arm and forwarded with its
/// content unchanged in the control arm, and both carry their arm to the meter.
#[test]
fn compression_holdout_control_arm_is_forwarded_uncompressed() {
    use crate::proxy::holdout::Arm;
    let _iso = crate::core::data_dir::isolated_data_dir();
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
    assert_eq!(treatment.compression_arm, Some(Arm::Treatment));

    let control = prepare(Arm::Control);
    assert_eq!(control.compressed_size, control.original_size);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&control.body).unwrap(),
        body,
        "the control arm must reach the upstream uncompressed"
    );
    assert_eq!(control.compression_arm, Some(Arm::Control));
}

#[test]
fn upstream_url_preserves_subpath() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/count_tokens");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages/count_tokens"
    );
}

#[test]
fn upstream_url_preserves_batches_subpath() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/batches/batch_123/results");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages/batches/batch_123/results"
    );
}

#[test]
fn upstream_url_exact_path() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn upstream_url_preserves_query_params() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/count_tokens?model=claude-4");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
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
fn forwards_grok_cli_chat_proxy_headers() {
    // Grok CLI → cli-chat-proxy.grok.com. Stripping these used to yield
    // HTTP 426 Upgrade Required with client-version "(none)" on /responses.
    for required in [
        "x-xai-token-auth",
        "x-models-etag",
        "x-grok-client-version",
        "x-grok-client-identifier",
        "x-grok-client-mode",
        "x-grok-client-surface",
        "x-grok-model-override",
        "x-grok-agent-id",
        "x-grok-session-id",
        "x-grok-turn-id",
        "x-grok-conv-id",
        "x-grok-req-id",
        "x-grok-deployment-id",
        "x-grok-user-id",
        "x-grok-context-window",
        "x-grok-max-completion-tokens",
        "x-grok-doom-loop-check",
        "x-grok-managed-gateway",
    ] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "request header `{required}` must be on the allowlist"
        );
    }
    // Internal gateway tag must stay off the allowlist (enterprise#11).
    assert!(!is_allowed_request_header("x-leanctx-project"));
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

/// #1638: Claude Code fills its plan-usage windows *only* from the
/// `anthropic-ratelimit-unified-*` response headers. The allowlist enumerated
/// the four legacy `requests-`/`tokens-` names, so when the `unified-*` family
/// arrived upstream the proxy silently dropped all of it: `rate_limits`
/// disappeared from the statusline payload, and — the part that actually
/// matters — the near-limit warning machinery reads the same state, so a user
/// behind the proxy was never warned before hitting a limit.
///
/// The names below are the full family as parsed by Claude Code 2.1.236,
/// quoted from the report. They are asserted individually rather than through
/// the prefix, so narrowing the rule back to an enumeration fails here.
#[test]
fn forwards_the_whole_anthropic_ratelimit_family() {
    for required in [
        "anthropic-ratelimit-unified-status",
        "anthropic-ratelimit-unified-reset",
        "anthropic-ratelimit-unified-5h-utilization",
        "anthropic-ratelimit-unified-5h-reset",
        "anthropic-ratelimit-unified-5h-surpassed-threshold",
        "anthropic-ratelimit-unified-7d-utilization",
        "anthropic-ratelimit-unified-7d-reset",
        "anthropic-ratelimit-unified-7d-surpassed-threshold",
        "anthropic-ratelimit-unified-grace-status",
        "anthropic-ratelimit-unified-grace-5h-utilization",
        "anthropic-ratelimit-unified-grace-7d-utilization",
        "anthropic-ratelimit-unified-overage-status",
        "anthropic-ratelimit-unified-overage-utilization",
        "anthropic-ratelimit-unified-overage-reset",
        "anthropic-ratelimit-unified-overage-period",
        "anthropic-ratelimit-unified-representative-claim",
        "anthropic-ratelimit-unified-fallback",
        "anthropic-ratelimit-unified-upgrade-paths",
        // The legacy names the allowlist used to carry explicitly must keep
        // working — the prefix replaced the enumeration, not the coverage.
        "anthropic-ratelimit-requests-limit",
        "anthropic-ratelimit-requests-remaining",
        "anthropic-ratelimit-tokens-limit",
        "anthropic-ratelimit-tokens-remaining",
    ] {
        assert!(
            is_forwarded_response_header(required),
            "response header `{required}` carries the client's own quota state \
             and must reach it"
        );
    }
}

/// Also reported in #1638: without `request-id` a user cannot correlate a
/// failed request with Anthropic support, and `x-should-retry` is what tells
/// an SDK whether a failure is worth retrying at all.
#[test]
fn forwards_request_correlation_and_retry_headers() {
    for required in ["request-id", "x-should-retry", "retry-after"] {
        assert!(
            is_forwarded_response_header(required),
            "response header `{required}` must reach the client"
        );
    }
}

/// The fix widens one family; it must not turn the allowlist into a
/// pass-through. Hop-by-hop and framing headers stay out — the proxy rewrites
/// the body (decompression, SSE re-framing), so relaying the upstream's
/// framing would describe a response that no longer exists.
#[test]
fn the_allowlist_stays_an_allowlist() {
    for denied in [
        "transfer-encoding",
        "connection",
        "content-length",
        "server",
        "set-cookie",
    ] {
        assert!(
            !is_forwarded_response_header(denied),
            "`{denied}` must not be relayed"
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

#[test]
fn cache_prompt_hash_is_content_sensitive() {
    let hash = super::super::ocla_cache_bridge::prompt_hash;
    assert_ne!(hash(b"one"), hash(b"two"));
}

/// Loopback upstream that answers exactly one request and hands back its raw,
/// lowercased request header block. Lets a test assert what actually leaves the
/// proxy, not only what the allowlist constant claims.
async fn upstream_capturing_headers() -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break request.len();
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..header_end]).to_lowercase()
    });
    (format!("http://{address}"), server)
}

/// Minimal state for the transport layer. `send_upstream` reads only
/// `state.client`; the watch receiver keeps serving its last value after the
/// sender drops, so no keep-alive is needed.
fn relay_test_state() -> ProxyState {
    let (_upstreams, state_upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
    ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(crate::proxy::ProxyStats::default()),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams: state_upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
    }
}

/// Stub upstream that answers `200 {}` and yields the exact request body
/// bytes it received, so a test can assert byte-identity on the wire.
async fn upstream_capturing_body() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let header = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
        let content_length = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        request[header_end..header_end + content_length].to_vec()
    });
    (format!("http://{address}"), server)
}

/// #1912: a request the proxy does not change must reach the provider as the
/// client's exact bytes. Re-serializing reorders keys and rewrites number and
/// whitespace formatting, which moves the prompt-cache prefix every turn.
#[tokio::test]
async fn unchanged_request_is_forwarded_byte_identical() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let (upstream, server) = upstream_capturing_body().await;
    // Key order, spacing and `1.0` all differ from serde_json's canonical form.
    let raw = br#"{ "temperature": 1.0,  "model": "claude-sonnet-4-5",
  "messages": [ {"role": "user", "content": "hi"} ] }"#
        .to_vec();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(raw.clone()))
        .unwrap();

    let response = forward_request(
        State(relay_test_state()),
        request,
        &upstream,
        "/v1/messages",
        |value, original_size| {
            let out = serde_json::to_vec(&value).unwrap();
            let size = out.len();
            (out, original_size, size)
        },
        "Anthropic",
        &[],
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    assert_eq!(
        server.await.unwrap(),
        raw,
        "an unchanged request must be forwarded as the client's exact bytes"
    );
}

#[test]
fn forwards_opencode_session_header() {
    // #1752: OpenCode (and OpenCode zen) key session affinity off their own
    // header. #1730 enumerated only the Claude Code spellings, so this sibling
    // kept being stripped and every relayed request looked like a new session.
    assert!(ALLOWED_REQUEST_HEADERS.contains(&"x-opencode-session"));
    assert!(is_allowed_request_header("x-opencode-session"));
    assert!(should_forward_request_header("x-opencode-session", false));
}

#[tokio::test]
async fn opencode_session_reaches_the_upstream_verbatim() {
    // #1752 end-to-end: assert what the upstream actually receives, including
    // the mixed-case spelling OpenCode puts on the wire.
    let (upstream, server) = upstream_capturing_headers().await;
    let parts = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("X-OpenCode-Session", "0f9c2b71-4d3a-4e58-9c11-7a6e5b2d8f40")
        .header("X-Leanctx-Project", "internal-only")
        .body(())
        .unwrap()
        .into_parts()
        .0;

    let response = transport::send_upstream(
        &relay_test_state(),
        &parts,
        &upstream,
        b"{}".to_vec(),
        "Anthropic",
        false,
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    let seen = server.await.unwrap();
    assert!(
        seen.contains("x-opencode-session: 0f9c2b71-4d3a-4e58-9c11-7a6e5b2d8f40"),
        "session header must reach the upstream verbatim, got: {seen}"
    );
    // Widening the allowlist for #1752 must not widen it for anything else.
    assert!(
        !seen.contains("x-leanctx-project"),
        "internal header must stay stripped, got: {seen}"
    );
}

#[test]
fn forwards_claude_code_session_id_header() {
    // #1730: Claude Code's per-conversation UUID must survive the relay so a
    // downstream proxy can key session affinity and prompt-cache reuse on it.
    assert!(ALLOWED_REQUEST_HEADERS.contains(&"x-claude-code-session-id"));
    assert!(is_allowed_request_header("x-claude-code-session-id"));
    assert!(should_forward_request_header(
        "x-claude-code-session-id",
        false
    ));
}

#[tokio::test]
async fn claude_code_session_id_reaches_the_upstream_verbatim() {
    // #1730 end-to-end: assert what the upstream actually receives, including
    // the mixed-case spelling Claude Code puts on the wire.
    let (upstream, server) = upstream_capturing_headers().await;
    let parts = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(
            "X-Claude-Code-Session-Id",
            "11111111-2222-3333-4444-555555555555",
        )
        .header("X-Leanctx-Project", "internal-only")
        .body(())
        .unwrap()
        .into_parts()
        .0;

    let response = transport::send_upstream(
        &relay_test_state(),
        &parts,
        &upstream,
        b"{}".to_vec(),
        "Anthropic",
        false,
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    let seen = server.await.unwrap();
    assert!(
        seen.contains("x-claude-code-session-id: 11111111-2222-3333-4444-555555555555"),
        "session id must reach the upstream verbatim, got: {seen}"
    );
    // Widening the allowlist for #1730 must not widen it for anything else:
    // the internal gateway tag stays stripped (enterprise#11).
    assert!(
        !seen.contains("x-leanctx-project"),
        "internal header must stay stripped, got: {seen}"
    );
}

#[test]
fn forwards_commandcode_cli_headers() {
    // Command Code (`cmd`) gates agent calls on `x-command-code-version`.
    // Stripping it yields 403 upgrade_required ("CLI is out of date").
    for required in [
        "x-command-code-version",
        "x-cli-environment",
        "x-oauth-token",
        "x-oauth-provider",
        "x-project-slug",
        "x-taste-learning",
        "x-taste-usage",
        "x-oss-primary-provider",
        "x-system-prompt-breakdown",
        "x-cmd-zdr",
        "x-session-id", // shared; pin so CC session stays wired
    ] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "Command Code CLI header `{required}` must be on the request allowlist"
        );
    }
}
