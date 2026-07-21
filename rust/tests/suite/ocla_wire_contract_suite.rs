//! Golden contract tests for the public OCLA wire protocol.

use axum::body::{Body, to_bytes};
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode, header};
use lean_ctx::core::ocla::types::{
    AGENT_ENVELOPE_SCHEMA_VERSION, AgentEnvelope, CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
    CanonicalTokenEnvelopeV1, OclaRequestContext, TokenBalanceV1, TokenEnvelopeSurface,
    TokenFlowDirection,
};
use lean_ctx::core::ocla::wire::{
    agent_envelope_schema, canonical_envelope_schema, decode_agent_envelope, decode_envelope,
    encode_agent_envelope, encode_envelope,
};
use lean_ctx::core::ocla::wire_api::ocla_router;
use lean_ctx::core::ocla::wire_stream::{StreamFrame, decode_frame, encode_frame};
use serde_json::Value;
use tower::ServiceExt;

const GOLDEN_ENVELOPE: &str = include_str!("../fixtures/ocla_envelope_golden.json");
const GOLDEN_SCHEMA: &str = include_str!("../fixtures/ocla_schema_golden.json");
const MIXED_BATCH: &str = include_str!("../fixtures/envelope_batch_mixed.json");
const LEGACY_ENVELOPE: &str = include_str!("../fixtures/envelope_v1_legacy.json");

fn golden_document() -> Value {
    serde_json::from_str(GOLDEN_ENVELOPE).expect("valid OCLA envelope fixture")
}

fn golden_agent() -> Value {
    let mut golden = golden_document()["agent"].clone();
    golden["relay_id"] = Value::String(agent_envelope().relay_id);
    golden
}

fn canonical_envelope() -> CanonicalTokenEnvelopeV1 {
    CanonicalTokenEnvelopeV1 {
        schema_version: CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
        context: OclaRequestContext {
            request_id: "request-golden-001".into(),
            session_id: "session-golden-001".into(),
            agent_id: "agent-golden-001".into(),
            content_ref: "blake3:0123456789abcdef".into(),
            tenant_id: Some("tenant-golden".into()),
            trace_id: "tr-unit".into(),
        },
        surface: TokenEnvelopeSurface::Proxy,
        direction: TokenFlowDirection::Output,
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        token_balance: TokenBalanceV1 {
            original_tokens: 1_234,
            materialized_tokens: 987,
            delivered_tokens: 876,
            provider_billed_tokens: 876,
        },
        route_ref: Some("route:golden-primary".into()),
        policy_ref: Some("policy:strict-v1".into()),
        idempotency_key: "request-golden-001:output".into(),
    }
}

fn agent_envelope() -> AgentEnvelope {
    let mut envelope = AgentEnvelope {
        schema_version: AGENT_ENVELOPE_SCHEMA_VERSION,
        relay_id: "agent-relay:pending".into(),
        context: OclaRequestContext {
            request_id: "agent-request-golden-001".into(),
            session_id: "agent-session-golden-001".into(),
            agent_id: "owner-agent".into(),
            content_ref: "blake3:fedcba9876543210".into(),
            tenant_id: Some("tenant-agent-golden".into()),
            trace_id: "tr-unit".into(),
        },
        from_agent_id: "owner-agent".into(),
        to_agent_id: "reviewer-agent".into(),
        capsule_ref: format!("capsule:{}", "abcdef0123456789".repeat(4)),
        budget_tokens: 4_096,
    };
    envelope.assign_relay_id().expect("assign golden relay ID");
    envelope
}

async fn post_envelope(body: String, idempotency_key: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/ocla/v1/envelope")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = idempotency_key {
        request = request.header("Idempotency-Key", key);
    }
    let response = ocla_router()
        .oneshot(request.body(Body::from(body)).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&body).expect("JSON response");
    (status, value)
}

async fn json_request(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            body.map_or_else(String::new, |body| body.to_string()),
        ))
        .expect("request");
    let response = ocla_router().oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("response body");
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).expect("JSON response")
    };
    (status, value)
}

async fn text_request(uri: &str, body: &str) -> (StatusCode, Value) {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(body.to_owned()))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&body).expect("JSON response");
    (status, value)
}

#[test]
fn canonical_envelope_matches_golden_fixture() {
    let wire = encode_envelope(&canonical_envelope()).expect("encode canonical envelope");
    let actual: Value = serde_json::from_str(&wire).expect("encoded canonical JSON");
    assert_eq!(actual, golden_document()["canonical"]);
}

#[test]
fn canonical_v1_golden_fixture_roundtrips_all_fields() {
    let fixture = golden_document()["canonical"].to_string();
    let decoded = decode_envelope(&fixture).expect("decode canonical v1 fixture");
    assert_eq!(decoded, canonical_envelope());
}

#[test]
fn agent_envelope_matches_golden_fixture() {
    let wire = encode_agent_envelope(&agent_envelope()).expect("encode agent envelope");
    let actual: Value = serde_json::from_str(&wire).expect("encoded agent JSON");
    assert_eq!(actual, golden_agent());
}

#[test]
fn agent_v1_golden_fixture_roundtrips_all_fields() {
    let fixture = golden_agent().to_string();
    let decoded = decode_agent_envelope(&fixture).expect("decode agent v1 fixture");
    assert_eq!(decoded, agent_envelope());
}

#[test]
fn canonical_schema_matches_golden_fixture() {
    let golden: Value = serde_json::from_str(GOLDEN_SCHEMA).expect("valid schema fixture");
    assert_eq!(canonical_envelope_schema(), golden);
}

#[test]
fn agent_schema_remains_self_describing() {
    let schema = agent_envelope_schema();
    assert_eq!(schema["title"], "LeanCTX AgentEnvelopeV1");
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);
}

#[test]
fn external_consumer_can_read_canonical_wire_as_serde_value() {
    let wire = encode_envelope(&canonical_envelope()).expect("encode canonical envelope");
    let value: Value = serde_json::from_str(&wire).expect("external consumer parses JSON");
    let object = value.as_object().expect("wire envelope is an object");
    for field in [
        "schema_version",
        "context",
        "surface",
        "direction",
        "provider",
        "model",
        "token_balance",
        "route_ref",
        "policy_ref",
        "idempotency_key",
    ] {
        assert!(object.contains_key(field), "missing wire field: {field}");
    }
    assert_eq!(value["context"]["tenant_id"], "tenant-golden");
    assert_eq!(value["token_balance"]["delivered_tokens"], 876);
}

#[tokio::test]
async fn batch_validation_returns_per_item_results() {
    let items: Vec<Value> = serde_json::from_str(MIXED_BATCH).expect("batch fixture");
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        results.push(post_envelope(item.to_string(), None).await.0);
    }
    assert_eq!(
        results,
        [StatusCode::OK, StatusCode::OK, StatusCode::BAD_REQUEST]
    );
}

#[tokio::test]
async fn idempotency_key_replays_the_same_response() {
    let body = encode_envelope(&canonical_envelope()).expect("encode envelope");
    let first = post_envelope(body.clone(), Some("contract-test-key")).await;
    let second = post_envelope(body, Some("contract-test-key")).await;
    assert_eq!(first, second);
}

#[tokio::test]
async fn payload_over_256_kib_is_rejected_with_413() {
    let body = format!("{{\"padding\":\"{}\"}}", "x".repeat(256 * 1024));
    let response = ocla_router()
        .layer(DefaultBodyLimit::max(256 * 1024))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/envelope")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn budget_post_returns_the_configured_limit_schema() {
    let scope = "user:contract-budget";
    let (status, body) = json_request(
        "POST",
        "/ocla/v1/budget",
        Some(serde_json::json!({
            "scope": scope,
            "max_tokens_per_day": 12_345,
            "max_usd_per_day": 6.75
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    for field in [
        "scope",
        "max_tokens_per_day",
        "max_usd_per_day",
        "consumed_tokens",
        "consumed_usd",
    ] {
        assert!(body.get(field).is_some(), "missing budget field: {field}");
    }
    assert_eq!(body["scope"], scope);
    assert_eq!(body["max_tokens_per_day"], 12_345);
}

#[tokio::test]
async fn budget_get_returns_existing_and_missing_scope_statuses() {
    let scope = "team:contract-budget-get";
    let (post_status, _) = json_request(
        "POST",
        "/ocla/v1/budget",
        Some(serde_json::json!({
            "scope": scope,
            "max_tokens_per_day": 100,
            "max_usd_per_day": 1.25
        })),
    )
    .await;
    assert_eq!(post_status, StatusCode::OK);

    let (status, body) = json_request("GET", &format!("/ocla/v1/budget/{scope}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scope"], scope);
    assert!(body["consumed_tokens"].is_number());
    assert!(body["consumed_usd"].is_number());

    let (missing_status, missing_body) =
        json_request("GET", "/ocla/v1/budget/user:contract-budget-missing", None).await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(missing_body["error"], "budget not found");
}

#[tokio::test]
async fn dlq_get_returns_entries_and_stats_schema() {
    let (status, body) = json_request("GET", "/ocla/v1/dlq", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["dead_letters"].is_array());
    for field in ["total", "oldest_age_seconds", "by_target_agent"] {
        assert!(
            body["stats"].get(field).is_some(),
            "missing DLQ field: {field}"
        );
    }
    assert!(body["stats"]["by_target_agent"].is_object());
}

#[tokio::test]
async fn health_get_returns_aggregated_report_schema() {
    let (status, body) = json_request("GET", "/ocla/v1/health", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["overall"].is_string() || body["overall"].is_object());
    assert!(body["components"].is_array());
    assert!(body["uptime_seconds"].is_u64());
    assert_eq!(body["version"], "ocla/v1");
    assert!(
        !body["components"]
            .as_array()
            .expect("components")
            .is_empty()
    );
}

#[tokio::test]
async fn capsule_register_returns_ref() {
    let (status, body) = text_request("/ocla/v1/capsule", "contract capsule").await;

    assert_eq!(status, StatusCode::CREATED);
    let capsule_ref = body["capsule_ref"].as_str().expect("capsule reference");
    assert!(capsule_ref.starts_with("capsule:"));
    assert_eq!(capsule_ref.len(), "capsule:".len() + 64);
}

#[tokio::test]
async fn capsule_resolve_returns_data() {
    let capsule_data = "contract resolve capsule";
    let (register_status, registered) = text_request("/ocla/v1/capsule", capsule_data).await;
    assert_eq!(register_status, StatusCode::CREATED);
    let capsule_ref = registered["capsule_ref"]
        .as_str()
        .expect("capsule reference");

    let (status, resolved) =
        json_request("GET", &format!("/ocla/v1/capsule/{capsule_ref}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resolved["capsule_ref"], capsule_ref);
    assert_eq!(resolved["data"], capsule_data);
}

#[tokio::test]
async fn capsule_fork_creates_child() {
    let (register_status, registered) =
        text_request("/ocla/v1/capsule", "contract fork capsule").await;
    assert_eq!(register_status, StatusCode::CREATED);
    let parent_ref = registered["capsule_ref"]
        .as_str()
        .expect("parent reference");

    let (fork_status, forked) = json_request(
        "POST",
        &format!("/ocla/v1/capsule/{parent_ref}/fork"),
        Some(serde_json::json!({"budget_tokens": 256})),
    )
    .await;
    assert_eq!(fork_status, StatusCode::CREATED);
    let child_ref = forked["capsule_ref"].as_str().expect("child reference");
    assert!(child_ref.starts_with("capsule:"));
    assert_ne!(child_ref, parent_ref);
}

#[tokio::test]
async fn capsule_resolve_unknown_returns_404() {
    let (status, body) =
        json_request("GET", "/ocla/v1/capsule/capsule:contract-missing", None).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "capsule not found");
}

#[test]
fn legacy_v1_fixture_decodes_without_schema_migration() {
    let decoded = decode_envelope(LEGACY_ENVELOPE).expect("decode legacy v1 fixture");
    assert_eq!(
        decoded.schema_version,
        CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION
    );
    assert_eq!(decoded.context.request_id, "legacy-request-001");
    assert_eq!(decoded.idempotency_key, "legacy-request-001:output");
}

#[test]
fn streaming_frames_roundtrip_in_order() {
    let frames = [
        StreamFrame::Data(Box::new(canonical_envelope())),
        StreamFrame::Heartbeat,
        StreamFrame::Cancel,
        StreamFrame::Done,
    ];
    let encoded: Vec<String> = frames
        .iter()
        .map(|frame| encode_frame(frame).expect("encode stream frame"))
        .collect();
    let decoded: Vec<StreamFrame> = encoded
        .iter()
        .map(|line| decode_frame(line).expect("decode stream frame"))
        .collect();
    assert_eq!(decoded, frames);
}
