// SPDX-License-Identifier: Apache-2.0
//! Real loopback coverage for the Engine's HTTP egress lifecycle.
//!
//! These tests drive the ordinary ProxyDriver preparation and dispatch path:
//! admission receipts are persisted before transport, and observations are
//! loaded back from the same isolated receipt store after transport.

use super::*;
use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    EgressAdmissionOutcomeV1, EgressTransportFailureV1, EgressTransportObservationV1,
    EgressTransportObserverV1, EgressTransportResultV1, EgressTransportV1,
};
use sha2::Digest as _;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const MAX_TEST_REQUEST_BYTES: usize = 64 * 1024;

// Hang guards, sized for a loaded gate host: a loopback upstream starts
// waiting before the driver's admission and receipt writes run, which took
// longer than the old 1-2 s windows under 13 000 parallel tests (the
// upstream then saw no request and the proxy a refused connection: 502).
// They bound waiting only; the 50 ms "nothing else connects" checks stay.
/// An upstream waits this long for the request it expects.
const ACCEPT_FIRST: Duration = Duration::from_secs(30);
/// ... and this long for a retry (one retry after < 350 ms backoff); a test
/// that expects no retry waits this long before it reports none.
const ACCEPT_RETRY: Duration = Duration::from_secs(5);
/// One read of a request the upstream has accepted.
const READ_BOUND: Duration = Duration::from_secs(10);
/// A whole dispatch or forward.
const DISPATCH_BOUND: Duration = Duration::from_secs(30);

fn isolated_governed_gateway() -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let config_dir = crate::core::paths::config_dir_read_only().expect("isolated config directory");
    std::fs::create_dir_all(&config_dir).expect("create isolated config directory");
    std::fs::write(
        config_dir.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("write isolated gateway policy");
    data
}

fn lifecycle_request() -> serde_json::Value {
    serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "bounded transport lifecycle regression"}]
    })
}

async fn prepared_driver(upstream: &str) -> (ProxyDriver<'_, TestCompressor>, TaskContext) {
    prepared_driver_on(proxy_test_state(upstream), upstream).await
}

/// Windows refuses a closed port only after SYN retries (~2 s): outlast that.
async fn prepared_refused_driver(upstream: &str) -> (ProxyDriver<'_, TestCompressor>, TaskContext) {
    let mut state = proxy_test_state(upstream);
    state.client = crate::proxy::upstream_http_client_builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("refused-upstream client");
    prepared_driver_on(state, upstream).await
}

async fn prepared_driver_on(
    state: ProxyState,
    upstream: &str,
) -> (ProxyDriver<'_, TestCompressor>, TaskContext) {
    let body = lifecycle_request();
    let mut driver = proxy_test_driver(state, &body, upstream);
    let context = proxy_test_context();
    driver
        .apply_security_boundaries(&context)
        .await
        .expect("security admission");
    driver
        .gather_context_strategy(&context)
        .await
        .expect("request preparation and egress admission");
    (driver, context)
}

fn prepared_admission_digest(driver: &ProxyDriver<'_, TestCompressor>) -> Sha256Digest {
    let Some(PreparedProxyRequest::Upstream(outbound)) = driver.prepared_request.as_ref() else {
        panic!("expected a prepared upstream request");
    };
    outbound
        .forwarded_body
        .admission_digest()
        .cloned()
        .expect("governed admission receipt was persisted before dispatch")
}

fn latest_engine_observations(
    expected_admission_digest: &Sha256Digest,
) -> Vec<EgressTransportObservationV1> {
    let (_, loaded) = crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        1,
    )
    .into_iter()
    .next()
    .expect("proxy decision receipt indexed");
    let decision = loaded.expect("proxy decision receipt verifies");
    let lifecycle =
        crate::core::context_admission::egress::load_egress_for_decision(&decision.digest)
            .expect("load linked egress lifecycle")
            .expect("proxy egress lifecycle exists");
    let admission = lifecycle.admission.expect("persisted admission receipt");
    assert_eq!(
        admission.digest, *expected_admission_digest,
        "transport must reference the exact persisted admission"
    );
    assert_eq!(
        admission.admission.outcome,
        EgressAdmissionOutcomeV1::Prepared,
        "only a prepared admission may be dispatched"
    );
    lifecycle
        .observations
        .into_iter()
        .map(|stored| stored.observation)
        .collect()
}

async fn read_bounded_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = tokio::time::timeout(READ_BOUND, stream.read(&mut buffer))
            .await
            .expect("bounded loopback request read")
            .expect("read loopback request");
        assert!(read > 0, "upstream closed before request headers");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= MAX_TEST_REQUEST_BYTES,
            "loopback request exceeded its test bound"
        );
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let expected = header_end.saturating_add(content_length);
    assert!(
        expected <= MAX_TEST_REQUEST_BYTES,
        "loopback request content length exceeded its test bound"
    );
    while request.len() < expected {
        let read = tokio::time::timeout(READ_BOUND, stream.read(&mut buffer))
            .await
            .expect("bounded loopback request body read")
            .expect("read loopback request body");
        assert!(read > 0, "upstream closed before request body completed");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= MAX_TEST_REQUEST_BYTES,
            "loopback request exceeded its test bound"
        );
    }
    request[header_end..expected].to_vec()
}

async fn body_observing_upstream() -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let address = listener.local_addr().expect("loopback address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(ACCEPT_FIRST, listener.accept())
            .await
            .expect("bounded upstream accept")
            .expect("accept loopback request");
        let body = read_bounded_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
            )
            .await
            .expect("write upstream response");
        let _ = stream.shutdown().await;
        body
    });
    (format!("http://{address}"), server)
}

fn body_digest(bytes: &[u8]) -> Sha256Digest {
    let digest = sha2::Sha256::digest(bytes);
    let hex = crate::core::agent_identity::hex_encode(&digest);
    Sha256Digest::new(format!("sha256:{hex}")).expect("canonical SHA-256 digest")
}

async fn status_sequence_upstream(statuses: Vec<u16>) -> (String, JoinHandle<Vec<u16>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let address = listener.local_addr().expect("loopback address");
    let server = tokio::spawn(async move {
        let mut observed = Vec::new();
        for (index, status) in statuses.into_iter().enumerate() {
            let accept_window = if index == 0 {
                ACCEPT_FIRST
            } else {
                ACCEPT_RETRY
            };
            let accepted = tokio::time::timeout(accept_window, listener.accept()).await;
            let Ok(Ok((mut stream, _))) = accepted else {
                break;
            };
            let _ = read_bounded_request(&mut stream).await;
            let reason = match status {
                200 => "OK",
                503 => "Service Unavailable",
                _ => "Test Response",
            };
            let wire = format!(
                "HTTP/1.1 {status} {reason}\r\n\
                 content-type: application/json\r\n\
                 content-length: 2\r\n\
                 connection: close\r\n\r\n\
                 {{}}"
            );
            stream
                .write_all(wire.as_bytes())
                .await
                .expect("write response");
            let _ = stream.shutdown().await;
            observed.push(status);
        }
        observed
    });
    (format!("http://{address}"), server)
}

async fn dispatch(
    driver: &mut ProxyDriver<'_, TestCompressor>,
    context: &TaskContext,
) -> ProxyPrimitive {
    tokio::time::timeout(DISPATCH_BOUND, driver.dispatch_primitive(context))
        .await
        .expect("bounded proxy dispatch")
        .expect("proxy dispatch stage")
        .0
}

fn upstream_result(primitive: ProxyPrimitive) -> Result<reqwest::Response, StatusCode> {
    match primitive {
        ProxyPrimitive::Upstream { response, .. } => response,
        ProxyPrimitive::Cache { .. } => panic!("unexpected response-cache dispatch"),
        ProxyPrimitive::Policy { .. } => panic!("unexpected policy refusal"),
    }
}

fn assert_response_observation(
    observation: &EgressTransportObservationV1,
    attempt: u32,
    status: u16,
) {
    assert_eq!(observation.attempt, attempt);
    assert_eq!(observation.transport, EgressTransportV1::Http);
    assert_eq!(
        observation.observer,
        EgressTransportObserverV1::EngineObserved
    );
    assert_eq!(
        observation.result,
        EgressTransportResultV1::ResponseObserved
    );
    assert_eq!(observation.http_status, Some(status));
    assert_eq!(observation.failure, None);
}

#[tokio::test]
async fn retry_records_each_status_the_engine_observed() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = status_sequence_upstream(vec![503, 200]).await;
    let (mut driver, context) = prepared_driver(&upstream).await;
    let admission_digest = prepared_admission_digest(&driver);
    assert!(
        latest_engine_observations(&admission_digest).is_empty(),
        "the persisted admission exists before transport, with no attempt yet"
    );

    let response =
        upstream_result(dispatch(&mut driver, &context).await).expect("successful retry response");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(server.await.expect("loopback server"), vec![503, 200]);

    let observations = latest_engine_observations(&admission_digest);
    assert_eq!(observations.len(), 2);
    assert_response_observation(&observations[0], 1, 503);
    assert_response_observation(&observations[1], 2, 200);
}

#[tokio::test]
async fn upstream_redirect_never_replays_admitted_body_to_another_origin() {
    for status in [307_u16, 308] {
        let _data = isolated_governed_gateway();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let secondary = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = format!("http://{}/unapproved", secondary.local_addr().unwrap());
        let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", primary.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = tokio::time::timeout(ACCEPT_FIRST, primary.accept())
                .await
                .unwrap()
                .unwrap();
            let body = read_bounded_request(&mut stream).await;
            let reply = format!(
                "HTTP/1.1 {status} Redirect\r\nlocation: {redirect}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            body
        });
        let (mut driver, context) = prepared_driver(&upstream).await;
        let digest = prepared_admission_digest(&driver);
        let primitive = dispatch(&mut driver, &context).await;
        let (processed, _) = driver
            .reversible_post_process(&context, primitive)
            .await
            .unwrap();
        assert_eq!(processed.result.error, None);
        {
            let response = processed.result.response.lock().unwrap();
            let response = response.as_ref().expect("original upstream response");
            assert_eq!(response.status().as_u16(), status);
            assert!(response.headers().get("location").is_none());
        }
        assert!(
            !server.await.unwrap().is_empty(),
            "primary received the body"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), secondary.accept())
                .await
                .is_err(),
            "a redirect must not establish a second upstream connection"
        );
        let observations = latest_engine_observations(&digest);
        assert_eq!(observations.len(), 1);
        assert_response_observation(&observations[0], 1, status);
    }
}

#[tokio::test]
async fn refused_loopback_connection_is_recorded_as_no_response() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve loopback port");
    let address = listener.local_addr().expect("reserved loopback address");
    drop(listener);
    let upstream = format!("http://{address}");
    let (mut driver, context) = prepared_refused_driver(&upstream).await;
    let admission_digest = prepared_admission_digest(&driver);

    let response = upstream_result(dispatch(&mut driver, &context).await);
    assert_eq!(response.unwrap_err(), StatusCode::BAD_GATEWAY);

    let observations = latest_engine_observations(&admission_digest);
    assert_eq!(observations.len(), 2, "connect failure is retried once");
    for (expected_attempt, observation) in observations.iter().enumerate() {
        assert_eq!(observation.attempt, expected_attempt as u32 + 1);
        assert_eq!(observation.transport, EgressTransportV1::Http);
        assert_eq!(
            observation.observer,
            EgressTransportObserverV1::EngineObserved
        );
        assert_eq!(
            observation.result,
            EgressTransportResultV1::NoResponseObserved
        );
        assert_eq!(observation.http_status, None);
        assert_eq!(observation.failure, Some(EgressTransportFailureV1::Connect));
    }
}

#[tokio::test]
async fn bedrock_redaction_is_the_exact_prepared_and_forwarded_body() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let secret = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let body = serde_json::json!({
        "model": "anthropic.claude-test",
        "messages": [{"role": "user", "content": format!("deploy with {secret} today")}]
    });
    let (upstream, server) = body_observing_upstream().await;
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    driver.provider_label = "Bedrock";
    driver.default_path = "/model/demo/invoke";
    driver.request = Some(
        axum::http::Request::builder()
            .method("POST")
            .uri("/model/demo/invoke")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&body).expect("encode request"),
            ))
            .expect("build Bedrock request"),
    );
    let context = proxy_test_context();
    driver
        .apply_security_boundaries(&context)
        .await
        .expect("security admission");
    driver
        .gather_context_strategy(&context)
        .await
        .expect("request preparation and egress admission");
    let admission_digest = prepared_admission_digest(&driver);

    let response =
        upstream_result(dispatch(&mut driver, &context).await).expect("loopback upstream response");
    assert_eq!(response.status().as_u16(), 200);
    let wire_body = server.await.expect("captured loopback body");
    let wire_text = String::from_utf8(wire_body.clone()).expect("JSON request body");
    assert!(
        !wire_text.contains(secret),
        "secret reached upstream: {wire_text}"
    );
    assert!(wire_text.contains("deploy with"), "{wire_text}");

    let expected_digest = body_digest(&wire_body);
    let (_, loaded) = crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        1,
    )
    .into_iter()
    .next()
    .expect("proxy decision receipt indexed");
    let decision = loaded.expect("proxy decision receipt verifies");
    assert_eq!(
        decision.receipt.final_context,
        Some(expected_digest.clone())
    );
    let lifecycle =
        crate::core::context_admission::egress::load_egress_for_decision(&decision.digest)
            .expect("load linked egress lifecycle")
            .expect("proxy egress lifecycle exists");
    let admission = lifecycle.admission.expect("prepared admission exists");
    assert_eq!(admission.digest, admission_digest);
    assert_eq!(
        admission.admission.outcome,
        EgressAdmissionOutcomeV1::Prepared
    );
    assert_eq!(
        admission.admission.prepared_bytes_digest,
        Some(expected_digest),
        "the prepared receipt must bind the exact upstream body"
    );
    assert_eq!(latest_engine_observations(&admission_digest).len(), 1);
}

#[tokio::test]
async fn first_observation_persistence_failure_prevents_retry() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = status_sequence_upstream(vec![503, 200]).await;
    let (mut driver, context) = prepared_driver(&upstream).await;
    let admission_digest = prepared_admission_digest(&driver);
    assert!(
        latest_engine_observations(&admission_digest).is_empty(),
        "the real prepared admission is persisted before fault injection"
    );

    let data_dir = crate::core::data_dir::lean_ctx_data_dir().expect("isolated data directory");
    let observation_dir = data_dir.join("gateway/egress/observations");
    assert!(
        !observation_dir.exists(),
        "admission persistence must not pre-create the observation object directory"
    );
    std::fs::write(
        &observation_dir,
        b"intentional observation-store obstruction",
    )
    .expect("inject a local observation persistence failure");
    assert!(observation_dir.is_file());

    let response = upstream_result(dispatch(&mut driver, &context).await)
        .expect("the observed HTTP response remains available");
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(
        server.await.expect("loopback server"),
        vec![503],
        "once the first send may have reached the provider, a receipt write failure must not retry"
    );
    assert!(
        latest_engine_observations(&admission_digest).is_empty(),
        "a failed persistence attempt must not create a false observation"
    );
}

/// AP10-C5: the observation of the retry that could not be saved was only a
/// log line — the stored lifecycle then shows attempt 1 (503) and nothing
/// else, while the reply is attempt 2's 200. The reply now states the gap.
#[tokio::test]
async fn a_lost_retry_observation_is_stated_on_the_reply() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = status_sequence_upstream(vec![503, 200]).await;
    let (mut driver, context) = prepared_driver(&upstream).await;
    let admission_digest = prepared_admission_digest(&driver);
    // Attempt 2's slot is taken by a different observation, so saving the
    // real one fails — deterministically, after attempt 1 was recorded.
    let planted = EgressTransportObservationV1 {
        schema_version: 1,
        admission_receipt_digest: admission_digest.clone(),
        attempt: 2,
        transport: EgressTransportV1::Http,
        observer: EgressTransportObserverV1::EngineObserved,
        result: EgressTransportResultV1::NoResponseObserved,
        http_status: None,
        failure: Some(EgressTransportFailureV1::Timeout),
    };
    crate::core::context_admission::egress::append_engine_observation(&planted)
        .expect("plant attempt 2");

    let primitive = dispatch(&mut driver, &context).await;
    let (processed, _) = driver
        .reversible_post_process(&context, primitive)
        .await
        .expect("post-process");
    assert_eq!(server.await.expect("loopback server"), vec![503, 200]);
    {
        let response = processed.result.response.lock().unwrap();
        let response = response.as_ref().expect("the retry's response");
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get(transport::EGRESS_EVIDENCE_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(transport::OBSERVATION_UNAVAILABLE)
        );
    }
    let observations = latest_engine_observations(&admission_digest);
    assert_response_observation(&observations[0], 1, 503);
    assert_eq!(observations[1], planted, "attempt 2 is not on record");

    // The same gap where no response came back and the proxy answers 502
    // itself: both attempts were refused connections.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("port");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let upstream = format!("http://{address}");
    let (mut driver, context) = prepared_refused_driver(&upstream).await;
    let admission_digest = prepared_admission_digest(&driver);
    crate::core::context_admission::egress::append_engine_observation(
        &EgressTransportObservationV1 {
            admission_receipt_digest: admission_digest,
            ..planted
        },
    )
    .expect("plant attempt 2");
    let primitive = dispatch(&mut driver, &context).await;
    let (processed, _) = driver
        .reversible_post_process(&context, primitive)
        .await
        .expect("post-process");
    assert_eq!(processed.result.error, Some(StatusCode::BAD_GATEWAY));
    let response = processed.result.response.lock().unwrap();
    let response = response.as_ref().expect("the proxy's stated answer");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers()[transport::EGRESS_EVIDENCE_HEADER],
        transport::OBSERVATION_UNAVAILABLE
    );
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Anthropic,
    OpenAiChat,
    OpenAiResponses,
    Gemini,
}

impl Route {
    fn uri(self) -> &'static str {
        match self {
            Self::Anthropic => "/v1/messages",
            Self::OpenAiChat => "/v1/chat/completions",
            Self::OpenAiResponses => "/v1/responses",
            Self::Gemini => "/v1beta/models/gemini-pro:generateContent",
        }
    }

    /// A request of this route's shape with `value` in one protocol position
    /// the egress walker used to treat as structure.
    fn body(self, position: &str, value: &str) -> serde_json::Value {
        let schema = || {
            serde_json::json!({
                "type": "object",
                "properties": { value: { "type": "string", "description": "d" } }
            })
        };
        let mut body = match self {
            Self::Anthropic => serde_json::json!({
                "model": "claude-test",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]
            }),
            Self::OpenAiChat => serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role": "user", "content": "hello"}]
            }),
            Self::OpenAiResponses => serde_json::json!({
                "model": "gpt-test",
                "input": [{"role": "user", "content": "hello"}]
            }),
            Self::Gemini => serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
            }),
        };
        match (self, position) {
            (Self::Anthropic, "tool-input-name") => {
                body["messages"] = serde_json::json!([
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": [{
                        "type": "tool_use", "id": "toolu_1", "name": "lookup",
                        "input": {"name": value}
                    }]},
                    {"role": "user", "content": [{
                        "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"
                    }]}
                ]);
            }
            (Self::Anthropic, "object-key") => {
                body["messages"] = serde_json::json!([
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": [{
                        "type": "tool_use", "id": "toolu_1", "name": "lookup",
                        "input": {value: 1}
                    }]},
                    {"role": "user", "content": [{
                        "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"
                    }]}
                ]);
            }
            (Self::Anthropic, "stop") => body["stop_sequences"] = serde_json::json!([value]),
            (Self::Anthropic, "tool-schema") => {
                body["tools"] = serde_json::json!([{
                    "name": "lookup", "description": format!("use {value}"),
                    "input_schema": schema()
                }]);
            }
            (Self::OpenAiChat, "tool-input-name" | "object-key") => {
                let arguments = if position == "object-key" {
                    serde_json::json!({ value: 1 })
                } else {
                    serde_json::json!({ "name": value })
                };
                body["messages"] = serde_json::json!([
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "tool_calls": [{
                        "id": "call_1", "type": "function",
                        "function": {"name": "lookup", "arguments": arguments}
                    }]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
                ]);
            }
            (Self::OpenAiChat | Self::OpenAiResponses, "stop") => {
                body["stop"] = serde_json::json!([value]);
            }
            (Self::OpenAiChat, "tool-schema") => {
                body["tools"] = serde_json::json!([{
                    "type": "function",
                    "function": {
                        "name": "lookup", "description": format!("use {value}"),
                        "parameters": schema()
                    }
                }]);
            }
            (Self::OpenAiResponses, "tool-input-name" | "object-key") => {
                let arguments = if position == "object-key" {
                    serde_json::json!({ value: 1 })
                } else {
                    serde_json::json!({ "name": value })
                };
                body["input"] = serde_json::json!([
                    {"role": "user", "content": "hello"},
                    {"type": "function_call", "call_id": "call_1", "name": "lookup",
                     "arguments": arguments},
                    {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
                ]);
            }
            (Self::OpenAiResponses, "tool-schema") => {
                body["tools"] = serde_json::json!([{
                    "type": "function", "name": "lookup",
                    "description": format!("use {value}"), "parameters": schema()
                }]);
            }
            (Self::Gemini, "tool-input-name" | "object-key") => {
                let args = if position == "object-key" {
                    serde_json::json!({ value: 1 })
                } else {
                    serde_json::json!({ "name": value })
                };
                body["contents"] = serde_json::json!([
                    {"role": "user", "parts": [{"text": "hello"}]},
                    {"role": "model", "parts": [{"functionCall": {"name": "lookup", "args": args}}]}
                ]);
            }
            (Self::Gemini, "stop") => {
                body["generationConfig"] = serde_json::json!({ "stopSequences": [value] });
            }
            (Self::Gemini, "tool-schema") => {
                body["tools"] = serde_json::json!([{"functionDeclarations": [{
                    "name": "lookup", "description": format!("use {value}"),
                    "parameters": schema()
                }]}]);
            }
            _ => {}
        }
        body
    }
}

/// Send `body` through the real handler of `route` against a recording
/// loopback upstream. Returns the proxy's answer and the body the upstream
/// received, if any connection was made.
async fn through_route(
    route: Route,
    body: Vec<u8>,
) -> (Result<Response, StatusCode>, Option<Vec<u8>>) {
    through_route_at(route, route.uri(), body).await
}

async fn through_route_at(
    route: Route,
    uri: &str,
    body: Vec<u8>,
) -> (Result<Response, StatusCode>, Option<Vec<u8>>) {
    through_route_bound(route, uri, body, None).await
}

/// [`through_route`] to the loopback upstream declared a remote provider by
/// the registry, as a tunnelled cloud endpoint is: the destination rules of
/// a remote model apply.
async fn through_remote_route(
    route: Route,
    body: Vec<u8>,
) -> (Result<Response, StatusCode>, Option<Vec<u8>>) {
    let remote = crate::proxy::providers::RegistryProviderId {
        id: "anthropic".into(),
        local: false,
        account_ref: None,
        region: None,
        gateway_managed_credential: false,
    };
    through_route_bound(route, route.uri(), body, Some(remote)).await
}

async fn through_route_bound(
    route: Route,
    uri: &str,
    body: Vec<u8>,
    registry: Option<crate::proxy::providers::RegistryProviderId>,
) -> (Result<Response, StatusCode>, Option<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let upstream = format!(
        "http://{}",
        listener.local_addr().expect("loopback address")
    );
    let (recorded, mut received) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = recorded.send(read_bounded_request(&mut stream).await);
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await;
            let _ = stream.shutdown().await;
        }
    });
    let mut request = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("request");
    if let Some(registry) = registry {
        request.extensions_mut().insert(registry);
    }
    let state = State(proxy_test_state(&upstream));
    let handled = async {
        match route {
            Route::Anthropic => crate::proxy::anthropic::handler(state, request).await,
            Route::OpenAiChat => crate::proxy::openai::handler(state, request).await,
            Route::OpenAiResponses => crate::proxy::openai_responses::handler(state, request).await,
            Route::Gemini => crate::proxy::google::handler(state, request).await,
        }
    };
    let result = tokio::time::timeout(DISPATCH_BOUND, handled)
        .await
        .expect("bounded proxy route");
    let seen = tokio::time::timeout(Duration::from_millis(50), received.recv())
        .await
        .ok()
        .flatten();
    server.abort();
    (result, seen)
}

const ROUTES: [Route; 4] = [
    Route::Anthropic,
    Route::OpenAiChat,
    Route::OpenAiResponses,
    Route::Gemini,
];

/// A protected value leaves through no protocol position: not as a tool
/// input member that happens to be called `name`, not as a stop sequence,
/// not in a tool schema's description or property name, not as an object
/// key. It is masked where the position may be rewritten and the request is
/// refused with the typed reason where it may not — never forwarded raw.
///
/// Admission API under test: `proxy::admitted::admit` →
/// `egress::admit_request_with_binding`. Fails until the shared egress
/// walker stops exempting members by key name alone.
#[tokio::test]
async fn protected_value_leaves_through_no_protocol_position() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_, secret) = masked_request();
    for route in ROUTES {
        for position in ["tool-input-name", "stop", "tool-schema", "object-key"] {
            let body = serde_json::to_vec(&route.body(position, secret)).expect("encode");
            let (result, seen) = through_route(route, body).await;
            if let Some(seen) = &seen {
                assert!(
                    !String::from_utf8_lossy(seen).contains(secret),
                    "{route:?}/{position}: the raw value reached the upstream"
                );
            } else {
                let response = result.expect("a refusal is a typed response");
                assert!(
                    response
                        .headers()
                        .contains_key(crate::proxy::admitted::REASON_CODES_HEADER),
                    "{route:?}/{position}: nothing was sent, so the answer must be the typed \
                     refusal"
                );
            }
        }
    }
}

/// AP10-C2: the caller's path and query left with the body without being
/// admitted, so a protected value in the URL reached the upstream on every
/// route. Now the URL is admitted like the body: such a request is refused
/// with the typed reason and nothing is sent — also percent-encoded, and
/// also as a `key` parameter, which is a credential only of Google's hosts
/// and only in its own shape. A clean URL still passes.
#[tokio::test]
async fn a_protected_value_in_the_request_url_is_refused_not_sent() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (_, secret) = masked_request();
    let encoded = secret
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect::<Vec<_>>()
        .concat();
    let clean = |route: Route| serde_json::to_vec(&route.body("stop", "END")).expect("encode");
    for route in ROUTES {
        for query in [
            format!("note={secret}"),
            format!("note={encoded}"),
            format!("key={secret}"),
        ] {
            let uri = format!("{}?{query}", route.uri());
            let (result, seen) = through_route_at(route, &uri, clean(route)).await;
            assert!(
                seen.is_none(),
                "{route:?}?{query}: the URL reached the upstream"
            );
            let response = result.expect("a refusal is a typed response");
            assert!(
                response
                    .headers()
                    .contains_key(crate::proxy::admitted::REASON_CODES_HEADER),
                "{route:?}?{query}: nothing was sent, so the answer must be the typed refusal"
            );
        }
    }
    let uri = format!("{}?alt=sse&ts=1760781123456", Route::Gemini.uri());
    let (_, seen) = through_route_at(Route::Gemini, &uri, clean(Route::Gemini)).await;
    assert!(seen.is_some(), "a clean URL passes");
}

/// Controls: a clean request, and one carrying a genuine provider-signed
/// block, reach the upstream byte for byte.
#[tokio::test]
async fn clean_and_provider_signed_requests_arrive_byte_identical() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    for route in ROUTES {
        let body = serde_json::to_vec(&route.body("clean", "")).expect("encode");
        let (result, seen) = through_route(route, body.clone()).await;
        assert!(result.is_ok(), "{route:?}");
        assert_eq!(seen.as_deref(), Some(body.as_slice()), "{route:?}");
    }
    let signed = serde_json::to_vec(&serde_json::json!({
        "model": "claude-test",
        "max_tokens": 16,
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan the answer", "signature": "c2lnbmVkLWJsb2Nr"},
                {"type": "text", "text": "hi"}
            ]},
            {"role": "user", "content": "continue"}
        ]
    }))
    .expect("encode");
    let (result, seen) = through_route(Route::Anthropic, signed.clone()).await;
    assert!(result.is_ok());
    assert_eq!(seen.as_deref(), Some(signed.as_slice()));
}

/// What "byte identical" above depends on: the proxy mode. In the default
/// `token` mode the verbosity steer is appended to the last user turn, and
/// that is the only change — every other turn, a provider-signed block among
/// them, and every other member of the request arrive as they were sent.
#[tokio::test]
async fn token_mode_changes_only_the_last_user_turn() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("token");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let sent = serde_json::json!({
        "model": "claude-test",
        "max_tokens": 16,
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan the answer", "signature": "c2lnbmVkLWJsb2Nr"},
                {"type": "text", "text": "hi"}
            ]},
            {"role": "user", "content": "continue"}
        ]
    });
    let body = serde_json::to_vec(&sent).expect("encode");
    let (result, seen) = through_route(Route::Anthropic, body).await;
    assert!(result.is_ok());
    let mut seen: serde_json::Value =
        serde_json::from_slice(&seen.expect("forwarded")).expect("forwarded JSON");
    let last = seen["messages"][2]["content"]
        .as_str()
        .expect("last user turn is still text")
        .to_owned();
    assert!(
        last.starts_with("continue\n\nBe concise:"),
        "the steer follows the user's own text: {last}"
    );
    seen["messages"][2]["content"] = serde_json::json!("continue");
    assert_eq!(seen, sent, "nothing else changed");
}

/// The 25./26.10. lockouts end to end: a tool result in the history whose
/// content may not go to a remote model is withheld in place; the request,
/// and every later turn re-sending that history, reaches the upstream.
#[tokio::test]
async fn a_restricted_tool_result_in_the_history_never_locks_the_session_out() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let turn = |next: &str| {
        serde_json::json!({
            "model": "claude-test",
            "max_tokens": 16,
            "messages": [
                {"role": "user", "content": "list the dev stack"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {"command": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1",
                     "content": "*** TOP SECRET ***\nlaunch window and codes"}
                ]},
                {"role": "user", "content": next}
            ]
        })
    };
    let mut notices = Vec::new();
    for next in ["continue", "and the next step"] {
        let body = serde_json::to_vec(&turn(next)).expect("encode");
        let (result, seen) = through_remote_route(Route::Anthropic, body).await;
        let response = result.expect("the request was answered");
        let Some(seen) = seen else {
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("refusal body");
            panic!(
                "{next}: not forwarded: {status} {}",
                String::from_utf8_lossy(&body)
            );
        };
        let seen: serde_json::Value = serde_json::from_slice(&seen).expect("forwarded JSON");
        let notice = seen["messages"][2]["content"][0]["content"]
            .as_str()
            .expect("the tool result is a notice")
            .to_owned();
        assert!(notice.contains("destination.remote_restricted"), "{notice}");
        assert!(notice.contains("lean-ctx gateway why w-"), "{notice}");
        assert!(!seen.to_string().contains("launch window"), "{seen}");
        // The last user turn may carry the proxy's verbosity steer.
        let last = seen["messages"][3]["content"].as_str().expect("last turn");
        assert!(last.starts_with(next), "{last}");
        notices.push(notice);
    }
    assert_eq!(notices[0], notices[1], "a re-send is decided the same way");
}

/// A classified original is blocked by the project rule on every route:
/// typed refusal, no upstream connection.
#[tokio::test]
async fn project_rule_blocks_a_classified_original_on_every_route() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(project_policy(
        "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
    )));
    for route in ROUTES {
        let body = route
            .body("clean", "")
            .to_string()
            // A banner line of its own marks content as classified; the
            // label inside a sentence is not a marking.
            .replace("hello", "CONFIDENTIAL\\nroadmap for the board");
        let (result, seen) = through_route(route, body.into_bytes()).await;
        assert!(seen.is_none(), "{route:?}: a blocked original was sent");
        let response = result.expect("a refusal is a typed response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{route:?}");
        assert!(
            response
                .headers()
                .contains_key(crate::proxy::admitted::REASON_CODES_HEADER),
            "{route:?}"
        );
    }
}

/// Answers the first request with 503 and every later one with 200, running
/// `after_first` once the first request is read and before it is answered —
/// that is, strictly before the proxy can start its retry.
async fn retry_upstream(
    after_first: impl FnOnce() + Send + 'static,
) -> (String, JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let address = listener.local_addr().expect("loopback address");
    let server = tokio::spawn(async move {
        let mut bodies = Vec::new();
        let mut after_first = Some(after_first);
        for (index, status) in ["503 Service Unavailable", "200 OK"]
            .into_iter()
            .enumerate()
        {
            let window = if index == 0 {
                ACCEPT_FIRST
            } else {
                ACCEPT_RETRY
            };
            let accepted = tokio::time::timeout(window, listener.accept()).await;
            let Ok(Ok((mut stream, _))) = accepted else {
                break;
            };
            bodies.push(read_bounded_request(&mut stream).await);
            if let Some(hook) = after_first.take() {
                hook();
            }
            let wire = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: 2\r\nconnection: close\r\n\r\n{{}}"
            );
            stream
                .write_all(wire.as_bytes())
                .await
                .expect("write response");
            let _ = stream.shutdown().await;
        }
        bodies
    });
    (format!("http://{address}"), server)
}

fn masked_request() -> (serde_json::Value, &'static str) {
    let secret = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": format!("deploy with {secret} today")}]
    });
    (body, secret)
}

/// The retry is not a new decision: when the policy changes during the
/// backoff, the bytes admitted under the old policy are not sent again. The
/// upstream sees exactly the first attempt and the caller gets the typed
/// refusal.
#[tokio::test]
async fn policy_change_during_backoff_stops_the_retry() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("isolated config directory");
    std::fs::create_dir_all(&config).expect("create isolated config directory");
    let (upstream, server) = retry_upstream(move || {
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("tighten the gateway policy");
    })
    .await;
    let (body, _) = masked_request();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("encode")))
        .expect("request");
    let result = tokio::time::timeout(
        DISPATCH_BOUND,
        forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("bounded forward");
    assert_typed_refusal(result, "policy.changed").await;
    assert_eq!(
        server.await.expect("loopback server").len(),
        1,
        "the retry must not be dispatched under a policy the bytes were not admitted under"
    );
}

/// Control: under an unchanged policy the retry resends the bound bytes —
/// byte-identical on both attempts, masked, and the ones the receipt binds.
#[tokio::test]
async fn unchanged_policy_retries_the_identical_bound_bytes() {
    let _data = isolated_governed_gateway();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = retry_upstream(|| {}).await;
    let (body, secret) = masked_request();
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    let context = proxy_test_context();
    driver
        .apply_security_boundaries(&context)
        .await
        .expect("security admission");
    driver
        .gather_context_strategy(&context)
        .await
        .expect("request preparation and egress admission");
    let Some(PreparedProxyRequest::Upstream(outbound)) = driver.prepared_request.as_ref() else {
        panic!("expected a prepared upstream request");
    };
    let bound = outbound.forwarded_body.bytes().to_vec();
    let admission_digest = prepared_admission_digest(&driver);

    let response =
        upstream_result(dispatch(&mut driver, &context).await).expect("successful retry response");
    assert_eq!(response.status().as_u16(), 200);
    let bodies = server.await.expect("loopback server");
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bound, "attempt 1 carries the bound bytes");
    assert_eq!(bodies[1], bound, "the retry carries the same bound bytes");
    assert!(!String::from_utf8_lossy(&bound).contains(secret));
    assert_eq!(latest_engine_observations(&admission_digest).len(), 2);
}

const POLICY_A: &str = "[context_gateway]\nmode = \"developer\"\n";
const POLICY_B: &str = "[context_gateway]\nenabled = false\n";
const BOUNDARY_REQUEST: &[u8] = br#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#;

/// Forward [`BOUNDARY_REQUEST`] through the shared path with a compressor
/// that changes the body, so both admissions run.
async fn forward_boundary_request(upstream: &str) -> Result<Response, StatusCode> {
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(BOUNDARY_REQUEST))
        .expect("request");
    tokio::time::timeout(
        DISPATCH_BOUND,
        forward_request(
            State(proxy_test_state(upstream)),
            request,
            upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("the request is refused or answered, never left waiting for an upstream")
}

async fn assert_no_upstream_connection(listener: &TcpListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "nothing may reach the upstream"
    );
}

/// Snapshot boundary 1: the policy changes after the authority was captured
/// and changes back after the shared admission decided under it. The
/// decision was taken under B; it must not leave bound to authority A.
#[tokio::test]
async fn decision_under_a_transient_policy_is_refused() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(POLICY_A);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let upstream = format!("http://{}", listener.local_addr().expect("address"));
    let mut changed = false;
    let _armed = failpoint::arm(move |point| match point {
        Point::AuthorityCaptured if !changed => {
            changed = true;
            write_gateway_config(POLICY_B);
        }
        Point::Decided => write_gateway_config(POLICY_A),
        _ => {}
    });

    let result = forward_boundary_request(&upstream).await;
    assert_typed_refusal(result, "policy.changed").await;
    assert_no_upstream_connection(&listener).await;
}

/// Two permitted semantic runtimes (existing staging directories) under the
/// SAME gateway and secret settings: only the runtime differs between the two
/// configuration texts, and a detector stays present in both.
struct TwoRuntimes {
    _first: tempfile::TempDir,
    _second: tempfile::TempDir,
    a: String,
    b: String,
    _stand_in: Box<dyn std::any::Any>,
}

fn two_semantic_runtimes() -> TwoRuntimes {
    use crate::core::intelligence_runtime::semantic_detectors::test_seam;
    let (first, second) = (
        tempfile::tempdir().expect("runtime root"),
        tempfile::tempdir().expect("runtime root"),
    );
    let text = |directory: &tempfile::TempDir| {
        let root = std::fs::canonicalize(directory.path()).expect("canonical runtime root");
        format!(
            "[context_gateway]\nmode = \"developer\"\n[intelligence_runtime]\nenabled = true\n\
             accept_proprietary = true\nstaging = true\nroot = '{}'\n",
            root.to_str().expect("utf-8 runtime root")
        )
    };
    let (a, b) = (text(&first), text(&second));
    // Stands in for the private runtime process: every text is clean.
    let stand_in = test_seam::stand_in(|_, text| Ok(test_seam::clean_answer(text)));
    TwoRuntimes {
        _first: first,
        _second: second,
        a,
        b,
        _stand_in: Box::new(stand_in),
    }
}

/// Q5 at the adapter, no project policy, gateway fields unchanged: the
/// semantic runtime changes Some(A) → Some(B) after the caller authority was
/// found current. The final admission decides under B; the transformed body
/// was admitted under A and must not be re-authorised under B.
#[tokio::test]
async fn semantic_runtime_change_before_the_final_admission_is_refused() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let runtimes = two_semantic_runtimes();
    write_gateway_config(&runtimes.a);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let upstream = format!("http://{}", listener.local_addr().expect("address"));
    let other = runtimes.b.clone();
    let _armed = failpoint::arm(move |point| {
        if point == Point::CallerVerified {
            write_gateway_config(&other);
        }
    });

    let result = forward_boundary_request(&upstream).await;
    assert_typed_refusal(result, "policy.changed").await;
    assert_no_upstream_connection(&listener).await;
}

/// Q5 at dispatch: the semantic runtime changes Some(A) → Some(B) during the
/// retry backoff. The bytes bound under A are not sent again.
#[tokio::test]
async fn semantic_runtime_change_during_backoff_stops_the_retry() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let runtimes = two_semantic_runtimes();
    write_gateway_config(&runtimes.a);
    let other = runtimes.b.clone();
    let (upstream, server) = retry_upstream(move || write_gateway_config(&other)).await;
    let (body, _) = masked_request();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("encode")))
        .expect("request");
    let result = tokio::time::timeout(
        DISPATCH_BOUND,
        forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("bounded forward");
    assert_typed_refusal(result, "policy.changed").await;
    assert_eq!(
        server.await.expect("loopback server").len(),
        1,
        "the retry must not be dispatched under another semantic runtime"
    );
}

/// Control for the two tests above: the SAME runtime written again is no
/// policy change — the request crosses both admissions and reaches the
/// upstream.
#[tokio::test]
async fn the_same_semantic_runtime_written_again_is_not_a_policy_change() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let runtimes = two_semantic_runtimes();
    write_gateway_config(&runtimes.a);
    let (upstream, server) = retry_upstream(|| {}).await;
    let same = runtimes.a.clone();
    let _armed = failpoint::arm(move |point| {
        if point == Point::CallerVerified {
            write_gateway_config(&same);
        }
    });
    let result = forward_boundary_request(&upstream).await;
    drop(result.expect("forwarded under the unchanged policy"));
    assert!(
        !server.await.expect("loopback server").is_empty(),
        "the upstream received the request"
    );
}

/// Q5 for an opaque payload held across a runtime change: its authority is
/// the full policy epoch too, so the check that every bind and dispatch runs
/// refuses it under runtime B and accepts it again under runtime A.
#[tokio::test]
async fn an_opaque_admission_does_not_outlive_its_semantic_runtime() {
    use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
    use crate::proxy::admitted::{Payload, admit};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let runtimes = two_semantic_runtimes();
    write_gateway_config(&runtimes.a);
    let target = EgressTarget {
        provider: "OpenAI",
        model: Some("m"),
        upstream_base: "http://127.0.0.1:9",
    };
    let Ok(admission) = admit(
        Payload::Opaque(64),
        &target,
        TrustedDestinationBinding::default(),
    ) else {
        panic!("developer mode admits an opaque local payload as a decision");
    };
    assert!(admission.still_current().is_ok(), "runtime A: current");
    write_gateway_config(&runtimes.b);
    assert!(
        admission.still_current().is_err(),
        "runtime B with unchanged gateway fields: no longer current"
    );
    write_gateway_config(&runtimes.a);
    assert!(
        admission.still_current().is_ok(),
        "runtime A again: current"
    );
}

/// Holds the policy-override lock without fixing a policy, so the project
/// pack on disk is the live policy for the duration of a test.
struct LiveProjectPolicy {
    _guard: crate::core::policy::runtime::TestOverrideGuard,
}

/// Snapshot boundary 2: a project policy appears after the caller authority
/// was found current and before the final admission. The transformed body
/// was admitted without it and must not be re-authorised under it.
#[tokio::test]
async fn project_policy_change_before_the_final_admission_is_refused() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _live = LiveProjectPolicy {
        _guard: crate::core::policy::runtime::lock_test_override(),
    };
    let project = tempfile::tempdir().expect("project root");
    let pack = project.path().join(".lean-ctx");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let upstream = format!("http://{}", listener.local_addr().expect("address"));
    let _armed = failpoint::arm(move |point| {
        if point == Point::CallerVerified {
            std::fs::create_dir_all(&pack).expect("policy directory");
            std::fs::write(
                pack.join("policy.toml"),
                "name = \"appeared\"\nversion = \"1.0.0\"\ndescription = \"boundary test\"\n",
            )
            .expect("project policy");
        }
    });

    let result = crate::core::policy::runtime::REQUEST_PROJECT
        .scope(
            std::cell::RefCell::new(Some(project.path().to_path_buf())),
            forward_boundary_request(&upstream),
        )
        .await;
    assert_typed_refusal(result, "policy.changed").await;
    assert_no_upstream_connection(&listener).await;
}

/// Control for both boundaries: with an unchanged policy the request crosses
/// all three and the upstream receives exactly the transformed body.
#[tokio::test]
async fn unchanged_policy_crosses_every_snapshot_boundary_and_is_forwarded() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(POLICY_A);
    let (upstream, server) = body_observing_upstream().await;
    let crossed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = crossed.clone();
    let _armed = failpoint::arm(move |point| seen.borrow_mut().push(point));

    let response = forward_boundary_request(&upstream)
        .await
        .expect("an admitted request is forwarded");
    assert_eq!(response.status(), StatusCode::OK);
    let (expected, _, _) = add_test_marker(
        serde_json::from_slice(BOUNDARY_REQUEST).expect("request JSON"),
        BOUNDARY_REQUEST.len(),
    );
    assert_eq!(server.await.expect("loopback upstream"), expected);
    let crossed = crossed.borrow();
    for point in [
        Point::AuthorityCaptured,
        Point::Decided,
        Point::CallerVerified,
    ] {
        assert!(crossed.contains(&point), "{point:?} was not crossed");
    }
}
