use axum::body::Body;
use http::{HeaderMap, HeaderValue, Response};
use lean_ctx::core::ocla::types::OclaRequestContext;
use lean_ctx::core::savings_ledger::event::{MECHANISM_COMPRESSION, SavingsEvent};
use lean_ctx::core::savings_ledger::store;
use lean_ctx::proxy::forward::trace_id::{extract_or_generate_trace_id, inject_trace_id};

fn request_context(trace_id: Option<&str>) -> OclaRequestContext {
    OclaRequestContext::new(
        "request-1".into(),
        "session-1".into(),
        "agent-1".into(),
        "content:fixture".into(),
        None,
        trace_id.map(str::to_owned),
    )
}

fn savings_event_with_trace(
    saved: u64,
    request_id: &str,
    session_id: &str,
    trace_id: &str,
) -> SavingsEvent {
    SavingsEvent {
        ts: "2026-07-29T12:00:00+00:00".into(),
        tool: "ctx_read".into(),
        mechanism: MECHANISM_COMPRESSION.into(),
        model_id: "fixture-model".into(),
        tokenizer: "o200k_base".into(),
        baseline_tokens: saved + 100,
        actual_tokens: 100,
        saved_tokens: saved,
        bounce_adjustment: 0,
        unit_price_per_m_usd: 2.0,
        saved_usd: saved as f64 * 2.0 / 1_000_000.0,
        repo_hash: "fixture-repo".into(),
        agent_id: "fixture-agent".into(),
        prev_hash: String::new(),
        entry_hash: String::new(),
        version: env!("CARGO_PKG_VERSION").into(),
        intent_tag: None,
        outcome: None,
        model_original: None,
        model_routed: None,
        routing_savings: None,
        response_original_tokens: None,
        response_delivered_tokens: None,
        agent_chain_id: None,
        chain_depth: None,
        measurement_method: None,
        evidence_class: None,
        confidence: None,
        request_id: Some(request_id.into()),
        session_id: Some(session_id.into()),
        trace_id: Some(trace_id.into()),
        quality_signal: None,
        attribution_group: None,
        attribution_id: None,
        baseline_ref: None,
        price_version: None,
        customer_approval: None,
        settlement_status: None,
        is_first_inject: None,
        cache_read_per_m_usd: None,
        cache_write_per_m_usd: None,
        governor: None,
        solution_decision: None,
        loc_added: None,
        loc_removed: None,
        path: None,
        lines_added: None,
        lines_removed: None,
        net: None,
    }
}

#[test]
fn trace_id_generated_when_absent() {
    let trace_id = request_context(None).trace_id;
    assert!(!trace_id.is_empty(), "must generate a trace ID");
    assert!(trace_id.len() > 8, "trace ID must be non-trivial");
}

#[test]
fn trace_id_preserved_when_present() {
    assert_eq!(
        request_context(Some("test-trace-123")).trace_id,
        "test-trace-123"
    );
}

#[test]
fn trace_id_deterministic_format() {
    let ids: Vec<String> = (0..10).map(|_| request_context(None).trace_id).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "generated trace IDs must be unique"
    );

    for id in ids {
        let uuid = id.strip_prefix("tr-").unwrap();
        assert_eq!(uuid.len(), 36, "trace ID must contain a UUID: {id}");
        assert!(
            uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "trace ID contains invalid chars: {id}"
        );
    }
}

#[test]
fn empty_trace_id_header_generates_new() {
    let mut headers = HeaderMap::new();
    headers.insert("x-trace-id", HeaderValue::from_static(""));
    let trace_id = extract_or_generate_trace_id(&headers);
    assert!(
        trace_id.starts_with("tr-"),
        "empty header must produce a generated trace ID, got: {trace_id}"
    );
    assert!(trace_id.len() > 8);
}

#[test]
fn trace_id_round_trips_through_ocla_wire_context() {
    let context = request_context(Some("trace-request-proxy-mcp"));
    let wire = serde_json::to_string(&context).unwrap();
    let restored: OclaRequestContext = serde_json::from_str(&wire).unwrap();
    assert_eq!(restored.trace_id, "trace-request-proxy-mcp");
    assert_eq!(restored.request_id, "request-1");
    assert_eq!(restored.session_id, "session-1");
}

#[test]
fn trace_id_injected_into_response() {
    let mut response = Response::builder().status(200).body(Body::empty()).unwrap();
    inject_trace_id(&mut response, "injected-trace-42");
    let header = response
        .headers()
        .get("x-trace-id")
        .expect("x-trace-id header must be present");
    assert_eq!(header.to_str().unwrap(), "injected-trace-42");
}

#[test]
fn savings_record_contains_trace_id() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");

    let ev = savings_event_with_trace(80, "req-42", "sess-7", "tr-abc");
    store::append(&path, ev).unwrap();

    let records = store::load(&path);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].request_id.as_deref(), Some("req-42"));
    assert_eq!(records[0].session_id.as_deref(), Some("sess-7"));
    assert_eq!(records[0].trace_id.as_deref(), Some("tr-abc"));
}
