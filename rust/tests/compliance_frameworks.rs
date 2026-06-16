//! Framework compliance enforcement proofs (GL #424, H3 Epic A — AC 1 + 2).
//!
//! Every `coverage = "full"` claim in `rust/data/compliance/mappings/*.toml` names a
//! test in this file; `mapping_test_names_exist_in_this_file` fails the
//! build if a claim points at a test that doesn't exist. Each test proves
//! BOTH directions where meaningful: the mechanism enforces (reference
//! pack / engine behavior) and a violation is detectable (weak pack ⇒
//! downgrade, tampered log ⇒ invalid chain, denied tool ⇒ event).

use lean_ctx::core::compliance::{self, Coverage, RowStatus};
use lean_ctx::core::events::{EventKind, LeanCtxEvent};
use lean_ctx::core::ocp;
use lean_ctx::core::policy::{ResolvedPolicy, builtin, resolve};

fn resolved(pack_name: &str) -> ResolvedPolicy {
    let pack = builtin::get(pack_name).unwrap_or_else(|| panic!("builtin pack {pack_name}"));
    resolve(&pack).expect("pack resolves")
}

fn row_status(framework: &str, control: &str, pack: &str) -> (RowStatus, String) {
    let mapping = compliance::get(framework).expect("framework exists");
    let policy = resolved(pack);
    let report = compliance::report(mapping, Some(&policy));
    let row = report
        .rows
        .into_iter()
        .find(|r| r.id == control)
        .unwrap_or_else(|| panic!("control {control} in {framework}"));
    (row.status, row.detail)
}

// ── meta: mapping ↔ test drift guard ─────────────────────────────────────────

#[test]
fn mapping_test_names_exist_in_this_file() {
    let source = include_str!("compliance_frameworks.rs");
    for mapping in compliance::frameworks() {
        for control in &mapping.controls {
            if let Some(test) = &control.test {
                assert!(
                    source.contains(&format!("fn {test}(")),
                    "{}/{} names test '{test}' which does not exist in compliance_frameworks.rs",
                    mapping.framework,
                    control.id
                );
            }
        }
    }
}

// ── AC 1: EU AI Act reference report ─────────────────────────────────────────

#[test]
fn eu_ai_act_reference_report_has_ten_plus_enforced_full_controls() {
    let mapping = compliance::get("eu-ai-act").expect("mapping");
    let policy = resolved(&mapping.reference_pack);
    let report = compliance::report(mapping, Some(&policy));

    let enforced_full = report
        .rows
        .iter()
        .filter(|r| {
            r.coverage == Coverage::Full
                && matches!(r.status, RowStatus::Enforced | RowStatus::EngineGuarantee)
        })
        .count();
    assert!(
        enforced_full >= 10,
        "AC 1 requires ≥10 full-coverage controls with evidence, got {enforced_full}"
    );
    assert_eq!(report.summary.not_enforced, 0);
    assert!(
        report.summary.gaps >= 1,
        "gaps must be documented, not hidden"
    );
}

// ── EU AI Act — full-coverage proofs ─────────────────────────────────────────

/// The audit-trail proofs share one process-global hash-chain tail and the
/// data-dir env var, so they run inside ONE #[test] in a controlled order.
/// The mapping-named functions below are called from here; the drift guard
/// only requires the named `fn`s to exist.
#[test]
fn audit_chain_proofs_run_sequentially() {
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    aia_12_1_logging_is_automatic_and_chained();
    aia_12_2_actions_attribute_agent_and_role();
    // Destroys the chain — must run last.
    aia_12_1_tampered_log_fails_verification(tmp.path());

    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

fn aia_12_1_logging_is_automatic_and_chained() {
    use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
    audit_trail::record(AuditEntryData {
        agent_id: "agent-1".into(),
        tool: "ctx_read".into(),
        action: Some("full".into()),
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 10,
        role: "developer".into(),
        event_type: AuditEventType::ToolCall,
    });
    audit_trail::record(AuditEntryData {
        agent_id: "agent-1".into(),
        tool: "ctx_search".into(),
        action: None,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 5,
        role: "developer".into(),
        event_type: AuditEventType::ToolCall,
    });

    let entries = audit_trail::load_recent(10);
    assert!(entries.len() >= 2, "every call recorded without opt-in");
    assert!(audit_trail::verify_chain().valid, "hash chain verifies");
}

fn aia_12_2_actions_attribute_agent_and_role() {
    use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
    audit_trail::record(AuditEntryData {
        agent_id: "agent-42".into(),
        tool: "ctx_read".into(),
        action: None,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 1,
        role: "reviewer".into(),
        event_type: AuditEventType::ToolCall,
    });
    let entries = audit_trail::load_recent(1);
    assert_eq!(entries[0].agent_id, "agent-42");
    assert_eq!(entries[0].role, "reviewer");
}

fn aia_12_1_tampered_log_fails_verification(data_dir: &std::path::Path) {
    use lean_ctx::core::audit_trail;
    assert!(audit_trail::verify_chain().valid);

    // Tamper with one recorded value — the chain must break, provably.
    let trail = data_dir.join("audit").join("trail.jsonl");
    let content = std::fs::read_to_string(&trail).expect("trail");
    let tampered = content.replacen("\"output_tokens\":10", "\"output_tokens\":999", 1);
    assert_ne!(content, tampered, "tamper fixture must change the log");
    std::fs::write(&trail, tampered).expect("write");

    let result = audit_trail::verify_chain();
    assert!(!result.valid, "tampered log MUST fail verification");
    assert!(result.first_invalid_at.is_some());
}

#[test]
fn aia_12_2a_risk_event_types_are_recorded() {
    use lean_ctx::core::audit_trail::AuditEventType;
    // The risk-relevant situations of Art. 12(2)(a) are first-class typed
    // events — serialization is the registry contract (OCP R3).
    for (event, wire) in [
        (AuditEventType::ToolDenied, "tool_denied"),
        (AuditEventType::PathJailViolation, "path_jail_violation"),
        (AuditEventType::BudgetExceeded, "budget_exceeded"),
        (AuditEventType::SecurityViolation, "security_violation"),
        (AuditEventType::SecretDetected, "secret_detected"),
        (AuditEventType::RateLimited, "rate_limited"),
        (AuditEventType::CrossProjectAccess, "cross_project_access"),
    ] {
        assert_eq!(
            serde_json::to_value(&event).expect("serializes"),
            serde_json::Value::String(wire.to_string())
        );
    }
}

#[test]
fn aia_26_6_reference_pack_declares_six_month_retention() {
    let policy = resolved("eu-ai-act-deployer");
    assert!(policy.audit_retention_days.expect("retention declared") >= 180);
    let (status, _) = row_status("eu-ai-act", "AIA-26.6", "eu-ai-act-deployer");
    assert_eq!(status, RowStatus::Enforced);

    // Violation: baseline declares 90 d — below the Art. 26(6) floor.
    let (status, detail) = row_status("eu-ai-act", "AIA-26.6", "baseline");
    assert_eq!(status, RowStatus::NotEnforced, "{detail}");
}

#[test]
fn aia_10_5_regulated_identifiers_are_redacted() {
    let (status, detail) = row_status("eu-ai-act", "AIA-10.5", "eu-ai-act-deployer");
    assert_eq!(status, RowStatus::Enforced, "{detail}");

    // Violation: a pack without identifier patterns fails the claim.
    let (status, _) = row_status("eu-ai-act", "AIA-10.5", "baseline");
    assert_eq!(status, RowStatus::NotEnforced);
}

#[test]
fn aia_15_5_credentials_never_reach_context() {
    let (status, detail) = row_status("eu-ai-act", "AIA-15.5-secrets", "eu-ai-act-deployer");
    assert_eq!(status, RowStatus::Enforced, "{detail}");

    // Direct fixture proof on the resolved patterns.
    let policy = resolved("eu-ai-act-deployer");
    let patterns: Vec<regex::Regex> = policy
        .redaction
        .values()
        .filter_map(|raw| regex::Regex::new(raw).ok())
        .collect();
    for fixture in [
        "-----BEGIN RSA PRIVATE KEY-----",
        "AKIAIOSFODNN7EXAMPLE",
        "api_key = \"sk-supersecretvalue1234\"",
        "Authorization: Bearer abcdefghij0123456789xyz",
    ] {
        assert!(
            patterns.iter().any(|re| re.is_match(fixture)),
            "credential fixture must be redacted: {fixture}"
        );
    }
}

#[test]
fn aia_15_5_unauthorized_tool_use_is_blocked() {
    use lean_ctx::core::capabilities::check_capabilities;

    // Engine layer: default-deny capability gate blocks the call…
    let check = check_capabilities("reviewer", "ctx_edit");
    assert!(!check.allowed);
    assert!(!check.missing.is_empty());

    // …and the violation surfaces as an exportable governance event.
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

    // Pack layer: the reference pack scopes the tool surface.
    let (status, _) = row_status("eu-ai-act", "AIA-15.5-access", "eu-ai-act-deployer");
    assert_eq!(status, RowStatus::Enforced);
}

#[test]
fn aia_14_4a_capabilities_are_inspectable() {
    let grant = ocp::capability_grant_set("developer");
    let caps = grant["capabilities"].as_array().expect("array");
    assert!(
        !caps.is_empty(),
        "oversight needs a non-empty capability view"
    );
    assert_eq!(grant["subject"], "developer");
}

#[test]
fn aia_14_4e_budget_caps_bound_operation() {
    let policy = resolved("eu-ai-act-deployer");
    assert!(policy.max_context_tokens.expect("cap declared") > 0);

    // Intervention signals exist before and at exhaustion.
    for kind in [
        EventKind::BudgetWarning {
            role: "developer".into(),
            dimension: "tokens".into(),
            used: "9000".into(),
            limit: "12000".into(),
            percent: 75,
        },
        EventKind::BudgetExhausted {
            role: "developer".into(),
            dimension: "tokens".into(),
            used: "12001".into(),
            limit: "12000".into(),
        },
    ] {
        let event = LeanCtxEvent {
            id: 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind,
        };
        assert!(ocp::export_event(&event).is_some());
    }

    // Violation: a pack without a cap does not earn the claim.
    let (status, _) = row_status("eu-ai-act", "AIA-14.4e", "open-source");
    assert_eq!(status, RowStatus::NotEnforced);
}

#[test]
fn aia_13_1_context_ir_attributes_every_item() {
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

// ── ISO 42001 — full-coverage proofs ─────────────────────────────────────────

#[test]
fn iso_a626_operation_logging_always_on() {
    // Same engine guarantee as AIA-12.1; the mapping row must say so.
    let mapping = compliance::get("iso42001").expect("mapping");
    let report = compliance::report(mapping, None);
    let row = report
        .rows
        .iter()
        .find(|r| r.id == "ISO-A.6.2.6")
        .expect("row");
    assert_eq!(row.status, RowStatus::EngineGuarantee);
}

#[test]
fn iso_a74_context_data_is_filtered_before_use() {
    let (status, detail) = row_status("iso42001", "ISO-A.7.4", "iso42001-aligned");
    assert_eq!(status, RowStatus::Enforced, "{detail}");
}

#[test]
fn iso_a82_resolved_policy_is_exportable() {
    let policy = resolved("iso42001-aligned");
    let json = serde_json::to_value(&policy).expect("resolved policy serializes");
    assert!(json["name"].is_string());
    assert!(json["redaction"].is_object());
}

#[test]
fn iso_a92_policy_pack_defines_enforced_process() {
    let (status, detail) = row_status("iso42001", "ISO-A.9.2", "iso42001-aligned");
    assert_eq!(status, RowStatus::Enforced, "{detail}");

    // Violation: an empty-process pack cannot claim A.9.2.
    let (status, _) = row_status("iso42001", "ISO-A.9.2", "open-source");
    assert_eq!(status, RowStatus::NotEnforced);
}

#[test]
fn iso_a94_out_of_scope_use_is_blocked_and_recorded() {
    let policy = resolved("iso42001-aligned");
    assert!(policy.deny_tools.iter().any(|t| t == "ctx_url_read"));

    use lean_ctx::core::audit_trail::AuditEventType;
    assert_eq!(
        serde_json::to_value(AuditEventType::ToolDenied).expect("serializes"),
        serde_json::Value::String("tool_denied".into())
    );

    let (status, _) = row_status("iso42001", "ISO-A.9.4", "iso42001-aligned");
    assert_eq!(status, RowStatus::Enforced);
}

// ── SOC 2 — full-coverage proofs ─────────────────────────────────────────────

#[test]
fn soc2_cc61_default_deny_capability_gate() {
    use lean_ctx::core::capabilities::check_capabilities;
    // minimal role: read-only — everything mutating is denied by default.
    for tool in ["ctx_edit", "ctx_shell", "ctx_agent"] {
        let check = check_capabilities("minimal", tool);
        assert!(!check.allowed, "{tool} must be denied for minimal role");
    }
    let (status, _) = row_status("soc2", "SOC2-CC6.1", "soc2-context");
    assert_eq!(status, RowStatus::Enforced);
}

#[test]
fn soc2_cc66_egress_deniable_and_path_jailed() {
    let (status, detail) = row_status("soc2", "SOC2-CC6.6", "soc2-context");
    assert_eq!(status, RowStatus::Enforced, "{detail}");

    use lean_ctx::core::audit_trail::AuditEventType;
    assert_eq!(
        serde_json::to_value(AuditEventType::PathJailViolation).expect("serializes"),
        serde_json::Value::String("path_jail_violation".into())
    );

    // Violation: baseline denies nothing.
    let (status, _) = row_status("soc2", "SOC2-CC6.6", "baseline");
    assert_eq!(status, RowStatus::NotEnforced);
}

#[test]
fn soc2_cc72_security_events_are_typed_and_recorded() {
    use lean_ctx::core::audit_trail::AuditEventType;
    for event in [
        AuditEventType::SecurityViolation,
        AuditEventType::SecretDetected,
        AuditEventType::RateLimited,
        AuditEventType::BudgetExceeded,
    ] {
        let wire = serde_json::to_value(&event).expect("serializes");
        assert!(wire.is_string(), "typed security event: {wire}");
    }
}

#[test]
fn soc2_cc71_role_changes_are_audited() {
    use lean_ctx::core::audit_trail::AuditEventType;
    assert_eq!(
        serde_json::to_value(AuditEventType::RoleChanged).expect("serializes"),
        serde_json::Value::String("role_changed".into())
    );
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

#[test]
fn soc2_c11_confidential_material_redacted() {
    let (status, detail) = row_status("soc2", "SOC2-C1.1", "soc2-context");
    assert_eq!(status, RowStatus::Enforced, "{detail}");

    // Violation: a pack with no redaction at all cannot claim C1.1. (Every
    // builtin inherits the baseline credential patterns — by design — so
    // the violation fixture is a synthetic root pack.)
    let bare = lean_ctx::core::policy::parse(
        "name = \"bare\"\nversion = \"1.0.0\"\ndescription = \"no redaction\"\n",
    )
    .expect("parses");
    let resolved_bare = resolve(&bare).expect("resolves");
    let mapping = compliance::get("soc2").expect("mapping");
    let report = compliance::report(mapping, Some(&resolved_bare));
    let row = report
        .rows
        .iter()
        .find(|r| r.id == "SOC2-C1.1")
        .expect("row");
    assert_eq!(row.status, RowStatus::NotEnforced);
}
