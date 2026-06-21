//! Contract tests: verify that generated rules for every IDE are consistent.
//!
//! Checks:
//! - Rules contain mode selection guidance
//! - Rules contain NEVER anti-pattern reinforcement
//! - MDC template has lean-ctx markers
//! - No contradictions in hybrid mode (`ctx_shell` vs lean-ctx -c)

use lean_ctx::rules_inject;

#[test]
fn shared_rules_contain_mode_guidance() {
    let content = rules_inject::rules_shared_content();
    assert!(
        content.contains("Mode Selection"),
        "RULES_SHARED must contain Mode Selection section"
    );
    assert!(
        content.contains("NEVER"),
        "RULES_SHARED must contain NEVER anti-pattern"
    );
}

#[test]
fn dedicated_rules_contain_modes_and_anti_patterns() {
    let content = rules_inject::rules_dedicated_markdown();
    assert!(
        content.contains("Mode Selection"),
        "RULES_DEDICATED must contain Mode Selection section"
    );
    assert!(
        content.contains("full"),
        "RULES_DEDICATED must mention full mode"
    );
    assert!(
        content.contains("map"),
        "RULES_DEDICATED must mention map mode"
    );
    assert!(
        content.contains("NEVER"),
        "RULES_DEDICATED must contain NEVER anti-pattern"
    );
}

#[test]
fn all_rules_contain_never_reinforcement() {
    let shared = rules_inject::rules_shared_content();
    let dedicated = rules_inject::rules_dedicated_markdown();

    assert!(
        shared.contains("NEVER"),
        "shared rules must contain NEVER reinforcement"
    );
    assert!(
        dedicated.contains("NEVER"),
        "dedicated rules must contain NEVER reinforcement"
    );
}

#[test]
fn canonical_hybrid_no_ctx_shell_as_must_use() {
    let table =
        lean_ctx::core::rules_canonical::tool_table(lean_ctx::core::rules_canonical::Mode::Hybrid);
    for line in table.lines() {
        assert!(
            !line.starts_with("| `ctx_shell"),
            "Hybrid table must not list ctx_shell in MUST USE column (first column)"
        );
    }
}

#[test]
fn canonical_mcp_no_lean_ctx_c_preferred() {
    let table =
        lean_ctx::core::rules_canonical::tool_table(lean_ctx::core::rules_canonical::Mode::Mcp);
    assert!(
        !table.contains("lean-ctx -c"),
        "MCP table must not list lean-ctx -c (should use ctx_shell)"
    );
}

#[test]
fn canonical_mcp_instructions_are_concise() {
    for mode in [
        lean_ctx::core::rules_canonical::Mode::Hybrid,
        lean_ctx::core::rules_canonical::Mode::Mcp,
    ] {
        let instructions = lean_ctx::core::rules_canonical::mcp_instructions(mode);
        assert!(
            instructions.contains("replace") || instructions.contains("lean-ctx"),
            "MCP instructions for {mode:?} must reference tool replacement"
        );
        assert!(
            instructions.len() < 250,
            "MCP instructions for {mode:?} should be concise (< 250 chars), got {}",
            instructions.len()
        );
    }
}

#[test]
fn cursor_mdc_template_has_lean_ctx_markers() {
    let mdc = include_str!("../src/templates/lean-ctx.mdc");
    assert!(mdc.contains("lean-ctx"), "Cursor MDC must mention lean-ctx");
    assert!(mdc.contains("ctx_read"), "Cursor MDC must mention ctx_read");
    assert!(
        mdc.contains("Tool Mapping") || mdc.contains("Mode Selection"),
        "Cursor MDC must have Tool Mapping or Mode Selection"
    );
}

#[test]
fn hybrid_mdc_template_has_lean_ctx_markers() {
    let mdc = include_str!("../src/templates/lean-ctx-hybrid.mdc");
    assert!(mdc.contains("lean-ctx"), "Hybrid MDC must mention lean-ctx");
}

#[test]
fn no_contradictions_in_hybrid_mdc() {
    let mdc = include_str!("../src/templates/lean-ctx-hybrid.mdc");
    for line in mdc.lines() {
        assert!(
            !line.starts_with("| `ctx_shell"),
            "Hybrid MDC must not list ctx_shell in MUST USE column (first column)"
        );
    }
}
