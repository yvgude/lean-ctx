use std::collections::HashSet;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use lean_ctx::core::a2a::dlq::{DeadLetter, DeadLetterQueue};
use lean_ctx::core::capsule_transport::LocalSignedCapsuleTransport;
use lean_ctx::core::context_capsule::{
    CONTEXT_CAPSULE_SCHEMA_VERSION, CapsuleReferenceKindV1, CapsuleSensitivityV1,
    ContextCapsuleBudgetV1, ContextCapsuleChainV1, ContextCapsuleReferenceV1, ContextCapsuleV1,
    SignedContextCapsuleV1,
};
use lean_ctx::core::ocla::budget::{BudgetLedger, BudgetLimit, BudgetScope};
use lean_ctx::core::ocla::capsule::CapsuleStore;
use lean_ctx::core::ocla::health::{HealthStatus, check_system_health};
use lean_ctx::core::ocla::response_cache::{CachedResponse, ResponseCache, ResponseCacheKey};
use lean_ctx::core::ocla::routing_quality::{
    RoutingDecision, RoutingOutcome, RoutingQualityTracker,
};
use lean_ctx::core::ocla::tracing::{SpanStatus, spans_for_trace, start_span};
use lean_ctx::core::ocla::wire_api::ocla_router;
use lean_ctx::core::{agents::AgentRegistry, savings_ledger};
use serde_json::Value;
use tower::ServiceExt;

struct IsolatedDataDir {
    temp: tempfile::TempDir,
    _lock: lean_ctx::core::data_dir::TestEnvGuard,
    previous_data_dir: Option<String>,
    previous_ledger_setting: Option<String>,
}

impl IsolatedDataDir {
    fn new() -> Self {
        let lock = lean_ctx::core::data_dir::test_env_lock();
        let temp = tempfile::tempdir().expect("isolated data directory");
        let previous_data_dir = std::env::var("LEAN_CTX_DATA_DIR").ok();
        let previous_ledger_setting = std::env::var("LEAN_CTX_SAVINGS_LEDGER").ok();
        // SAFETY: test_env_lock serializes all environment mutations in this suite.
        unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", temp.path()) };
        // SAFETY: test_env_lock serializes all environment mutations in this suite.
        unsafe { std::env::set_var("LEAN_CTX_SAVINGS_LEDGER", "on") };
        Self {
            temp,
            _lock: lock,
            previous_data_dir,
            previous_ledger_setting,
        }
    }
}

impl Drop for IsolatedDataDir {
    fn drop(&mut self) {
        // SAFETY: test_env_lock remains held until after Drop restores the env.
        match &self.previous_data_dir {
            Some(value) => unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", value) },
            None => unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") },
        }
        // SAFETY: test_env_lock remains held until after Drop restores the env.
        match &self.previous_ledger_setting {
            Some(value) => unsafe { std::env::set_var("LEAN_CTX_SAVINGS_LEDGER", value) },
            None => unsafe { std::env::remove_var("LEAN_CTX_SAVINGS_LEDGER") },
        }
    }
}

fn capsule() -> ContextCapsuleV1 {
    let mut capsule = ContextCapsuleV1 {
        schema_version: CONTEXT_CAPSULE_SCHEMA_VERSION,
        capsule_id: "capsule:pending".into(),
        request_id: "request:integration".into(),
        session_id: "session:integration".into(),
        agent_id: "integration-parent".into(),
        intent_ref: "intent:integration".into(),
        task_ref: "task:integration".into(),
        expected_outcome_ref: "outcome:integration".into(),
        acceptance_criteria_refs: vec!["criteria:green".into()],
        references: vec![ContextCapsuleReferenceV1 {
            kind: CapsuleReferenceKindV1::File,
            content_ref: "blake3:integration".into(),
            freshness_ref: "freshness:integration".into(),
            recovery_ref: None,
        }],
        finding_refs: vec![],
        decision_refs: vec![],
        uncertainty_refs: vec![],
        negative_result_refs: vec![],
        source_ref: "source:integration".into(),
        policy_ref: "policy:integration".into(),
        contract_ref: "contract:integration".into(),
        freshness_ref: "freshness:integration".into(),
        sensitivity: CapsuleSensitivityV1::Internal,
        allowed_agent_ids: vec!["integration-child".into()],
        budget: ContextCapsuleBudgetV1 {
            tokens_used: 100,
            tokens_remaining: 900,
            cost_micros_used: 10,
            cost_micros_remaining: 90,
            latency_ms_used: 20,
            latency_ms_remaining: 80,
        },
        chain: ContextCapsuleChainV1 {
            chain_id: "chain:integration".into(),
            parent_capsule_ref: None,
            owner_agent_id: "integration-parent".into(),
            attribution_ref: "attribution:integration".into(),
            hop: 0,
        },
        quality_signal_refs: vec![],
        recovery_refs: vec![],
        delta_from: None,
    };
    capsule.assign_capsule_id().expect("capsule ID");
    capsule.validate().expect("valid capsule");
    capsule
}

fn signed(capsule: &ContextCapsuleV1) -> SignedContextCapsuleV1 {
    SignedContextCapsuleV1::sign(capsule, &SigningKey::from_bytes(&[7; 32]))
        .expect("signed capsule")
}

fn reconciled_event_count(data_dir: &std::path::Path) -> usize {
    let unified_path = data_dir.join("savings/unified_ledger.jsonl");
    let unified_hashes: HashSet<String> = std::fs::read_to_string(unified_path)
        .expect("unified ledger")
        .lines()
        .map(|line| {
            serde_json::from_str::<lean_ctx::core::ocla::unified_ledger::UnifiedSavingsEventV2>(
                line,
            )
            .expect("unified event")
            .event_hash
        })
        .collect();
    savings_ledger::all_events()
        .iter()
        .filter(|event| unified_hashes.contains(&event.entry_hash))
        .count()
}

#[test]
fn test_full_pipeline() {
    let data = IsolatedDataDir::new();
    let parent = capsule();
    let transport = LocalSignedCapsuleTransport::default();
    let delivery = transport
        .deliver(&signed(&parent), "integration-child")
        .expect("capsule registration");
    assert_eq!(delivery.capsule_ref, parent.capsule_id);

    let scope = BudgetScope::Org("integration-test".into());
    let mut budget = BudgetLedger::new();
    budget.set_limit(BudgetLimit {
        scope: scope.clone(),
        max_tokens_per_day: 1_000,
        max_usd_per_day: 10.0,
    });
    budget.check_budget(&scope, 100).expect("budget admission");
    budget.record_consumption(&scope, 40, 0.40);

    let mut routing = RoutingQualityTracker::new();
    routing.record(RoutingOutcome {
        decision: RoutingDecision {
            decision_id: "integration-route".into(),
            original_model: "expensive-model".into(),
            routed_model: "integration-model".into(),
            reason: "integration route".into(),
            timestamp: Utc::now().to_rfc3339(),
        },
        quality_score: Some(0.95),
        tokens_saved: 60,
        latency_delta_ms: -10,
    });
    assert!(!routing.should_fallback());

    let cache = ResponseCache::new(4, Duration::from_secs(30));
    let key = ResponseCacheKey::new("integration-model", 42, 0.0, 128);
    cache.put(
        key.clone(),
        CachedResponse {
            body: b"cached response".to_vec(),
            status: 200,
            tokens: 40,
            created_at: Instant::now(),
            ttl: Duration::ZERO,
        },
    );
    assert_eq!(cache.get(&key).expect("cache hit").tokens, 40);

    savings_ledger::record_tool_event("ocla_integration", 100, 40, None, None);
    assert_eq!(savings_ledger::all_events().len(), 1);
    assert_eq!(reconciled_event_count(data.temp.path()), 1);
    assert!(savings_ledger::verify().valid);

    AgentRegistry::mutate_locked(|registry| registry.register("integration", Some("test"), "."))
        .expect("agent registration");
    assert_eq!(check_system_health().overall, HealthStatus::Healthy);
}

#[test]
fn full_capsule_lifecycle() {
    let store = CapsuleStore::new();
    let data = b"integration capsule lifecycle";
    let parent_ref = store.register(data);
    let child_ref = store.fork(&parent_ref, 512).expect("capsule fork");

    assert_ne!(parent_ref, child_ref);
    assert_eq!(store.resolve(&parent_ref).expect("parent resolve"), data);
    assert_eq!(store.resolve(&child_ref).expect("child resolve"), data);
}

#[test]
fn health_reports_all_components() {
    let health = check_system_health();
    assert!(health.components.len() >= 7);
    for name in ["a2a_bus", "ledger", "budget", "dlq"] {
        assert!(
            health
                .components
                .iter()
                .any(|component| component.name == name),
            "missing health component: {name}"
        );
    }
}

fn canonical_envelope_body() -> String {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/ocla_envelope_golden.json"))
        .expect("valid envelope fixture");
    fixture["canonical"].to_string()
}

async fn endpoint_status(method: &str, uri: &str, body: Option<&str>) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.unwrap_or_default().to_owned()))
        .expect("request");
    ocla_router()
        .oneshot(request)
        .await
        .expect("response")
        .status()
}

#[tokio::test]
async fn all_endpoints_return_valid_status() {
    let envelope = canonical_envelope_body();
    let scope = "user:integration-all-endpoints";
    let budget = serde_json::json!({
        "scope": scope,
        "max_tokens_per_day": 100,
        "max_usd_per_day": 1.0
    })
    .to_string();
    let batch = format!("[{envelope}]");
    let requests = vec![
        ("GET", "/ocla/v1/health".to_owned(), None),
        ("GET", "/ocla/v1/capabilities".to_owned(), None),
        ("POST", "/ocla/v1/envelope".to_owned(), Some(envelope)),
        ("POST", "/ocla/v1/envelope/batch".to_owned(), Some(batch)),
        ("GET", "/ocla/v1/agents".to_owned(), None),
        ("GET", "/ocla/v1/metrics".to_owned(), None),
        ("GET", "/ocla/v1/ledger/summary".to_owned(), None),
        ("POST", "/ocla/v1/budget".to_owned(), Some(budget)),
        ("GET", format!("/ocla/v1/budget/{scope}"), None),
        ("DELETE", format!("/ocla/v1/budget/{scope}"), None),
        ("GET", "/ocla/v1/dlq".to_owned(), None),
        (
            "POST",
            "/ocla/v1/dlq/integration-missing/retry".to_owned(),
            None,
        ),
        (
            "DELETE",
            "/ocla/v1/dlq/integration-missing".to_owned(),
            None,
        ),
        (
            "POST",
            "/ocla/v1/capsule".to_owned(),
            Some("integration endpoint capsule".to_owned()),
        ),
        (
            "GET",
            "/ocla/v1/capsule/capsule:integration-missing".to_owned(),
            None,
        ),
        (
            "POST",
            "/ocla/v1/capsule/capsule:integration-missing/fork".to_owned(),
            Some(r#"{"budget_tokens":64}"#.to_owned()),
        ),
    ];

    for (method, uri, body) in requests {
        let status = endpoint_status(method, &uri, body.as_deref()).await;
        assert!(
            !status.is_server_error(),
            "{method} {uri} returned {status}"
        );
    }
}

#[test]
fn test_budget_blocks_over_limit() {
    let scope = BudgetScope::Org("integration-budget-block".into());
    let mut budget = BudgetLedger::new();
    budget.set_limit(BudgetLimit {
        scope: scope.clone(),
        max_tokens_per_day: 10,
        max_usd_per_day: 1.0,
    });
    budget.record_consumption(&scope, 10, 0.10);
    assert!(budget.check_budget(&scope, 1).is_err());
}

#[test]
#[ignore = "chain/budget fields not yet implemented on ContextCapsuleV1"]
fn test_capsule_fork_and_resolve() {
    // Blocked: ContextCapsuleV1 does not yet have .chain/.budget fields.
    // Re-enable once capsule chain and budget tracking are implemented.
}

#[test]
fn test_dlq_lifecycle() {
    let queue = DeadLetterQueue::new();
    queue.enqueue(DeadLetter {
        id: "integration-dead-letter".into(),
        original_message: "integration message".into(),
        target_agent: "integration-child".into(),
        error: "delivery failed".into(),
        attempts: 1,
        first_failed_at: Utc::now().to_rfc3339(),
        last_failed_at: Utc::now().to_rfc3339(),
    });
    assert_eq!(queue.peek_all().len(), 1);
    assert!(queue.dequeue("integration-dead-letter").is_some());
    assert!(queue.peek_all().is_empty());
}

#[test]
fn test_tracing_spans() {
    let trace_id = format!(
        "integration-trace-{}",
        Utc::now().timestamp_nanos_opt().unwrap()
    );
    let parent = start_span(&trace_id, "integration.parent");
    let child = start_span(&trace_id, "integration.child");
    child.set_status(SpanStatus::Ok);
    drop(child);
    drop(parent);

    let spans = spans_for_trace(&trace_id);
    assert_eq!(spans.len(), 2);
    assert!(spans.iter().all(|span| {
        span.end_ns.expect("span end") >= span.start_ns && span.trace_id == trace_id
    }));
    let child = spans
        .iter()
        .find(|span| span.operation == "integration.child")
        .expect("child span");
    let parent = spans
        .iter()
        .find(|span| span.operation == "integration.parent")
        .expect("parent span");
    assert_eq!(
        child.parent_span_id.as_deref(),
        Some(parent.span_id.as_str())
    );
}
