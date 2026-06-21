//! OCP self-validation (GL #430, H3 Epic C — AC 2).
//!
//! `LeanCTX` is the Open Context Protocol reference implementation. This
//! suite proves it: real engine output for every OCP part is validated
//! against the published JSON Schemas (vendored in `docs/contracts/ocp/`,
//! source of truth: the `open-context-protocol` repo).
//!
//! Single #[test] entry point: the evidence part mutates process-global
//! state (data dir env var, audit chain tail), so the parts run
//! sequentially by construction instead of relying on test ordering.

use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
use lean_ctx::core::context_ir::{ContextIrSourceKindV1, ContextIrV1, RecordIrInput};
use lean_ctx::core::events::{EventKind, LeanCtxEvent};
use lean_ctx::core::ocp;
use lean_ctx::core::policy::builtin;

fn validator(schema_file: &str) -> jsonschema::Validator {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/ocp")
        .join(schema_file);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("schema {} unreadable: {e}", path.display()));
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("schema must be JSON");
    jsonschema::validator_for(&schema).expect("schema must compile")
}

fn assert_valid(v: &jsonschema::Validator, instance: &serde_json::Value, what: &str) {
    let errors: Vec<String> = v.iter_errors(instance).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "{what} violates OCP schema:\n{}\ninstance: {instance:#}",
        errors.join("\n")
    );
}

#[test]
fn engine_output_validates_against_ocp_schemas() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    part1_context_ir();
    part2_capabilities();
    part3_policy_packs();
    part4_evidence_chain();
    part5_events();

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

fn part1_context_ir() {
    let v = validator("context-ir.schema.json");

    let mut ir = ContextIrV1::new();
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Read,
        tool: "ctx_read",
        client_name: Some("cursor".into()),
        agent_id: Some("agent-1".into()),
        path: Some("src/main.rs"),
        command: None,
        pattern: None,
        input_tokens: 1200,
        output_tokens: 80,
        duration: std::time::Duration::from_millis(12),
        content_excerpt: "fn main() {}",
    });
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Shell,
        tool: "ctx_shell",
        client_name: None,
        agent_id: None,
        path: None,
        command: Some("cargo test"),
        pattern: None,
        input_tokens: 0,
        output_tokens: 40,
        duration: std::time::Duration::from_millis(900),
        content_excerpt: &"x".repeat(5000), // forces truncation path
    });
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Search,
        tool: "ctx_search",
        client_name: None,
        agent_id: None,
        path: Some("rust/src"),
        command: None,
        pattern: Some("fn handle"),
        input_tokens: 300,
        output_tokens: 30,
        duration: std::time::Duration::from_millis(5),
        content_excerpt: "",
    });

    let doc = serde_json::to_value(&ir).expect("IR serializes");
    assert_valid(&v, &doc, "Context-IR document (Part 1)");
}

fn part2_capabilities() {
    let v = validator("capabilities.schema.json");

    for role in ["admin", "reviewer", "minimal", "developer"] {
        assert_valid(
            &v,
            &ocp::capability_grant_set(role),
            &format!("grant set for role {role} (Part 2)"),
        );
    }
    for (role, tool) in [
        ("minimal", "ctx_shell"), // denied → missing populated
        ("admin", "ctx_shell"),   // allowed → missing empty
        ("reviewer", "ctx_edit"), // denied
    ] {
        assert_valid(
            &v,
            &ocp::capability_check_result(role, tool),
            &format!("check result {role}/{tool} (Part 2)"),
        );
    }
}

fn part3_policy_packs() {
    let v = validator("policy-pack.schema.json");

    let packs = builtin::all();
    assert!(!packs.is_empty(), "engine ships builtin policy packs");
    for pack in packs {
        let json = serde_json::to_value(pack).expect("pack serializes");
        assert_valid(
            &v,
            &json,
            &format!("builtin policy pack {} (Part 3)", pack.name),
        );
    }
}

fn part4_evidence_chain() {
    let v = validator("evidence-entry.schema.json");

    audit_trail::record(AuditEntryData {
        agent_id: "agent-1".into(),
        tool: "ctx_read".into(),
        action: Some("full".into()),
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 42,
        role: "developer".into(),
        event_type: AuditEventType::ToolCall,
    });
    audit_trail::record(AuditEntryData {
        agent_id: "agent-1".into(),
        tool: "ctx_shell".into(),
        action: None,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 0,
        role: "reviewer".into(),
        event_type: AuditEventType::ToolDenied,
    });

    let entries = audit_trail::load_recent(10);
    assert!(entries.len() >= 2, "audit trail must persist entries");
    for entry in &entries {
        let json = serde_json::to_value(entry).expect("entry serializes");
        assert_valid(&v, &json, "evidence entry (Part 4)");
    }

    let chain = audit_trail::verify_chain();
    assert!(chain.valid, "hash chain must verify (Part 4 §4.3)");
    assert_eq!(chain.first_invalid_at, None);
}

fn part5_events() {
    let v = validator("event.schema.json");
    let ts = chrono::Utc::now().to_rfc3339();

    let governance_kinds = vec![
        EventKind::ToolCall {
            tool: "ctx_read".into(),
            tokens_original: 1000,
            tokens_saved: 900,
            mode: Some("auto".into()),
            duration_ms: 7,
            path: Some("src/lib.rs".into()),
        },
        EventKind::AgentAction {
            agent_id: "agent-1".into(),
            action: "handoff".into(),
            tool: Some("ctx_handoff".into()),
        },
        EventKind::KnowledgeUpdate {
            category: "decision".into(),
            key: "ocp".into(),
            action: "remember".into(),
        },
        EventKind::BudgetWarning {
            role: "developer".into(),
            dimension: "tokens".into(),
            used: "80000".into(),
            limit: "100000".into(),
            percent: 80,
        },
        EventKind::BudgetExhausted {
            role: "developer".into(),
            dimension: "tokens".into(),
            used: "100001".into(),
            limit: "100000".into(),
        },
        EventKind::PolicyViolation {
            role: "reviewer".into(),
            tool: "ctx_shell".into(),
            reason: "tool denied by pack baseline".into(),
        },
        EventKind::RoleChanged {
            from: "developer".into(),
            to: "reviewer".into(),
        },
    ];

    for (i, kind) in governance_kinds.into_iter().enumerate() {
        let event = LeanCtxEvent {
            id: i as u64,
            timestamp: ts.clone(),
            kind,
        };
        let exported =
            ocp::export_event(&event).unwrap_or_else(|| panic!("governance event {i} must export"));
        assert_valid(&v, &exported, &format!("governance event #{i} (Part 5)"));
    }

    // Product telemetry stays out of the OCP surface by design (ADR-0001).
    let telemetry = LeanCtxEvent {
        id: 99,
        timestamp: ts,
        kind: EventKind::CacheHit {
            path: "src/lib.rs".into(),
            saved_tokens: 10,
        },
    };
    assert!(ocp::export_event(&telemetry).is_none());
}
