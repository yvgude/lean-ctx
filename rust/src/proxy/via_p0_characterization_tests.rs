// SPDX-License-Identifier: Apache-2.0

use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ANTHROPIC_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/via-p0/anthropic-messages.json"
));
const OPENAI_RESPONSES_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/via-p0/openai-responses.json"
));
const AUTH_SENTINEL: &str = concat!("via-auth-", "sentinel");

fn parse_fixture(raw: &str) -> (Value, Vec<u8>) {
    let value: Value = serde_json::from_str(raw).expect("valid provider fixture");
    let bytes = serde_json::to_vec(&value).expect("serializable provider fixture");
    (value, bytes)
}

#[derive(Debug)]
struct CapturedProviderRequest {
    headers: String,
    body: Value,
}

async fn capture_one_provider_request() -> (String, tokio::task::JoinHandle<CapturedProviderRequest>)
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake provider");
    let address = listener.local_addr().expect("fake provider address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept edge request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        let header_end = loop {
            let read = stream.read(&mut buffer).await.expect("read edge request");
            assert!(read > 0, "edge closed request before headers completed");
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end])
            .expect("UTF-8 provider request headers")
            .to_owned();
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.eq_ignore_ascii_case("content-length").then(|| {
                    value
                        .trim()
                        .parse::<usize>()
                        .expect("numeric content length")
                })
            })
            .expect("content-length header");
        while request.len() < header_end + content_length {
            let read = stream.read(&mut buffer).await.expect("read provider body");
            assert!(read > 0, "edge closed request before body completed");
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
            )
            .await
            .expect("write fake provider response");
        CapturedProviderRequest {
            headers,
            body: serde_json::from_slice(&request[header_end..header_end + content_length])
                .expect("provider-shaped JSON body"),
        }
    });
    (format!("http://{address}"), server)
}

fn proxy_state(anthropic_upstream: String) -> super::ProxyState {
    let (_sender, upstreams) =
        tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
            anthropic: anthropic_upstream,
            openai: "https://api.openai.com".to_owned(),
            chatgpt: "https://chatgpt.com".to_owned(),
            gemini: "https://generativelanguage.googleapis.com".to_owned(),
            providers: Vec::new(),
        }));
    super::ProxyState {
        client: reqwest::Client::new(),
        port: 0,
        stats: Arc::new(super::ProxyStats::default()),
        break_even: Arc::new(super::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(super::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams,
        chatgpt_cookies: super::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
        web_app_tracker: Arc::new(std::sync::Mutex::new(
            super::web_app::conversation_tracker::ConversationTracker::default(),
        )),
    }
}

#[test]
fn p0_anthropic_characterization_pins_tool_reduction_and_protected_fields() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (original, original_bytes) = parse_fixture(ANTHROPIC_FIXTURE);

    let (output, original_size, compressed_size) =
        super::anthropic::compress_request_body(original.clone(), original_bytes.len());
    let transformed: Value = serde_json::from_slice(&output).expect("valid transformed request");

    assert_eq!(original_size, original_bytes.len());
    assert!(
        compressed_size < original_size,
        "fixture must prove a real reduction"
    );
    for field in ["model", "system", "tools", "stream", "max_tokens"] {
        assert_eq!(
            transformed[field], original[field],
            "protected field changed: {field}"
        );
    }
    assert_eq!(transformed["messages"][0], original["messages"][0]);
    assert_eq!(transformed["messages"][2], original["messages"][2]);
    assert_eq!(
        transformed["messages"][3]["content"][0]["text"]
            .as_str()
            .expect("transformed current-user text")
            .as_bytes(),
        original["messages"][3]["content"][0]["text"]
            .as_str()
            .expect("original current-user text")
            .as_bytes(),
        "current-user text bytes changed"
    );
    assert_eq!(
        transformed["messages"][3]["content"][0], original["messages"][3]["content"][0],
        "current-user text changed"
    );
    if let Some(steer) = transformed["messages"][3]["content"]
        .as_array()
        .and_then(|content| content.get(1))
    {
        assert_eq!(steer["text"], super::verbosity::STEER);
    }

    let before = original["messages"][1]["content"][0]["content"]
        .as_str()
        .expect("tool output text");
    let after = transformed["messages"][1]["content"][0]["content"]
        .as_str()
        .expect("transformed tool output text");
    assert!(after.len() < before.len(), "tool output did not shrink");
    assert_eq!(
        transformed["messages"][1]["content"][0]["tool_use_id"],
        original["messages"][1]["content"][0]["tool_use_id"]
    );
}

#[test]
fn p0_openai_responses_characterization_pins_tool_reduction_and_protected_fields() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (original, original_bytes) = parse_fixture(OPENAI_RESPONSES_FIXTURE);

    let (output, original_size, compressed_size) =
        super::openai_responses::compress_request_body(original.clone(), original_bytes.len());
    let transformed: Value = serde_json::from_slice(&output).expect("valid transformed request");

    assert_eq!(original_size, original_bytes.len());
    assert!(
        compressed_size < original_size,
        "fixture must prove a real reduction"
    );
    for field in ["model", "instructions", "tools", "text", "stream"] {
        assert_eq!(
            transformed[field], original[field],
            "protected field changed: {field}"
        );
    }
    assert_eq!(transformed["input"][0], original["input"][0]);
    assert_eq!(transformed["input"][2], original["input"][2]);
    assert_eq!(
        transformed["input"][3]["content"][0], original["input"][3]["content"][0],
        "current-user text changed"
    );
    if let Some(steer) = transformed["input"][3]["content"]
        .as_array()
        .and_then(|content| content.get(1))
    {
        assert_eq!(steer["text"], super::verbosity::STEER);
    }

    let before = original["input"][1]["output"]
        .as_str()
        .expect("tool output text");
    let after = transformed["input"][1]["output"]
        .as_str()
        .expect("transformed tool output text");
    assert!(after.len() < before.len(), "tool output did not shrink");
    assert_eq!(
        transformed["input"][1]["call_id"],
        original["input"][1]["call_id"]
    );
}

#[tokio::test]
async fn p0_anthropic_edge_forwards_auth_and_transformed_body_to_fake_provider() {
    use axum::body::{Body, to_bytes};
    use axum::extract::State;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};

    let _isolation = crate::core::data_dir::isolated_data_dir();
    super::prefix_replay::clear();
    let (upstream, server) = capture_one_provider_request().await;
    let (original, original_bytes) = parse_fixture(ANTHROPIC_FIXTURE);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(CONTENT_TYPE, "application/json")
        .header("x-api-key", AUTH_SENTINEL)
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(original_bytes))
        .expect("provider request");

    let response = super::anthropic::handler(State(proxy_state(upstream)), request)
        .await
        .expect("edge response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response bytes"),
        "{}"
    );

    let captured = server.await.expect("fake provider capture");
    assert!(
        captured
            .headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("x-api-key: {AUTH_SENTINEL}"))),
        "provider authentication was not forwarded to the provider"
    );
    assert_eq!(captured.body["model"], original["model"]);
    assert_eq!(captured.body["system"], original["system"]);
    assert_eq!(captured.body["tools"], original["tools"]);
    assert_eq!(captured.body["stream"], original["stream"]);
    assert_eq!(captured.body["max_tokens"], original["max_tokens"]);
    assert_eq!(captured.body["messages"][0], original["messages"][0]);
    assert_eq!(
        captured.body["messages"][3]["content"][0]["text"]
            .as_str()
            .expect("forwarded current-user text")
            .as_bytes(),
        original["messages"][3]["content"][0]["text"]
            .as_str()
            .expect("original current-user text")
            .as_bytes(),
        "full Edge path changed current-user text bytes"
    );
    assert_eq!(
        captured.body["messages"][0]["content"][0]["id"],
        original["messages"][0]["content"][0]["id"]
    );
    assert_eq!(
        captured.body["messages"][1]["content"][0]["tool_use_id"],
        original["messages"][1]["content"][0]["tool_use_id"]
    );
    assert_eq!(
        captured.body["messages"][0]["content"][0]["id"],
        captured.body["messages"][1]["content"][0]["tool_use_id"],
        "full Edge path broke tool-use/tool-result pairing"
    );
    assert!(
        captured.body["messages"][1]["content"][0]["content"]
            .as_str()
            .expect("forwarded tool output")
            .len()
            < original["messages"][1]["content"][0]["content"]
                .as_str()
                .expect("original tool output")
                .len(),
        "full Edge path did not reduce the synthetic tool output"
    );
}
