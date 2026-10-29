// SPDX-License-Identifier: Apache-2.0
use super::*;
use axum::body::Bytes;
use futures::channel::mpsc;
use std::sync::{Arc, Mutex, atomic::Ordering};

fn capture_task(
    sink: mpsc::UnboundedSender<Message>,
    task: Arc<Mutex<Option<lean_ctx_protocol::TaskEnvelopeV1>>>,
) -> impl Sink<Message, Error = mpsc::SendError> + Send + Unpin {
    sink.with(move |message| {
        *task.lock().unwrap() = crate::core::task_spine::TaskSpine::current();
        futures::future::ready(Ok(message))
    })
}

const COMPLETED: &str = r#"{"type":"response.completed","response":{"model":"gpt-5.4","usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":20},"output_tokens":7}}}"#;

#[tokio::test(flavor = "current_thread")]
async fn required_receipt_failure_never_connects_to_ws_turn_upstream() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("governed config");
    let data = crate::core::data_dir::lean_ctx_data_dir().expect("data directory");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::write(data.join("gateway"), b"blocked test store").expect("store obstruction");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let upstream = format!("http://{}", listener.local_addr().expect("address"));
    let (mut sink, mut messages) = mpsc::unbounded();
    let ledger = Mutex::new(WsLedger::default());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        forward_turn(
            &mut sink,
            &test_state(),
            &upstream,
            "/v1/responses",
            &[],
            serde_json::json!({"model":"m","input":"hello"}),
            &ledger,
        ),
    )
    .await
    .expect("receipt refusal must not wait for upstream");
    assert_eq!(result.termination, TurnTermination::UpstreamRejected);
    let frame: Value = serde_json::from_str(
        &messages
            .next()
            .await
            .expect("error frame")
            .into_text()
            .expect("text"),
    )
    .expect("JSON frame");
    assert_eq!(frame["status"], 503);
    assert_eq!(frame["error"]["type"], "context_gateway");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

fn test_state() -> ProxyState {
    let (_, upstreams) = tokio::sync::watch::channel(Arc::new(crate::core::config::Upstreams {
        anthropic: "http://127.0.0.1:1".to_owned(),
        openai: "http://127.0.0.1:1".to_owned(),
        chatgpt: "http://127.0.0.1:1".to_owned(),
        gemini: "http://127.0.0.1:1".to_owned(),
        providers: Vec::new(),
    }));
    ProxyState {
        client: crate::proxy::upstream_http_client_builder()
            .build()
            .unwrap(),
        port: 0,
        stats: Arc::new(super::super::ProxyStats::default()),
        break_even: Arc::new(super::super::break_even::BreakEvenCalculator::new(1500)),
        introspect: Arc::new(super::super::introspect::IntrospectState::default()),
        ocla_cache: None,
        upstreams,
        chatgpt_cookies: super::super::chatgpt_cookies::shared_chatgpt_cloudflare_cookie_store(),
        mcp_servers: Arc::new(Vec::new()),
    }
}

async fn upstream(
    status: axum::http::StatusCode,
    body: String,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), handle)
}

#[tokio::test]
async fn relay_preserves_fragmented_frames_and_returns_measured_usage() {
    let (mut sink, mut messages) = mpsc::unbounded();
    let chunks: Vec<Result<Bytes, ()>> = vec![
        Ok(Bytes::from_static(
            b"event: response.created\r\ndata: {\"type\":\"response.",
        )),
        Ok(Bytes::from_static(b"created\"}\r\n\r\n")),
        Ok(Bytes::from(format!("data: {COMPLETED}"))),
    ];
    let (termination, usage) =
        stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
    assert_eq!(termination, TurnTermination::CleanEof);
    let usage = usage.unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens
        ),
        (100, 20, 7)
    );
    assert_eq!(
        messages.next().await.unwrap().into_text().unwrap(),
        r#"{"type":"response.created"}"#
    );
    assert_eq!(
        messages.next().await.unwrap().into_text().unwrap(),
        COMPLETED
    );
}

#[tokio::test]
async fn interrupted_stream_keeps_socket_open_and_retains_terminal_usage() {
    let (mut sink, mut messages) = mpsc::unbounded();
    let chunks = vec![Ok(Bytes::from(format!("data: {COMPLETED}\n"))), Err(())];
    let (termination, usage) =
        stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
    assert_eq!(termination, TurnTermination::StreamInterrupted);
    assert_eq!(usage.unwrap().output_tokens, 7);
    assert_eq!(
        messages.next().await.unwrap().into_text().unwrap(),
        COMPLETED
    );
    let frame: Value =
        serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
    assert_eq!(frame["status"], 502);
    assert_eq!(frame["error"]["type"], "upstream_error");
}

#[tokio::test]
async fn disconnect_including_last_unterminated_frame_is_not_clean_eof() {
    for suffix in ["\n", ""] {
        let (mut sink, messages) = mpsc::unbounded::<Message>();
        drop(messages);
        let chunks: Vec<Result<Bytes, ()>> =
            vec![Ok(Bytes::from(format!("data: {COMPLETED}{suffix}")))];
        let (termination, usage) =
            stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
        assert_eq!(termination, TurnTermination::ClientDisconnected);
        assert_eq!(usage.unwrap().output_tokens, 7);
    }
}

#[tokio::test]
async fn interrupted_nonterminal_usage_stays_unbooked() {
    let (mut sink, _messages) = mpsc::unbounded();
    let partial = COMPLETED.replace("response.completed", "response.in_progress");
    let chunks = vec![Ok(Bytes::from(format!("data: {partial}\n"))), Err(())];
    let (termination, usage) =
        stream_sse_to_ws(&mut sink, futures::stream::iter(chunks), None).await;
    assert_eq!(termination, TurnTermination::StreamInterrupted);
    assert!(usage.is_none());
}

#[tokio::test]
async fn invalid_frame_does_not_start_a_lifecycle_or_record_a_request() {
    let state = test_state();
    let (mut sink, mut messages) = mpsc::unbounded();
    let before = crate::core::task_spine::TaskSpine::current().map(|task| task.task_id);
    let flow = run_turn(
        &mut sink,
        &state,
        "http://127.0.0.1:1",
        "/v1/responses",
        &[],
        "not json",
        "invalid-frame-test",
    )
    .await;
    assert!(flow.is_continue());
    assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 0);
    assert_eq!(
        crate::core::task_spine::TaskSpine::current().map(|task| task.task_id),
        before
    );
    let frame: Value =
        serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
    assert_eq!(frame["status"], 400);
}

#[tokio::test]
async fn identical_turns_each_dispatch_and_record_once_without_acceptance() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let (url, server) = upstream(
        axum::http::StatusCode::OK,
        "data: {\"type\":\"response.completed\"}\n".to_owned(),
    )
    .await;
    let (sink, mut messages) = mpsc::unbounded();
    let mut ids = Vec::new();
    let captured = Arc::new(Mutex::new(None));
    let mut sink = capture_task(sink, captured.clone());
    let outer = crate::core::task_spine::TaskSpine::current().map(|task| task.task_id);
    for count in 1..=2 {
        let flow = run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            r#"{"type":"response.create","input":[]}"#,
            "same-session",
        )
        .await;
        assert!(flow.is_continue());
        assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), count);
        assert_eq!(
            crate::core::task_spine::TaskSpine::current().map(|task| task.task_id),
            outer
        );
        let task = captured.lock().unwrap().clone().unwrap();
        let outcome = ExecutionLifecycle::global()
            .outcome_for(task.task_id.as_str())
            .unwrap();
        assert_eq!(
            outcome.accepted_outcome.accepted,
            lean_ctx_protocol::AcceptanceState::Unknown
        );
        assert!(outcome.assessment.is_none());
        ids.push(task.task_id);
        assert_eq!(
            messages.next().await.unwrap().into_text().unwrap(),
            r#"{"type":"response.completed"}"#
        );
    }
    assert_ne!(
        ids[0], ids[1],
        "identical payloads must not reuse a prior turn"
    );
    server.abort();
}

/// Bypass path "WebSocket": a credential in a Responses WebSocket turn is
/// masked before the turn leaves, exactly as on the HTTP rail.
#[tokio::test]
async fn websocket_turn_reaches_the_upstream_without_the_credential() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let received = Arc::new(Mutex::new(String::new()));
    let seen = received.clone();
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move |body: String| {
            *seen.lock().unwrap() = body;
            async { "data: {\"type\":\"response.completed\"}\n" }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let credential = format!("{}{}{}", "AK", "IA", "Q3EGRZ7WSRAILX4KEY");
    let frame = serde_json::json!({
        "type": "response.create",
        "model": "gpt-5.4",
        "input": [{"role": "user", "content": [
            {"type": "input_text", "text": format!("deploy fails with {credential}")}
        ]}],
    })
    .to_string();
    let (mut sink, _messages) = mpsc::unbounded();
    let flow = run_turn(
        &mut sink,
        &state,
        &url,
        "/v1/responses",
        &[],
        &frame,
        "ws-egress",
    )
    .await;
    assert!(flow.is_continue());
    let forwarded = received.lock().unwrap().clone();
    assert!(
        forwarded.contains("deploy fails with"),
        "the turn was forwarded"
    );
    assert!(
        !forwarded.contains(&credential),
        "the credential left on the WebSocket rail"
    );
    server.abort();
}

#[tokio::test]
async fn websocket_upstream_redirect_never_replays_the_admitted_turn() {
    use lean_ctx_protocol::context_gateway::{
        EgressTransportObservationV1, EgressTransportResultV1, EgressTransportV1,
    };
    for status in [307_u16, 308] {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().unwrap();
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .unwrap();
        let secondary = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = format!("http://{}/unapproved", secondary.local_addr().unwrap());
        let location = redirect.clone();
        let received = Arc::new(Mutex::new(Vec::new()));
        let seen = received.clone();
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(move |body: String| {
                seen.lock().unwrap().push(body);
                let location = location.clone();
                async move {
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [(axum::http::header::LOCATION, location)],
                        "",
                    )
                }
            }),
        );
        let primary = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = format!("http://{}", primary.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(primary, app).await.unwrap() });
        let (mut sink, mut messages) = mpsc::unbounded();
        let ledger = Mutex::new(WsLedger::default());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            forward_turn(
                &mut sink,
                &test_state(),
                &upstream,
                "/v1/responses",
                &[],
                serde_json::json!({"model":"m","input":"safe redirect turn"}),
                &ledger,
            ),
        )
        .await;
        server.abort();
        let result = result.expect("bounded redirect response");
        assert_eq!(result.termination, TurnTermination::UpstreamRejected);
        assert_eq!(received.lock().unwrap().len(), 1);
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), messages.next())
            .await
            .expect("bounded redirect error frame")
            .expect("redirect error frame");
        let frame: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        assert_eq!(frame["status"], status);
        assert!(!frame.to_string().contains(&redirect));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), secondary.accept(),)
                .await
                .is_err(),
            "a redirected turn must not connect to a second upstream"
        );
        let (_, receipt) = crate::core::context_admission::receipt_store::latest(
            crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
            1,
        )
        .into_iter()
        .next()
        .expect("turn decision receipt");
        let lifecycle = crate::core::context_admission::egress::load_egress_for_decision(
            &receipt.unwrap().digest,
        )
        .unwrap()
        .expect("turn egress lifecycle");
        assert_eq!(lifecycle.observations.len(), 1);
        let observation: &EgressTransportObservationV1 = &lifecycle.observations[0].observation;
        assert_eq!(observation.http_status, Some(status));
        assert_eq!(observation.transport, EgressTransportV1::Http);
        assert_eq!(
            observation.result,
            EgressTransportResultV1::ResponseObserved
        );
        assert_eq!(observation.attempt, 1);
    }
}

#[tokio::test]
async fn upstream_rejection_preserves_error_and_defers_stats_to_ledger() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let (url, server) = upstream(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#.to_owned(),
    )
    .await;
    let (mut sink, mut messages) = mpsc::unbounded();
    let result = forward_turn(
        &mut sink,
        &state,
        &url,
        "/v1/responses",
        &[],
        json!({"input":[],"stream":true}),
        &Mutex::new(WsLedger::default()),
    )
    .await;
    assert_eq!(result.termination, TurnTermination::UpstreamRejected);
    assert!(result.flow().is_continue());
    assert!(result.usage.is_none());
    assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 0);
    let frame: Value =
        serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
    assert_eq!(frame["status"], 429);
    assert_eq!(frame["error"]["type"], "rate_limit_error");
    let driver = WsTurnDriver {
        socket: &mut sink,
        state: &state,
        upstream: &url,
        path: "/v1/responses",
        headers: &[],
        document: None,
        model: "gpt-5.4".to_owned(),
        ledger: Arc::new(Mutex::new(WsLedger::default())),
    };
    let observation = driver.observe(&Ok(result));
    assert!(!observation.success);
    assert!(
        observation.outcome_signals.is_empty(),
        "{:?}",
        observation.outcome_signals
    );
    assert_eq!(observation.provider, "OpenAI");
    assert_eq!(observation.model, "gpt-5.4");
    server.abort();
}

#[tokio::test]
async fn upstream_rejection_finishes_unknown_without_quality_evidence() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let (url, server) = upstream(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"type":"rate_limit_error"}}"#.to_owned(),
    )
    .await;
    let (sink, _messages) = mpsc::unbounded();
    let captured = Arc::new(Mutex::new(None));
    let mut sink = capture_task(sink, captured.clone());
    let flow = run_turn(
        &mut sink,
        &state,
        &url,
        "/v1/responses",
        &[],
        r#"{"type":"response.create","input":[]}"#,
        "rejected-transport",
    )
    .await;
    assert!(flow.is_continue());
    assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
    let task = captured.lock().unwrap().clone().unwrap();
    let outcome = ExecutionLifecycle::global()
        .outcome_for(task.task_id.as_str())
        .unwrap();
    assert_eq!(
        outcome.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert!(outcome.assessment.is_none());
    server.abort();
}

#[tokio::test]
async fn ws_conversion_and_whitespace_are_not_compression_savings() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let (url, server) = upstream(axum::http::StatusCode::OK, String::new()).await;
    let (mut sink, _messages) = mpsc::unbounded();
    let event =
        r#"{ "type": "response.create", "generate": true, "background": false, "input": [] }"#;
    let baseline = serde_json::to_vec(&build_upstream_body(event).unwrap())
        .unwrap()
        .len() as u64;
    assert_ne!(baseline, event.len() as u64);
    assert!(
        run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            event,
            "wire-conversion"
        )
        .await
        .is_continue()
    );
    assert_eq!(state.stats.bytes_original.load(Ordering::Relaxed), baseline);
    assert_eq!(
        state.stats.bytes_compressed.load(Ordering::Relaxed),
        baseline
    );
    assert_eq!(state.stats.tokens_saved.load(Ordering::Relaxed), 0);
    assert_eq!(state.stats.requests_compressed.load(Ordering::Relaxed), 0);
    server.abort();
}

#[tokio::test]
async fn measured_usage_is_booked_once_even_when_quality_is_unknown() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let model = "ws-phase5-measured-fixture";
    let before = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model)
        .map_or(0, |usage| usage.requests);
    let body = format!("data: {}\n", COMPLETED.replace("gpt-5.4", model));
    let (url, server) = upstream(axum::http::StatusCode::OK, body).await;
    let (sink, _messages) = mpsc::unbounded();
    let captured = Arc::new(Mutex::new(None));
    let mut sink = capture_task(sink, captured.clone());
    let flow = run_turn(
        &mut sink,
        &state,
        &url,
        "/v1/responses",
        &[],
        r#"{"type":"response.create","input":[]}"#,
        "measured-transport",
    )
    .await;
    assert!(flow.is_continue());
    let usage = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model)
        .unwrap();
    assert_eq!(usage.requests, before + 1);
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens
        ),
        (100, 20, 7)
    );
    assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
    let task = captured.lock().unwrap().clone().unwrap();
    let outcome = ExecutionLifecycle::global()
        .outcome_for(task.task_id.as_str())
        .unwrap();
    assert_eq!(
        outcome.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert!(outcome.assessment.is_none());
    server.abort();
}

#[tokio::test]
async fn terminal_usage_reaches_lifecycle_ledger_after_upstream_stream_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let model = "ws-terminal-error-lifecycle";
    let before = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model)
        .map_or(0, |usage| usage.requests);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = format!("data: {}\n", COMPLETED.replace("gpt-5.4", model));
    let (completion_seen, completion_received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut bytes = [0; 1024];
            let count = socket.read(&mut bytes).await.unwrap();
            assert!(count > 0 && request.len() < 8192);
            request.extend_from_slice(&bytes[..count]);
        }
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n",
            body.len(),
            body
        );
        socket.write_all(wire.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
        completion_received.await.unwrap();
        // Missing terminating zero chunk causes a real upstream body error,
        // only after the WS sink has observed the complete usage event.
    });
    let (sink, mut messages) = mpsc::unbounded();
    let captured = Arc::new(Mutex::new(None));
    let sink = capture_task(sink, captured.clone());
    let mut completion_seen = Some(completion_seen);
    let mut sink = sink.with(move |message: Message| {
        if matches!(&message, Message::Text(text) if text.contains("response.completed"))
            && let Some(signal) = completion_seen.take()
        {
            signal.send(()).unwrap();
        }
        futures::future::ready(Ok::<_, mpsc::SendError>(message))
    });
    let flow = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_turn(
            &mut sink,
            &state,
            &url,
            "/v1/responses",
            &[],
            r#"{"type":"response.create","input":[]}"#,
            "terminal-error-lifecycle",
        ),
    )
    .await
    .unwrap();
    assert!(flow.is_continue());
    server.await.unwrap();
    let completed: Value =
        serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
    assert_eq!(completed["type"], "response.completed");
    let error: Value =
        serde_json::from_str(&messages.next().await.unwrap().into_text().unwrap()).unwrap();
    assert_eq!(error["status"], 502);
    let measured = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model)
        .unwrap();
    assert_eq!(measured.requests, before + 1);
    assert_eq!(
        (
            measured.input_tokens,
            measured.cache_read_tokens,
            measured.output_tokens
        ),
        (100, 20, 7)
    );
    assert_eq!(state.stats.requests_total.load(Ordering::Relaxed), 1);
    let task = captured.lock().unwrap().clone().unwrap();
    let outcome = ExecutionLifecycle::global()
        .outcome_for(task.task_id.as_str())
        .unwrap();
    assert_eq!(
        outcome.accepted_outcome.accepted,
        lean_ctx_protocol::AcceptanceState::Unknown
    );
    assert!(outcome.assessment.is_none());
}

async fn cancelled_turn_usage(terminal: bool) {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let state = test_state();
    let model = if terminal {
        "ws-cancel-terminal-evidence"
    } else {
        "ws-cancel-partial-control"
    };
    let before = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model)
        .map_or(0, |usage| usage.requests);
    let event = if terminal {
        COMPLETED.replace("gpt-5.4", model)
    } else {
        json!({"type":"response.created","response":{
            "model":model,"usage":{"input_tokens":120,"output_tokens":0}
        }})
        .to_string()
    };
    let body = Bytes::from(format!("data: {event}\n\n"));
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move || {
            let body = body.clone();
            async move {
                axum::body::Body::from_stream(
                    futures::stream::once(async move { Ok::<_, std::convert::Infallible>(body) })
                        .chain(futures::stream::pending()),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (sink, mut messages) = mpsc::unbounded();
    let captured = Arc::new(Mutex::new(None));
    let sink = capture_task(sink, captured.clone());
    let (seen, received) = tokio::sync::oneshot::channel();
    let mut seen = Some(seen);
    let mut sink = sink.with(move |message: Message| {
        if let Some(signal) = seen.take() {
            signal.send(()).unwrap();
        }
        futures::future::ready(Ok::<_, mpsc::SendError>(message))
    });
    let mut turn = Box::pin(run_turn(
        &mut sink,
        &state,
        &url,
        "/v1/responses",
        &[],
        r#"{"type":"response.create","input":[]}"#,
        model,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            result = &mut turn => panic!("turn completed before cancellation: {result:?}"),
            observed = received => observed.unwrap(),
        }
    })
    .await
    .unwrap();
    // Cancel the actual lifecycle future while upstream remains open, after
    // its scanner and WS sink have observed the event; do not fake an error.
    drop(turn);
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), messages.next())
        .await
        .unwrap()
        .unwrap();
    let relayed: Value = serde_json::from_str(&message.into_text().unwrap()).unwrap();
    assert_eq!(
        relayed["type"],
        if terminal {
            "response.completed"
        } else {
            "response.created"
        }
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    let task = captured.lock().unwrap().clone().unwrap();
    let outcome = ExecutionLifecycle::global()
        .outcome_for(task.task_id.as_str())
        .expect("canonical guard must finalize cancellation");
    let after = super::super::usage_meter::snapshot()
        .into_iter()
        .find(|usage| usage.model == model);
    let measured_tokens = after.as_ref().map(|usage| {
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.output_tokens,
        )
    });
    // Report quality and measurement together: an invalid rejection must
    // not hide the independently missing provider measurement.
    assert_eq!(
        (
            outcome.accepted_outcome.accepted,
            outcome.assessment.is_none(),
            after.map_or(0, |usage| usage.requests) - before,
            measured_tokens,
        ),
        (
            lean_ctx_protocol::AcceptanceState::Unknown,
            true,
            u64::from(terminal),
            terminal.then_some((100, 20, 7)),
        ),
    );
}

#[tokio::test]
async fn cancelled_turn_retains_observed_terminal_usage() {
    cancelled_turn_usage(true).await;
}

#[tokio::test]
async fn cancelled_turn_does_not_book_partial_usage() {
    cancelled_turn_usage(false).await;
}

#[test]
fn build_upstream_body_strips_ws_fields_and_forces_stream() {
    let event = r#"{
            "type": "response.create",
            "model": "gpt-5.5",
            "generate": false,
            "background": true,
            "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]
        }"#;
    let body = build_upstream_body(event).expect("valid response.create");
    let obj = body.as_object().unwrap();
    assert!(!obj.contains_key("type"), "WS event type must be stripped");
    assert!(
        !obj.contains_key("generate"),
        "warmup hint must be stripped"
    );
    assert!(
        !obj.contains_key("background"),
        "background must be stripped"
    );
    assert_eq!(obj.get("stream"), Some(&Value::Bool(true)));
    assert_eq!(obj.get("model").and_then(Value::as_str), Some("gpt-5.5"));
    assert!(obj.contains_key("input"), "input must be preserved");
}

#[test]
fn build_upstream_body_preserves_previous_response_id() {
    let event = r#"{"type":"response.create","previous_response_id":"resp_123","input":[]}"#;
    let body = build_upstream_body(event).unwrap();
    assert_eq!(
        body.get("previous_response_id").and_then(Value::as_str),
        Some("resp_123"),
        "continuation chaining must survive the bridge"
    );
}

#[test]
fn build_upstream_body_rejects_non_create_events() {
    assert!(build_upstream_body(r#"{"type":"response.cancel"}"#).is_none());
    assert!(build_upstream_body("not json").is_none());
    assert!(build_upstream_body("[]").is_none());
    assert!(build_upstream_body(r#"{"input":[]}"#).is_none());
}

#[test]
fn sse_data_payload_extracts_event_json() {
    assert_eq!(
        sse_data_payload(b"data: {\"type\":\"response.created\"}"),
        Some("{\"type\":\"response.created\"}".to_string())
    );
    // No space after the colon is still valid SSE.
    assert_eq!(
        sse_data_payload(b"data:{\"a\":1}"),
        Some("{\"a\":1}".to_string())
    );
}

#[test]
fn sse_data_payload_ignores_metadata_and_done() {
    assert!(sse_data_payload(b"event: response.created").is_none());
    assert!(sse_data_payload(b": keep-alive comment").is_none());
    assert!(sse_data_payload(b"id: 42").is_none());
    assert!(sse_data_payload(b"").is_none());
    assert!(sse_data_payload(b"data: [DONE]").is_none());
}

/// [real registers and store] The observed WebSocket turn. Under protection
/// the turn's passes shorten a tool output only with a recovery copy bound to
/// its lineage, and only if the turn's own `tools` list names the Engine's
/// expand tool: the output then leaves with a stub that names that tool
/// exactly as the host listed it and resolves to the exact original. The
/// proxy declares no tool; every turn's `tools` leave as they came. A turn
/// that lists no usable expand tool, or forbids tool calls, keeps its output
/// whole and stores nothing; so does a turn that lists the tool before its
/// host is provably served by an Engine of this store (a live MCP Engine
/// registered for the proxy's project, loopback listener), and an output of
/// unknown lineage. The turn's persisted receipt notes why.
#[tokio::test]
async fn websocket_turn_shortens_only_with_a_recovery_copy_and_notes_what_stays_whole() {
    use crate::core::archive::delivered;
    use crate::core::archive::presented::{PresentedBinding, record_presentation};

    let isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[context_gateway]\nmode = \"governed\"\n\n[proxy]\nproject_root = '{}'\n",
            root.display()
        ),
    )
    .expect("gateway config");
    let log = (1..=400)
        .map(|i| format!("[info] processed item {i:04} ok"))
        .collect::<Vec<_>>()
        .join("\n");
    let unknown = format!("{log}\n[info] a line the Engine never delivered");
    // The test plays the Engine: `log` is prepared context from a presented
    // source, recorded with its lineage.
    crate::proxy::request_project::in_project(&root, || {
        let binding = PresentedBinding::admitted_now(
            &root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(record_presentation(&binding));
        assert!(delivered::record_lineage(
            &delivered::digest(&log),
            &delivered::Lineage::of_presented(vec![binding])
        ));
    });

    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = received.clone();
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move |body: String| {
            seen.lock().unwrap().push(body);
            async { "data: {\"type\":\"response.completed\"}\n" }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    const PREFIXED: &str = "mcp__lean-ctx__ctx_expand";
    let listing = |expand: Option<&str>| {
        let mut tools = vec![serde_json::json!({"type": "function", "name": "bash"})];
        if let Some(expand) = expand {
            tools.push(serde_json::json!({"type": "function", "name": expand}));
        }
        Value::Array(tools)
    };
    let frame = |output: &str, expand: Option<&str>, forbid: bool| {
        let mut turn = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.4",
            "instructions": "websocket recovery test",
            "tools": listing(expand),
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": output},
            ],
        });
        if forbid {
            turn["tool_choice"] = serde_json::json!("none");
        }
        turn.to_string()
    };
    // What each turn lists, in the order the turns are sent. The first one
    // is sent before the host is bound to this store, the others after.
    let turns = [
        (&log, Some(PREFIXED), false),
        (&log, None, false),
        (&log, Some("mcp__lean ctx__ctx_expand"), false),
        (&log, Some(PREFIXED), true),
        (&log, Some(PREFIXED), false),
        (&log, Some("ctx_expand"), false),
        (&unknown, Some(PREFIXED), false),
    ];
    let copies = || {
        fn count(directory: &std::path::Path) -> usize {
            std::fs::read_dir(directory).map_or(0, |entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            count(&path)
                        } else {
                            usize::from(path.extension().is_some_and(|ext| ext == "txt"))
                        }
                    })
                    .sum()
            })
        }
        count(isolation.path())
    };
    let stored_before = copies();
    let mut stored_after = Vec::new();
    let state = test_state();
    let (mut sink, _messages) = mpsc::unbounded();
    let mut bound = None;
    for (index, (output, expand, forbid)) in turns.into_iter().enumerate() {
        if index == 1 {
            // From here on a bound standard host: a process of this
            // executable is the live MCP Engine of the project, the listener
            // is loopback.
            bound = Some(crate::proxy::store_binding::bound_host(&root));
        }
        let connection = "ws-recovery-one";
        let flow = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_turn(
                &mut sink,
                &state,
                &url,
                "/v1/responses",
                &[],
                &frame(output, expand, forbid),
                connection,
            ),
        )
        .await
        .expect("a bounded turn");
        assert!(flow.is_continue(), "{connection}: the turn was forwarded");
        stored_after.push(copies());
    }
    drop(bound);
    server.abort();

    let forwarded: Vec<Value> = received
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).expect("a JSON turn"))
        .collect();
    assert_eq!(forwarded.len(), 7, "every turn reached the upstream");
    let output_of = |turn: &Value| {
        turn["input"][1]["output"]
            .as_str()
            .expect("text output")
            .to_owned()
    };
    // The proxy declares nothing: every turn leaves with the tools its host
    // listed, byte for byte.
    for (turn, (_, expand, _)) in forwarded.iter().zip(turns) {
        assert_eq!(
            serde_json::to_string(&turn["tools"]).unwrap(),
            serde_json::to_string(&listing(expand)).unwrap()
        );
    }

    // The tool listed by a host not bound to this store; then, bound: no
    // expand tool listed, a name a stub cannot carry, tool calls forbidden.
    // Whole, and nothing is stored.
    // And, interim, the tool listed by the bound host under either name:
    // the Engine behind the listed tool is not correlated with this store
    // per block yet, so no turn leaves with a stub.
    for index in 0..6 {
        assert_eq!(output_of(&forwarded[index]), log, "turn {index}: whole");
        assert_eq!(stored_after[index], stored_before, "turn {index}: no copy");
    }

    assert_eq!(output_of(&forwarded[6]), unknown, "unknown lineage: whole");
    assert_eq!(stored_after[6], stored_before, "turn 6: no copy");

    let noted = |code: &str| {
        crate::core::context_admission::receipt_store::latest(
            crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
            16,
        )
        .into_iter()
        .map(|(_, loaded)| loaded.expect("receipt verifies").receipt)
        .filter(|receipt| {
            receipt.decisions.iter().any(|decision| {
                decision
                    .reason_codes
                    .iter()
                    .any(|reason| reason.as_str() == code)
            })
        })
        .count()
    };
    assert_eq!(
        noted("recovery.handle_not_followable"),
        7,
        "the receipt of every turn: followability is decided first, and no \
         handle is followable while the serving Engine is not correlated"
    );
    assert_eq!(
        noted("recovery.unknown_lineage"),
        0,
        "the lineage is not even looked up for a handle nobody could follow"
    );
}

/// Recovery copies on disk under `directory` (the store's `.txt` entries).
fn stored_copies(directory: &std::path::Path) -> usize {
    std::fs::read_dir(directory).map_or(0, |entries| {
        entries
            .flatten()
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    stored_copies(&path)
                } else {
                    usize::from(path.extension().is_some_and(|ext| ext == "txt"))
                }
            })
            .sum()
    })
}

/// [real register, store and presence registry] The correlated WebSocket
/// turn, under protection, for a bound host with rolling history pruning
/// (every `input` item before the last six is old). The live pass leaves the
/// output of a `ctx_*` Engine tool untouched in every mode (#479), so the
/// pruning of an old output is the one thing that shortens a turn.
///
/// - A turn that carries the producing `function_call` in its OWN `input`,
///   with the `call_id` of the `function_call_output`, whose bytes this
///   store has on record for exactly that id, and whose host lists the
///   expand tool under the producing tool's prefix: the old output is pruned
///   and ends with a stub that names that tool and resolves to the exact
///   original.
/// - The negative twin, the same bytes on record without the id: whole,
///   nothing stored.
/// - A turn WITHOUT the producing `function_call` — the output alone, as a
///   connection sends it once the provider holds the history; this is most
///   WebSocket turns: whole, nothing stored, although the bytes and the id
///   are on record.
///
/// Every turn's `tools` leave as they came, and the receipt of each turn
/// that kept its output whole notes `recovery.handle_not_followable`.
#[tokio::test]
async fn websocket_turn_prunes_an_old_output_only_for_the_call_this_stores_engine_answered() {
    use crate::proxy::store_binding::{bound_host, engine_delivers};

    const EXPAND: &str = "mcp__lean-ctx__ctx_expand";
    const SHELL: &str = "mcp__lean-ctx__ctx_shell";
    const NOT_FOLLOWABLE: &str = "recovery.handle_not_followable";
    let isolation = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[context_gateway]\nmode = \"governed\"\n\n\
             [proxy]\nproject_root = '{}'\nhistory_mode = \"rolling\"\n",
            root.display()
        ),
    )
    .expect("gateway config");
    // A bound standard host: this process is the live MCP Engine of the
    // project, the listener is loopback.
    let _bound = bound_host(&root);
    let log = (1..=400)
        .map(|i| format!("[info] processed item {i:04} ok"))
        .collect::<Vec<_>>()
        .join("\n");
    // What the Engine of this store delivered, and for which call.
    let served = format!("{log}\n[case] served for call X");
    engine_delivers(&root, "served.log", &served, &served, Some("call_X"));
    let no_id = format!("{log}\n[case] delivered without a call id");
    engine_delivers(&root, "no_id.log", &no_id, &no_id, None);

    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = received.clone();
    let app = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(move |body: String| {
            seen.lock().unwrap().push(body);
            async { "data: {\"type\":\"response.completed\"}\n" }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let listing = serde_json::json!([
        {"type": "function", "name": "bash"},
        {"type": "function", "name": EXPAND},
    ]);
    // `history` and six host-authored messages after it: `history` is old.
    let frame =
        |history: &[Value]| {
            let mut input = history.to_vec();
            input.extend((1..=6).map(
                |step| serde_json::json!({"role": "user", "content": format!("go on {step}")}),
            ));
            serde_json::json!({
                "type": "response.create",
                "model": "gpt-5.4",
                "instructions": "websocket correlation test",
                "tools": listing,
                "input": input,
            })
            .to_string()
        };
    let call = serde_json::json!({
        "type": "function_call", "call_id": "call_X", "name": SHELL, "arguments": "{}"
    });
    let output = |text: &str| serde_json::json!({"type": "function_call_output", "call_id": "call_X", "output": text});
    let turns = [
        ("correlated", vec![call.clone(), output(&served)]),
        ("the bytes without the id", vec![call, output(&no_id)]),
        ("no producing call in the turn", vec![output(&served)]),
    ];
    let mut stored = vec![stored_copies(isolation.path())];
    let state = test_state();
    let (mut sink, _messages) = mpsc::unbounded();
    for (why, history) in &turns {
        let flow = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_turn(
                &mut sink,
                &state,
                &url,
                "/v1/responses",
                &[],
                &frame(history),
                "ws-correlation-one",
            ),
        )
        .await
        .expect("a bounded turn");
        assert!(flow.is_continue(), "{why}: the turn was forwarded");
        stored.push(stored_copies(isolation.path()));
    }
    server.abort();

    let forwarded: Vec<Value> = received
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).expect("a JSON turn"))
        .collect();
    assert_eq!(
        forwarded.len(),
        turns.len(),
        "every turn reached the upstream"
    );
    for (turn, (why, _)) in forwarded.iter().zip(&turns) {
        assert_eq!(
            serde_json::to_string(&turn["tools"]).unwrap(),
            serde_json::to_string(&listing).unwrap(),
            "{why}: the tools left as the host listed them"
        );
    }
    let output_at = |turn: &Value, index: usize| {
        turn["input"][index]["output"]
            .as_str()
            .expect("text output")
            .to_owned()
    };

    // Correlated: pruned to the first three lines, one line that counts the
    // rest, the last two lines — and the stub of the producing server.
    let pruned = output_at(&forwarded[0], 1);
    assert!(pruned.len() < served.len(), "correlated: shortened");
    let (_, rest) = pruned
        .rsplit_once("ctx_expand(id=\"")
        .expect("the output ends with a recovery stub");
    let id = rest.split('"').next().expect("an id").to_owned();
    let recovery_id = crate::core::archive::RecoveryId::parse(&id).expect("a recovery id");
    let lines: Vec<&str> = served.lines().collect();
    assert_eq!(
        pruned,
        format!(
            "{}\n[...{} lines pruned by lean-ctx...]\n{}\n{}",
            lines[..3].join("\n"),
            lines.len() - 5,
            lines[lines.len() - 2..].join("\n"),
            crate::proxy::compress::recovery_stub_for(EXPAND, &recovery_id)
        ),
        "the pruned form, and the stub as its last line"
    );
    assert_eq!(
        crate::proxy::compress::resolve_recovery(&id),
        Some(Ok(served.clone())),
        "the id resolves to the exact original"
    );
    assert_eq!(
        forwarded[0]["input"][0], turns[0].1[0],
        "the call is untouched"
    );
    assert!(stored[1] > stored[0], "the recovery copy is stored");

    assert_eq!(output_at(&forwarded[1], 1), no_id, "without the id: whole");
    assert_eq!(stored[2], stored[1], "without the id: nothing stored");
    assert_eq!(
        output_at(&forwarded[2], 0),
        served,
        "no producing call in the turn: whole"
    );
    assert_eq!(stored[3], stored[2], "no producing call: nothing stored");

    let noting = crate::core::context_admission::receipt_store::latest(
        crate::core::context_admission::egress::PROXY_RECEIPT_KEY,
        16,
    )
    .into_iter()
    .map(|(_, loaded)| loaded.expect("receipt verifies").receipt)
    .filter(|receipt| {
        receipt.decisions.iter().any(|decision| {
            decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == NOT_FOLLOWABLE)
        })
    })
    .count();
    assert_eq!(
        noting, 2,
        "the receipts of the two turns that kept their output whole, and \
         not the receipt of the pruned one"
    );
}
