//! End-to-end smoke test for the LLM proxy runtime path: bind → serve → health
//! → auth guard → shutdown. This exercises the *actual* runtime (a real TCP
//! listener and the full middleware stack), not just unit-level helpers.
//!
//! It stays hermetic and offline: a provider route sent without credentials is
//! rejected by the auth guard (401) *before* any upstream is contacted, so no
//! network egress and no real provider key are required.

use std::time::Duration;

/// Reserve an ephemeral loopback port, then release it for the proxy to bind.
/// The brief race between release and re-bind is tolerated by the health poll.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe socket");
    let port = listener.local_addr().expect("probe local_addr").port();
    drop(listener);
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_serves_health_and_enforces_auth() {
    let port = free_port();
    let token = "smoke-token".to_string();

    let server = tokio::spawn({
        let token = token.clone();
        async move {
            let _ = lean_ctx::proxy::start_proxy_with_token(port, Some(token)).await;
        }
    });

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // Wait for the proxy to come up (bind + serve), up to ~5s.
    let mut healthy = false;
    for _ in 0..50 {
        if let Ok(resp) = client.get(format!("{base}/health")).send().await
            && resp.status().is_success()
        {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(healthy, "proxy /health never became ready");

    // A provider route with no credentials must be rejected by the auth guard
    // (401) — this happens before any upstream forwarding, keeping the test
    // offline while still proving routing + host guard + auth guard are wired.
    let unauth = client
        .post(format!("{base}/v1/messages"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("request to proxy");
    assert_eq!(
        unauth.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "provider route without a token/key must return 401"
    );

    // Teardown: cancel the serve task (drops the listener).
    server.abort();
}
