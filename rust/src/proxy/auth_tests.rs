//! Split from `proxy/mod.rs` (#660 LOC gate): `auth_tests`.

use super::*;

// P0-4 (#416): the proxy must never run unauthenticated — `None` means
// "resolve the session token", not "no auth".
#[test]
fn effective_auth_token_never_yields_empty() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

    assert_eq!(effective_auth_token(Some("tok".into())), "tok");
    let auto = effective_auth_token(None);
    assert!(!auto.trim().is_empty(), "None must auto-resolve a token");
    let blank = effective_auth_token(Some("   ".into()));
    assert!(!blank.trim().is_empty(), "blank tokens must be replaced");

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn expired_timestamped_proxy_token_is_rejected() {
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(200_000);
    let expired = "proxy-token.113599";
    assert!(proxy_token_expired(expired, now));
}

#[test]
fn fresh_timestamped_proxy_token_is_accepted() {
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(200_000);
    let fresh = "proxy-token.113600";
    assert!(!proxy_token_expired(fresh, now));
}

#[test]
fn plain_proxy_token_is_accepted_for_backwards_compatibility() {
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(200_000);
    assert!(!proxy_token_expired("proxy-token", now));
}

// #597: the Codex ChatGPT WS passthrough opens a wss://chatgpt.com socket via
// tokio-tungstenite, which needs a process-default rustls CryptoProvider. The
// tree has both aws-lc-rs and ring, so one must be installed explicitly or the
// handshake aborts. Guards against that regression.
#[test]
fn installs_default_crypto_provider_for_ws_passthrough() {
    install_default_crypto_provider();
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        "WS passthrough needs a process-default CryptoProvider"
    );
}

#[test]
fn is_provider_route_v1() {
    assert!(is_provider_route("/v1/chat/completions"));
    assert!(is_provider_route("/v1/messages"));
    assert!(is_provider_route("/v1/completions"));
}

#[test]
fn is_provider_route_anthropic_subpaths() {
    assert!(is_provider_route("/v1/messages/count_tokens"));
    assert!(is_provider_route("/v1/messages/batches"));
    assert!(is_provider_route("/v1/messages/batches/batch_123"));
}

#[test]
fn is_provider_route_v1beta() {
    assert!(is_provider_route("/v1beta/models"));
}

#[test]
fn is_provider_route_chat() {
    assert!(is_provider_route("/chat/completions"));
}

#[test]
fn is_provider_route_chatgpt_backend_api() {
    assert!(is_provider_route("/backend-api/codex/responses"));
    assert!(is_provider_route("/backend-api/codex/responses/resp_123"));
    assert!(is_provider_route("/backend-api/wham/session"));
    assert!(is_provider_route("/backend-api/ps/mcp"));
    assert!(is_provider_route("/backend-api/codex_apps"));
    assert!(is_provider_route("/backend-api/codex_apps/mcp"));
    assert!(is_provider_route("/backend-api/mcp/codex_apps"));
    assert!(is_provider_route("/backend-api/apps/codex_apps/mcp"));
}

#[test]
fn is_provider_route_rejects_non_provider() {
    assert!(!is_provider_route("/health"));
    assert!(!is_provider_route("/api/v2/test"));
    assert!(!is_provider_route("/"));
}

#[test]
fn is_provider_route_model_catalog() {
    // enterprise#63: `GET /v1/models` (and the bare `/models` of clients whose
    // base URL omits `/v1`) must authenticate like any provider route.
    assert!(is_provider_route("/v1/models"));
    assert!(is_provider_route("/models"));
    // Nothing else under a bare `/models*` prefix becomes a provider route.
    assert!(!is_provider_route("/modelsx"));
}

#[test]
fn is_provider_route_registry_providers() {
    // enterprise#7 + Grok subscription rail: base URL is
    // `http://127.0.0.1:4444/providers/grok-chat/v1`, so model list and chat
    // hit `/providers/{id}/v1/...` with the client's own session/API Bearer.
    // These must authenticate via provider-key fallback (loopback), not lean-ctx
    // token alone — otherwise Grok's mode/model picker stays empty.
    assert!(is_provider_route("/providers/grok-chat/v1/models"));
    assert!(is_provider_route(
        "/providers/grok-chat/v1/chat/completions"
    ));
    assert!(is_provider_route("/providers/grok-chat/v1/responses"));
    assert!(is_provider_route("/providers/xai/v1/models"));
    assert!(is_provider_route("/providers/foundry/v1/chat/completions"));
    // Command Code gateway rail: base URL is
    // `http://127.0.0.1:4444/providers/commandcode`, so the CLI hits
    // `/providers/commandcode/alpha/...` with its own session Bearer.
    assert!(is_provider_route("/providers/commandcode/alpha/whoami"));
    assert!(is_provider_route(
        "/providers/commandcode/alpha/agent/generate"
    ));
    // Bare prefix without trailing path still matches the registry mount.
    assert!(is_provider_route("/providers/grok-chat"));
    // Unrelated paths stay closed.
    assert!(!is_provider_route("/provider/grok-chat/v1/models"));
    assert!(!is_provider_route("/api/providers/x"));
}

fn build_request(headers: &[(&str, &str)], path: &str) -> axum::extract::Request {
    let mut builder = axum::http::Request::builder().uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

#[test]
fn has_provider_api_key_x_api_key() {
    let req = build_request(&[("x-api-key", "sk-ant-abc123")], "/v1/messages");
    assert!(has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_x_goog() {
    let req = build_request(&[("x-goog-api-key", "AIzaSyAbc")], "/v1beta/models");
    assert!(has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_azure() {
    let req = build_request(&[("api-key", "deadbeef")], "/v1/completions");
    assert!(has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_bearer_sk() {
    let req = build_request(
        &[("authorization", "Bearer [REDACTED:Bearer token]")],
        "/v1/chat/completions",
    );
    assert!(has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_empty_rejected() {
    let req = build_request(&[("x-api-key", "  ")], "/v1/messages");
    assert!(!has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_no_headers() {
    let req = build_request(&[], "/v1/messages");
    assert!(!has_provider_api_key(&req));
}

#[test]
fn has_provider_api_key_accepts_non_sk_bearer() {
    // #362: OpenAI-*compatible* providers (Azure, OpenRouter, Groq, vLLM/
    // Ollama gateways, project/service keys) issue keys without the sk-/gsk_
    // prefix. OpenCode (@ai-sdk/openai) forwards them as `Bearer <key>`; they
    // must authenticate on a loopback provider route. The upstream validates
    // the real key — the proxy never injects one.
    for key in [
        "Bearer [REDACTED:Bearer token]", // OpenRouter
        "Bearer [REDACTED:Bearer token]", // (still works)
        "Bearer [REDACTED:Bearer token]", // gateway/service token
        "Bearer [REDACTED:Bearer token]", // opaque
    ] {
        let req = build_request(&[("authorization", key)], "/v1/responses");
        assert!(
            has_provider_api_key(&req),
            "non-sk Bearer must count as a provider credential: {key}"
        );
    }
}

#[test]
fn has_provider_api_key_empty_bearer_rejected() {
    // A blank credential — or a bare scheme word with no token (some HTTP
    // stacks trim trailing whitespace down to just "Bearer") — is not auth.
    for bad in ["Bearer    ", "", "Bearer", "bearer", "   "] {
        let req = build_request(&[("authorization", bad)], "/responses");
        assert!(
            !has_provider_api_key(&req),
            "blank/scheme-only Authorization must not authenticate: {bad:?}"
        );
    }
}

// --- #334: opt-in strict proxy auth (proxy_require_token) ---

#[test]
fn provider_key_fallback_allowed_in_default_mode() {
    // Default (require_token = false): a provider key on a provider route is
    // sufficient. This is what lets a local AI tool authenticate with its own
    // key and no lean-ctx Bearer token (the loopback-friendly behavior).
    assert!(provider_key_fallback_allowed(false, true, true, false));
}

#[test]
fn provider_key_fallback_denied_in_strict_mode_for_builtin_routes() {
    // Strict (require_token = true): built-in /v1/* routes still require the
    // lean-ctx Bearer — provider-key alone is not enough.
    assert!(!provider_key_fallback_allowed(true, true, true, false));
}

#[test]
fn provider_key_fallback_allowed_for_client_auth_registry_under_require_token() {
    // Grok dual-rail /providers/{id} with api_key_env=None: Authorization (or
    // x-xai-token-auth) must carry the upstream session, so provider-key
    // fallback stays open even when require_token is set.
    assert!(provider_key_fallback_allowed(true, true, true, true));
    assert!(!provider_key_fallback_allowed(true, false, true, true));
    assert!(!provider_key_fallback_allowed(true, true, false, true));
}

#[test]
fn provider_key_fallback_requires_key_and_provider_route() {
    // The fallback never fires without a provider key, nor off a provider
    // route — regardless of mode.
    assert!(!provider_key_fallback_allowed(false, false, true, false));
    assert!(!provider_key_fallback_allowed(false, true, false, false));
    assert!(!provider_key_fallback_allowed(true, false, true, false));
}

#[test]
fn has_provider_api_key_x_xai_token_auth() {
    // Grok subscription rail may authenticate solely via x-xai-token-auth.
    let req = build_request(
        &[("x-xai-token-auth", "sess-xyz")],
        "/providers/grok-chat/v1",
    );
    assert!(has_provider_api_key(&req));
    let empty = build_request(&[("x-xai-token-auth", "  ")], "/providers/grok-chat/v1");
    assert!(!has_provider_api_key(&empty));
}

#[test]
fn proxy_require_token_defaults_off() {
    assert!(!crate::core::config::Config::default().proxy_require_token);
}

#[test]
fn proxy_loopback_open_defaults_off() {
    assert!(!crate::core::config::Config::default().proxy_loopback_open);
}

#[test]
fn auth_error_response_mcp_hint() {
    let resp = auth_error_response("/mcp/sse");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn auth_error_response_provider_hint() {
    let resp = auth_error_response("/v1/messages");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn auth_error_response_generic_hint() {
    let resp = auth_error_response("/status");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- enterprise#11: identity tags + x-leanctx-project header ---

#[cfg(feature = "enterprise")]
#[test]
fn attach_gateway_tags_header_overrides_default_project() {
    // The per-request header wins over the key's default_project: one
    // person books work onto different projects request by request.
    let mut req = build_request(&[("x-leanctx-project", "billing")], "/v1/messages");
    attach_gateway_tags(
        &mut req,
        gateway_identity::GatewayTags {
            person: Some("yves".into()),
            team: Some("platform".into()),
            project: Some("ai-gateway".into()),
        },
    );
    let tags = req
        .extensions()
        .get::<gateway_identity::GatewayTags>()
        .expect("tags attached");
    assert_eq!(tags.person.as_deref(), Some("yves"));
    assert_eq!(tags.project.as_deref(), Some("billing"));
}

#[cfg(feature = "enterprise")]
#[test]
fn attach_gateway_tags_header_works_without_key_identity() {
    // Solo/local mode: project tagging must not require a gateway key.
    let mut req = build_request(&[("x-leanctx-project", "side-quest")], "/v1/messages");
    attach_gateway_tags(&mut req, gateway_identity::GatewayTags::default());
    let tags = req
        .extensions()
        .get::<gateway_identity::GatewayTags>()
        .expect("tags attached");
    assert_eq!(tags.person, None);
    assert_eq!(tags.project.as_deref(), Some("side-quest"));
}

#[cfg(feature = "enterprise")]
#[test]
fn attach_gateway_tags_empty_leaves_no_extension() {
    // No identity, no header: nothing to stamp — the extension stays absent
    // so downstream code can treat its presence as "gateway context exists".
    let mut req = build_request(&[], "/v1/messages");
    attach_gateway_tags(&mut req, gateway_identity::GatewayTags::default());
    assert!(
        req.extensions()
            .get::<gateway_identity::GatewayTags>()
            .is_none()
    );
}

#[cfg(feature = "enterprise")]
#[test]
fn attach_gateway_tags_rejects_oversized_or_blank_header() {
    // Defensive bound: a blank or absurdly long project header is ignored,
    // the key's default project stays authoritative.
    let long = "p".repeat(200);
    for bad in ["", "   ", long.as_str()] {
        let mut req = build_request(&[("x-leanctx-project", bad)], "/v1/messages");
        attach_gateway_tags(
            &mut req,
            gateway_identity::GatewayTags {
                person: Some("yves".into()),
                team: None,
                project: Some("default-proj".into()),
            },
        );
        let tags = req
            .extensions()
            .get::<gateway_identity::GatewayTags>()
            .expect("tags attached");
        assert_eq!(
            tags.project.as_deref(),
            Some("default-proj"),
            "bad header {bad:?} must not override"
        );
    }
}

#[cfg(feature = "enterprise")]
#[test]
fn attach_gateway_tags_rejects_control_characters() {
    // #54/#59: control chars in the project header would poison usage rows
    // and logs (log-injection). The tag is dropped, the key default stays.
    // HTAB is the only control byte the HTTP layer lets through to us — the
    // rest (`\n`, ESC, DEL, …) is rejected by HeaderValue itself.
    let mut req = build_request(&[("x-leanctx-project", "bil\tling")], "/v1/messages");
    attach_gateway_tags(
        &mut req,
        gateway_identity::GatewayTags {
            person: Some("yves".into()),
            team: None,
            project: Some("default-proj".into()),
        },
    );
    let tags = req
        .extensions()
        .get::<gateway_identity::GatewayTags>()
        .expect("tags attached");
    assert_eq!(
        tags.project.as_deref(),
        Some("default-proj"),
        "control chars must not override"
    );
}

#[test]
fn x_leanctx_project_never_forwarded_upstream() {
    // Internal gateway header: it must NOT be on the upstream allowlist,
    // otherwise org-internal project names leak to the provider.
    assert!(!forward::is_allowed_request_header("x-leanctx-project"));
}

// --- #353: bare provider endpoints (OpenCode / @ai-sdk/openai) ---

#[test]
fn is_provider_route_bare_responses_and_messages() {
    // Clients that point their base URL at the proxy root (no `/v1`) send the
    // bare endpoint; auth must still recognise it as a provider route.
    assert!(is_provider_route("/responses"));
    assert!(is_provider_route("/responses/resp_123/input_items"));
    assert!(is_provider_route("/messages"));
}

#[test]
fn canonical_provider_path_rewrites_bare_endpoints() {
    assert_eq!(
        canonical_provider_path("/responses").as_deref(),
        Some("/v1/responses")
    );
    assert_eq!(
        canonical_provider_path("/chat/completions").as_deref(),
        Some("/v1/chat/completions")
    );
    assert_eq!(
        canonical_provider_path("/messages").as_deref(),
        Some("/v1/messages")
    );
}

#[test]
fn canonical_provider_path_preserves_subpaths() {
    assert_eq!(
        canonical_provider_path("/responses/resp_abc/cancel").as_deref(),
        Some("/v1/responses/resp_abc/cancel")
    );
    assert_eq!(
        canonical_provider_path("/messages/batches/batch_1").as_deref(),
        Some("/v1/messages/batches/batch_1")
    );
}

#[test]
fn canonical_provider_path_ignores_already_canonical_and_unknown() {
    // Already canonical → no rewrite (avoids `/v1/v1/...`).
    assert_eq!(canonical_provider_path("/v1/responses"), None);
    assert_eq!(canonical_provider_path("/v1/chat/completions"), None);
    // Unrelated paths are untouched.
    assert_eq!(canonical_provider_path("/health"), None);
    assert_eq!(canonical_provider_path("/responsesx"), None);
    assert_eq!(canonical_provider_path("/"), None);
}

#[test]
fn canonical_provider_path_collapses_double_v1_prefix() {
    // OPENAI_BASE_URL now advertises `/v1` (#366); a client treating it as an
    // origin and appending `/v1/...` itself produces a double prefix.
    assert_eq!(
        canonical_provider_path("/v1/v1/responses").as_deref(),
        Some("/v1/responses")
    );
    assert_eq!(
        canonical_provider_path("/v1/v1/chat/completions").as_deref(),
        Some("/v1/chat/completions")
    );
}

#[test]
fn normalized_provider_uri_rewrites_path_and_preserves_query() {
    use axum::http::Uri;
    let uri: Uri = "/responses?stream=true".parse().unwrap();
    let rewritten = normalized_provider_uri(&uri).expect("bare /responses must rewrite");
    assert_eq!(rewritten.path(), "/v1/responses");
    assert_eq!(rewritten.query(), Some("stream=true"));
    assert_eq!(
        rewritten
            .path_and_query()
            .map(axum::http::uri::PathAndQuery::as_str),
        Some("/v1/responses?stream=true")
    );
}

#[test]
fn normalized_provider_uri_noop_for_canonical() {
    use axum::http::Uri;
    let uri: Uri = "/v1/responses".parse().unwrap();
    assert!(normalized_provider_uri(&uri).is_none());
}

// --- enterprise#8: gateway mode (non-loopback bind) hardening ---

#[test]
fn resolved_bind_host_defaults_to_loopback_and_never_opens_on_typo() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_PROXY_BIND_HOST");
    let cfg = crate::core::config::Config::default();
    assert!(cfg.resolved_proxy_bind_host().is_loopback());

    // A typo in the config must narrow to loopback — never open the bind.
    let typo = crate::core::config::Config {
        proxy_bind_host: Some("all-interfaces-please".into()),
        ..Default::default()
    };
    assert!(typo.resolved_proxy_bind_host().is_loopback());

    let open = crate::core::config::Config {
        proxy_bind_host: Some("0.0.0.0".into()),
        ..Default::default()
    };
    assert!(!open.resolved_proxy_bind_host().is_loopback());
}

#[test]
fn host_allowed_loopback_always_passes_and_allowlist_extends() {
    let allowed = vec!["gateway.example.com".to_string()];
    for h in ["127.0.0.1", "127.0.0.1:4444", "localhost:9999", "[::1]:80"] {
        assert!(host_allowed(h, &allowed), "loopback host must pass: {h}");
    }
    assert!(host_allowed("gateway.example.com", &allowed));
    assert!(host_allowed("Gateway.Example.COM:443", &allowed));
    // Trailing-dot FQDN normalizes to the same allowlisted name.
    assert!(host_allowed("gateway.example.com.:443", &allowed));
    assert!(!host_allowed("evil.example.com", &allowed));
    assert!(!host_allowed("gateway.example.com.evil.io", &allowed));
    // Empty allowlist (gateway not configured) → only loopback passes.
    assert!(!host_allowed("gateway.example.com", &[]));
}

// --- enterprise#37: proxy rate limiting ---

#[tokio::test]
async fn rate_limiter_enforces_burst_then_recovers() {
    let limiter = RateLimiter::new(10, 3);
    assert!(limiter.allow().await);
    assert!(limiter.allow().await);
    assert!(limiter.allow().await);
    assert!(!limiter.allow().await, "burst of 3 must exhaust the bucket");
    // 10 rps refill → one token back after ~100ms.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(limiter.allow().await, "bucket must refill at max_rps");
}

// A `[[proxy.providers]]` entry with `api_key_env` holds the upstream key and
// injects it. A caller on such a route must present the lean-ctx Bearer token:
// any non-empty provider header would otherwise buy the operator's credential.
#[tokio::test]
async fn gateway_credential_route_requires_lean_ctx_token() {
    let provider = |id: &str, api_key_env: Option<&str>| crate::core::config::ResolvedProvider {
        id: id.into(),
        shape: crate::core::config::WireShape::OpenAi,
        base_url: "http://127.0.0.1:9".into(),
        api_key_env: api_key_env.map(str::to_string),
        aws_region: None,
        account_ref: None,
        region: None,
        local: true,
    };
    let mut upstreams = crate::core::config::Config::default()
        .proxy
        .resolve_all_disk();
    upstreams.providers = vec![
        provider("foundry", Some("FOUNDRY_API_KEY")),
        provider("grok-chat", None),
    ];
    let (_sender, upstreams) = tokio::sync::watch::channel(Arc::new(upstreams));
    #[cfg(feature = "enterprise")]
    let gateway_keys = Arc::new(gateway_identity::GatewayKeys::default());
    let app = axum::Router::new()
        .route(
            "/providers/{id}/{*rest}",
            axum::routing::any(|| async { "reached" }),
        )
        .route(
            "/v1/chat/completions",
            axum::routing::any(|| async { "reached" }),
        )
        .layer(axum::middleware::from_fn(move |request, next| {
            proxy_auth_guard(
                request,
                next,
                "gateway-fixture-token".into(),
                false,
                false,
                #[cfg(feature = "enterprise")]
                gateway_keys.clone(),
                upstreams.clone(),
            )
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let foundry = "/providers/foundry/v1/chat/completions";
    let grok = "/providers/grok-chat/v1/chat/completions";
    for (path, header, value, expected) in [
        // Gateway-held key: a provider header alone is not authentication.
        (foundry, "x-api-key", "anything", 401),
        (foundry, "authorization", "Bearer junk", 401),
        (foundry, "api-key", "x", 401),
        (
            foundry,
            "authorization",
            "Bearer gateway-fixture-token",
            200,
        ),
        // Client-auth rail and built-in loopback routes keep the fallback.
        (grok, "authorization", "Bearer sess", 200),
        ("/v1/chat/completions", "x-api-key", "sk-caller", 200),
    ] {
        let status = client
            .post(format!("http://{address}{path}"))
            .header(header, value)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(status, expected, "{path} with {header}: {value}");
    }

    stop.send(()).unwrap();
    server.await.unwrap();
}
