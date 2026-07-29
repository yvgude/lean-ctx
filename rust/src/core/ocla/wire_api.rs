//! Axum REST projection for the public OCLA wire contract.

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use super::budget::{BudgetLedger, BudgetLimit, BudgetScope};
use super::capsule::CapsuleStore;
use super::health::{SystemHealth, check_system_health};
use super::{
    CanonicalTokenEnvelopeV1, OCLA_API_VERSION, OclaCapability, OclaCapabilityKind, OclaRegistry,
};
use crate::core::a2a::dlq::{DeadLetter, DlqStats};
use crate::core::ocla::wire::decode_envelope;

/// Builds the stateless OCLA REST router for merging into an Axum application.
pub fn ocla_router() -> Router {
    Router::new()
        .route("/ocla/v1/health", get(health))
        .route("/ocla/v1/capabilities", get(capabilities))
        .route("/ocla/v1/envelope", post(envelope))
        .route("/ocla/v1/envelope/batch", post(envelope_batch))
        .route("/ocla/v1/agents", get(agents))
        .route("/ocla/v1/metrics", get(metrics))
        .route("/ocla/v1/ledger/summary", get(ledger_summary))
        .route("/ocla/v1/budget", post(set_budget))
        .route(
            "/ocla/v1/budget/{scope}",
            get(get_budget).delete(delete_budget),
        )
        .route("/ocla/v1/dlq", get(dlq))
        .route("/ocla/v1/dlq/{id}/retry", post(dlq_retry))
        .route("/ocla/v1/dlq/{id}", delete(dlq_delete))
        .route("/ocla/v1/capsule", post(capsule_register))
        .route("/ocla/v1/capsule/{ref}", get(capsule_resolve))
        .route("/ocla/v1/capsule/{ref}/fork", post(capsule_fork))
        .route("/ocla/v1/delivery/check", post(delivery_check))
        .route("/ocla/v1/delivery/record", post(delivery_record))
        .route("/ocla/v1/delivery/stats", get(delivery_stats))
}

#[derive(Default)]
struct BudgetStore {
    ledger: BudgetLedger,
    limits: HashMap<BudgetScope, BudgetLimit>,
}

static BUDGET_STORE: OnceLock<Mutex<BudgetStore>> = OnceLock::new();

fn budget_store() -> &'static Mutex<BudgetStore> {
    BUDGET_STORE.get_or_init(|| Mutex::new(BudgetStore::default()))
}

pub fn admit_budgeted_request(scope: &str, tokens: u64, usd: f64) -> Result<(), String> {
    let scope = parse_budget_scope(scope)?;
    let mut store = budget_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    store
        .ledger
        .check_budget_with_cost(&scope, tokens, usd)
        .map_err(|err| err.to_string())?;
    store.ledger.record_consumption(&scope, tokens, usd);
    Ok(())
}

#[cfg(test)]
pub(crate) fn set_test_budget_limit(limit: BudgetLimit) {
    let mut store = budget_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    store.ledger = BudgetLedger::new();
    store.limits.clear();
    store.ledger.set_limit(limit.clone());
    store.limits.insert(limit.scope.clone(), limit);
}

#[derive(Debug, Deserialize)]
struct SetBudgetRequest {
    scope: String,
    max_tokens_per_day: u64,
    max_usd_per_day: f64,
}

#[derive(Serialize)]
struct BudgetResponse {
    scope: String,
    max_tokens_per_day: u64,
    max_usd_per_day: f64,
    consumed_tokens: u64,
    consumed_usd: f64,
}

fn parse_budget_scope(raw: &str) -> Result<BudgetScope, String> {
    let (kind, name) = raw
        .split_once(':')
        .ok_or_else(|| "scope must use org:name, team:name, or user:name".to_string())?;
    if name.is_empty() || name.contains(':') {
        return Err("scope name must be non-empty and contain no ':'".to_string());
    }
    match kind {
        "org" => Ok(BudgetScope::Org(name.to_string())),
        "team" => Ok(BudgetScope::Team(name.to_string())),
        "user" => Ok(BudgetScope::User(name.to_string())),
        _ => Err("scope must use org:name, team:name, or user:name".to_string()),
    }
}

fn budget_scope_name(scope: &BudgetScope) -> String {
    match scope {
        BudgetScope::Org(name) => format!("org:{name}"),
        BudgetScope::Team(name) => format!("team:{name}"),
        BudgetScope::User(name) => format!("user:{name}"),
    }
}

fn budget_response(
    scope: &BudgetScope,
    limit: &BudgetLimit,
    ledger: &BudgetLedger,
) -> BudgetResponse {
    BudgetResponse {
        scope: budget_scope_name(scope),
        max_tokens_per_day: limit.max_tokens_per_day,
        max_usd_per_day: limit.max_usd_per_day,
        consumed_tokens: ledger.consumed_tokens(scope),
        consumed_usd: ledger.consumed_usd(scope),
    }
}

async fn set_budget(
    Json(request): Json<SetBudgetRequest>,
) -> Result<Json<BudgetResponse>, (StatusCode, Json<Value>)> {
    if !request.max_usd_per_day.is_finite() || request.max_usd_per_day < 0.0 {
        return Err(invalid_request(
            "max_usd_per_day must be finite and non-negative",
        ));
    }
    let scope = parse_budget_scope(&request.scope).map_err(invalid_request)?;
    let limit = BudgetLimit {
        scope: scope.clone(),
        max_tokens_per_day: request.max_tokens_per_day,
        max_usd_per_day: request.max_usd_per_day,
    };
    let mut store = budget_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    store.ledger.set_limit(limit.clone());
    store.limits.insert(scope.clone(), limit.clone());
    Ok(Json(budget_response(&scope, &limit, &store.ledger)))
}

async fn get_budget(
    Path(raw_scope): Path<String>,
) -> Result<Json<BudgetResponse>, (StatusCode, Json<Value>)> {
    let scope = parse_budget_scope(&raw_scope).map_err(invalid_request)?;
    let store = budget_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(limit) = store.limits.get(&scope) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "budget not found"})),
        ));
    };
    Ok(Json(budget_response(&scope, limit, &store.ledger)))
}

async fn delete_budget(
    Path(raw_scope): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let scope = parse_budget_scope(&raw_scope).map_err(invalid_request)?;
    let mut store = budget_store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if store.limits.remove(&scope).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "budget not found"})),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn health() -> Json<SystemHealth> {
    Json(check_system_health())
}

#[derive(Serialize)]
struct CapabilitiesResponse {
    version: &'static str,
    capabilities: Vec<OclaCapability>,
}

async fn capabilities() -> Json<CapabilitiesResponse> {
    let registry = OclaRegistry::global();
    let capabilities = vec![
        registry.observation_hook.capability(),
        registry.usage_sink.capability(),
        registry.metrics_exporter.capability(),
        registry.savings_ledger.capability(),
        registry.intent_classifier.capability(),
        registry.outcome_tracker.capability(),
        registry.compression_provider.capability(),
        registry.response_optimizer.capability(),
        registry.model_router.capability(),
        registry.efficiency_analyzer.capability(),
        registry.config_tuner.capability(),
        registry.experiment_runner.capability(),
        registry.connector_scheduler.capability(),
        registry.agent_gateway.capability(),
        registry.delivery_registry.capability(),
    ];
    debug_assert_eq!(capabilities.len(), OclaCapabilityKind::ALL.len());

    Json(CapabilitiesResponse {
        version: OCLA_API_VERSION,
        capabilities,
    })
}

async fn envelope(
    body: String,
) -> Result<Json<CanonicalTokenEnvelopeV1>, (StatusCode, Json<Value>)> {
    decode_envelope(&body).map(Json).map_err(invalid_request)
}

#[derive(Serialize)]
struct BatchEnvelopeResult {
    valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    envelope: Option<CanonicalTokenEnvelopeV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn envelope_batch(Json(envelopes): Json<Vec<Value>>) -> Json<Vec<BatchEnvelopeResult>> {
    let results = envelopes
        .into_iter()
        .map(|envelope| match serde_json::to_string(&envelope) {
            Ok(json) => match decode_envelope(&json) {
                Ok(envelope) => BatchEnvelopeResult {
                    valid: true,
                    envelope: Some(envelope),
                    error: None,
                },
                Err(error) => BatchEnvelopeResult {
                    valid: false,
                    envelope: None,
                    error: Some(error.to_string()),
                },
            },
            Err(error) => BatchEnvelopeResult {
                valid: false,
                envelope: None,
                error: Some(error.to_string()),
            },
        })
        .collect();
    Json(results)
}

#[derive(Serialize)]
struct AgentsResponse {
    agents: Vec<Value>,
}

async fn agents() -> Json<AgentsResponse> {
    Json(AgentsResponse { agents: Vec::new() })
}

#[derive(Serialize)]
struct MetricsResponse {
    total_events: usize,
    saved_tokens: u64,
    saved_usd: f64,
    trait_adoption_count: usize,
}

async fn metrics() -> Json<MetricsResponse> {
    let summary = crate::core::savings_ledger::summary();
    Json(MetricsResponse {
        total_events: summary.total_events,
        saved_tokens: summary.saved_tokens,
        saved_usd: summary.saved_usd,
        trait_adoption_count: OclaCapabilityKind::ALL.len(),
    })
}

#[derive(Serialize)]
struct LedgerSummaryResponse {
    events: usize,
    tokens: u64,
    usd: f64,
}

async fn ledger_summary() -> Json<LedgerSummaryResponse> {
    let summary = crate::core::savings_ledger::summary();
    Json(LedgerSummaryResponse {
        events: summary.total_events,
        tokens: summary.saved_tokens,
        usd: summary.saved_usd,
    })
}

#[derive(Serialize)]
struct DlqResponse {
    dead_letters: Vec<DeadLetter>,
    stats: DlqStats,
}

async fn dlq() -> Json<DlqResponse> {
    let queue = super::health::dead_letter_queue();
    Json(DlqResponse {
        dead_letters: queue.peek_all(),
        stats: queue.stats(),
    })
}

async fn dlq_retry(Path(id): Path<String>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::health::dead_letter_queue()
        .retry(&id)
        .map(|()| Json(json!({"id": id, "retried": true})))
        .map_err(invalid_request)
}

async fn dlq_delete(Path(id): Path<String>) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    if super::health::dead_letter_queue().dequeue(&id).is_some() {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("dead letter not found: {id}")})),
        ))
    }
}

fn invalid_request(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error.to_string()})),
    )
}

static CAPSULE_STORE: OnceLock<CapsuleStore> = OnceLock::new();

fn capsule_store() -> &'static CapsuleStore {
    CAPSULE_STORE.get_or_init(CapsuleStore::new)
}

async fn capsule_register(body: String) -> (StatusCode, Json<Value>) {
    let capsule_ref = capsule_store().register(body.as_bytes());
    (
        StatusCode::CREATED,
        Json(json!({"capsule_ref": capsule_ref})),
    )
}

async fn capsule_resolve(Path(capsule_ref): Path<String>) -> (StatusCode, Json<Value>) {
    match capsule_store().resolve(&capsule_ref) {
        Ok(data) => {
            let text = String::from_utf8_lossy(&data);
            (
                StatusCode::OK,
                Json(json!({"capsule_ref": capsule_ref, "data": text})),
            )
        }
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "capsule not found"})),
        ),
    }
}

#[derive(Deserialize)]
struct ForkRequest {
    budget_tokens: u64,
}

async fn capsule_fork(
    Path(capsule_ref): Path<String>,
    Json(req): Json<ForkRequest>,
) -> (StatusCode, Json<Value>) {
    match capsule_store().fork(&capsule_ref, req.budget_tokens) {
        Ok(child_ref) => (StatusCode::CREATED, Json(json!({"capsule_ref": child_ref}))),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "parent capsule not found"})),
        ),
    }
}

#[derive(Deserialize)]
struct DeliveryCheckRequest {
    blake3: [u8; 12],
    mtime: u64,
    path: String,
    requester_agent_id: Option<String>,
    requester_conversation_id: Option<String>,
}

async fn delivery_check(Json(req): Json<DeliveryCheckRequest>) -> (StatusCode, Json<Value>) {
    let reg = OclaRegistry::global();
    match reg.delivery_registry.check_delivery(
        &req.blake3,
        req.mtime,
        &req.path,
        req.requester_agent_id.as_deref(),
        req.requester_conversation_id.as_deref(),
    ) {
        Some(record) => (
            StatusCode::OK,
            Json(json!({
                "hit": true,
                "path": record.path,
                "line_count": record.line_count,
                "token_count": record.token_count,
                "agent_id": record.agent_id,
                "conversation_id": record.conversation_id,
                "read_at": record.read_at,
                "fresh": record.fresh,
            })),
        ),
        None => (StatusCode::OK, Json(json!({"hit": false}))),
    }
}

async fn delivery_record(Json(entry): Json<crate::core::ocla::types::DeliveryEntry>) -> StatusCode {
    let reg = OclaRegistry::global();
    reg.delivery_registry.record_delivery(entry);
    StatusCode::NO_CONTENT
}

async fn delivery_stats() -> Json<Value> {
    let reg = OclaRegistry::global();
    let stats = reg.delivery_registry.delivery_stats();
    Json(json!({
        "total_entries": stats.total_entries,
        "stubs_served": stats.stubs_served,
        "tokens_saved": stats.tokens_saved,
        "unique_paths": stats.unique_paths,
        "unique_agents": stats.unique_agents,
    }))
}

#[cfg(test)]
mod tests {
    use super::{CanonicalTokenEnvelopeV1, OCLA_API_VERSION, OclaCapabilityKind, ocla_router};
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
        assert_eq!(body["components"].as_array().expect("components").len(), 21);
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
        assert_eq!(json_response(response).await, json!({"agents": []}));
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
        assert_eq!(body["trait_adoption_count"], 15);
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
        let response = ocla_router()
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
        let retry = ocla_router()
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

        let delete = ocla_router()
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
        let body =
            json!({"blake3": [0,0,0,0,0,0,0,0,0,0,0,0], "mtime": 1000, "path": "missing.rs"});
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
        assert_eq!(record_resp.status(), StatusCode::NO_CONTENT);

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
}
