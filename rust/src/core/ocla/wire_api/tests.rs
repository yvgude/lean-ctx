// SPDX-License-Identifier: Apache-2.0

#[tokio::test]
async fn legacy_delivery_cannot_write_scoped_records() {
    let body = serde_json::json!({
        "blake3":vec![31;12], "path":"scoped.rs", "line_count":1,
        "token_count":10,"agent_id":"forged-owner","conversation_id":"c","mtime":1,
        "access":{"scope":{"account_id":"victim","project_id":"project"},"privacy":"project"}
    });
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delivery_checks_reject_missing_or_empty_paths() {
    for path in [None, Some(""), Some(" "), Some("bad\0path")] {
        let mut check = serde_json::json!({"blake3":vec![0;12], "mtime":1});
        if let Some(path) = path {
            check["path"] = serde_json::json!(path);
        }
        for endpoint in [
            "/ocla/v1/delivery/check",
            "/ocla/v1/delivery/batch-check",
            "/v1/delivery/batch-check",
        ] {
            let body = if endpoint.ends_with("batch-check") {
                serde_json::json!({"checks":[check.clone()]})
            } else {
                check.clone()
            };
            let response = ocla_router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(endpoint)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
    }
}

#[test]
fn delivery_lookup_does_not_count_unserved_savings() {
    use crate::core::ocla::builtin::delivery_registry::BuiltinDeliveryRegistry;
    use crate::core::ocla::traits::DeliveryRegistry;
    let registry = BuiltinDeliveryRegistry::with_config(8, 10);
    registry.record_delivery(crate::core::ocla::types::DeliveryEntry {
        access: None,
        blake3: [42; 12],
        path: "src/lookup-only.rs".into(),
        line_count: 10,
        token_count: 100,
        agent_id: "original".into(),
        conversation_id: "original-conversation".into(),
        mtime: 1,
        relay_content: None,
        relay_mode: None,
    });
    for _ in 0..3 {
        let (status, axum::Json(value)) = super::delivery_check_with_registry(
            &registry,
            &super::DeliveryCheckRequest {
                blake3: [42; 12],
                mtime: 1,
                path: "src/lookup-only.rs".into(),
                requester_agent_id: Some("requester".into()),
                requester_conversation_id: Some("requester-conversation".into()),
            },
        );
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(value["hit"], true);
    }
    assert_eq!(registry.delivery_stats().stubs_served, 0);
    assert_eq!(registry.delivery_stats().tokens_saved, 0);
    let record = registry
        .check_delivery(&[42; 12], 1, "src/lookup-only.rs", Some("requester"), None)
        .unwrap();
    registry.record_stub_served(&record, 10);
    assert_eq!(registry.delivery_stats().stubs_served, 1);
    assert_eq!(registry.delivery_stats().tokens_saved, 90);
}

use super::{
    CanonicalTokenEnvelopeV1, DlqScope, OCLA_API_VERSION, OclaCapabilityKind, ocla_router,
    ocla_router_with_dlq,
};
use axum::body::Body;
use axum::body::to_bytes;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

fn request_context() -> super::super::OclaRequestContext {
    super::super::OclaRequestContext {
        request_id: "request-1".into(),
        session_id: "session-1".into(),
        agent_id: "agent-1".into(),
        content_ref: "blake3:content".into(),
        tenant_id: None,
        trace_id: "trace-1".into(),
        task_id: None,
        parent_task_id: None,
    }
}

fn valid_envelope() -> CanonicalTokenEnvelopeV1 {
    CanonicalTokenEnvelopeV1 {
        schema_version: super::super::CANONICAL_TOKEN_ENVELOPE_SCHEMA_VERSION,
        context: request_context(),
        surface: super::super::TokenEnvelopeSurface::Proxy,
        direction: super::super::TokenFlowDirection::Input,
        provider: "openai".into(),
        model: "gpt-5".into(),
        token_balance: super::super::TokenBalanceV1 {
            original_tokens: 100,
            materialized_tokens: 80,
            delivered_tokens: 60,
            provider_billed_tokens: 60,
        },
        route_ref: Some("route-1".into()),
        policy_ref: None,
        idempotency_key: "request-1:input".into(),
    }
}

async fn json_response(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("response body");
    serde_json::from_slice(&body).expect("JSON response")
}

fn budget_request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .expect("request")
}

async fn set_budget_for_test(scope: &str, tokens: u64, usd: f64) {
    ocla_router()
        .oneshot(budget_request(
            "POST",
            "/ocla/v1/budget",
            Some(json!({"scope": scope, "max_tokens_per_day": tokens, "max_usd_per_day": usd})),
        ))
        .await
        .expect("response");
}

#[tokio::test]
async fn health_endpoint_returns_full_report() {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["version"], OCLA_API_VERSION);
    assert_eq!(
        body["components"].as_array().expect("components").len(),
        super::super::types::OclaCapabilityKind::ALL.len()
            + 7
            + super::super::registry::OclaRegistry::global()
                .adapters
                .len()
    );
    assert!(body.get("overall").is_some());
    assert!(body.get("uptime_seconds").is_some());
}

#[tokio::test]
async fn capabilities_endpoint_lists_all_statuses() {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/capabilities")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["version"], OCLA_API_VERSION);
    assert_eq!(
        body["capabilities"].as_array().expect("list").len(),
        OclaCapabilityKind::ALL.len()
    );
    assert!(
        body["capabilities"]
            .as_array()
            .expect("list")
            .iter()
            .all(|capability| capability["status"] == "available")
    );
}

#[tokio::test]
async fn envelope_endpoint_decodes_valid_json_and_rejects_invalid_json() {
    let wire = serde_json::to_string(&valid_envelope()).expect("envelope JSON");
    let response = ocla_router()
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/envelope")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(wire))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_response(response).await, json!(valid_envelope()));

    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/envelope")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"schema_version":99}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ledger_summary_endpoint_returns_events_tokens_and_usd() {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/ledger/summary")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert!(body.get("events").is_some());
    assert!(body.get("tokens").is_some());
    assert!(body.get("usd").is_some());
}

#[tokio::test]
#[serial_test::serial]
async fn agents_endpoint_returns_registered_agents_schema() {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/agents")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(json_response(response).await.is_array());
}

#[tokio::test]
#[serial_test::serial]
async fn agents_endpoint_returns_server_error_for_corrupt_identity_registry() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    std::fs::write(agents_dir.join("identity-registry.json"), b"{not json")
        .expect("corrupt identity registry");

    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/agents")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_response(response).await;
    assert_eq!(body["error"], "agent registry unavailable");
}

#[tokio::test]
async fn metrics_endpoint_returns_key_ocla_metrics() {
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/ocla/v1/metrics")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert!(body.get("total_events").is_some());
    assert!(body.get("saved_tokens").is_some());
    assert!(body.get("saved_usd").is_some());
    assert_eq!(body["trait_adoption_count"], 14);
}

#[tokio::test]
async fn envelope_batch_endpoint_reports_valid_and_invalid_items() {
    let body = json!([valid_envelope(), {"schema_version": 99}]);
    let response = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/envelope/batch")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let results = json_response(response).await;
    assert_eq!(results.as_array().expect("results").len(), 2);
    assert_eq!(results[0]["valid"], true);
    assert_eq!(results[0]["envelope"], json!(valid_envelope()));
    assert_eq!(results[1]["valid"], false);
    assert!(results[1].get("error").is_some());
}

#[tokio::test]
async fn budget_post_endpoint_sets_and_returns_limit() {
    let response = ocla_router()
        .oneshot(budget_request(
            "POST",
            "/ocla/v1/budget",
            Some(json!({
                "scope": "org:wire-api-set",
                "max_tokens_per_day": 100_000,
                "max_usd_per_day": 50.0,
            })),
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["max_tokens_per_day"], 100_000);
}

#[tokio::test]
async fn dlq_endpoint_returns_entries_and_stats() {
    let response =
        ocla_router_with_dlq(DlqScope::new("tenant-a", "project-a").expect("valid scope"))
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ocla/v1/dlq")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert!(body.get("dead_letters").is_some());
    assert!(body.get("stats").is_some());
}

#[tokio::test]
async fn dlq_endpoint_is_fail_closed_and_scope_isolated() {
    use crate::core::a2a::dlq::{DeadLetter, DeadLetterDelivery};

    let failed_at = "2026-01-01T00:00:00Z".to_string();
    for (id, tenant_id) in [
        ("wire-dlq-scope-a", "tenant-wire-a"),
        ("wire-dlq-scope-b", "tenant-wire-b"),
    ] {
        super::super::health::dead_letter_queue()
            .enqueue(DeadLetter {
                id: id.into(),
                peer_id: "legacy".into(),
                delivery_id: id.into(),
                tenant_id: tenant_id.into(),
                project_id: "project-wire".into(),
                delivery: DeadLetterDelivery::LocalAgentBus,
                original_message: "message".into(),
                target_agent: "agent-wire".into(),
                error: "failed".into(),
                attempts: 1,
                first_failed_at: failed_at.clone(),
                last_failed_at: failed_at.clone(),
            })
            .unwrap();
    }

    let missing_scope = ocla_router()
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ocla/v1/dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_scope.status(), StatusCode::NOT_FOUND);

    let scoped =
        ocla_router_with_dlq(DlqScope::new("tenant-wire-a", "project-wire").expect("valid scope"))
            .oneshot(
                Request::builder()
                    .uri("/ocla/v1/dlq")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    let body = json_response(scoped).await;
    let ids: Vec<_> = body["dead_letters"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert!(ids.contains(&"wire-dlq-scope-a"));
    assert!(!ids.contains(&"wire-dlq-scope-b"));
}
#[tokio::test]
async fn budget_get_endpoint_returns_configured_limit_and_consumption() {
    set_budget_for_test("team:wire-api-get", 500, 5.0).await;

    let response = ocla_router()
        .oneshot(budget_request(
            "GET",
            "/ocla/v1/budget/team:wire-api-get",
            None,
        ))
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["max_tokens_per_day"], 500);
    assert_eq!(body["max_usd_per_day"], 5.0);
}

#[tokio::test]
async fn budget_delete_endpoint_removes_limit() {
    set_budget_for_test("user:wire-api-delete", 25, 1.0).await;

    let response = ocla_router()
        .oneshot(budget_request(
            "DELETE",
            "/ocla/v1/budget/user:wire-api-delete",
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = ocla_router()
        .oneshot(budget_request(
            "GET",
            "/ocla/v1/budget/user:wire-api-delete",
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dlq_retry_and_delete_return_not_found_for_missing_id() {
    let app = ocla_router_with_dlq(DlqScope::new("tenant-a", "project-a").expect("valid scope"));
    let retry = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/dlq/missing/retry")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(retry.status(), StatusCode::BAD_REQUEST);

    let delete = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/ocla/v1/dlq/missing")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(delete.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delivery_check_miss_returns_no_hit() {
    let app = ocla_router();
    let body = json!({"blake3": [0,0,0,0,0,0,0,0,0,0,0,0], "mtime": 1000, "path": "missing.rs"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["hit"], false);
}

#[tokio::test]
async fn delivery_record_then_check_returns_hit() {
    let app = ocla_router();

    let entry = json!({
        "blake3": [1,2,3,4,5,6,7,8,9,10,11,12],
        "path": "src/test.rs",
        "line_count": 42,
        "token_count": 168,
        "agent_id": "agent-x",
        "conversation_id": "conv-x",
        "mtime": 2000
    });
    let record_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&entry).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(record_resp.status(), StatusCode::OK);
    assert_eq!(
        json_response(record_resp).await,
        json!({
            "already_recorded": false,
            "updated": false,
        })
    );

    let check_body =
        json!({"blake3": [1,2,3,4,5,6,7,8,9,10,11,12], "mtime": 2000, "path": "src/test.rs"});
    let check_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&check_body).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(check_resp.status(), StatusCode::OK);
    let bytes = to_bytes(check_resp.into_body(), usize::MAX).await.unwrap();
    let val: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["hit"], true);
    assert_eq!(val["path"], "src/test.rs");
    assert_eq!(val["agent_id"], "agent-x");
}

#[tokio::test]
async fn delivery_batch_check_returns_hits_and_misses_in_order() {
    let app = ocla_router();
    let entry = json!({
        "blake3": [91,2,3,4,5,6,7,8,9,10,11,12],
        "path": "src/batch.rs",
        "line_count": 42,
        "token_count": 168,
        "agent_id": "batch-agent",
        "conversation_id": "batch-conversation",
        "mtime": 2000
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(entry.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let checks = json!({"checks": [
        {"blake3": [91,2,3,4,5,6,7,8,9,10,11,12], "mtime": 2000, "path": "src/batch.rs"},
        {"blake3": [92,2,3,4,5,6,7,8,9,10,11,12], "mtime": 2000, "path": "src/missing.rs"}
    ]});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/delivery/batch-check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(checks.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_response(response).await;
    assert_eq!(body["results"][0]["hit"], true);
    assert_eq!(body["results"][0]["record"]["path"], "src/batch.rs");
    assert_eq!(body["results"][1], json!({"hit": false, "record": null}));
}

#[tokio::test]
async fn delivery_record_reports_idempotent_and_updated_results() {
    let app = ocla_router();
    let entry = json!({
        "blake3": [93,2,3,4,5,6,7,8,9,10,11,12],
        "path": "src/idempotent-wire.rs",
        "line_count": 42,
        "token_count": 168,
        "agent_id": "wire-agent",
        "conversation_id": "wire-conversation",
        "mtime": 2000
    });
    let request = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/ocla/v1/delivery/record")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    };

    let first = app.clone().oneshot(request(entry.clone())).await.unwrap();
    assert_eq!(
        json_response(first).await,
        json!({"already_recorded": false, "updated": false})
    );
    let duplicate = app.clone().oneshot(request(entry.clone())).await.unwrap();
    assert_eq!(
        json_response(duplicate).await,
        json!({"already_recorded": true, "updated": false})
    );
    let mut updated = entry;
    updated["mtime"] = json!(3000);
    let changed = app.oneshot(request(updated)).await.unwrap();
    assert_eq!(
        json_response(changed).await,
        json!({"already_recorded": false, "updated": true})
    );
}

#[tokio::test]
async fn delivery_stats_returns_counts() {
    let app = ocla_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/ocla/v1/delivery/stats")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(val["total_entries"].is_number());
    assert!(val["stubs_served"].is_number());
}

// ── Generalized cross-agent cache endpoint tests ──────────────────

fn cache_entry_fixture(key_str: &str) -> Value {
    json!({
        "schema_version": 2,
        "key": key_str,
        "kind": "shell_command",
        "validator": "immutable",
        "handle": {
            "algorithm": "blake3",
            "digest": "d".repeat(64),
            "byte_len": 100,
            "media_type": "text/plain"
        },
        "display_path": "cargo test",
        "line_count": 50,
        "token_count": 2000,
        "producer": {
            "agent_id": "agent-A",
            "conversation_id": "conv-A",
            "host": "cursor"
        },
        "created_at_epoch_ms": 1000000,
        "expires_at_epoch_ms": 9_999_999_999_999_u64
    })
}

async fn cache_validator_request(endpoint: &str, body: Value) -> axum::response::Response {
    ocla_router()
        .oneshot(budget_request("POST", endpoint, Some(body)))
        .await
        .expect("cache response")
}

fn cache_validator_check(key: &str, validator: &str) -> Value {
    json!({
        "key": key,
        "validator": validator,
        "requester_agent_id": "agent-B",
        "requester_conversation_id": "conv-A"
    })
}

async fn assert_cache_validator_rejections(kind: &str, stored: Value, valid: &str) {
    let malformed = [
        "",
        "unknown",
        "Immutable",
        "file:",
        "file:invalid",
        "directory:-1",
        "file:340282366920938463463374607431768211456",
        "directory:340282366920938463463374607431768211456",
    ];
    let key = format!("cache:v1:shell_command:validator_rejection_{kind}");
    let mut entry = cache_entry_fixture(&key);
    entry["validator"] = stored;
    let recorded = cache_validator_request("/ocla/v1/cache/record", entry.clone()).await;
    assert_eq!(recorded.status(), StatusCode::NO_CONTENT);
    for invalid in malformed {
        for endpoint in ["/ocla/v1/cache/check", "/ocla/v1/cache/batch-check"] {
            let check = cache_validator_check(&key, invalid);
            let request = if endpoint.ends_with("batch-check") {
                json!({"checks": [check]})
            } else {
                check
            };
            let rejected = cache_validator_request(endpoint, request).await;
            let recovered =
                cache_validator_request("/ocla/v1/cache/check", cache_validator_check(&key, valid))
                    .await;
            assert_eq!(recovered.status(), StatusCode::OK);
            let recovered = json_response(recovered).await;
            assert_eq!(recovered["hit"], true, "invalid request evicted {kind}");
            assert_eq!(recovered["entry"], entry);
            assert_eq!(
                rejected.status(),
                StatusCode::BAD_REQUEST,
                "{endpoint}: {invalid}"
            );
            let error = json_response(rejected).await;
            assert!(error["error"].is_string());
            assert!(error.get("hit").is_none());
        }
    }
    let valid_batch = cache_validator_request(
        "/ocla/v1/cache/batch-check",
        json!({"checks": [cache_validator_check(&key, valid)]}),
    )
    .await;
    assert_eq!(valid_batch.status(), StatusCode::OK);
    assert_eq!(
        json_response(valid_batch).await,
        json!([{"hit": true, "entry": entry}])
    );
}

#[tokio::test]
async fn cache_validators_reject_malformed_immutable_requests() {
    assert_cache_validator_rejections("immutable", json!("immutable"), "immutable").await;
}

#[tokio::test]
async fn cache_validators_preserve_file_entries_after_invalid_requests() {
    assert_cache_validator_rejections("file", json!({"file": {"mtime_ns": 42}}), "file:42").await;
}

#[tokio::test]
async fn cache_validators_preserve_directory_entries_after_invalid_requests() {
    assert_cache_validator_rejections(
        "directory",
        json!({"directory": {"mtime_ns": 42}}),
        "directory:42",
    )
    .await;
}

#[tokio::test]
async fn cache_validators_validate_entire_batch_before_any_lookup() {
    let key = "cache:v1:shell_command:validator_batch_atomicity";
    let entry = cache_entry_fixture(key);
    let recorded = cache_validator_request("/ocla/v1/cache/record", entry.clone()).await;
    assert_eq!(recorded.status(), StatusCode::NO_CONTENT);
    let rejected = cache_validator_request(
        "/ocla/v1/cache/batch-check",
        json!({"checks": [
            cache_validator_check(key, "file:42"),
            cache_validator_check(key, "invalid")
        ]}),
    )
    .await;
    // A valid but stale first lookup would evict the immutable entry; the
    // malformed second validator must reject the whole request before that.
    let recovered = cache_validator_request(
        "/ocla/v1/cache/check",
        cache_validator_check(key, "immutable"),
    )
    .await;
    assert_eq!(recovered.status(), StatusCode::OK);
    let recovered = json_response(recovered).await;
    assert_eq!(
        recovered["hit"], true,
        "partially validated batch evicted the entry"
    );
    assert_eq!(recovered["entry"], entry);
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(json_response(rejected).await["error"].is_string());
}

#[tokio::test]
async fn cache_check_miss_returns_no_hit() {
    let app = ocla_router();
    let body = json!({"key": "cache:v1:test:unknown", "validator": "immutable"});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let val = json_response(resp).await;
    assert_eq!(val["hit"], false);
}

#[tokio::test]
async fn cache_check_withholds_hit_across_conversations() {
    if crate::test_env::run_with_conversation_scope(
        "core::ocla::wire_api::tests::cache_check_withholds_hit_across_conversations",
        true,
    ) {
        return;
    }
    let key_str = "cache:v1:shell_command:test_record_check";
    let entry = cache_entry_fixture(key_str);

    let record_resp = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&entry).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(record_resp.status(), StatusCode::NO_CONTENT);

    let check_body = json!({
        "key": key_str,
        "validator": "immutable",
        "requester_agent_id": "agent-B",
        "requester_conversation_id": "conv-B"
    });
    let check_resp = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&check_body).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(check_resp.status(), StatusCode::OK);
    let val = json_response(check_resp).await;
    assert_eq!(
        val["hit"], false,
        "a new conversation must not receive a cross-agent stub (#1478)"
    );
}

#[tokio::test]
async fn cache_check_returns_hit_for_same_conversation_different_agent() {
    let key_str = "cache:v1:shell_command:test_same_conv_cross_agent";
    let entry = cache_entry_fixture(key_str);

    ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&entry).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");

    let check_body = json!({
        "key": key_str,
        "validator": "immutable",
        "requester_agent_id": "agent-B",
        "requester_conversation_id": "conv-A"
    });
    let check_resp = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&check_body).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(check_resp.status(), StatusCode::OK);
    let val = json_response(check_resp).await;
    assert_eq!(val["hit"], true);
    assert_eq!(val["entry"]["token_count"], 2000);
    assert_eq!(val["entry"]["producer"]["agent_id"], "agent-A");
}

#[tokio::test]
async fn cache_check_excludes_same_agent_same_conversation() {
    let key_str = "cache:v1:shell_command:test_self_exclude";
    let entry = cache_entry_fixture(key_str);

    ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/record")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&entry).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");

    let self_check = json!({
        "key": key_str,
        "validator": "immutable",
        "requester_agent_id": "agent-A",
        "requester_conversation_id": "conv-A"
    });
    let resp = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&self_check).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    let val = json_response(resp).await;
    assert_eq!(
        val["hit"], false,
        "same agent+conversation must be excluded"
    );
}

#[tokio::test]
async fn cache_batch_check_returns_mixed_results() {
    if crate::test_env::run_with_conversation_scope(
        "core::ocla::wire_api::tests::cache_batch_check_returns_mixed_results",
        true,
    ) {
        return;
    }
    let key_a = "cache:v1:shell_command:batch_a";
    let key_b = "cache:v1:shell_command:batch_b";

    for key in [key_a, key_b] {
        let entry = cache_entry_fixture(key);
        ocla_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ocla/v1/cache/record")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&entry).unwrap()))
                    .expect("request"),
            )
            .await
            .expect("response");
    }

    let batch = json!({
        "checks": [
            {"key": key_a, "validator": "immutable", "requester_agent_id": "agent-B", "requester_conversation_id": "conv-A"},
            {"key": "cache:v1:shell_command:nonexistent", "validator": "immutable"},
            {"key": key_b, "validator": "immutable", "requester_agent_id": "agent-B", "requester_conversation_id": "conv-B"}
        ]
    });
    let resp = ocla_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ocla/v1/cache/batch-check")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&batch).unwrap()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let results: Vec<Value> =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["hit"], true, "same conversation should hit");
    assert_eq!(results[1]["hit"], false, "nonexistent should miss");
    assert_eq!(
        results[2]["hit"], false,
        "different conversation must not receive a stub (#1478)"
    );
}
