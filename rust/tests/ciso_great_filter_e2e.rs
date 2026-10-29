//! End-to-end "durchspielbar" proof for **The Great Filter** (CISO product,
//! Epic #678). Real crypto, real audit chain — no mocks, no stubs.
//!
//! It walks the whole CISO golden path exactly as the MCP hot path
//! (`server::call_tool`) does, but driven from a test so it is CI-gated:
//!
//! 1. **#674 central, signed distribution** — an admin authors a CISO pack and
//!    ships it as an Ed25519-signed `OrgPolicyV1`. A signed-but-untrusted
//!    artifact is *not* applied: the configured but untrusted policy fails
//!    closed for agent operations while recovery tools remain available. Once
//!    the org key is trust-pinned it becomes an enforcement floor.
//! 2. **#673 runtime enforcement** — a denied tool is blocked; **#676** egress
//!    DLP stops a forbidden prod-DB action before dispatch; **#675** inbound
//!    filters detect + redact PII; pack `[redaction]` scrubs an employee id.
//!    Every decision is written to the append-only audit chain.
//! 3. **Audit evidence** — every one of those decisions is on the hash-chained
//!    audit trail with the right event type, and the chain verifies. (The
//!    signed compliance report built from it is part of LeanCTX Enterprise.)
//!
//! Local-Free Invariant: this only ever constrains the agent pipeline; the test
//! asserts the meta tools (`ctx_session`) can never be locked out.

use lean_ctx::core::audit_trail::{self, AuditEventType};
use lean_ctx::core::input_filters;
use lean_ctx::core::policy::org::{self, OrgPolicyV1};
use lean_ctx::core::policy::runtime;
use lean_ctx::server::policy_guard;

const ORG: &str = "ciso-bank";

/// A realistic, regulated CISO pack: deny outbound web fetches, redact inbound
/// PII, block writes to the production database, cap context + audit retention.
const CISO_PACK: &str = r#"
name = "ciso-bank-floor"
version = "1.0.0"
description = "CISO Great Filter: deny web egress, redact inbound PII, block prod-DB writes"
extends = "strict-redaction"

[context]
deny_tools = ["ctx_url_read"]
max_context_tokens = 12000
audit_retention_days = 365

[redaction]
employee_id = 'EMP-\d{4}'

[filters]
pii = "redact"
injection = "warn"

[egress]
forbidden_patterns = ['prod\.db\.internal']
block_secrets = true
max_writes_per_min = 120
"#;

const ENV_VARS: &[&str] = &[
    "LEAN_CTX_DATA_DIR",
    "LEAN_CTX_CONFIG_DIR",
    "LEAN_CTX_STATE_DIR",
    "LEAN_CTX_CACHE_DIR",
];

#[test]
fn great_filter_golden_path_enforces_and_attests() {
    let tmp = std::env::temp_dir().join(format!("lctx-ciso-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir temp dir");
    let original_cwd = std::env::current_dir().expect("cwd");

    // SAFETY: single-threaded integration-test binary; env + cwd restored below.
    // All four XDG categories + cwd point at the temp dir so config (org store /
    // trust), data (audit chain + keystore) and the local-pack lookup are
    // hermetic and never touch the developer machine.
    unsafe {
        for var in ENV_VARS {
            std::env::set_var(var, &tmp);
        }
        std::env::set_var("LEAN_CTX_AGENT_ID", "ciso-machine");
    }
    std::env::set_current_dir(&tmp).expect("chdir temp");

    // ── 1. Central, SIGNED org policy distribution (#674) ─────────────────────
    let mut artifact =
        OrgPolicyV1::build(ORG, "2026.06.1", true, CISO_PACK).expect("pack is valid + resolvable");
    artifact.sign().expect("sign with org key");
    assert!(
        artifact.verify().signature_valid,
        "org artifact self-verifies offline"
    );
    let signer = artifact
        .signer_public_key
        .clone()
        .expect("artifact is signed");

    org::store::install(&artifact).expect("install signed artifact");

    // A configured but untrusted artifact must not grant admission or supply
    // policy rules. The runtime deliberately fails closed, not open; pinning
    // the signer below is what authorizes applying the actual org policy.
    assert_eq!(
        org::active_resolved_checked().unwrap_err(),
        "org policy signer is not pinned for this organization",
        "the rejection must be the missing org trust anchor"
    );
    runtime::reload();
    let rejected = runtime::active().expect("untrusted configured policy fails closed");
    assert!(
        !rejected.tool_allowed("ctx_read") && !rejected.tool_allowed("ctx_url_read"),
        "untrusted configured policy must not allow ordinary agent operations"
    );
    assert!(
        rejected.redaction.is_empty(),
        "untrusted artifact rules are not applied"
    );
    assert!(
        policy_guard::check_tool_access("ctx_read").blocked,
        "agent admission fails closed before trust pinning"
    );
    assert!(
        !policy_guard::check_tool_access("ctx_session").blocked,
        "recovery remains available before trust pinning"
    );

    // Pin the org's key out-of-band → the floor becomes un-bypassable.
    org::trust::pin(ORG, &signer).expect("pin org trust anchor");
    runtime::reload();
    let active = runtime::active().expect("signed + trusted + enforced → active floor");
    assert!(
        active
            .resolved
            .deny_tools
            .iter()
            .any(|t| t == "ctx_url_read"),
        "deny list folded in"
    );
    assert!(active.filters.is_active(), "[filters] compiled + active");
    assert!(active.egress.is_active(), "[egress] compiled + active");

    // ── 2. Runtime ENFORCEMENT on the agent pipeline (mirrors call_tool) ──────
    // (a) #673 tool gating: a denied tool is blocked + audited; allowed tools
    //     pass; meta tools can never be locked out (Local-Free recovery).
    assert!(
        policy_guard::check_tool_access("ctx_url_read").blocked,
        "denied tool is blocked"
    );
    assert!(
        !policy_guard::check_tool_access("ctx_read").blocked,
        "allowed tool passes"
    );
    assert!(
        !policy_guard::check_tool_access("ctx_session").blocked,
        "meta tool is never policy-denied"
    );

    // (b) #676 egress DLP: a forbidden prod-DB action is stopped before dispatch.
    let action = "psql postgres://prod.db.internal/main -c 'select * from accounts'";
    let reason = active
        .egress
        .check_content(action, &active.redaction)
        .expect("forbidden prod-DB action is blocked");
    assert!(reason.starts_with("forbidden-pattern:"), "blocked by rule");
    policy_guard::audit_egress("ctx_shell", &reason);

    // (c) #675 inbound filters: PII is detected and redacted (not blocked, since
    //     the pack chose `redact`), and the decision is audited.
    let tool_output =
        "Customer jane.roe@example.com, IBAN DE89370400440532013000, approved by EMP-4711.";
    let outcome = input_filters::apply(tool_output, &active.filters);
    assert!(!outcome.blocked, "redact lets scrubbed content through");
    assert!(!outcome.audit.is_empty(), "PII detected");
    assert_ne!(outcome.text, tool_output, "PII spans were redacted");
    policy_guard::audit_filter("ctx_read", &outcome.audit, outcome.blocked);

    // (d) pack [redaction]: outbound content has the employee id scrubbed.
    let (redacted, hits) = policy_guard::redact_result("change approved by EMP-4711");
    assert!(
        hits >= 1 && redacted.contains("[REDACTED:employee_id]"),
        "employee id redacted: {redacted}"
    );

    // ── 3. Audit evidence: every decision recorded, chain intact ──────────────
    let entries = audit_trail::load_recent(10_000);
    let blocked = entries
        .iter()
        .filter(|e| matches!(e.event_type, AuditEventType::ToolDenied))
        .count();
    assert_eq!(
        blocked, 3,
        "pre-trust denial + trusted tool-deny + egress block must each be recorded"
    );
    assert!(
        entries
            .iter()
            .any(|e| matches!(e.event_type, AuditEventType::SecretDetected)),
        "PII filter decision recorded"
    );
    let chain = audit_trail::verify_chain();
    assert!(
        chain.valid && chain.source_present,
        "audit hash chain intact"
    );

    // ── teardown ──────────────────────────────────────────────────────────────
    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    // SAFETY: single-threaded integration-test binary; restores the env set in
    // the setup block above, at the end of this file's only test.
    unsafe {
        for var in ENV_VARS {
            std::env::remove_var(var);
        }
        std::env::remove_var("LEAN_CTX_AGENT_ID");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
