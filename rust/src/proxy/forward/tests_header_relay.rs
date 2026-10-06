// SPDX-License-Identifier: Apache-2.0

//! Upstream URL construction and request/response header relay.

#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn upstream_url_preserves_subpath() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/count_tokens");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages/count_tokens"
    );
}

#[test]
fn upstream_url_preserves_batches_subpath() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/batches/batch_123/results");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages/batches/batch_123/results"
    );
}

#[test]
fn upstream_url_exact_path() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn upstream_url_preserves_query_params() {
    let base = "https://api.anthropic.com";
    let parts = parts_for("/v1/messages/count_tokens?model=claude-4");
    assert_eq!(
        crate::proxy::codec::build_upstream_url(&parts, base, "/v1/messages"),
        "https://api.anthropic.com/v1/messages/count_tokens?model=claude-4"
    );
}

#[test]
fn forwards_openai_project_and_auth_headers() {
    // #366: project-scoped OpenAI keys carry the scope via `OpenAI-Project`.
    // It must be forwarded upstream, otherwise the Responses API rejects the
    // call with `Missing scopes: api.responses.write`.
    for required in ["authorization", "openai-project", "openai-organization"] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "request header `{required}` must be forwarded upstream"
        );
    }
}

#[test]
fn forwards_chatgpt_codex_oauth_headers() {
    for required in [
        "authorization",
        "chatgpt-account-id",
        "x-openai-fedramp",
        "x-openai-internal-codex-residency",
        "x-openai-product-sku",
        "oai-product-sku",
        "x-client-request-id",
        "x-codex-installation-id",
        "x-codex-turn-metadata",
        "x-openai-subagent",
        "x-codex-turn-state",
        "originator",
    ] {
        assert!(
            is_allowed_request_header(required),
            "request header `{required}` must be forwarded upstream"
        );
    }
}

#[test]
fn forwards_streamable_http_mcp_headers() {
    for required in ["mcp-session-id", "last-event-id"] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "request header `{required}` must be forwarded upstream"
        );
    }
    assert!(
        is_forwarded_response_header("mcp-session-id"),
        "MCP session id response header must be forwarded downstream"
    );
}

#[test]
fn forwards_grok_cli_chat_proxy_headers() {
    // Grok CLI → cli-chat-proxy.grok.com. Stripping these used to yield
    // HTTP 426 Upgrade Required with client-version "(none)" on /responses.
    for required in [
        "x-xai-token-auth",
        "x-models-etag",
        "x-grok-client-version",
        "x-grok-client-identifier",
        "x-grok-client-mode",
        "x-grok-client-surface",
        "x-grok-model-override",
        "x-grok-agent-id",
        "x-grok-session-id",
        "x-grok-turn-id",
        "x-grok-conv-id",
        "x-grok-req-id",
        "x-grok-deployment-id",
        "x-grok-user-id",
        "x-grok-context-window",
        "x-grok-max-completion-tokens",
        "x-grok-doom-loop-check",
        "x-grok-managed-gateway",
    ] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "request header `{required}` must be on the allowlist"
        );
    }
    // Internal gateway tag must stay off the allowlist (enterprise#11).
    assert!(!is_allowed_request_header("x-leanctx-project"));
}

#[test]
fn forwards_codex_state_response_headers() {
    for required in [
        "x-codex-turn-state",
        "x-codex-primary-used-percent",
        "openai-model",
        "x-models-etag",
        "x-reasoning-included",
        "x-oai-request-id",
        "cf-ray",
        "x-openai-authorization-error",
        "x-error-json",
    ] {
        assert!(
            is_forwarded_response_header(required),
            "response header `{required}` must be forwarded downstream"
        );
    }
}

/// #1638: Claude Code fills its plan-usage windows *only* from the
/// `anthropic-ratelimit-unified-*` response headers. The allowlist enumerated
/// the four legacy `requests-`/`tokens-` names, so when the `unified-*` family
/// arrived upstream the proxy silently dropped all of it: `rate_limits`
/// disappeared from the statusline payload, and — the part that actually
/// matters — the near-limit warning machinery reads the same state, so a user
/// behind the proxy was never warned before hitting a limit.
///
/// The names below are the full family as parsed by Claude Code 2.1.236,
/// quoted from the report. They are asserted individually rather than through
/// the prefix, so narrowing the rule back to an enumeration fails here.
#[test]
fn forwards_the_whole_anthropic_ratelimit_family() {
    for required in [
        "anthropic-ratelimit-unified-status",
        "anthropic-ratelimit-unified-reset",
        "anthropic-ratelimit-unified-5h-utilization",
        "anthropic-ratelimit-unified-5h-reset",
        "anthropic-ratelimit-unified-5h-surpassed-threshold",
        "anthropic-ratelimit-unified-7d-utilization",
        "anthropic-ratelimit-unified-7d-reset",
        "anthropic-ratelimit-unified-7d-surpassed-threshold",
        "anthropic-ratelimit-unified-grace-status",
        "anthropic-ratelimit-unified-grace-5h-utilization",
        "anthropic-ratelimit-unified-grace-7d-utilization",
        "anthropic-ratelimit-unified-overage-status",
        "anthropic-ratelimit-unified-overage-utilization",
        "anthropic-ratelimit-unified-overage-reset",
        "anthropic-ratelimit-unified-overage-period",
        "anthropic-ratelimit-unified-representative-claim",
        "anthropic-ratelimit-unified-fallback",
        "anthropic-ratelimit-unified-upgrade-paths",
        // The legacy names the allowlist used to carry explicitly must keep
        // working — the prefix replaced the enumeration, not the coverage.
        "anthropic-ratelimit-requests-limit",
        "anthropic-ratelimit-requests-remaining",
        "anthropic-ratelimit-tokens-limit",
        "anthropic-ratelimit-tokens-remaining",
    ] {
        assert!(
            is_forwarded_response_header(required),
            "response header `{required}` carries the client's own quota state \
             and must reach it"
        );
    }
}

/// Also reported in #1638: without `request-id` a user cannot correlate a
/// failed request with Anthropic support, and `x-should-retry` is what tells
/// an SDK whether a failure is worth retrying at all.
#[test]
fn forwards_request_correlation_and_retry_headers() {
    for required in ["request-id", "x-should-retry", "retry-after"] {
        assert!(
            is_forwarded_response_header(required),
            "response header `{required}` must reach the client"
        );
    }
}

/// The fix widens one family; it must not turn the allowlist into a
/// pass-through. Hop-by-hop and framing headers stay out — the proxy rewrites
/// the body (decompression, SSE re-framing), so relaying the upstream's
/// framing would describe a response that no longer exists.
#[test]
fn the_allowlist_stays_an_allowlist() {
    for denied in [
        "transfer-encoding",
        "connection",
        "content-length",
        "server",
        "set-cookie",
    ] {
        assert!(
            !is_forwarded_response_header(denied),
            "`{denied}` must not be relayed"
        );
    }
}

#[test]
fn chatgpt_responses_use_openai_responses_holdout_key() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::core::config::Config::update_global(|c| {
        c.proxy.output_holdout = Some(1.0);
    })
    .unwrap();

    let body = serde_json::json!({
        "model": "gpt-5",
        "input": "same conversation",
    });

    assert_eq!(
        cohort_arm(&body, "ChatGPT", "/backend-api/codex/responses"),
        cohort_arm(&body, "OpenAI", "/v1/responses")
    );
}

#[test]
fn cache_prompt_hash_is_content_sensitive() {
    let hash = super::super::super::ocla_cache_bridge::prompt_hash;
    assert_ne!(hash(b"one"), hash(b"two"));
}

/// Loopback upstream that answers exactly one request and hands back its raw,
/// lowercased request header block. Lets a test assert what actually leaves the
/// proxy, not only what the allowlist constant claims.
async fn upstream_capturing_headers() -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break request.len();
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..header_end]).to_lowercase()
    });
    (format!("http://{address}"), server)
}

/// Minimal state for the transport layer. `send_upstream` reads only
/// `state.client`; the watch receiver keeps serving its last value after the
/// sender drops, so no keep-alive is needed.
fn relay_test_state() -> ProxyState {
    let (_upstreams, state_upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            chatgpt: "https://chatgpt.com".into(),
            gemini: "https://generativelanguage.googleapis.com".into(),
            providers: Vec::new(),
        }));
    ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(crate::proxy::ProxyStats::default()),
        break_even: Arc::new(crate::proxy::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(crate::proxy::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams: state_upstreams,
        chatgpt_cookies: crate::proxy::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            crate::proxy::web_app::conversation_tracker::ConversationTracker::default(),
        )),
    }
}

#[test]
fn forwards_opencode_session_header() {
    // #1752: OpenCode (and OpenCode zen) key session affinity off their own
    // header. #1730 enumerated only the Claude Code spellings, so this sibling
    // kept being stripped and every relayed request looked like a new session.
    assert!(ALLOWED_REQUEST_HEADERS.contains(&"x-opencode-session"));
    assert!(is_allowed_request_header("x-opencode-session"));
    assert!(should_forward_request_header("x-opencode-session", false));
}

#[tokio::test]
async fn opencode_session_reaches_the_upstream_verbatim() {
    // #1752 end-to-end: assert what the upstream actually receives, including
    // the mixed-case spelling OpenCode puts on the wire.
    let (upstream, server) = upstream_capturing_headers().await;
    let parts = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("X-OpenCode-Session", "0f9c2b71-4d3a-4e58-9c11-7a6e5b2d8f40")
        .header("X-Leanctx-Project", "internal-only")
        .body(())
        .unwrap()
        .into_parts()
        .0;

    let response = transport::send_upstream(
        &relay_test_state(),
        &parts,
        &upstream,
        b"{}".to_vec(),
        "Anthropic",
        false,
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    let seen = server.await.unwrap();
    assert!(
        seen.contains("x-opencode-session: 0f9c2b71-4d3a-4e58-9c11-7a6e5b2d8f40"),
        "session header must reach the upstream verbatim, got: {seen}"
    );
    // Widening the allowlist for #1752 must not widen it for anything else.
    assert!(
        !seen.contains("x-leanctx-project"),
        "internal header must stay stripped, got: {seen}"
    );
}

#[test]
fn forwards_claude_code_session_id_header() {
    // #1730: Claude Code's per-conversation UUID must survive the relay so a
    // downstream proxy can key session affinity and prompt-cache reuse on it.
    assert!(ALLOWED_REQUEST_HEADERS.contains(&"x-claude-code-session-id"));
    assert!(is_allowed_request_header("x-claude-code-session-id"));
    assert!(should_forward_request_header(
        "x-claude-code-session-id",
        false
    ));
}

#[tokio::test]
async fn claude_code_session_id_reaches_the_upstream_verbatim() {
    // #1730 end-to-end: assert what the upstream actually receives, including
    // the mixed-case spelling Claude Code puts on the wire.
    let (upstream, server) = upstream_capturing_headers().await;
    let parts = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(
            "X-Claude-Code-Session-Id",
            "11111111-2222-3333-4444-555555555555",
        )
        .header("X-Leanctx-Project", "internal-only")
        .body(())
        .unwrap()
        .into_parts()
        .0;

    let response = transport::send_upstream(
        &relay_test_state(),
        &parts,
        &upstream,
        b"{}".to_vec(),
        "Anthropic",
        false,
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    let seen = server.await.unwrap();
    assert!(
        seen.contains("x-claude-code-session-id: 11111111-2222-3333-4444-555555555555"),
        "session id must reach the upstream verbatim, got: {seen}"
    );
    // Widening the allowlist for #1730 must not widen it for anything else:
    // the internal gateway tag stays stripped (enterprise#11).
    assert!(
        !seen.contains("x-leanctx-project"),
        "internal header must stay stripped, got: {seen}"
    );
}

#[test]
fn forwards_commandcode_cli_headers() {
    // Command Code (`cmd`) gates agent calls on `x-command-code-version`.
    // Stripping it yields 403 upgrade_required ("CLI is out of date").
    for required in [
        "x-command-code-version",
        "x-cli-environment",
        "x-oauth-token",
        "x-oauth-provider",
        "x-project-slug",
        "x-taste-learning",
        "x-taste-usage",
        "x-oss-primary-provider",
        "x-system-prompt-breakdown",
        "x-cmd-zdr",
        "x-session-id", // shared; pin so CC session stays wired
    ] {
        assert!(
            ALLOWED_REQUEST_HEADERS.contains(&required),
            "Command Code CLI header `{required}` must be on the request allowlist"
        );
    }
}

/// Stub upstream that answers `200 {}` and yields the exact request body
/// bytes it received, so a test can assert byte-identity on the wire.
async fn upstream_capturing_body() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let header = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
        let content_length = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        request[header_end..header_end + content_length].to_vec()
    });
    (format!("http://{address}"), server)
}

/// #1912: a request the proxy does not change must reach the provider as the
/// client's exact bytes. Re-serializing reorders keys and rewrites number and
/// whitespace formatting, which moves the prompt-cache prefix every turn.
#[tokio::test]
async fn unchanged_request_is_forwarded_byte_identical() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let (upstream, server) = upstream_capturing_body().await;
    // Key order, spacing and `1.0` all differ from serde_json's canonical form.
    let raw = br#"{ "temperature": 1.0,  "model": "claude-sonnet-4-5",
  "messages": [ {"role": "user", "content": "hi"} ] }"#
        .to_vec();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(raw.clone()))
        .unwrap();

    let response = forward_request(
        State(relay_test_state()),
        request,
        &upstream,
        "/v1/messages",
        |value, original_size| {
            let out = serde_json::to_vec(&value).unwrap();
            let size = out.len();
            (out, original_size, size)
        },
        "Anthropic",
        &[],
    )
    .await
    .unwrap();
    assert!(response.status().is_success());

    assert_eq!(
        server.await.unwrap(),
        raw,
        "an unchanged request must be forwarded as the client's exact bytes"
    );
}
