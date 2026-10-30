use lean_ctx::core::savings_ledger::event::{MECHANISM_COMPRESSION, SavingsEvent};
use lean_ctx::core::savings_ledger::store;

fn event(saved_tokens: u64, attribution_group: &str) -> SavingsEvent {
    SavingsEvent {
        ts: "2026-07-29T12:00:00+00:00".into(),
        tool: "ctx_read".into(),
        mechanism: MECHANISM_COMPRESSION.into(),
        model_id: "fixture-model".into(),
        tokenizer: "o200k_base".into(),
        baseline_tokens: saved_tokens + 100,
        actual_tokens: 100,
        saved_tokens,
        bounce_adjustment: 0,
        unit_price_per_m_usd: 2.0,
        saved_usd: saved_tokens as f64 * 2.0 / 1_000_000.0,
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
        request_id: None,
        session_id: None,
        trace_id: None,
        solution_decision: None,
        loc_added: None,
        loc_removed: None,
        path: None,
        lines_added: None,
        lines_removed: None,
        net: None,
        quality_signal: Some("fixture-quality".into()),
        attribution_group: Some(attribution_group.into()),
        attribution_id: None,
        baseline_ref: Some("fixture-baseline".into()),
        price_version: None,
        customer_approval: None,
        settlement_status: None,
        is_first_inject: None,
        cache_read_per_m_usd: None,
        cache_write_per_m_usd: None,
        governor: None,
    }
}

fn event_with_session(saved: u64, group: &str, session_id: &str, trace_id: &str) -> SavingsEvent {
    let mut ev = event(saved, group);
    ev.session_id = Some(session_id.into());
    ev.trace_id = Some(trace_id.into());
    ev.request_id = Some(format!("req-{saved}"));
    ev
}

#[test]
fn attribution_no_double_count() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    for saved_tokens in [60, 40] {
        store::append(&path, event(saved_tokens, "session-a")).unwrap();
    }

    let summary = store::summarize(&path);
    let mechanism_total: u64 = summary
        .by_mechanism
        .iter()
        .map(|(_, saved_tokens, _)| *saved_tokens)
        .sum();
    assert_eq!(summary.total_events, 2);
    assert_eq!(summary.saved_tokens, 100);
    assert_eq!(mechanism_total, summary.saved_tokens);
}

#[test]
fn attribution_group_is_persisted_per_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");
    for saved_tokens in [60, 40] {
        store::append(&path, event(saved_tokens, "shared-group")).unwrap();
    }

    let records = store::load(&path);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.attribution_group.as_deref() == Some("shared-group"))
    );
}

#[test]
fn attribution_trace_groups_requests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");

    store::append(&path, event_with_session(60, "group-a", "sess-1", "tr-100")).unwrap();
    store::append(&path, event_with_session(40, "group-a", "sess-1", "tr-100")).unwrap();
    store::append(&path, event_with_session(30, "group-b", "sess-2", "tr-200")).unwrap();

    let records = store::load(&path);
    let trace_100: Vec<_> = records
        .iter()
        .filter(|r| r.trace_id.as_deref() == Some("tr-100"))
        .collect();
    let trace_200: Vec<_> = records
        .iter()
        .filter(|r| r.trace_id.as_deref() == Some("tr-200"))
        .collect();

    assert_eq!(trace_100.len(), 2, "trace tr-100 should group 2 events");
    assert_eq!(trace_200.len(), 1, "trace tr-200 should group 1 event");
    let total_100: u64 = trace_100.iter().map(|e| e.saved_tokens).sum();
    assert_eq!(total_100, 100);
}

#[test]
fn attribution_cross_session_isolation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ledger.jsonl");

    store::append(
        &path,
        event_with_session(50, "group-x", "session-alpha", "tr-a"),
    )
    .unwrap();
    store::append(
        &path,
        event_with_session(70, "group-y", "session-beta", "tr-b"),
    )
    .unwrap();

    let records = store::load(&path);
    let alpha: Vec<_> = records
        .iter()
        .filter(|r| r.session_id.as_deref() == Some("session-alpha"))
        .collect();
    let beta: Vec<_> = records
        .iter()
        .filter(|r| r.session_id.as_deref() == Some("session-beta"))
        .collect();

    assert_eq!(alpha.len(), 1);
    assert_eq!(beta.len(), 1);
    assert_ne!(
        alpha[0].trace_id, beta[0].trace_id,
        "different sessions must have different trace IDs"
    );
}

#[test]
fn attribution_quality_ref_present() {
    let record = event(60, "session-a");
    assert_eq!(record.quality_signal.as_deref(), Some("fixture-quality"));
    assert_eq!(record.baseline_ref.as_deref(), Some("fixture-baseline"));
}

#[test]
fn savings_export_json_has_required_fields() {
    let ev = event_with_session(80, "export-group", "sess-export", "tr-export");
    let json = serde_json::to_value(&ev).unwrap();

    for field in [
        "request_id",
        "session_id",
        "trace_id",
        "baseline_tokens",
        "actual_tokens",
        "saved_tokens",
        "quality_signal",
    ] {
        assert!(
            json.get(field).is_some(),
            "export JSON missing required field: {field}"
        );
    }
    assert_eq!(json["session_id"], "sess-export");
    assert_eq!(json["trace_id"], "tr-export");
}
