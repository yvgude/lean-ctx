// SPDX-License-Identifier: Apache-2.0
//! Proofs of engine guarantees that governance assessments rely on.
//!
//! Framework assessments (LeanCTX Enterprise) name these tests for every control
//! the engine itself guarantees; the runtime's mapping test fails when a named
//! test is missing here. Each proof shows the mechanism and, where meaningful,
//! that a violation is detectable (tampered log ⇒ invalid chain, denied tool ⇒
//! exported event).

use lean_ctx::core::events::{EventKind, LeanCtxEvent};
use lean_ctx::core::ocp;
use lean_ctx::core::policy::{builtin, resolve};

/// The audit-chain proofs build on one trail in a fixed order; the named
/// functions below are what assessments reference. They use an explicit trail
/// path (the same append and verification code `record` uses), so the suite's
/// hermetic data dir is never touched.
#[test]
fn audit_chain_proofs_run_sequentially() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let trail = tmp.path().join("trail.jsonl");
    audit_logging_is_automatic_and_hash_chained(&trail);
    audit_entries_attribute_agent_and_role(&trail);
    // Destroys the chain — must run last.
    tampered_audit_log_fails_verification(&trail);
}

fn record(trail: &std::path::Path, agent: &str, tool: &str, tokens: u32, role: &str) {
    use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
    audit_trail::record_at(
        trail,
        AuditEntryData {
            agent_id: agent.into(),
            tool: tool.into(),
            action: None,
            input_hash: audit_trail::hash_input(&serde_json::Map::new()),
            output_tokens: tokens,
            role: role.into(),
            event_type: AuditEventType::ToolCall,
        },
    );
}

fn audit_logging_is_automatic_and_hash_chained(trail: &std::path::Path) {
    use lean_ctx::core::audit_trail;
    record(trail, "agent-1", "ctx_read", 10, "developer");
    record(trail, "agent-1", "ctx_search", 5, "developer");
    assert_eq!(
        audit_trail::load_all_at(trail).len(),
        2,
        "every call recorded"
    );
    assert!(
        audit_trail::verify_chain_at(trail).valid,
        "hash chain verifies"
    );
}

fn audit_entries_attribute_agent_and_role(trail: &std::path::Path) {
    record(trail, "agent-42", "ctx_read", 1, "reviewer");
    let entries = lean_ctx::core::audit_trail::load_all_at(trail);
    let last = entries.last().expect("entry");
    assert_eq!(last.agent_id, "agent-42");
    assert_eq!(last.role, "reviewer");
}

fn tampered_audit_log_fails_verification(trail: &std::path::Path) {
    use lean_ctx::core::audit_trail;
    assert!(audit_trail::verify_chain_at(trail).valid);
    let content = std::fs::read_to_string(trail).expect("trail");
    let tampered = content.replacen("\"output_tokens\":10", "\"output_tokens\":999", 1);
    assert_ne!(content, tampered, "tamper fixture must change the log");
    std::fs::write(trail, tampered).expect("write");
    let result = audit_trail::verify_chain_at(trail);
    assert!(!result.valid, "tampered log MUST fail verification");
    assert!(result.first_invalid_at.is_some());
}

/// Risk- and security-relevant situations are first-class typed audit events
/// (their wire names are the registry contract).
#[test]
fn security_events_are_typed_audit_events() {
    use lean_ctx::core::audit_trail::AuditEventType;
    for (event, wire) in [
        (AuditEventType::ToolDenied, "tool_denied"),
        (AuditEventType::PathJailViolation, "path_jail_violation"),
        (AuditEventType::BudgetExceeded, "budget_exceeded"),
        (AuditEventType::SecurityViolation, "security_violation"),
        (AuditEventType::SecretDetected, "secret_detected"),
        (AuditEventType::RateLimited, "rate_limited"),
        (AuditEventType::CrossProjectAccess, "cross_project_access"),
        (AuditEventType::RoleChanged, "role_changed"),
    ] {
        assert_eq!(
            serde_json::to_value(&event).expect("serializes"),
            serde_json::Value::String(wire.to_string())
        );
    }
}

#[test]
fn role_changes_are_exported_events() {
    let event = LeanCtxEvent {
        id: 1,
        timestamp: chrono::Utc::now().to_rfc3339(),
        kind: EventKind::RoleChanged {
            from: "developer".into(),
            to: "reviewer".into(),
        },
    };
    assert!(ocp::export_event(&event).is_some());
}

/// Default-deny capability gate: a role without the capability is refused,
/// and the refusal is an exportable governance event.
#[test]
fn unauthorized_tool_use_is_blocked_and_exported() {
    use lean_ctx::core::capabilities::check_capabilities;
    let check = check_capabilities("reviewer", "ctx_edit");
    assert!(!check.allowed);
    assert!(!check.missing.is_empty());
    for tool in ["ctx_edit", "ctx_shell", "ctx_agent"] {
        assert!(
            !check_capabilities("minimal", tool).allowed,
            "{tool} must be denied for the read-only role"
        );
    }
    let event = LeanCtxEvent {
        id: 1,
        timestamp: chrono::Utc::now().to_rfc3339(),
        kind: EventKind::PolicyViolation {
            role: "reviewer".into(),
            tool: "ctx_edit".into(),
            reason: "missing capability fs:write".into(),
        },
    };
    let exported = ocp::export_event(&event).expect("violation exports");
    assert_eq!(exported["kind"]["type"], "policy_violation");
}

#[test]
fn capabilities_are_inspectable() {
    let grant = ocp::capability_grant_set("developer");
    let caps = grant["capabilities"].as_array().expect("array");
    assert!(
        !caps.is_empty(),
        "oversight needs a non-empty capability view"
    );
    assert_eq!(grant["subject"], "developer");
}

#[test]
fn context_ir_attributes_every_item() {
    use lean_ctx::core::context_ir::{ContextIrSourceKindV1, ContextIrV1, RecordIrInput};
    let mut ir = ContextIrV1::new();
    ir.record(RecordIrInput {
        kind: ContextIrSourceKindV1::Read,
        tool: "ctx_read",
        client_name: Some("cursor".into()),
        agent_id: Some("agent-1".into()),
        path: Some("src/main.rs"),
        command: None,
        pattern: None,
        input_tokens: 100,
        output_tokens: 10,
        duration: std::time::Duration::from_millis(1),
        content_excerpt: "fn main() {}",
    });
    let item = &ir.items[0];
    assert_eq!(item.source.tool, "ctx_read");
    assert!(
        item.verification.content_md5.is_some(),
        "verifiable excerpt"
    );
    assert!(item.safety.redacted, "redaction ran before storage");
}

#[test]
fn resolved_policy_is_exportable() {
    let policy = resolve(&builtin::get("strict-redaction").expect("pack")).expect("resolves");
    let json = serde_json::to_value(&policy).expect("resolved policy serializes");
    assert!(json["name"].is_string());
    assert!(json["redaction"].is_object());
}

/// The community redaction floor removes every credential class before content
/// reaches the model, and the compliance facts report exactly that.
#[test]
fn credential_classes_are_redacted_by_the_floor() {
    let policy = resolve(&builtin::get("strict-redaction").expect("pack")).expect("resolves");
    let facts = lean_ctx::core::compliance_evidence::facts::facts(&[], Some(&policy));
    let facts = facts.policy.expect("policy facts");
    assert!(facts.credential_classes.total > 0);
    assert!(
        facts.credential_classes.missing.is_empty(),
        "{:?}",
        facts.credential_classes
    );
    let bare = resolve(
        &lean_ctx::core::policy::parse(
            "name = \"bare\"\nversion = \"1.0.0\"\ndescription = \"no redaction\"\n",
        )
        .expect("parses"),
    )
    .expect("resolves");
    let bare_facts = lean_ctx::core::compliance_evidence::facts::facts(&[], Some(&bare));
    assert!(
        !bare_facts
            .policy
            .expect("policy facts")
            .credential_classes
            .missing
            .is_empty(),
        "a pack without redaction must be reported as such"
    );
}

/// An attempt outside the project jail is a typed, exportable violation.
#[test]
fn path_jail_violation_is_typed() {
    use lean_ctx::core::audit_trail::AuditEventType;
    assert_eq!(
        serde_json::to_value(AuditEventType::PathJailViolation).expect("serializes"),
        serde_json::Value::String("path_jail_violation".into())
    );
}
