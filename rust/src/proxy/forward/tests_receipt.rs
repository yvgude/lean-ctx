// SPDX-License-Identifier: Apache-2.0
//! The bytes that leave and the one receipt that binds them, through the real
//! forward path against a recording loopback upstream: single receipt for
//! untouched, once- and twice-transformed bodies; opaque payloads; client
//! abort; partial detector coverage; prefix replay and the request frame.

#[allow(clippy::wildcard_imports)]
use super::*;
use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::ContextDecisionReceiptV1;
use sha2::Digest as _;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

async fn read_request_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .expect("bounded request read")
            .expect("read request");
        assert!(read > 0, "upstream closed before the request headers");
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |value| value.trim().parse::<usize>().expect("length"));
    while request.len() < header_end + length {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .expect("bounded body read")
            .expect("read body");
        assert!(read > 0, "upstream closed before the request body");
        request.extend_from_slice(&buffer[..read]);
    }
    request[header_end..header_end + length].to_vec()
}

/// A loopback upstream that answers one connection per entry of `statuses`
/// and returns every request body it received. `after_first` runs once the
/// first request is read and before it is answered.
///
/// It waits for connections until [`Recorder::bodies`] is called (after the
/// forward returned): never on a clock, so a slow, loaded machine cannot end
/// it before the proxy connects.
async fn recording_upstream(
    statuses: Vec<u16>,
    after_first: impl FnOnce() + Send + 'static,
) -> (String, Recorder) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let address = listener.local_addr().expect("loopback address");
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stopped = stop.clone();
    let server = tokio::spawn(async move {
        let mut bodies = Vec::new();
        let mut after_first = Some(after_first);
        for status in statuses {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                () = stopped.notified() => break,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            bodies.push(read_request_body(&mut stream).await);
            if let Some(hook) = after_first.take() {
                hook();
            }
            let wire = format!(
                "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n\
                 content-length: 2\r\nconnection: close\r\n\r\n{{}}"
            );
            let _ = stream.write_all(wire.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        bodies
    });
    (format!("http://{address}"), Recorder { server, stop })
}

/// The recording upstream of a test: [`Recorder::bodies`] ends it once the
/// forward under test has returned and hands back what it received.
struct Recorder {
    server: JoinHandle<Vec<Vec<u8>>>,
    stop: std::sync::Arc<tokio::sync::Notify>,
}

impl Recorder {
    async fn bodies(self) -> Vec<Vec<u8>> {
        // A stored permit: a loop that is not waiting yet stops at its next
        // accept, after every connection the forward made was answered.
        self.stop.notify_one();
        self.server.await.expect("upstream")
    }
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    let hex = crate::core::agent_identity::hex_encode(&sha2::Sha256::digest(bytes));
    Sha256Digest::new(format!("sha256:{hex}")).expect("canonical SHA-256 digest")
}

/// Every proxy receipt the isolated store holds.
fn proxy_receipts() -> Vec<ContextDecisionReceiptV1> {
    crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        16,
    )
    .into_iter()
    .map(|(_, loaded)| loaded.expect("receipt verifies").receipt)
    .collect()
}

fn has_reason(receipt: &ContextDecisionReceiptV1, code: &str) -> bool {
    receipt.decisions.iter().any(|decision| {
        decision
            .reason_codes
            .iter()
            .any(|reason| reason.as_str() == code)
    })
}

fn chat_request(body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("request")
}

async fn forward_chat(upstream: &str, body: Vec<u8>) -> Result<Response, StatusCode> {
    tokio::time::timeout(
        Duration::from_secs(30),
        forward_request(
            State(proxy_test_state(upstream)),
            chat_request(body),
            upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        ),
    )
    .await
    .expect("the request is refused or answered, never left waiting")
}

/// Forward a request whose user message carries a protected value through
/// the driver with `compressor`; return what the upstream received and the
/// receipts written for it.
async fn forward_masked(
    compressor: Option<TestCompressor>,
) -> (Vec<u8>, Vec<ContextDecisionReceiptV1>) {
    let key = example_access_key();
    let body = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": format!("deploy with {key} today")}]
    });
    let (upstream, server) = recording_upstream(vec![200], || {}).await;
    let mut driver = proxy_test_driver(proxy_test_state(&upstream), &body, &upstream);
    if let Some(compressor) = compressor {
        driver.compress_body = Some(compressor);
    }
    let context = proxy_test_context();
    driver.apply_security_boundaries(&context).await.unwrap();
    driver.gather_context_strategy(&context).await.unwrap();
    let _ = driver.dispatch_primitive(&context).await.unwrap();
    let mut bodies = server.bodies().await;
    assert_eq!(bodies.len(), 1, "exactly one upstream request");
    let received = bodies.remove(0);
    assert!(
        !String::from_utf8_lossy(&received).contains(key),
        "the protected value reached the upstream"
    );
    (received, proxy_receipts())
}

/// Q5: the bytes that leave are bound by exactly ONE receipt that carries the
/// caller content's redaction provenance and the final decision — for a body
/// no transformation changed, a body compression changed once, and a body
/// the final admission had to mask again after compression put a value back.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn one_receipt_binds_the_final_bytes_for_untouched_and_transformed_bodies() {
    let cases: [(&str, Option<TestCompressor>, u32); 3] = [
        ("untouched", None, 1),
        (
            "transformed once",
            Some(add_test_marker as TestCompressor),
            1,
        ),
        (
            "transformed twice",
            Some(reintroducing_compressor as TestCompressor),
            2,
        ),
    ];
    for (case, compressor, redactions) in cases {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (received, receipts) = forward_masked(compressor).await;
        assert_eq!(receipts.len(), 1, "{case}: one request, one receipt");
        let receipt = &receipts[0];
        assert!(
            receipt.security.redactions >= redactions,
            "{case}: the receipt shows {} redaction(s), expected at least {redactions}",
            receipt.security.redactions
        );
        assert_eq!(
            receipt.final_context,
            Some(sha256(&received)),
            "{case}: the receipt binds exactly the bytes the upstream received"
        );
    }
}

/// AP10-C6: the proxy tells callers apart by the credential they present,
/// but its receipt named only the agent id a caller claims. Now the trace
/// sidecar carries a keyed reference of the told-apart caller: the same for
/// the same credential, another for another, and neither the credential nor
/// its unkeyed digest.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn the_receipt_names_the_caller_the_proxy_told_apart_by_a_keyed_reference() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("governed config");
    let (upstream, server) = recording_upstream(vec![200, 200, 200], || {}).await;
    for (turn, credential) in ["Bearer caller-a", "Bearer caller-a", "Bearer caller-b"]
        .into_iter()
        .enumerate()
    {
        let body = serde_json::json!({
            "model": "gpt-5",
            "messages": [{"role": "user", "content": format!("which caller sent turn {turn}")}]
        });
        let mut request = chat_request(serde_json::to_vec(&body).expect("body"));
        request.headers_mut().insert(
            "authorization",
            axum::http::HeaderValue::from_static(credential),
        );
        let response = forward_request(
            State(proxy_test_state(&upstream)),
            request,
            &upstream,
            "/v1/chat/completions",
            add_test_marker,
            "OpenAI",
            &[],
        )
        .await
        .expect("forwarded");
        assert!(response.status().is_success());
    }
    assert_eq!(server.bodies().await.len(), 3);

    let refs: Vec<String> = crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        16,
    )
    .into_iter()
    .map(|(_, loaded)| {
        let digest = loaded.expect("receipt verifies").digest;
        match crate::core::context_admission::receipt_store::trace::load(&digest) {
            crate::core::context_admission::receipt_store::trace::StoredTrace::Bound {
                sidecar,
                ..
            } => sidecar.principal_ref.expect("the caller is named"),
            other => panic!("the governed receipt has its sidecar: {other:?}"),
        }
    })
    .collect();
    assert_eq!(refs.len(), 3, "{refs:?}");
    let distinct: std::collections::BTreeSet<&String> = refs.iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "same credential, same reference: {refs:?}"
    );
    let unkeyed = crate::proxy::forward::principal_of(
        &[(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer caller-a"),
        )]
        .into_iter()
        .collect(),
        None,
    )
    .expect("a principal");
    for reference in &refs {
        assert!(!reference.contains("caller-") && !reference.contains(&unkeyed));
    }
}

const OPAQUE_BODY: &[u8] = &[0xff, 0xfe, 0x00, 0x01, 0x80, 0x7f];

/// Developer mode forwards a body nothing can read byte for byte, and its one
/// receipt says what it is: an opaque payload, not inspected content.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn opaque_body_is_forwarded_unchanged_and_recorded_as_opaque_in_developer_mode() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (upstream, server) = recording_upstream(vec![200], || {}).await;
    let response = forward_chat(&upstream, OPAQUE_BODY.to_vec())
        .await
        .expect("an opaque body is forwarded in developer mode");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(server.bodies().await, vec![OPAQUE_BODY.to_vec()]);
    let receipts = proxy_receipts();
    assert_eq!(receipts.len(), 1);
    assert!(has_reason(&receipts[0], "egress.opaque_payload"));
    assert_eq!(receipts[0].final_context, Some(sha256(OPAQUE_BODY)));
}

/// Governed mode refuses the same body with the typed reason; nothing is sent.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn opaque_body_is_refused_in_governed_mode_without_a_connection() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config("[context_gateway]\nmode = \"governed\"\n");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let response = forward_chat(&upstream, OPAQUE_BODY.to_vec())
        .await
        .expect("a refusal is a typed response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let codes = response.headers()[crate::proxy::admitted::REASON_CODES_HEADER]
        .to_str()
        .expect("reason codes")
        .to_owned();
    assert!(codes.contains("egress.opaque_payload"), "{codes}");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "a refused opaque body must not open an upstream connection"
    );
}

/// The opaque path has the same barrier as documents: when the policy
/// tightens after the opaque body was admitted and before it is bound, the
/// old admission does not carry it out.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn policy_change_before_bind_refuses_an_admitted_opaque_body() {
    use crate::proxy::admitted::failpoint::{self, Point};
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config("[context_gateway]\nmode = \"developer\"\n");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let _armed = failpoint::arm(|point| {
        if point == Point::CallerVerified {
            write_gateway_config("[context_gateway]\nmode = \"governed\"\n");
        }
    });
    let result = forward_chat(&upstream, OPAQUE_BODY.to_vec()).await;
    assert_typed_refusal(result, "policy.changed").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "an opaque body admitted under the old policy must not leave under the new one"
    );
}

/// The retry of an opaque body proves its epoch again: a policy change during
/// the backoff stops it after the first attempt.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn policy_change_during_backoff_stops_the_retry_of_an_opaque_body() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config("[context_gateway]\nmode = \"developer\"\n");
    let (upstream, server) = recording_upstream(vec![503, 200], || {
        write_gateway_config("[context_gateway]\nmode = \"governed\"\n");
    })
    .await;
    let result = forward_chat(&upstream, OPAQUE_BODY.to_vec()).await;
    assert_typed_refusal(result, "policy.changed").await;
    assert_eq!(
        server.bodies().await.len(),
        1,
        "the retry must not be dispatched after the policy changed"
    );
}

/// Partial detector coverage through the proxy: content a detector could not
/// fully inspect is withheld from the upstream in governed mode and delivered
/// unchanged in developer mode.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn partially_inspected_content_is_withheld_in_governed_and_delivered_in_developer_mode() {
    let text = "a sentence long enough to exceed the tiny inspection budget of this test";
    let body = serde_json::to_vec(&serde_json::json!({
        "model": "m",
        "messages": [{"role": "user", "content": text}]
    }))
    .unwrap();
    // Two ways a detector does not finish: the byte budget is exhausted, or
    // its time budget is (a zero deadline has always passed).
    let levers = ["max_inspected_bytes = 8", "detector_timeout_ms = 0"];
    let modes = [("governed", false), ("developer", true)];
    for (lever, (mode, delivered)) in levers
        .iter()
        .flat_map(|lever| modes.iter().map(move |mode| (*lever, *mode)))
    {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        write_gateway_config(&format!("[context_gateway]\nmode = \"{mode}\"\n{lever}\n"));
        let (upstream, server) = recording_upstream(vec![200], || {}).await;
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        let identity: TestCompressor = |value, original_size| {
            let out = serde_json::to_vec(&value).unwrap();
            let size = out.len();
            (out, original_size, size)
        };
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            forward_request(
                State(proxy_test_state(&upstream)),
                request,
                &upstream,
                "/v1/chat/completions",
                identity,
                "OpenAI",
                &[],
            ),
        )
        .await
        .expect("bounded forward");
        assert!(
            result.is_ok(),
            "{mode}, {lever}: {:?}",
            result.as_ref().err()
        );
        // Governed mode either withholds the text inside a forwarded request
        // or refuses the request; in neither case does the text arrive.
        let bodies = server.bodies().await;
        let arrived = bodies
            .iter()
            .any(|received| String::from_utf8_lossy(received).contains(text));
        assert_eq!(arrived, delivered, "{mode}, {lever}");
        if delivered {
            assert_eq!(
                bodies,
                vec![body.clone()],
                "{mode}, {lever}: byte-identical"
            );
        }
    }
}

/// A client that goes away mid-stream ends the upstream request: the proxy
/// does not keep reading (or holding) a stream nobody receives.
#[tokio::test]
async fn client_abort_mid_stream_closes_the_upstream_connection() {
    use futures::StreamExt;
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request_body(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {\"n\":1}\n\n",
            )
            .await
            .unwrap();
        // The stream stays open; only the proxy closing it ends this read.
        let mut buffer = [0_u8; 64];
        matches!(
            tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buffer)).await,
            Ok(Ok(0) | Err(_))
        )
    });
    let body = br#"{"model":"m","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let response = forward_chat(&upstream, body.to_vec())
        .await
        .expect("streaming response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut frames = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), frames.next())
        .await
        .expect("the first frame is relayed while the stream is open");
    assert!(first.is_some());
    drop(frames);
    assert!(
        server.await.unwrap(),
        "the upstream connection must close when the client is gone"
    );
}

fn enable_prefix_replay() {
    let mut config = crate::core::config::Config::default();
    config.proxy.proxy_mode = Some("cache".into());
    config.save().expect("cache-mode config");
    assert_eq!(
        crate::core::config::Config::load()
            .proxy
            .resolved_proxy_mode(),
        crate::core::config::ProxyMode::Cache
    );
}

#[derive(Clone, Copy)]
enum ReplayRoute {
    Anthropic,
    OpenAiChat,
}

/// One turn through the real route handler; returns the upstream's body.
async fn turn(route: ReplayRoute, body: &serde_json::Value) -> Vec<u8> {
    let (upstream, server) = recording_upstream(vec![200], || {}).await;
    let (uri, state) = (
        match route {
            ReplayRoute::Anthropic => "/v1/messages",
            ReplayRoute::OpenAiChat => "/v1/chat/completions",
        },
        State(proxy_test_state(&upstream)),
    );
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let handled = async {
        match route {
            ReplayRoute::Anthropic => crate::proxy::anthropic::handler(state, request).await,
            ReplayRoute::OpenAiChat => crate::proxy::openai::handler(state, request).await,
        }
    };
    let response = tokio::time::timeout(Duration::from_secs(30), handled)
        .await
        .expect("bounded route")
        .expect("forwarded");
    assert_eq!(response.status(), StatusCode::OK);
    server.bodies().await.remove(0)
}

fn continued(first: &serde_json::Value) -> serde_json::Value {
    serde_json::json!([
        first,
        {"role": "assistant", "content": "noted"},
        {"role": "user", "content": "continue"}
    ])
}

/// Q11: a later turn of the same conversation with a changed request frame —
/// another model, limit, tool set or stream flag — reaches the upstream with
/// ITS frame. The stored bytes of the earlier turn never override it.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn prefix_replay_never_sends_an_earlier_turns_frame() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    enable_prefix_replay();
    let changes: [(&str, serde_json::Value); 4] = [
        ("model", serde_json::json!({"model": "claude-other"})),
        ("max_tokens", serde_json::json!({"max_tokens": 64})),
        (
            "tools",
            serde_json::json!({"tools": [{"name": "lookup", "input_schema": {"type": "object"}}]}),
        ),
        ("stream", serde_json::json!({"stream": true})),
    ];
    for (changed, overlay) in changes {
        let first = serde_json::json!({
            "role": "user",
            "content": format!("prefix replay frame regression: {changed}")
        });
        let turn_one = serde_json::json!({
            "model": "claude-test", "max_tokens": 16, "messages": [first]
        });
        let _ = turn(ReplayRoute::Anthropic, &turn_one).await;

        let mut turn_two = serde_json::json!({
            "model": "claude-test", "max_tokens": 16, "messages": continued(&first)
        });
        for (key, value) in overlay.as_object().expect("frame change") {
            turn_two[key] = value.clone();
        }
        let received: serde_json::Value =
            serde_json::from_slice(&turn(ReplayRoute::Anthropic, &turn_two).await).unwrap();
        assert_eq!(
            received, turn_two,
            "{changed}: the upstream must receive the current request, not an earlier frame"
        );
    }
}

/// Q11: the store is shared by every route. An OpenAI turn and a later
/// Anthropic turn that begin with the same message are different requests:
/// nothing of the first is replayed into the second.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn prefix_replay_does_not_cross_routes() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    enable_prefix_replay();
    let first = serde_json::json!({
        "role": "user", "content": "prefix replay cross-route regression"
    });
    let _ = turn(
        ReplayRoute::OpenAiChat,
        &serde_json::json!({"model": "gpt-test", "messages": [first]}),
    )
    .await;
    let anthropic = serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "messages": continued(&first)
    });
    let received: serde_json::Value =
        serde_json::from_slice(&turn(ReplayRoute::Anthropic, &anthropic).await).unwrap();
    assert_eq!(received, anthropic);
}

/// Q11 control: with an unchanged frame the replay keeps its benefit — the
/// next turn's prefix is byte-identical to what was forwarded before.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn prefix_replay_keeps_the_prefix_byte_stable_for_an_unchanged_frame() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    enable_prefix_replay();
    let first = serde_json::json!({
        "role": "user", "content": "prefix replay stable-frame control"
    });
    let turn_one = serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "messages": [first]
    });
    let forwarded_one = turn(ReplayRoute::Anthropic, &turn_one).await;
    let messages = continued(&first);
    let turn_two = serde_json::json!({
        "model": "claude-test", "max_tokens": 16, "messages": messages
    });
    let forwarded_two = turn(ReplayRoute::Anthropic, &turn_two).await;
    let delta = &messages.as_array().expect("messages")[1..];
    assert_eq!(
        Some(forwarded_two),
        crate::proxy::prefix_replay::overlay_prefix(&forwarded_one, delta),
        "the second turn is the first turn's forwarded bytes plus the new messages"
    );
}

#[path = "tests_sealed.rs"]
mod sealed;
