//! Security hardening tests — validates all Critical and High fixes from the
//! bank-readiness audit (2026-05-08).

/// Reads the full `LeanCtxServer` dispatch source across its split submodules
/// (`mod.rs`, `call_tool.rs`, `server_handler.rs`, plus the extracted
/// `post_process.rs` / `post_dispatch.rs` stages) so that the security-invariant
/// checks below stay robust to internal module structure.
fn server_dispatch_src() -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        include_str!("../../src/server/mod.rs"),
        include_str!("../../src/server/call_tool.rs"),
        include_str!("../../src/server/server_handler.rs"),
        include_str!("../../src/server/post_process.rs"),
        include_str!("../../src/server/post_dispatch.rs"),
    )
}

// ---------------------------------------------------------------------------
// C1 — Dashboard: token not leaked without valid ?token= query
// ---------------------------------------------------------------------------
#[test]
fn dashboard_route_response_omits_token_without_valid_query() {
    let src = include_str!("../../src/dashboard/routes/mod.rs");

    assert!(
        src.contains("is_some_and(|q| super::constant_time_eq(q.as_bytes(), expected.as_bytes()))"),
        "C1: dashboard must use constant_time_eq via is_some_and for query token validation"
    );
    assert!(
        src.contains("if valid_query"),
        "C1: token embedding gated on valid_query (no loopback bypass)"
    );
    assert!(
        !src.contains("if valid_query || is_loopback"),
        "C1: loopback must NOT bypass token validation — removed for security"
    );
    assert!(
        src.contains("_is_loopback: bool"),
        "C1: route_response signature accepts _is_loopback (unused, kept for API compat)"
    );
}

#[test]
fn dashboard_api_auth_never_bypassed_for_loopback() {
    let src = include_str!("../../src/dashboard/mod.rs");
    assert!(
        !src.contains("if requires_auth && !has_header_auth && !is_loopback"),
        "C1: API auth must NOT be bypassed for loopback — only HTML token injection is allowed"
    );
    assert!(
        src.contains("if requires_auth && !has_header_auth"),
        "C1: API auth gate must remain unconditional (no loopback exception)"
    );
}

#[test]
fn dashboard_check_auth_uses_constant_time_eq() {
    let src = include_str!("../../src/dashboard/mod.rs");
    assert!(
        src.contains("constant_time_eq(token.trim().as_bytes(), expected_token.as_bytes())"),
        "C1: check_auth must use constant_time_eq, not plain =="
    );
}

#[test]
fn dashboard_probe_sends_bearer_token() {
    let src = include_str!("../../src/dashboard/mod.rs");
    assert!(
        src.contains("Authorization: Bearer {t}"),
        "C1: dashboard_responding probe must send saved Bearer token"
    );
}

#[test]
fn dashboard_metrics_requires_auth() {
    let src = include_str!("../../src/dashboard/mod.rs");
    assert!(
        src.contains(r#"path == "/metrics""#),
        "C1: /metrics must be in the requires_auth path"
    );
}

// ---------------------------------------------------------------------------
// C2 — Team server: workspace enforced from TeamRequestContext
// ---------------------------------------------------------------------------
#[test]
fn context_views_use_resolve_workspace() {
    let src = include_str!("../../src/http_server/context_views.rs");

    assert!(
        src.contains("fn resolve_workspace"),
        "C2: context_views must have a resolve_workspace helper"
    );
    let search_fn = src
        .find("v1_events_search")
        .expect("v1_events_search missing");
    let search_body = &src[search_fn..search_fn + 600];
    assert!(
        search_body.contains("resolve_workspace("),
        "C2: v1_events_search must call resolve_workspace"
    );
}

#[test]
fn lineage_filters_by_workspace() {
    let src = include_str!("../../src/core/context_os/context_bus.rs");

    let lineage_fn = src.find("fn lineage(").expect("lineage fn missing");
    let lineage_sig = &src[lineage_fn..lineage_fn + 200];
    assert!(
        lineage_sig.contains("workspace_id: &str"),
        "C2: lineage() must take workspace_id parameter"
    );

    let lineage_body = &src[lineage_fn..lineage_fn + 800];
    assert!(
        lineage_body.contains("AND workspace_id = ?2"),
        "C2: lineage SQL must filter by workspace_id"
    );
}

// ---------------------------------------------------------------------------
// H1 — Shell CWD jail enforcement
// ---------------------------------------------------------------------------
#[test]
fn effective_cwd_calls_jail() {
    let src = include_str!("../../src/core/session/state.rs");

    // `effective_cwd` must not bypass the jail: it delegates to the checked
    // variant, which is the single enforcement point (#629/#633 refactor).
    let ecwd_fn = src
        .find("fn effective_cwd(")
        .expect("effective_cwd missing");
    let ecwd_body = &src[ecwd_fn..ecwd_fn + 500];
    assert!(
        ecwd_body.contains("effective_cwd_checked("),
        "H1: effective_cwd must delegate to effective_cwd_checked"
    );

    // The checked variant enforces the project-root jail for an explicit cwd.
    let checked_fn = src
        .find("fn effective_cwd_checked(")
        .expect("effective_cwd_checked missing");
    let checked_body = &src[checked_fn..checked_fn + 500];
    assert!(
        checked_body.contains("jail_cwd(cwd, root)"),
        "H1: effective_cwd_checked must call jail_cwd for explicit cwd"
    );
}

#[test]
fn update_shell_cwd_calls_jail_path() {
    let src = include_str!("../../src/core/session/state.rs");

    let uscwd_fn = src
        .find("fn update_shell_cwd(")
        .expect("update_shell_cwd missing");
    let uscwd_body = &src[uscwd_fn..uscwd_fn + 600];
    assert!(
        uscwd_body.contains("jail_path("),
        "H1: update_shell_cwd must jail_path check before storing"
    );
}

// ---------------------------------------------------------------------------
// H2 — MCP ctx_read path has secret check
// ---------------------------------------------------------------------------
#[test]
fn resolve_path_includes_secret_check() {
    let src = include_str!("../../src/tools/server_paths.rs");

    let resolve_fn = src.find("fn resolve_path(").expect("resolve_path missing");
    let resolve_body = &src[resolve_fn..];
    let end = resolve_body
        .find("fn resolve_path_or_passthrough")
        .unwrap_or(resolve_body.len());
    let resolve_body = &resolve_body[..end];
    assert!(
        resolve_body.contains("check_secret_path_for_tool"),
        "H2: resolve_path must call check_secret_path_for_tool"
    );
}

// ---------------------------------------------------------------------------
// H3 — REST event responses are redacted
// ---------------------------------------------------------------------------
#[test]
fn event_search_applies_redaction() {
    let src = include_str!("../../src/http_server/context_views.rs");

    let search_fn = src
        .find("v1_events_search")
        .expect("v1_events_search missing");
    let search_body = &src[search_fn..search_fn + 800];
    assert!(
        search_body.contains("redact_event_payload("),
        "H3: v1_events_search must redact event payloads"
    );
}

#[test]
fn event_lineage_applies_redaction() {
    let src = include_str!("../../src/http_server/context_views.rs");

    let lineage_fn = src
        .find("v1_event_lineage")
        .expect("v1_event_lineage missing");
    let lineage_body = &src[lineage_fn..lineage_fn + 800];
    assert!(
        lineage_body.contains("redact_event_payload("),
        "H3: v1_event_lineage must redact event payloads"
    );
}

// ---------------------------------------------------------------------------
// H4 — JSON-RPC batch scope bypass prevention
// ---------------------------------------------------------------------------
#[test]
fn team_auth_rejects_batch_requests() {
    let src = include_str!("../../src/http_server/team/mod.rs");

    assert!(
        src.contains("batch_requests_not_supported"),
        "H4: team auth must reject JSON-RPC batch (array) requests"
    );
    assert!(
        src.contains("let mut allow = false;"),
        "H4: team auth must default allow to false"
    );
}

// ---------------------------------------------------------------------------
// H5 — npm postinstall SHA256 verification
// ---------------------------------------------------------------------------
#[test]
fn postinstall_has_sha256_verification() {
    let src = include_str!("../../../packages/lean-ctx-bin/postinstall.js");

    assert!(
        src.contains("createHash(\"sha256\")") || src.contains("createHash('sha256')"),
        "H5: postinstall.js must compute SHA256 hash"
    );
    assert!(
        src.contains("SHA256SUMS"),
        "H5: postinstall.js must download SHA256SUMS for verification"
    );
    assert!(
        src.contains("SHA256 mismatch"),
        "H5: postinstall.js must abort on hash mismatch"
    );
}

// ---------------------------------------------------------------------------
// H6 — Pipeline archive redaction
// ---------------------------------------------------------------------------
#[test]
fn pipeline_archive_uses_redacted_output() {
    let src = crate::suite::security_hardening::server_dispatch_src();

    assert!(
        src.contains("redact_text_if_enabled"),
        "H6: shell archive must use redact_text_if_enabled before storing"
    );
}

// ---------------------------------------------------------------------------
// M2 — ReDoS guard in ctx_search
// ---------------------------------------------------------------------------
#[test]
fn ctx_search_has_pattern_length_limit() {
    let src = include_str!("../../src/tools/ctx_search.rs");

    assert!(
        src.contains("MAX_PATTERN_LEN"),
        "M2: ctx_search must limit pattern length"
    );
    assert!(
        src.contains(".size_limit("),
        "M2: ctx_search must set regex size_limit"
    );
    assert!(
        src.contains("RegexBuilder::new("),
        "M2: ctx_search must use RegexBuilder (not Regex::new) for size limits"
    );
}

// ---------------------------------------------------------------------------
// M3 — MCP stdio max_length
// ---------------------------------------------------------------------------
#[test]
fn mcp_stdio_has_bounded_max_length() {
    let src = include_str!("../../src/mcp_stdio.rs");

    assert!(
        !src.contains("max_length: usize::MAX"),
        "M3: MCP stdio must NOT use usize::MAX for max_length"
    );
    assert!(
        src.contains("32 * 1024 * 1024"),
        "M3: MCP stdio max_length must be 32 MiB"
    );
}

// ---------------------------------------------------------------------------
// M5 — UDS socket permissions (now in ipc/unix.rs)
// ---------------------------------------------------------------------------
#[test]
fn uds_socket_sets_permissions() {
    let src = include_str!("../../src/ipc/unix.rs");

    assert!(
        src.contains("PermissionsExt"),
        "M5: ipc/unix.rs must import PermissionsExt"
    );
    assert!(
        src.contains("0o600"),
        "M5: ipc/unix.rs must set socket permissions to 0o600"
    );
}

// ---------------------------------------------------------------------------
// L1 — Error responses sanitized
// ---------------------------------------------------------------------------
#[test]
fn http_server_sanitizes_error_responses() {
    let src = include_str!("../../src/http_server/mod.rs");

    let v1_fn = src.find("v1_tool_call").expect("v1_tool_call missing");
    let v1_body = &src[v1_fn..v1_fn + 600];
    assert!(
        !v1_body.contains("e.to_string()"),
        "L1: v1_tool_call must not return internal error details"
    );
}

// ---------------------------------------------------------------------------
// CSP nonce injection for ALL inline scripts
// ---------------------------------------------------------------------------
#[test]
fn csp_nonce_covers_all_inline_scripts() {
    let src = include_str!("../../src/dashboard/mod.rs");
    assert!(
        src.contains("add_nonce_to_inline_scripts"),
        "dashboard must use add_nonce_to_inline_scripts for all inline scripts"
    );
    assert!(
        !src.contains(r"<script>window.__LEAN_CTX_TOKEN__")
            || src.contains("add_nonce_to_inline_scripts"),
        "token script nonce must go through add_nonce_to_inline_scripts, not ad-hoc replace"
    );
}

#[test]
fn add_nonce_skips_external_scripts() {
    let html = r#"<script src="foo.js"></script><script>inline()</script><script type="module">boot()</script>"#;
    let nonce = ["abc", "123"].concat();
    let result = lean_ctx::dashboard::add_nonce_to_inline_scripts(html, &nonce);
    assert!(
        result.contains(r#"<script src="foo.js">"#),
        "external script must NOT get nonce: {result}"
    );
    assert!(
        result.contains(&format!(r#"<script nonce="{nonce}">inline()"#)),
        "inline script must get nonce: {result}"
    );
    assert!(
        result.contains(&format!(r#"<script nonce="{nonce}" type="module">boot()"#)),
        "inline module must get nonce: {result}"
    );
}

// ---------------------------------------------------------------------------
// raw=true bypasses ALL post-processing in call_tool
// ---------------------------------------------------------------------------
#[test]
fn raw_shell_skips_all_postprocessing() {
    let src = crate::suite::security_hardening::server_dispatch_src().replace("\r\n", "\n");
    assert!(
        src.contains("let is_raw_shell = name == \"ctx_shell\""),
        "call_tool must compute is_raw_shell flag"
    );
    assert!(
        src.contains("if minimal || is_raw_shell {"),
        "archive_hint must be skipped for raw shell"
    );
    assert!(
        src.contains("is_raw_shell")
            && src.contains("result_text.len() != pre_terse_len && output_tokens > 0")
            && src.contains("skip_terse"),
        "skip_terse must include is_raw_shell; post-terse stats must guard on terse delta"
    );
    // The verify_output guard may AND extra predicates after !is_raw_shell
    // (e.g. !firewalled, verify_footer()), so accept both the bare guard and
    // the extended form rather than pinning a brittle exact literal.
    assert!(
        (src.contains("if !is_raw_shell {") || src.contains("if !is_raw_shell &&"))
            && src.contains("verify_output"),
        "output verification must be skipped for raw shell"
    );
    assert!(
        src.contains("!is_raw_shell && name == \"ctx_shell\""),
        "shell_efficiency_hint must be skipped for raw shell"
    );
}
