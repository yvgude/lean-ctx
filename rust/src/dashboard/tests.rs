// SPDX-License-Identifier: Apache-2.0
//! Tests for dashboard auth, host allow-list, CSRF and token handling.

use super::routes::helpers::{detect_project_root_for_dashboard, normalize_dashboard_demo_path};
#[allow(clippy::wildcard_imports)]
use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn work_graph_control_enforces_http_auth_and_csrf() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (auth_enabled, headers, expected) in [
        (true, "", "401 Unauthorized"),
        (true, "Authorization: Bearer wrong\r\n", "401 Unauthorized"),
        (
            true,
            "Authorization: Bearer control-test\r\nOrigin: https://attacker.invalid\r\n",
            "403 Forbidden",
        ),
        (false, "", "403 Forbidden"),
        (
            true,
            "Authorization: Bearer control-test\r\n",
            "400 Bad Request",
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_request(
                stream,
                auth_enabled.then(|| Arc::new("control-test".to_string())),
                Arc::new(String::new()),
                Arc::new(allowed_loopback()),
            )
            .await;
        });
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!(
                "POST /api/agents/work-graph HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            client.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).await.unwrap();
            response
        }).await;
        if response.is_err() {
            server.abort();
        }
        let response = response.expect("bounded HTTP response");
        server.await.unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {expected}")),
            "{response}"
        );
    }
}

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

/// AP04-G8: without an explicit binding and outside a project, the dashboard
/// is unbound; it never adopts the project whose session was saved last.
#[test]
fn dashboard_without_a_binding_does_not_show_the_last_active_project() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("tempdir");
    std::fs::create_dir(project.path().join(".git")).expect("git dir");
    let mut session = crate::core::session::SessionState::new();
    session.project_root = Some(project.path().to_string_lossy().to_string());
    session.save().expect("save session");
    let old = std::env::var_os("LEAN_CTX_PROJECT_ROOT");
    crate::test_env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
    crate::test_env::remove_var("LEAN_CTX_PROJECT_ROOT");

    let binding = super::routes::helpers::dashboard_project_binding_in(None);
    if let Some(old) = old {
        crate::test_env::set_var("LEAN_CTX_PROJECT_ROOT", old);
    }

    assert_eq!(
        binding,
        (
            None,
            super::routes::helpers::DashboardProjectSource::Unbound
        )
    );
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

/// AP04 review I5: the dashboard binds a folder exactly as an MCP session
/// would — an explicit root as given, never promoted to the enclosing
/// checkout, and a start folder with its own project marker as itself.
#[test]
fn dashboard_binds_a_folder_as_an_mcp_session_would() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let td = tempdir().expect("tempdir");
    let checkout = td.path().join("checkout");
    let nested = checkout.join("nested");
    std::fs::create_dir_all(&nested).expect("mkdir checkout");
    std::fs::create_dir(td.path().join("git-data")).expect("mkdir gitdir");
    std::fs::write(checkout.join(".git"), "gitdir: ../git-data\n").expect("write gitfile");
    std::fs::write(nested.join("Cargo.toml"), "[package]\n").expect("marker");
    let nested_s = nested.to_string_lossy().to_string();
    let _flag = crate::test_env::VarGuard::set("LEAN_CTX_DASHBOARD_PROJECT", "");
    let helpers =
        |cwd: Option<&std::path::Path>| super::routes::helpers::dashboard_project_binding_in(cwd);

    let pinned = {
        let _pin = crate::test_env::VarGuard::set("LEAN_CTX_PROJECT_ROOT", &nested_s);
        helpers(None)
    };
    let _pin = crate::test_env::VarGuard::set("LEAN_CTX_PROJECT_ROOT", "");
    let started = helpers(Some(&nested));

    use super::routes::helpers::DashboardProjectSource::{Cwd, Env};
    assert_eq!(pinned, (Some(nested_s), Env));
    assert_eq!(
        started,
        (crate::server::derive_project_root_from(&nested), Cwd)
    );
    assert_ne!(started.0, Some(checkout.to_string_lossy().to_string()));
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
    let _env_lock = crate::core::data_dir::test_env_lock();

    // The route persists project overlays separately from the isolated ledger.
    // Never use the developer checkout (read-only in CI) as that project.
    struct RestoreEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            for (key, previous) in &self.0 {
                match previous {
                    Some(value) => crate::test_env::set_var(key, value),
                    None => crate::test_env::remove_var(key),
                }
            }
        }
    }
    let _restore = RestoreEnvironment(
        [
            "LEAN_CTX_DASHBOARD_PROJECT",
            "LEAN_CTX_DATA_DIR",
            "LEAN_CTX_CONFIG_DIR",
            "LEAN_CTX_STATE_DIR",
            "LEAN_CTX_CACHE_DIR",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect(),
    );
    // Restore caller overrides after `_iso` clears them, with the lock held.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("overlay project");
    let project_root = project
        .path()
        .canonicalize()
        .expect("canonical overlay project");
    crate::test_env::set_var("LEAN_CTX_DASHBOARD_PROJECT", &project_root);
    let source = project_root.join("src/gate715.rs");
    let normalized_source = crate::core::pathutil::normalize_tool_path(&source.to_string_lossy());
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("source directory");
    std::fs::write(&source, "pub fn gate715() {}\n").expect("source fixture");

    let mut ledger = crate::core::context_ledger::ContextLedger::with_window_size(100_000);
    ledger.record(&source.to_string_lossy(), "full", 500, 500);
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
    let overlays = crate::core::context_overlay::OverlayStore::load_project(&project_root);
    assert_eq!(overlays.all().len(), 1, "project exclusion must persist");
    assert!(matches!(
        overlays.all()[0].operation,
        crate::core::context_overlay::OverlayOp::Exclude { .. }
    ));
    assert_eq!(
        overlays.all()[0].scope,
        crate::core::context_overlay::OverlayScope::Project
    );
    assert_eq!(
        overlays.all()[0].target,
        crate::core::context_field::ContextItemId::from_file(&normalized_source)
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
fn api_episodes_returns_json() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (status, _ct, body) =
        routes::route_response("/api/episodes", "", None, None, false, "GET", "");
    assert_eq!(status, "200 OK", "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(v.get("project_hash").is_some());
    assert!(v.get("stats").is_some());
    assert!(v.get("recent").and_then(|a| a.as_array()).is_some());
}

#[test]
fn api_procedures_returns_json() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (status, _ct, body) =
        routes::route_response("/api/procedures", "", None, None, false, "GET", "");
    assert_eq!(status, "200 OK", "{body}");
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
    assert!(src.is_empty(), "{src:?}");
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
    assert!(src.is_empty(), "{src:?}");
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

/// One request through the dashboard server with `cockpit` as the Cockpit.
async fn exchange(
    cockpit: super::cockpit::CockpitResolver,
    token: Option<&str>,
    request: String,
) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = token.map(|t| Arc::new(t.to_string()));
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_request_with(
            stream,
            token,
            Arc::new(String::new()),
            Arc::new(allowed_loopback()),
            cockpit,
        )
        .await;
    });
    let response = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        response
    })
    .await
    .expect("bounded HTTP response");
    server.await.unwrap();
    response
}

fn split_response(raw: &[u8]) -> (String, Vec<u8>) {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header end");
    (
        String::from_utf8_lossy(&raw[..end]).into_owned(),
        raw[end + 4..].to_vec(),
    )
}

/// The dashboard is the single entry point: after its own token check it
/// hands the Cockpit's assets to the Cockpit byte for byte, with the Cockpit
/// secret and the project header, and never the dashboard credential.
#[tokio::test]
async fn the_cockpit_is_served_through_the_dashboard_after_its_token_check() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let font: Vec<u8> = (0u8..=255).rev().cycle().take(3000).collect();
    let cockpit = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = cockpit.local_addr().unwrap().port();
    let served = font.clone();
    let fake_cockpit = tokio::spawn(async move {
        let (mut stream, _) = cockpit.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).await.unwrap();
            request.extend_from_slice(&buf[..n]);
        }
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Type: font/woff2\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_vec();
        response.extend_from_slice(format!("{:x}\r\n", served.len()).as_bytes());
        response.extend_from_slice(&served);
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        stream.write_all(&response).await.unwrap();
        String::from_utf8(request).unwrap()
    });
    let resolved = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = resolved.clone();
    let resolver: super::cockpit::CockpitResolver = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(super::cockpit::CockpitTarget {
            port,
            token: "cockpit-secret".into(),
        })
    });

    // The Cockpit's API needs the dashboard token; without it the Cockpit
    // is never asked.
    let refused = exchange(
        resolver.clone(),
        Some("dash-token"),
        "GET /api/gateway/receipts HTTP/1.1\r\nHost: localhost\r\n\r\n".into(),
    )
    .await;
    assert!(refused.starts_with(b"HTTP/1.1 401"));
    assert_eq!(resolved.load(std::sync::atomic::Ordering::SeqCst), 0);

    let raw = exchange(
        resolver,
        Some("dash-token"),
        "GET /assets/fonts/inter.woff2?token=dash-token HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer dash-token\r\nX-LeanCTX-Project: /work/alpha\r\n\r\n"
            .into(),
    )
    .await;
    let (head, body) = split_response(&raw);
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(head.contains("Content-Type: font/woff2"));
    assert_eq!(body, font, "binary body passed through unchanged");

    let forwarded = fake_cockpit.await.unwrap();
    assert!(forwarded.starts_with("GET /assets/fonts/inter.woff2 HTTP/1.1\r\n"));
    assert!(forwarded.contains("X-LeanCTX-Cockpit: cockpit-secret\r\n"));
    assert!(forwarded.contains("X-LeanCTX-Project: /work/alpha\r\n"));
    assert!(!forwarded.contains("dash-token"), "{forwarded}");
}

/// Without the Cockpit, `/` explains what it is, why it is not shown and
/// how to get it; the engine's own `/api/gateway/*` keeps answering.
#[tokio::test]
async fn without_the_cockpit_the_start_page_says_how_to_install_it() {
    use super::cockpit::CockpitUnavailable;
    for reason in [
        CockpitUnavailable::NotInstalled,
        CockpitUnavailable::Disabled,
        CockpitUnavailable::StartFailed,
    ] {
        let resolver: super::cockpit::CockpitResolver = Arc::new(move || Err(reason));
        let raw = exchange(
            resolver,
            Some("dash-token"),
            "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n".into(),
        )
        .await;
        let (head, body) = split_response(&raw);
        let body = String::from_utf8(body).unwrap();
        assert!(head.starts_with("HTTP/1.1 503"), "{head}");
        assert!(head.contains("text/html"));
        assert!(body.contains("Select · Control · Prove"));
        assert!(body.contains(reason.message()), "{reason:?}");
        assert!(body.contains(super::cockpit::INSTALL_COMMAND));
        assert!(body.contains("lean-ctx gain"));
        assert!(!body.contains("dash-token"));
    }

    let resolver: super::cockpit::CockpitResolver =
        Arc::new(|| Err(CockpitUnavailable::NotInstalled));
    let raw = exchange(
        resolver,
        Some("dash-token"),
        "GET /api/gateway/unknown HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer dash-token\r\n\r\n"
            .into(),
    )
    .await;
    let (head, _) = split_response(&raw);
    assert!(head.starts_with("HTTP/1.1 404"), "engine answers: {head}");
}

/// `X-LeanCTX-Project` binds one read to one pool project: concurrent reads
/// for two projects each see only their own, and a foreign path is refused
/// without a hint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_for_two_projects_never_cross() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let projects = tempdir().expect("tempdir");
    let mut roots = Vec::new();
    for name in ["alpha", "beta"] {
        let root = projects.path().join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let root = root.to_string_lossy().into_owned();
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(root.clone());
        session.save().expect("save session");
        roots.push(root);
    }
    let foreign = projects.path().join("foreign");
    std::fs::create_dir_all(foreign.join(".git")).unwrap();

    let request = |project: &str| {
        format!(
            "GET /api/scope HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dash-token\r\n\
             X-LeanCTX-Project: {project}\r\n\r\n"
        )
    };
    let mut reads = Vec::new();
    for round in 0..6 {
        let root = roots[round % 2].clone();
        let request = request(&root);
        reads.push(tokio::spawn(async move {
            let raw = exchange(
                Arc::new(|| Err(super::cockpit::CockpitUnavailable::NotInstalled)),
                Some("dash-token"),
                request,
            )
            .await;
            (root, raw)
        }));
    }
    for read in reads {
        let (root, raw) = read.await.unwrap();
        let (head, body) = split_response(&raw);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let scope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(scope["workspace_root"], root.as_str());
        assert_eq!(scope["binding_source"], "request");
        assert_eq!(
            scope["partition_id"],
            crate::core::project_hash::hash_project_root(&root)
        );
    }

    let foreign = foreign.to_string_lossy().into_owned();
    let raw = exchange(
        Arc::new(|| Err(super::cockpit::CockpitUnavailable::NotInstalled)),
        Some("dash-token"),
        request(&foreign),
    )
    .await;
    let (head, body) = split_response(&raw);
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    assert_eq!(body, br#"{"error":"forbidden"}"#);
}

/// F12a/F12b: a named project binds every route that answers for it (reads
/// and the session note), device-wide reads answer unchanged, and a route
/// that can do neither refuses instead of answering for another project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_named_project_is_honoured_or_refused_never_ignored() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let projects = tempdir().expect("tempdir");
    let mut roots = Vec::new();
    for name in ["alpha", "beta"] {
        let root = projects.path().join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("marker.txt"),
            format!("only in {name}: {name}-marker\n"),
        )
        .unwrap();
        let root = root.to_string_lossy().into_owned();
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(root.clone());
        session.save().expect("save session");
        roots.push(root);
    }
    let (alpha, beta) = (roots[0].clone(), roots[1].clone());
    let send = |method: &'static str, path: &'static str, project: String, body: &'static str| async move {
        let raw = exchange(
            Arc::new(|| Err(super::cockpit::CockpitUnavailable::NotInstalled)),
            Some("dash-token"),
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dash-token\r\n\
                 X-LeanCTX-Project: {project}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        let (head, body) = split_response(&raw);
        (head, String::from_utf8_lossy(&body).into_owned())
    };

    // A read the Cockpit does not list follows the named project.
    let (head, body) = send(
        "GET",
        "/api/compression-demo?path=marker.txt",
        alpha.clone(),
        "",
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(body.contains("alpha-marker"), "{body}");
    assert!(!body.contains("beta-marker"), "{body}");

    // Device-wide data answers the same for every project.
    let (head, _) = send("GET", "/api/telemetry", alpha.clone(), "").await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    // A route that cannot answer for the project refuses it.
    let (head, body) = send("GET", "/api/hive/explained", alpha.clone(), "").await;
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    assert!(body.contains("named project"), "{body}");

    // The session note lands in the named project's session, not the newest
    // session of any project (beta's).
    let (head, body) = send(
        "POST",
        "/api/session/note",
        alpha.clone(),
        r#"{"note":"for alpha"}"#,
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}: {body}");
    let notes = |root: &str| -> Vec<String> {
        crate::core::session::SessionState::load_latest_for_project_root(root)
            .expect("project session")
            .progress
            .into_iter()
            .filter(|entry| entry.action == "note")
            .filter_map(|entry| entry.detail)
            .collect()
    };
    assert_eq!(notes(&alpha), vec!["for alpha".to_string()]);
    assert!(notes(&beta).is_empty());
}

/// The ledger figures of a read that names its project count that project's
/// folders (here a worktree of it) and never another project's; without a
/// project they count every project.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_figures_follow_the_requested_project() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let projects = tempdir().expect("tempdir");
    let base = crate::core::pathutil::safe_canonicalize_or_self(projects.path());
    let alpha = base.join("alpha");
    let beta = base.join("beta");
    let worktree = base.join("alpha-wt");
    std::fs::create_dir_all(alpha.join(".git/worktrees/wt")).unwrap();
    std::fs::create_dir_all(beta.join(".git")).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", alpha.join(".git/worktrees/wt").display()),
    )
    .unwrap();
    let ledger = crate::core::savings_ledger::store::default_path().expect("ledger path");
    for (folder, saved) in [(&alpha, 700u64), (&worktree, 50), (&beta, 300)] {
        let folder = folder.to_string_lossy().into_owned();
        let mut session = crate::core::session::SessionState::new();
        session.project_root = Some(folder.clone());
        session.save().expect("save session");
        let event: crate::core::savings_ledger::SavingsEvent =
            serde_json::from_value(serde_json::json!({
                "ts": "2026-10-20T10:00:00+00:00", "tool": "ctx_read", "mechanism": "compression",
                "model_id": "m", "tokenizer": "o200k_base", "baseline_tokens": saved + 100,
                "actual_tokens": 100, "saved_tokens": saved, "bounce_adjustment": 0,
                "unit_price_per_m_usd": 3.0, "saved_usd": 0.0,
                "repo_hash": crate::core::savings_ledger::hash_folder(&folder),
                "agent_id": "a", "prev_hash": "", "entry_hash": "", "version": "3.12.0",
            }))
            .unwrap();
        crate::core::savings_ledger::store::append(&ledger, event).unwrap();
    }

    let saved = |project: Option<String>| async move {
        let header = project.map_or_else(String::new, |p| format!("X-LeanCTX-Project: {p}\r\n"));
        let raw = exchange(
            Arc::new(|| Err(super::cockpit::CockpitUnavailable::NotInstalled)),
            Some("dash-token"),
            format!(
                "GET /api/value HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dash-token\r\n{header}\r\n"
            ),
        )
        .await;
        let (head, body) = split_response(&raw);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let proof: serde_json::Value = serde_json::from_slice(&body).unwrap();
        proof["tokens_saved"].as_u64().unwrap()
    };
    assert_eq!(saved(Some(alpha.to_string_lossy().into_owned())).await, 750);
    assert_eq!(saved(Some(beta.to_string_lossy().into_owned())).await, 300);
    assert_eq!(saved(None).await, 1_050);

    // Spend and usage counters split the same way: a project reads its share,
    // the device view the total with the split per project.
    use crate::core::savings_ledger::project_scope::project_key_of;
    let alpha_key = project_key_of(&alpha).expect("alpha is a project");
    let turn = |key: &str, input| crate::proxy::usage::RealUsage {
        model: "claude-sonnet-4.5".into(),
        input_tokens: input,
        output_tokens: 10,
        wire: Some(Box::new(crate::proxy::usage::WireContext {
            project_key: Some(key.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };
    crate::proxy::usage_meter::record(&turn(&alpha_key, 4_000));
    crate::proxy::usage_meter::record(&turn(&project_key_of(&beta).unwrap(), 1_000));
    for (folder, calls) in [(&worktree, 3), (&beta, 1)] {
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(folder.clone())),
            || {
                for _ in 0..calls {
                    crate::core::stats::record("ctx_read", 100, 40);
                }
            },
        );
    }
    let read = |path: &'static str, project: Option<String>| async move {
        let header = project.map_or_else(String::new, |p| format!("X-LeanCTX-Project: {p}\r\n"));
        let raw = exchange(
            Arc::new(|| Err(super::cockpit::CockpitUnavailable::NotInstalled)),
            Some("dash-token"),
            format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer dash-token\r\n{header}\r\n"
            ),
        )
        .await;
        let (head, body) = split_response(&raw);
        assert!(head.starts_with("HTTP/1.1 200"), "{path}: {head}");
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()
    };
    let alpha_text = alpha.to_string_lossy().into_owned();
    let spend = read("/api/spend", Some(alpha_text.clone())).await;
    assert_eq!(spend["per_model"][0]["input_tokens"], 4_000);
    assert!(spend.get("by_project").is_none());
    let device = read("/api/spend", None).await;
    let names: Vec<&str> = device["by_project"]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["project"].as_str())
        .collect();
    assert!(
        names.contains(&"alpha") && names.contains(&"beta"),
        "{names:?}"
    );
    let stats = read("/api/stats", Some(alpha_text)).await;
    assert_eq!(
        stats["total_commands"], 3,
        "the worktree's calls count for alpha"
    );
}

/// A map view of a project without an index answers `202 building` at once
/// (the Cockpit polls it) instead of waiting for a scan and answering empty.
#[test]
fn map_routes_answer_building_without_waiting_for_an_index() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("project");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    std::fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();
    let root = project.path().to_string_lossy().into_owned();
    for route in [
        "/api/graph",
        "/api/graph-files",
        "/api/heatmap",
        "/api/routes",
        "/api/architecture",
        "/api/tree",
        "/api/symbols",
        "/api/call-graph",
    ] {
        let started = std::time::Instant::now();
        let (status, _, body) = super::project_binding::in_request_project(Some(&root), || {
            routes::route_response(route, "", None, None, true, "GET", "")
        });
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{route} waited"
        );
        assert_eq!(status, "202 Accepted", "{route}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "building", "{route}: {body}");
        assert!(
            v["files_total"].is_u64() && v["files_done"].is_u64(),
            "{route}"
        );
    }
}

/// The building answer carries the scan's file counts while this process
/// runs the scan.
#[test]
fn the_building_answer_reports_the_scan_progress() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("project");
    let root = project.path().to_string_lossy().into_owned();
    let _scan = crate::core::index_progress::ProgressGuard::new(
        root.clone(),
        crate::core::index_progress::IndexComponent::Graph,
    );
    _scan.report(7, 40);
    let (status, _, body) = super::project_binding::in_request_project(Some(&root), || {
        routes::route_response("/api/tree", "", None, None, true, "GET", "")
    });
    assert_eq!(status, "202 Accepted", "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["files_done"].as_u64(), v["files_total"].as_u64()),
        (Some(7), Some(40))
    );
}

/// `/api/compression-demo` errors carry their status with the error JSON.
#[test]
fn compression_demo_errors_are_not_200() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("project");
    let root = project.path().to_string_lossy().into_owned();
    let demo = |query: &str| {
        super::project_binding::in_request_project(Some(&root), || {
            routes::route_response("/api/compression-demo", query, None, None, true, "GET", "")
        })
    };
    let (status, _, body) = demo("");
    assert_eq!(status, "400 Bad Request");
    assert!(body.contains("missing path"), "{body}");
    let (status, _, body) = demo("path=src/absent.rs");
    assert_eq!(status, "404 Not Found");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"], "failed to read file");
    assert_eq!(v["requested_path"], "src/absent.rs");
}

fn get_json(root: &str, route: &str, query: &str) -> (&'static str, serde_json::Value) {
    let (status, _, body) = super::project_binding::in_request_project(Some(root), || {
        routes::route_response(route, query, None, None, true, "GET", "")
    });
    let value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{route}: {e}: {body}"));
    (status, value)
}

/// The Project Map routes over an indexed project: each answers its
/// documented shape from the real index.
#[test]
fn map_routes_answer_from_the_index() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempdir().expect("project");
    let src = project.path().join("src");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub mod util;\n\npub fn entry() -> u32 {\n    util::helper() + 1\n}\n",
    )
    .unwrap();
    std::fs::write(src.join("util.rs"), "pub fn helper() -> u32 {\n    41\n}\n").unwrap();
    let root = project.path().to_string_lossy().into_owned();
    crate::core::graph_provider::build_property_graph(&root).expect("index the project");

    let (status, graph) = get_json(&root, "/api/graph", "");
    assert_eq!(status, "200 OK", "{graph}");
    let files: Vec<&str> = graph["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(files.iter().any(|f| f.ends_with("util.rs")), "{files:?}");
    assert!(graph["edges"].is_array() && graph["language_matrix"].is_array());

    let (status, listed) = get_json(&root, "/api/graph-files", "");
    assert_eq!(status, "200 OK");
    assert_eq!(listed["files"].as_array().map(Vec::len), Some(files.len()));

    let (status, heat) = get_json(&root, "/api/heatmap", "");
    assert_eq!(status, "200 OK");
    assert_eq!(heat.as_array().map(Vec::len), Some(files.len()));

    let (status, symbols) = get_json(&root, "/api/symbols", "q=helper");
    assert_eq!(status, "200 OK");
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, ["helper"], "{symbols}");

    let (status, tree) = get_json(&root, "/api/tree", "");
    assert_eq!(status, "200 OK");
    assert_eq!(tree["file_count"].as_u64(), Some(files.len() as u64));
    assert!(tree["symbol_count"].as_u64() >= Some(2), "{tree}");

    let (status, report) = get_json(&root, "/api/architecture", "");
    assert_eq!(status, "200 OK");
    assert!(
        report["markdown"]
            .as_str()
            .is_some_and(|md| md.starts_with("# Architecture Report")),
        "{report}"
    );

    let (status, found) = get_json(&root, "/api/routes", "");
    assert_eq!(status, "200 OK");
    assert!(found["routes"].is_array());
    assert_eq!(
        found["indexed_file_count"].as_u64(),
        Some(files.len() as u64)
    );

    // The call graph builds in the background: `202` until it is ready.
    let started = std::time::Instant::now();
    let calls = loop {
        let (status, calls) = get_json(&root, "/api/call-graph", "");
        if status == "200 OK" {
            break calls;
        }
        assert_eq!(status, "202 Accepted", "{calls}");
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(calls["status"], "ready");
    assert!(
        calls["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["callee_name"]
                .as_str()
                .is_some_and(|c| c.ends_with("helper"))),
        "{calls}"
    );
    let (status, progress) = get_json(&root, "/api/call-graph/status", "");
    assert_eq!(status, "200 OK");
    assert_eq!(progress["status"], "ready");
}

/// `/api/learning`, `/api/solution` and `/api/snapshots` answer their
/// documented shapes on a fresh installation.
#[test]
fn learning_solution_and_snapshot_routes_answer_their_shapes() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("project");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let root = project.path().to_string_lossy().into_owned();

    let (status, learning) = get_json(&root, "/api/learning", "");
    assert_eq!(status, "200 OK");
    for key in ["playbook", "scents"] {
        assert!(learning[key].is_array(), "{key}: {learning}");
    }
    assert!(learning.get("litm").is_none(), "{learning}");
    assert!(learning.get("efficacy").is_some());

    let (status, solution) = get_json(&root, "/api/solution", "");
    assert_eq!(status, "200 OK");
    assert!(solution["enabled"].is_boolean() && solution["intensity"].is_string());
    for key in ["output_savings", "loc", "decisions"] {
        assert!(solution[key].is_object(), "{key}: {solution}");
    }
    assert!(solution["trend_7d"].is_array() && solution["top_patterns"].is_array());

    let (status, timeline) = get_json(&root, "/api/snapshots", "");
    assert_eq!(status, "200 OK");
    assert_eq!(timeline["project_root"], root.as_str());
    assert_eq!(timeline["count"], 0);
    assert!(timeline["head"].is_null() && timeline["entries"].is_array());
    let (status, missing) = get_json(&root, "/api/snapshot", "");
    assert_eq!(status, "400 Bad Request");
    assert!(missing["error"].is_string());
    let (status, unknown) = get_json(&root, "/api/snapshot", "id=deadbeef");
    assert_eq!(status, "404 Not Found");
    assert!(unknown["error"].is_string());
}

/// Every file under `dirs` with its size and mtime, except `*.lock` files: a
/// read takes the stores' cross-process locks, which hold no state.
fn files_under(
    dirs: &[&std::path::Path],
) -> std::collections::BTreeMap<std::path::PathBuf, (u64, std::time::SystemTime)> {
    dirs.iter()
        .flat_map(|dir| {
            walkdir::WalkDir::new(dir)
                .into_iter()
                .filter_map(Result::ok)
        })
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_none_or(|ext| ext != "lock"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((entry.into_path(), (meta.len(), meta.modified().ok()?)))
        })
        .collect()
}

/// A GET only reads: the context-control list saves neither the ledger nor
/// the overlay store, the telemetry status creates no installation ID, and
/// the graph enrichment, which writes, answers only a POST.
#[test]
fn get_routes_write_nothing() {
    let data = crate::core::data_dir::isolated_data_dir();
    let project = tempdir().expect("project");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let get = |route: &str| {
        super::project_binding::in_request_project(Some(&root), || {
            routes::route_response(route, "", None, None, true, "GET", "")
        })
    };
    // The config loader's one-time migration marker is written by the first
    // config load of any process, not by a route.
    let _ = crate::core::config::Config::try_load_global();
    let _ = crate::core::config::Config::load();
    let before = files_under(&[data.path(), project.path()]);

    let (status, _, body) = get("/api/context-control");
    assert_eq!(status, "200 OK", "{body}");
    let control: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(control["result"].is_string() && control["overlays"].is_array());

    let (status, _, body) = get("/api/telemetry");
    assert_eq!(status, "200 OK", "{body}");
    let telemetry: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(telemetry["installation_id"].is_null(), "{body}");

    let (status, _, body) = get("/api/graph/enrich");
    assert_eq!(status, "405 Method Not Allowed");
    assert!(body.contains("POST"), "{body}");

    let after = files_under(&[data.path(), project.path()]);
    let changed: Vec<_> = after
        .iter()
        .filter(|(path, stamp)| before.get(*path) != Some(stamp))
        .map(|(path, _)| path)
        .collect();
    assert!(changed.is_empty(), "a GET wrote {changed:?}");
    assert_eq!(after.len(), before.len(), "a GET removed files");
    assert!(crate::core::installation_id::existing().is_none());
}
