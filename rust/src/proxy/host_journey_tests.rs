// SPDX-License-Identifier: Apache-2.0
//! G7 host journeys: a Claude Code turn and a Codex (ChatGPT subscription)
//! turn go through the real proxy handlers on the routes those hosts use, to a
//! local upstream that records the full request. Proves for both hosts that a
//! credential inside a tool result never leaves the machine while the rest of
//! the turn does. No model is called.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::ProxyState;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

fn proxy_state(upstream: &str) -> ProxyState {
    let (_tx, rx) = tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
        anthropic: upstream.into(),
        openai: upstream.into(),
        chatgpt: upstream.into(),
        gemini: upstream.into(),
        providers: Vec::new(),
    }));
    ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(super::ProxyStats::default()),
        break_even: Arc::new(super::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(super::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams: rx,
        chatgpt_cookies: super::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            super::web_app::conversation_tracker::ConversationTracker::default(),
        )),
    }
}

/// One-shot upstream that returns the complete request (headers and body).
async fn spawn_recording_upstream() -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let n = socket.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .await;
    });
    (format!("http://{addr}"), rx)
}

fn assert_secret_stayed_local(received: &str, surrounding: &str) {
    assert!(
        !received.contains(AWS_KEY),
        "the raw credential left the machine: {received}"
    );
    assert!(
        received.contains(surrounding),
        "the rest of the turn arrives: {received}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_code_turn_reaches_the_model_without_the_credential() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (upstream, received) = spawn_recording_upstream().await;
    let body = serde_json::json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 64,
        "system": "You are a coding agent.",
        "messages": [
            {"role": "user", "content": "Why does the deploy fail?"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": ".env"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1",
                 "content": format!("AWS_ACCESS_KEY_ID={AWS_KEY}\nREGION=eu-central-2")}
            ]}
        ]
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("x-api-key", "sk-ant-api03-test-only")
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        super::anthropic::handler(State(proxy_state(&upstream)), request),
    )
    .await
    .expect("the handler answers from the local upstream");

    let received = received.await.unwrap();
    assert!(received.starts_with("POST /v1/messages"), "{received}");
    assert_secret_stayed_local(&received, "REGION=eu-central-2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_subscription_turn_reaches_the_model_without_the_credential() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (upstream, received) = spawn_recording_upstream().await;
    let body = serde_json::json!({
        "model": "gpt-5.5",
        "instructions": "You are Codex.",
        "input": [
            {"type": "message", "role": "user",
             "content": [{"type": "input_text", "text": "Why does the deploy fail?"}]},
            {"type": "function_call", "call_id": "call_1", "name": "shell",
             "arguments": "{\"command\":[\"cat\",\".env\"]}"},
            {"type": "function_call_output", "call_id": "call_1",
             "output": format!("AWS_ACCESS_KEY_ID={AWS_KEY}\nREGION=eu-central-2")}
        ],
        "stream": false
    });
    let request = Request::builder()
        .method("POST")
        .uri("/backend-api/codex/responses")
        .header("authorization", "Bearer test-only-subscription-token")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        super::chatgpt::codex_responses_handler(State(proxy_state(&upstream)), request),
    )
    .await
    .expect("the handler answers from the local upstream");

    let received = received.await.unwrap();
    assert!(
        received.starts_with("POST /backend-api/codex/responses"),
        "{received}"
    );
    assert_secret_stayed_local(&received, "REGION=eu-central-2");
}
