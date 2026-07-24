//! Tests for dashboard auth, host allow-list, CSRF and token handling.

use super::routes::helpers::{detect_project_root_for_dashboard, normalize_dashboard_demo_path};
#[allow(clippy::wildcard_imports)]
use super::*;
use tempfile::tempdir;

#[test]
fn dashboard_project_root_honors_general_env_override() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path().join("project");
    std::fs::create_dir_all(&root).expect("mkdir");
    let root_s = root.to_string_lossy().to_string();

    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
    crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", &root_s);
    assert_eq!(detect_project_root_for_dashboard(), root_s);
    crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");
}

#[test]
fn dashboard_project_root_ignores_broken_ancestor_gitfile() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let workspace = td.path().join("workspace");
    let root = workspace.join("project");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(workspace.join(".git"), "gitdir: ../missing/git-dir\n").expect("write gitfile");
    let root_s = root.to_string_lossy().to_string();

    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
    crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", &root_s);
    let detected = detect_project_root_for_dashboard();
    crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");

    assert_eq!(detected, root_s);
}

#[test]
fn dashboard_project_root_honors_resolvable_ancestor_gitfile() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let checkout = td.path().join("checkout");
    let root = checkout.join("nested");
    std::fs::create_dir_all(&root).expect("mkdir checkout");
    std::fs::create_dir(td.path().join("git-data")).expect("mkdir gitdir");
    std::fs::write(checkout.join(".git"), "gitdir: ../git-data\n").expect("write gitfile");
    let root_s = root.to_string_lossy().to_string();
    let checkout_s = checkout.to_string_lossy().to_string();

    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
    crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", &root_s);
    let detected = detect_project_root_for_dashboard();
    crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");

    assert_eq!(detected, checkout_s);
}

#[test]
fn check_auth_with_valid_bearer() {
    let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer lctx_abc123\r\n\r\n";
    assert!(check_auth(req, "lctx_abc123"));
}

#[test]
fn check_auth_with_invalid_bearer() {
    let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer wrong_token\r\n\r\n";
    assert!(!check_auth(req, "lctx_abc123"));
}

#[test]
fn open_mode_flag_parses_all_variants() {
    // Explicit flag wins and never consults the environment (#424).
    assert_eq!(resolve_open_mode(Some("none")), DashboardOpen::None);
    assert_eq!(resolve_open_mode(Some("off")), DashboardOpen::None);
    assert_eq!(resolve_open_mode(Some("no")), DashboardOpen::None);
    assert_eq!(resolve_open_mode(Some("vscode")), DashboardOpen::Vscode);
    assert_eq!(resolve_open_mode(Some("code")), DashboardOpen::Vscode);
    assert_eq!(resolve_open_mode(Some("editor")), DashboardOpen::Vscode);
    assert_eq!(resolve_open_mode(Some("VSCode")), DashboardOpen::Vscode);
    assert_eq!(resolve_open_mode(Some("browser")), DashboardOpen::Browser);
    // Unknown values fall back to the historical default rather than erroring.
    assert_eq!(resolve_open_mode(Some("wat")), DashboardOpen::Browser);
}

#[test]
fn open_mode_env_is_used_when_no_flag() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_DASHBOARD_OPEN", "none");
    assert_eq!(resolve_open_mode(None), DashboardOpen::None);
    crate::test_env::set_var("LEAN_CTX_DASHBOARD_OPEN", "vscode");
    assert_eq!(resolve_open_mode(None), DashboardOpen::Vscode);
    // Flag still overrides the env var.
    assert_eq!(resolve_open_mode(Some("browser")), DashboardOpen::Browser);
    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_OPEN");
    assert_eq!(resolve_open_mode(None), DashboardOpen::Browser);
}

#[test]
fn check_auth_missing_header() {
    let req = "GET /api/stats HTTP/1.1\r\nHost: localhost\r\n\r\n";
    assert!(!check_auth(req, "lctx_abc123"));
}

#[test]
fn check_auth_lowercase_bearer() {
    let req = "GET /api/stats HTTP/1.1\r\nauthorization: bearer lctx_abc123\r\n\r\n";
    assert!(check_auth(req, "lctx_abc123"));
}

#[test]
fn query_token_parsing() {
    let raw_path = "/index.html?token=lctx_abc123&other=val";
    let idx = raw_path.find('?').unwrap();
    let qs = &raw_path[idx + 1..];
    let tok = qs.split('&').find_map(|pair| pair.strip_prefix("token="));
    assert_eq!(tok, Some("lctx_abc123"));
}

#[test]
fn api_path_detection() {
    assert!("/api/stats".starts_with("/api/"));
    assert!("/api/version".starts_with("/api/"));
    assert!(!"/".starts_with("/api/"));
    assert!(!"/index.html".starts_with("/api/"));
    assert!(!"/favicon.ico".starts_with("/api/"));
}

#[test]
fn api_session_exposes_unmodified_session_stats() {
    let _iso = crate::core::data_dir::isolated_data_dir();

    let (status, _content_type, body) =
        routes::route_response("/api/session", "", None, None, true, "GET", "");
    assert_eq!(status, "200 OK");

    let payload: serde_json::Value = serde_json::from_str(&body).expect("session JSON");
    assert!(payload["session_stats"].is_object());
    assert_eq!(payload["session_stats"]["total_tool_calls"], 0);
}

#[test]
fn context_endpoints_pair_proxy_model_with_its_window() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let snapshot = serde_json::json!({
        "ts": now,
        "proxy_active": true,
        "last_breakdown": {
            "model": "gpt-5.5",
            "total_input_tokens": 64_000
        }
    });
    std::fs::write(
        iso.path().join("proxy-introspect.json"),
        snapshot.to_string(),
    )
    .expect("proxy snapshot");

    for path in ["/api/context-history", "/api/context-model"] {
        let (status, _content_type, body) =
            routes::route_response(path, "", None, None, true, "GET", "");
        assert_eq!(status, "200 OK");
        let payload: serde_json::Value = serde_json::from_str(&body).expect("context JSON");
        let model = if path == "/api/context-history" {
            &payload["model"]
        } else {
            &payload
        };
        assert_eq!(model["model"], "gpt-5.5");
        assert_eq!(model["window_size"], 1_048_576);
        assert_eq!(model["source"], "proxy_request");
    }
}

#[test]
fn dashboard_responding_true_for_lean_ctx_version_endpoint() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut req = [0u8; 512];
        let n = stream.read(&mut req).unwrap();
        assert!(String::from_utf8_lossy(&req[..n]).starts_with("GET /api/version HTTP/1.1"));
        let body = r#"{"current":"3.8.18","latest":"3.8.18","update_available":false,"checked_age_secs":null}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });

    assert!(dashboard_responding("127.0.0.1", port));
    handle.join().unwrap();
}

#[test]
fn dashboard_responding_false_for_non_dashboard_service() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut req = [0u8; 512];
        let _ = stream.read(&mut req);
        // A 200 from an unrelated service must NOT be mistaken for our dashboard.
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let _ = stream.write_all(response.as_bytes());
    });

    assert!(!dashboard_responding("127.0.0.1", port));
    handle.join().unwrap();
}

#[test]
fn normalize_dashboard_demo_path_strips_rooted_relative_windows_path() {
    // #715: forward slashes on every host — ledger entries store the
    // forward-slash canonical form, so a `\`-separated output missed them.
    let normalized = normalize_dashboard_demo_path(r"\backend\list_tables.js");
    assert_eq!(normalized, "backend/list_tables.js");
}

#[test]
fn normalize_dashboard_demo_path_preserves_absolute_windows_path() {
    let input = r"C:\repo\backend\list_tables.js";
    assert_eq!(normalize_dashboard_demo_path(input), input);
}

#[test]
fn normalize_dashboard_demo_path_preserves_unc_path() {
    let input = r"\\server\share\backend\list_tables.js";
    assert_eq!(normalize_dashboard_demo_path(input), input);
}

#[test]
fn normalize_dashboard_demo_path_strips_dot_slash_prefix() {
    assert_eq!(
        normalize_dashboard_demo_path("./src/main.rs"),
        "src/main.rs"
    );
    assert_eq!(
        normalize_dashboard_demo_path(r".\src\main.rs"),
        "src/main.rs"
    );
}

#[test]
fn api_context_overlay_evict_removes_ledger_entry() {
    // #715: the dashboard Evict must remove the ledger entry (pressure
    // drops), resolving basenames against absolute canonical entries.
    let _iso = crate::core::data_dir::isolated_data_dir();

    let mut ledger = crate::core::context_ledger::ContextLedger::with_window_size(100_000);
    ledger.record("/tmp/proj715/src/gate715.rs", "full", 500, 500);
    ledger.save();

    let body = r#"{"action":"evict","path":"gate715.rs"}"#;
    let (status, _ct, resp) =
        routes::route_response("/api/context-overlay", "", None, None, true, "POST", body);
    assert_eq!(status, "200 OK", "evict route must succeed: {resp}");
    assert!(
        resp.contains("gate715.rs"),
        "response names the evicted canonical path: {resp}"
    );

    let reloaded = crate::core::context_ledger::ContextLedger::load();
    assert!(
        reloaded
            .entries
            .iter()
            .all(|e| !e.path.contains("gate715.rs")),
        "entry must be gone after dashboard evict"
    );

    // Unknown targets are a diagnosed 400, not a silent success.
    let (status, _ct, resp) = routes::route_response(
        "/api/context-overlay",
        "",
        None,
        None,
        true,
        "POST",
        r#"{"action":"evict","path":"missing715.rs"}"#,
    );
    assert_eq!(status, "400 Bad Request");
    assert!(resp.contains("not in ledger"), "{resp}");
}

#[test]
fn api_profile_returns_json() {
    let (_status, _ct, body) =
        routes::route_response("/api/profile", "", None, None, false, "GET", "");
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("active_name").is_some(), "missing active_name");
    assert!(
        v.pointer("/profile/profile/name")
            .and_then(|n| n.as_str())
            .is_some(),
        "missing profile.profile.name"
    );
    assert!(v.get("available").and_then(|a| a.as_array()).is_some());
}

#[test]
fn api_billing_badge_returns_cosmetic_shape() {
    let (status, ct, body) =
        routes::route_response("/api/billing-badge", "", None, None, false, "GET", "");
    assert_eq!(status, "200 OK");
    assert_eq!(ct, "application/json");
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("plan").and_then(|p| p.as_str()).is_some());
    assert!(
        v.get("supporter")
            .and_then(serde_json::Value::as_bool)
            .is_some()
    );
    assert!(
        matches!(
            v.get("source").and_then(|s| s.as_str()),
            Some("live" | "cached" | "expired" | "none")
        ),
        "unexpected source: {body}"
    );
}

#[test]
fn api_episodes_returns_json() {
    let (_status, _ct, body) =
        routes::route_response("/api/episodes", "", None, None, false, "GET", "");
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("project_hash").is_some());
    assert!(v.get("stats").is_some());
    assert!(v.get("recent").and_then(|a| a.as_array()).is_some());
}

#[test]
fn api_procedures_returns_json() {
    let (_status, _ct, body) =
        routes::route_response("/api/procedures", "", None, None, false, "GET", "");
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("project_hash").is_some());
    assert!(v.get("procedures").and_then(|a| a.as_array()).is_some());
    assert!(v.get("suggestions").and_then(|a| a.as_array()).is_some());
}

#[test]
fn api_compression_demo_heals_moved_file_paths() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::create_dir_all(root.join("src").join("moved")).expect("mkdir");
    std::fs::write(
        root.join("src").join("moved").join("foo.rs"),
        "pub fn foo() { println!(\"hi\"); }\n",
    )
    .expect("write foo.rs");

    let root_s = root.to_string_lossy().to_string();
    crate::test_env::set_var("LEAN_CTX_DASHBOARD_PROJECT", &root_s);

    let (_status, _ct, body) = routes::route_response(
        "/api/compression-demo",
        "path=src/foo.rs",
        None,
        None,
        false,
        "GET",
        "",
    );
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("error").is_none(), "unexpected error: {body}");
    assert_eq!(
        v.get("resolved_from").and_then(|x| x.as_str()),
        Some("src/moved/foo.rs")
    );

    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
    if let Some(dir) = crate::core::graph_index::ProjectIndex::index_dir(&root_s) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn resolve_token_uses_env_var_verbatim() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_mystatic");
    let (token, src) = resolve_requested_token(None);
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    assert_eq!(
        src, HTTP_TOKEN_ENV,
        "token should be reported as env-sourced"
    );
    assert_eq!(token.as_deref(), Some("lctx_mystatic"));
}

#[test]
fn resolve_token_trims_env_var() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(HTTP_TOKEN_ENV, "  lctx_padded  ");
    let (token, src) = resolve_requested_token(None);
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    assert_eq!(src, HTTP_TOKEN_ENV);
    assert_eq!(token.as_deref(), Some("lctx_padded"));
}

#[test]
fn resolve_token_falls_back_to_random_when_unset() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    let (token, src) = resolve_requested_token(None);
    assert!(token.is_none(), "unset env requests no fixed token");
    assert!(src.is_empty());
    // The production fallback in `start()` generates a random token.
    let generated = token.unwrap_or_else(generate_token);
    assert!(
        generated.starts_with("lctx_"),
        "generated token prefix, got {generated}"
    );
    assert!(
        generated.len() > 12,
        "generated token should be 32-byte hex"
    );
}

#[test]
fn resolve_token_ignores_empty_env() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(HTTP_TOKEN_ENV, "   ");
    let (token, src) = resolve_requested_token(None);
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    assert!(
        token.is_none(),
        "whitespace-only env requests no fixed token"
    );
    assert!(src.is_empty());
}

#[test]
fn resolve_token_flag_overrides_env() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    // #377: --auth-token must win over LEAN_CTX_HTTP_TOKEN so it survives
    // environments that strip/fail to inherit the env var.
    crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_fromenv");
    let (token, src) = resolve_requested_token(Some("lctx_fromflag"));
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    assert_eq!(src, "--auth-token");
    assert_eq!(token.as_deref(), Some("lctx_fromflag"));
}

#[test]
fn resolve_token_uses_flag_when_env_unset() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    let (token, src) = resolve_requested_token(Some("  lctx_flag_padded  "));
    assert_eq!(src, "--auth-token");
    assert_eq!(token.as_deref(), Some("lctx_flag_padded"));
}

#[test]
fn resolve_token_empty_flag_falls_back_to_env() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(HTTP_TOKEN_ENV, "lctx_fromenv");
    let (token, src) = resolve_requested_token(Some("   "));
    crate::test_env::remove_var(HTTP_TOKEN_ENV);
    assert_eq!(src, HTTP_TOKEN_ENV);
    assert_eq!(token.as_deref(), Some("lctx_fromenv"));
}

#[test]
fn parse_human_bool_accepts_common_forms() {
    for s in ["true", "TRUE", "1", "yes", "on", " On "] {
        assert_eq!(parse_human_bool(s), Some(true), "{s}");
    }
    for s in ["false", "FALSE", "0", "no", "off", " Off "] {
        assert_eq!(parse_human_bool(s), Some(false), "{s}");
    }
    assert_eq!(parse_human_bool("maybe"), None);
}

#[test]
fn build_allowed_hosts_covers_loopback_and_bound_host() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var(ALLOWED_HOSTS_ENV);
    let allowed = build_allowed_hosts("0.0.0.0", 3333);
    assert!(host_allowed("127.0.0.1:3333", &allowed));
    assert!(host_allowed("localhost:3333", &allowed));
    assert!(host_allowed("[::1]:3333", &allowed));
    assert!(host_allowed("127.0.0.1", &allowed)); // bare host, no port
    // 0.0.0.0 binds all interfaces but is never a valid browser Host.
    assert!(!host_allowed("0.0.0.0:3333", &allowed));
    assert!(!host_allowed("evil.com", &allowed));
}

#[test]
fn build_allowed_hosts_honors_env_extra_hosts() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(ALLOWED_HOSTS_ENV, "box.local:3333, 10.0.0.5:3333");
    let allowed = build_allowed_hosts("127.0.0.1", 3333);
    crate::test_env::remove_var(ALLOWED_HOSTS_ENV);
    assert!(host_allowed("box.local:3333", &allowed));
    assert!(host_allowed("10.0.0.5:3333", &allowed));
}

fn allowed_loopback() -> Vec<String> {
    vec![
        "127.0.0.1:3333".into(),
        "localhost:3333".into(),
        "127.0.0.1".into(),
        "localhost".into(),
    ]
}

#[test]
fn no_auth_allows_non_browser_client() {
    // curl: no Sec-Fetch-Site, no Origin, just a loopback Host.
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:3333\r\n\r\n";
    assert!(no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_allows_same_origin_browser_request() {
    let req = "GET /api/stats HTTP/1.1\r\nHost: localhost:3333\r\n\
               Origin: http://localhost:3333\r\nSec-Fetch-Site: same-origin\r\n\r\n";
    assert!(no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_allows_direct_navigation() {
    // Top-level navigation carries Sec-Fetch-Site: none and no Origin.
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:3333\r\nSec-Fetch-Site: none\r\n\r\n";
    assert!(no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_rejects_cross_site_fetch() {
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:3333\r\n\
               Sec-Fetch-Site: cross-site\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_rejects_same_site_fetch() {
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:3333\r\n\
               Sec-Fetch-Site: same-site\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_rejects_foreign_origin() {
    let req = "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:3333\r\n\
               Origin: http://evil.com\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_rejects_dns_rebinding_host() {
    // After DNS rebinding the browser sends the attacker's hostname as Host.
    let req = "GET /api/stats HTTP/1.1\r\nHost: evil.com\r\n\
               Sec-Fetch-Site: same-origin\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn no_auth_rejects_missing_host() {
    let req = "GET /api/stats HTTP/1.1\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn host_is_loopback_covers_literals_on_any_port() {
    assert!(host_is_loopback("127.0.0.1"));
    assert!(host_is_loopback("127.0.0.1:3333"));
    assert!(host_is_loopback("127.0.0.1:60000")); // port-remapped Docker publish
    assert!(host_is_loopback("localhost"));
    assert!(host_is_loopback("localhost:60000"));
    assert!(host_is_loopback("LocalHost:8080"));
    assert!(host_is_loopback("127.5.6.7:9999")); // whole 127.0.0.0/8 is loopback
    assert!(host_is_loopback("[::1]"));
    assert!(host_is_loopback("[::1]:60000"));
    assert!(host_is_loopback("::1"));
    // Non-loopback hosts must NOT be treated as loopback.
    assert!(!host_is_loopback("evil.com"));
    assert!(!host_is_loopback("evil.com:60000"));
    assert!(!host_is_loopback("10.0.0.5:3333"));
    assert!(!host_is_loopback("[2001:db8::1]:3333"));
    assert!(!host_is_loopback("box.local:3333"));
}

#[test]
fn no_auth_allows_loopback_host_on_remapped_port() {
    // Container binds 0.0.0.0:3333; Docker publishes -p 60000:3333; the host
    // browser reaches 127.0.0.1:60000, so Host carries the *published* port,
    // which the bind-port allowlist does not contain. It must still pass via
    // the loopback-any-port rule (GH no-auth Docker port-remap).
    let allowed = build_allowed_hosts("0.0.0.0", 3333);
    assert!(!host_allowed("127.0.0.1:60000", &allowed));
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:60000\r\n\
               Sec-Fetch-Site: same-origin\r\n\r\n";
    assert!(no_auth_request_ok(req, &allowed));

    // localhost on a remapped port works too.
    let req2 = "GET /api/stats HTTP/1.1\r\nHost: localhost:60000\r\n\r\n";
    assert!(no_auth_request_ok(req2, &allowed));
}

#[test]
fn no_auth_still_rejects_rebinding_on_remapped_port() {
    // The loopback-any-port relaxation must not weaken DNS-rebinding defense:
    // a non-loopback Host is rejected regardless of port.
    let allowed = build_allowed_hosts("0.0.0.0", 3333);
    let req = "GET /api/stats HTTP/1.1\r\nHost: evil.com:60000\r\n\
               Sec-Fetch-Site: same-origin\r\n\r\n";
    assert!(!no_auth_request_ok(req, &allowed));
}

#[test]
fn no_auth_allows_null_origin() {
    // Some sandboxed/file contexts send Origin: null — not attributable to a
    // site, and the Host + Sec-Fetch-Site checks still apply.
    let req = "GET /api/stats HTTP/1.1\r\nHost: 127.0.0.1:3333\r\nOrigin: null\r\n\r\n";
    assert!(no_auth_request_ok(req, &allowed_loopback()));
}

#[test]
fn topbar_uses_explicit_editor_presence_not_transport_alias() {
    let function = COCKPIT_INDEX_HTML
        .split_once("function applyAgentsToTop")
        .expect("agent badge function")
        .1
        .split_once("function hasMeaningfulSession")
        .expect("next dashboard function")
        .0;

    assert!(function.contains("logical_session_presence_available"));
    assert!(function.contains("logical_session_count"));
    assert!(!function.contains("total_active"));
    assert!(function.contains("Sessions \\u2014"));
}

#[test]
fn pulse_refresh_targets_only_the_active_view() {
    let coordinator = COCKPIT_INDEX_HTML
        .split_once("function refreshActiveView()")
        .expect("targeted refresh helper")
        .1
        .split_once("if (window.LctxRouter && window.LctxRouter.init)")
        .expect("router initialization")
        .0;

    assert!(coordinator.contains("router.navigateTo(router.getActiveViewId())"));
    assert_eq!(coordinator.matches("refreshActiveView();").count(), 3);
    assert!(coordinator.contains("hasPendingUpdate"));
    assert!(!coordinator.contains("CustomEvent('lctx:refresh')"));
}
