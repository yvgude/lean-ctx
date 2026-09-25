use super::event::{
    CustomerApproval, EvidenceClass, MECHANISM_COMPRESSION, MeasurementMethod, SavingsEvent,
    SettlementStatus, compute_hash,
};
use super::store::{self, GENESIS};
use serde_json::{Value, json};
use std::path::PathBuf;

fn legacy_json() -> Value {
    json!({
        "ts": "2026-06-01T12:00:00+00:00",
        "tool": "ctx_read",
        "model_id": "claude-3.5-sonnet",
        "tokenizer": "o200k_base",
        "baseline_tokens": 1000,
        "actual_tokens": 300,
        "saved_tokens": 700,
        "bounce_adjustment": 0,
        "unit_price_per_m_usd": 3.0,
        "saved_usd": 0.0021,
        "repo_hash": "repo",
        "agent_id": "local",
        "prev_hash": "",
        "entry_hash": ""
    })
}

fn event(value: Value) -> SavingsEvent {
    serde_json::from_value(value).expect("fixture must deserialize")
}

fn assert_legacy_defaults(ev: &SavingsEvent) {
    assert_eq!(ev.mechanism, MECHANISM_COMPRESSION);
    assert!(ev.version.is_empty());
    assert_p5_defaults(ev);
}

fn assert_p5_defaults(ev: &SavingsEvent) {
    assert!(ev.intent_tag.is_none());
    assert!(ev.outcome.is_none());
    assert!(ev.model_original.is_none());
    assert!(ev.model_routed.is_none());
    assert!(ev.routing_savings.is_none());
    assert!(ev.response_original_tokens.is_none());
    assert!(ev.response_delivered_tokens.is_none());
    assert!(ev.agent_chain_id.is_none());
    assert!(ev.chain_depth.is_none());
    assert!(ev.measurement_method.is_none());
    assert!(ev.evidence_class.is_none());
    assert!(ev.confidence.is_none());
    assert!(ev.solution_decision.is_none());
    assert!(ev.loc_added.is_none());
    assert!(ev.loc_removed.is_none());
    assert!(ev.quality_signal.is_none());
    assert!(ev.attribution_group.is_none());
    assert!(ev.attribution_id.is_none());
    assert!(ev.baseline_ref.is_none());
    assert!(ev.price_version.is_none());
    assert!(ev.customer_approval.is_none());
    assert!(ev.settlement_status.is_none());
}

fn full_event() -> SavingsEvent {
    let mut value = legacy_json();
    value["mechanism"] = json!("compression");
    value["version"] = json!("5.0.0");
    value["intent_tag"] = json!("code_generation");
    value["outcome"] = json!("used");
    value["model_original"] = json!("claude-opus-4.5");
    value["model_routed"] = json!("claude-sonnet-4");
    value["routing_savings"] = json!(42);
    value["response_original_tokens"] = json!(800);
    value["response_delivered_tokens"] = json!(500);
    value["agent_chain_id"] = json!("chain-1");
    value["chain_depth"] = json!(2);
    value["measurement_method"] = json!("direct_count");
    value["evidence_class"] = json!("measured");
    value["confidence"] = json!(0.99);
    value["quality_signal"] = json!("accepted");
    value["attribution_group"] = json!("group-a");
    value["attribution_id"] = json!("blake3-scope");
    value["baseline_ref"] = json!("baseline-1");
    value["price_version"] = json!("pricing-2026-06");
    value["customer_approval"] = json!("approved");
    value["settlement_status"] = json!("settled");
    event(value)
}

fn linked(mut ev: SavingsEvent, prev: &str, version: u8) -> SavingsEvent {
    ev.prev_hash = prev.to_string();
    let content = match version {
        1 => ev.canonical_content_legacy(),
        3 => ev.canonical_content_v3(),
        4 => ev.canonical_content_v4(),
        5 => ev.canonical_content_v5(),
        6 => ev.canonical_content_v6(),
        7 => ev.canonical_content(),
        _ => unreachable!("test fixture version"),
    };
    ev.entry_hash = compute_hash(prev, &content);
    ev
}

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("lean-ctx-migration-{tag}-{nanos}.jsonl"))
}

#[test]
fn v1_ledger_still_loads() {
    let ev = event(legacy_json());
    assert_legacy_defaults(&ev);
}

#[test]
fn v2_ledger_still_loads() {
    let mut value = legacy_json();
    value["mechanism"] = json!("compression");
    let ev = event(value);
    assert_legacy_defaults(&ev);
}

#[test]
fn v3_ledger_still_loads() {
    let mut value = legacy_json();
    value["mechanism"] = json!("routing");
    let ev = event(value);
    assert_eq!(ev.mechanism, "routing");
    assert!(ev.version.is_empty());
    assert_p5_defaults(&ev);
}

#[test]
fn v4_ledger_still_loads() {
    let mut value = legacy_json();
    value["version"] = json!("4.0.0");
    let ev = event(value);
    assert_eq!(ev.version, "4.0.0");
    assert_p5_defaults(&ev);
}

#[test]
fn v5_ledger_includes_p5() {
    let parsed: SavingsEvent =
        serde_json::from_str(&serde_json::to_string(&full_event()).expect("serialize full event"))
            .expect("roundtrip full event");
    assert_eq!(parsed, full_event());
    assert_eq!(
        parsed.measurement_method,
        Some(MeasurementMethod::DirectCount)
    );
    assert_eq!(parsed.evidence_class, Some(EvidenceClass::Measured));
    assert_eq!(parsed.customer_approval, Some(CustomerApproval::Approved));
    assert_eq!(parsed.settlement_status, Some(SettlementStatus::Settled));
}

#[test]
fn mixed_version_chain_verifies() {
    let v1 = linked(event(legacy_json()), GENESIS, 1);
    let v3 = linked(
        event({
            let mut value = legacy_json();
            value["mechanism"] = json!("routing");
            value
        }),
        &v1.entry_hash,
        3,
    );
    let v4 = linked(
        event({
            let mut value = legacy_json();
            value["version"] = json!("4.0.0");
            value
        }),
        &v3.entry_hash,
        4,
    );
    let v5 = linked(full_event(), &v4.entry_hash, 5);
    let v6 = linked(full_event(), &v5.entry_hash, 6);
    let v7 = linked(
        {
            let mut ev = full_event();
            ev.session_id = Some("sess-42".into());
            ev
        },
        &v6.entry_hash,
        7,
    );

    assert!(v1.hash_matches(GENESIS));
    assert!(v3.hash_matches(&v1.entry_hash));
    assert!(v4.hash_matches(&v3.entry_hash));
    assert!(v5.hash_matches(&v4.entry_hash));
    assert!(v6.hash_matches(&v5.entry_hash));
    assert!(v7.hash_matches(&v6.entry_hash));
}

#[test]
fn p5_fields_omitted_in_json_when_none() {
    let json = serde_json::to_string(&event(legacy_json())).expect("serialize legacy event");
    for field in [
        "intent_tag",
        "outcome",
        "model_original",
        "model_routed",
        "routing_savings",
        "response_original_tokens",
        "response_delivered_tokens",
        "agent_chain_id",
        "chain_depth",
        "measurement_method",
        "evidence_class",
        "confidence",
        "quality_signal",
        "attribution_group",
        "attribution_id",
        "baseline_ref",
        "price_version",
        "customer_approval",
        "settlement_status",
    ] {
        assert!(
            !json.contains(&format!("\"{field}\"")),
            "field {field} present"
        );
    }
}

#[test]
fn rechain_preserves_p5_fields() {
    let path = temp_path("rechain");
    store::append(&path, full_event()).expect("append first event");
    let mut second = full_event();
    second.intent_tag = Some("summarization".into());
    second.attribution_id = Some("scope-2".into());
    store::append(&path, second).expect("append second event");
    let before = store::load(&path);

    assert_eq!(store::rechain(&path).expect("rechain ledger"), 2);
    let after = store::load(&path);
    assert_eq!(after.len(), before.len());
    for (old, new) in before.iter().zip(after.iter()) {
        assert_eq!(new.intent_tag, old.intent_tag);
        assert_eq!(new.measurement_method, old.measurement_method);
        assert_eq!(new.evidence_class, old.evidence_class);
        assert_eq!(new.attribution_id, old.attribution_id);
        assert_eq!(new.customer_approval, old.customer_approval);
        assert_eq!(new.settlement_status, old.settlement_status);
    }
    assert!(store::verify(&path).valid);
    let _ = std::fs::remove_file(path);
}
