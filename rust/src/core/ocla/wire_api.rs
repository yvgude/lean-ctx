//! Axum REST projection for the public OCLA wire contract.

use axum::{
    Json, Router,
    extract::{Extension, Path},
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
use crate::core::a2a::dlq::{DeadLetter, DeadLetterDelivery, DlqScope, DlqStats};
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
        .route("/ocla/v1/capsule", post(capsule_register))
        .route("/ocla/v1/capsule/{ref}", get(capsule_resolve))
        .route("/ocla/v1/capsule/{ref}/fork", post(capsule_fork))
        .route("/ocla/v1/delivery/check", post(delivery_check))
        .route("/ocla/v1/delivery/batch-check", post(delivery_batch_check))
        .route("/v1/delivery/batch-check", post(delivery_batch_check))
        .route("/ocla/v1/delivery/record", post(delivery_record))
        .route("/ocla/v1/delivery/stats", get(delivery_stats))
        .route("/ocla/v1/cache/check", post(cache_check))
        .route("/ocla/v1/cache/record", post(cache_record))
        .route("/ocla/v1/cache/batch-check", post(cache_batch_check))
}

/// Adds the privileged DLQ administration surface for one authenticated scope.
///
/// The scope is supplied by trusted server configuration, never by request data.
pub fn ocla_router_with_dlq(scope: DlqScope) -> Router {
    ocla_router().merge(
        Router::new()
            .route("/ocla/v1/dlq", get(dlq))
            .route("/ocla/v1/dlq/{id}/retry", post(dlq_retry))
            .route("/ocla/v1/dlq/{id}", delete(dlq_delete))
            .layer(Extension(scope)),
    )
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

async fn health() -> Result<Json<SystemHealth>, (StatusCode, Json<Value>)> {
    tokio::task::spawn_blocking(check_system_health)
        .await
        .map(Json)
        .map_err(service_unavailable)
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

async fn agents() -> Result<Json<serde_json::Value>, (StatusCode, Json<Value>)> {
    match crate::core::agents::list_unified() {
        Ok(unified) => Ok(Json(serde_json::to_value(unified).unwrap_or_default())),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "agent registry unavailable" })),
        )),
    }
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

async fn dlq(
    Extension(scope): Extension<DlqScope>,
) -> Result<Json<DlqResponse>, (StatusCode, Json<Value>)> {
    let queue = super::health::dead_letter_queue().clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        Ok::<_, super::OclaError>((queue.peek(&scope)?, queue.stats(Some(&scope))?))
    })
    .await
    .map_err(service_unavailable)?
    .map_err(service_unavailable)?;
    Ok(Json(DlqResponse {
        dead_letters: snapshot.0,
        stats: snapshot.1,
    }))
}

async fn dlq_retry(
    Path(id): Path<String>,
    Extension(scope): Extension<DlqScope>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let queue = super::health::dead_letter_queue().clone();
    let lock_queue = queue.clone();
    let lock_scope = scope.clone();
    let lock_id = id.clone();
    let _retry_guard =
        tokio::task::spawn_blocking(move || lock_queue.lock_retry(&lock_scope, &lock_id))
            .await
            .map_err(service_unavailable)?
            .map_err(service_unavailable)?;
    let lookup_scope = scope.clone();
    let lookup_id = id.clone();
    let letter = tokio::task::spawn_blocking(move || queue.get(&lookup_scope, &lookup_id))
        .await
        .map_err(service_unavailable)?
        .map_err(service_unavailable)?
        .ok_or_else(|| invalid_request(format!("dead letter not found: {id}")))?;

    match letter.delivery {
        DeadLetterDelivery::LocalAgentBus => {
            let queue = super::health::dead_letter_queue().clone();
            let retry_scope = scope.clone();
            let retry_id = id.clone();
            tokio::task::spawn_blocking(move || queue.retry(&retry_scope, &retry_id))
                .await
                .map_err(service_unavailable)?
                .map_err(invalid_request)?;
        }
        DeadLetterDelivery::RemoteHttp { .. } => {
            let transport = tokio::task::spawn_blocking(
                super::builtin::agent_gateway::BuiltinAgentGateway::load_remote_transport,
            )
            .await
            .map_err(service_unavailable)?
            .ok_or_else(|| invalid_request("no authenticated remote transport configured"))?;
            transport
                .retry_dead_letter(&letter)
                .await
                .map_err(invalid_request)?;
            let queue = super::health::dead_letter_queue().clone();
            let delete_scope = scope.clone();
            let delete_id = id.clone();
            tokio::task::spawn_blocking(move || queue.dequeue(&delete_scope, &delete_id))
                .await
                .map_err(service_unavailable)?
                .map_err(service_unavailable)?;
        }
    }
    Ok(Json(json!({"id": id, "retried": true})))
}

async fn dlq_delete(
    Path(id): Path<String>,
    Extension(scope): Extension<DlqScope>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let queue = super::health::dead_letter_queue().clone();
    let delete_id = id.clone();
    if tokio::task::spawn_blocking(move || queue.dequeue(&scope, &delete_id))
        .await
        .map_err(service_unavailable)?
        .map_err(service_unavailable)?
        .is_some()
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("dead letter not found: {id}")})),
        ))
    }
}

fn service_unavailable(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    tracing::warn!(error = %error, "DLQ store unavailable");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "dead letter store unavailable"})),
    )
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
    #[serde(deserialize_with = "deserialize_delivery_path")]
    path: String,
    requester_agent_id: Option<String>,
    requester_conversation_id: Option<String>,
}

fn deserialize_delivery_path<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let path = String::deserialize(deserializer)?;
    if path.trim().is_empty() || path.len() > 4096 || path.contains('\0') {
        return Err(serde::de::Error::custom(
            "a bounded delivery path is required",
        ));
    }
    Ok(path)
}

#[derive(Deserialize)]
struct DeliveryBatchCheckRequest {
    checks: Vec<DeliveryCheckRequest>,
}

#[derive(Serialize)]
struct DeliveryBatchCheckResult {
    hit: bool,
    record: Option<crate::core::ocla::types::DeliveryRecord>,
}

#[derive(Serialize)]
struct DeliveryBatchCheckResponse {
    results: Vec<DeliveryBatchCheckResult>,
}

async fn delivery_check(Json(req): Json<DeliveryCheckRequest>) -> (StatusCode, Json<Value>) {
    let reg = OclaRegistry::global();
    delivery_check_with_registry(reg.delivery_registry.as_ref(), &req)
}

fn delivery_check_with_registry(
    registry: &dyn super::traits::DeliveryRegistry,
    req: &DeliveryCheckRequest,
) -> (StatusCode, Json<Value>) {
    // Lookup is not delivery. Only the serving path knows the actual output
    // token count and may record estimated savings after producing the response.
    match registry.check_delivery(
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
                "relay_content": record.relay_content,
                "relay_mode": record.relay_mode,
            })),
        ),
        None => (StatusCode::OK, Json(json!({"hit": false}))),
    }
}
async fn delivery_batch_check(
    Json(request): Json<DeliveryBatchCheckRequest>,
) -> Json<DeliveryBatchCheckResponse> {
    let reg = OclaRegistry::global();
    let results = request
        .checks
        .into_iter()
        .map(|check| {
            let record = reg.delivery_registry.check_delivery(
                &check.blake3,
                check.mtime,
                &check.path,
                check.requester_agent_id.as_deref(),
                check.requester_conversation_id.as_deref(),
            );
            if let Some(record) = record {
                DeliveryBatchCheckResult {
                    hit: true,
                    record: Some(record),
                }
            } else {
                DeliveryBatchCheckResult {
                    hit: false,
                    record: None,
                }
            }
        })
        .collect();
    Json(DeliveryBatchCheckResponse { results })
}

async fn delivery_record(
    Json(entry): Json<crate::core::ocla::types::DeliveryEntry>,
) -> (StatusCode, Json<Value>) {
    if entry.access.is_some() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"scoped delivery requires authenticated scope authority"})),
        );
    }
    let reg = OclaRegistry::global();
    (
        StatusCode::OK,
        Json(json!(reg.delivery_registry.record_delivery(entry))),
    )
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
        "relay_served": stats.relay_served,
        "relay_tokens_saved": stats.relay_tokens_saved,
    }))
}

// ── Generalized cross-agent cache endpoints ──────────────────────────

#[derive(Debug, thiserror::Error)]
#[error("invalid cache validator")]
struct InvalidCacheValidator;

fn parse_validator(
    s: &str,
) -> Result<crate::core::ocla::cache_types::CacheValidator, InvalidCacheValidator> {
    use crate::core::ocla::cache_types::CacheValidator;
    if s == "immutable" {
        return Ok(CacheValidator::Immutable);
    }
    if let Some(ns) = s.strip_prefix("file:") {
        if let Ok(mtime_ns) = ns.parse::<u128>() {
            return Ok(CacheValidator::File { mtime_ns });
        }
    }
    if let Some(ns) = s.strip_prefix("directory:") {
        if let Ok(mtime_ns) = ns.parse::<u128>() {
            return Ok(CacheValidator::Directory { mtime_ns });
        }
    }
    Err(InvalidCacheValidator)
}

#[allow(dead_code)]
fn serialize_validator(v: &crate::core::ocla::cache_types::CacheValidator) -> String {
    use crate::core::ocla::cache_types::CacheValidator;
    match v {
        CacheValidator::Immutable => "immutable".into(),
        CacheValidator::File { mtime_ns } => format!("file:{mtime_ns}"),
        CacheValidator::Directory { mtime_ns } => format!("directory:{mtime_ns}"),
    }
}

#[derive(Deserialize)]
struct CacheCheckRequest {
    key: String,
    validator: String,
    requester_agent_id: Option<String>,
    requester_conversation_id: Option<String>,
}

fn cache_cross_agent_hit(
    entry: &crate::core::ocla::cache_types::DeliveryEntryV2,
    requester_agent_id: Option<&str>,
    requester_conversation_id: Option<&str>,
) -> bool {
    let same_agent = requester_agent_id.is_some_and(|a| a == entry.producer.agent_id);
    let same_conv = requester_conversation_id.is_some_and(|c| c == entry.producer.conversation_id);
    if same_agent && same_conv {
        return false;
    }
    crate::core::ocla::cache_delivery::entry_allows_stub(entry, requester_conversation_id)
}

async fn cache_check(
    Json(req): Json<CacheCheckRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let validator = parse_validator(&req.validator).map_err(invalid_request)?;
    let coordinator = crate::core::ocla::cache_coordinator::materialized_cache();
    use crate::core::ocla::cache_coordinator::CacheCoordinator;
    let key = crate::core::ocla::cache_types::CacheKey(req.key);
    Ok(match coordinator.check(&key, &validator) {
        Some(entry) => {
            if cache_cross_agent_hit(
                &entry,
                req.requester_agent_id.as_deref(),
                req.requester_conversation_id.as_deref(),
            ) {
                Json(json!({"hit": true, "entry": entry}))
            } else {
                Json(json!({"hit": false}))
            }
        }
        None => Json(json!({"hit": false})),
    })
}

async fn cache_record(
    Json(entry): Json<crate::core::ocla::cache_types::DeliveryEntryV2>,
) -> StatusCode {
    let coordinator = crate::core::ocla::cache_coordinator::materialized_cache();
    use crate::core::ocla::cache_coordinator::CacheCoordinator;
    coordinator.record(entry);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct CacheBatchCheckRequest {
    checks: Vec<CacheCheckRequest>,
}

#[derive(Serialize)]
struct CacheBatchCheckResult {
    hit: bool,
    entry: Option<crate::core::ocla::cache_types::DeliveryEntryV2>,
}

async fn cache_batch_check(
    Json(request): Json<CacheBatchCheckRequest>,
) -> Result<Json<Vec<CacheBatchCheckResult>>, (StatusCode, Json<Value>)> {
    // A stale lookup can evict entries, so reject the complete request before
    // obtaining the coordinator or performing any lookup.
    let validators = request
        .checks
        .iter()
        .map(|check| parse_validator(&check.validator))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_request)?;
    let coordinator = crate::core::ocla::cache_coordinator::materialized_cache();
    use crate::core::ocla::cache_coordinator::CacheCoordinator;
    let results = request
        .checks
        .into_iter()
        .zip(validators)
        .map(|(check, validator)| {
            let key = crate::core::ocla::cache_types::CacheKey(check.key);
            match coordinator.check(&key, &validator) {
                Some(entry) => {
                    if cache_cross_agent_hit(
                        &entry,
                        check.requester_agent_id.as_deref(),
                        check.requester_conversation_id.as_deref(),
                    ) {
                        CacheBatchCheckResult {
                            hit: true,
                            entry: Some(entry),
                        }
                    } else {
                        CacheBatchCheckResult {
                            hit: false,
                            entry: None,
                        }
                    }
                }
                None => CacheBatchCheckResult {
                    hit: false,
                    entry: None,
                },
            }
        })
        .collect();
    Ok(Json(results))
}

#[cfg(test)]
mod tests;
