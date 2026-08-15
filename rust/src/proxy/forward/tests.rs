use super::*;
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

struct SpyIntentClassifier(Arc<AtomicUsize>);

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
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap()
    });
    (format!("http://{address}"), server)
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
