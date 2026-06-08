//! Integration tests against a real (in-process) HTTP server bound to an
//! ephemeral localhost port. No mocking: the client speaks genuine HTTP over a
//! TCP socket and we assert both the parsed responses and the bytes the server
//! actually received (auth + workspace header forwarding).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use lean_ctx_client::{CallContext, EventQuery, LeanCtxClient, LeanCtxError};
use serde_json::json;

/// One observed request (for server-side assertions).
struct ReqLog {
    method: String,
    path: String,
    authorization: Option<String>,
    workspace: Option<String>,
    body: String,
}

/// Serve exactly `count` connections (one request each), then return the log.
fn start_server(count: usize) -> (String, thread::JoinHandle<Vec<ReqLog>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let base = format!("http://{addr}");
    let handle = thread::spawn(move || {
        let mut log = Vec::new();
        for _ in 0..count {
            let (stream, _) = listener.accept().expect("accept");
            log.push(handle_conn(stream));
        }
        log
    });
    (base, handle)
}

fn handle_conn(mut stream: TcpStream) -> ReqLog {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    let mut request_line = String::new();
    reader.read_line(&mut request_line).expect("request line");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    let mut authorization = None;
    let mut workspace = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("header");
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let (key, value) = trimmed.split_once(':').unwrap_or((trimmed, ""));
        let value = value.trim().to_string();
        match key.to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.parse().unwrap_or(0),
            "authorization" => authorization = Some(value),
            "x-leanctx-workspace" => workspace = Some(value),
            _ => {}
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).expect("body");
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let response = route(&method, &path, &authorization, &body);
    stream.write_all(response.as_bytes()).expect("write");
    stream.flush().ok();

    ReqLog {
        method,
        path,
        authorization,
        workspace,
        body,
    }
}

fn http(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn route(method: &str, path: &str, authorization: &Option<String>, _body: &str) -> String {
    let route_path = path.split('?').next().unwrap_or(path);
    match (method, route_path) {
        ("GET", "/health") => http("200 OK", "text/plain", "ok"),
        ("GET", "/v1/capabilities") => http(
            "200 OK",
            "application/json",
            &json!({ "contract_version": 1, "plane": "personal" }).to_string(),
        ),
        ("GET", "/v1/tools") => http(
            "200 OK",
            "application/json",
            &json!({ "tools": [{ "name": "ctx_search" }], "total": 1, "offset": 0, "limit": 2 })
                .to_string(),
        ),
        ("POST", "/v1/tools/call") => {
            let auth = authorization.clone().unwrap_or_default();
            http(
                "200 OK",
                "application/json",
                &json!({ "result": { "content": [{ "type": "text", "text": format!("pong:{auth}") }] } })
                    .to_string(),
            )
        }
        ("GET", "/v1/events") => {
            let frames = "data: {\"id\":1,\"workspaceId\":\"w\",\"channelId\":\"c\",\"kind\":\"tool_call\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"consistencyLevel\":\"local\",\"payload\":{}}\n\n: ping\n\ndata: {\"id\":2,\"workspaceId\":\"w\",\"channelId\":\"c\",\"kind\":\"session_update\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"consistencyLevel\":\"eventual\",\"payload\":{}}\n\n";
            http("200 OK", "text/event-stream", frames)
        }
        _ => http(
            "401 Unauthorized",
            "application/json",
            &json!({ "error": "invalid bearer token", "error_code": "unauthorized" }).to_string(),
        ),
    }
}

#[test]
fn health_capabilities_and_tools() {
    let (base, server) = start_server(3);
    let client = LeanCtxClient::new(&base).unwrap();

    assert_eq!(client.health().unwrap(), "ok");

    let caps = client.capabilities().unwrap();
    assert_eq!(caps["contract_version"], 1);
    assert_eq!(caps["plane"], "personal");

    let tools = client.list_tools(Some(0), Some(2)).unwrap();
    assert_eq!(tools.total, 1);
    assert_eq!(tools.limit, 2);
    assert_eq!(tools.tools.len(), 1);

    let log = server.join().unwrap();
    assert_eq!(log[2].method, "GET");
    assert!(log[2].path.contains("offset=0"));
    assert!(log[2].path.contains("limit=2"));
}

#[test]
fn call_tool_forwards_auth_and_workspace() {
    let (base, server) = start_server(1);
    let client = LeanCtxClient::builder(&base)
        .bearer_token("secret-token")
        .workspace_id("acme")
        .build()
        .unwrap();

    let text = client
        .call_tool_text(
            "ctx_search",
            Some(json!({ "pattern": "x" })),
            None::<&CallContext>,
        )
        .unwrap();
    assert_eq!(text, "pong:Bearer secret-token");

    let log = server.join().unwrap();
    assert_eq!(log[0].method, "POST");
    assert_eq!(log[0].authorization.as_deref(), Some("Bearer secret-token"));
    assert_eq!(log[0].workspace.as_deref(), Some("acme"));
    assert!(log[0].body.contains("\"workspaceId\":\"acme\""));
    assert!(log[0].body.contains("\"name\":\"ctx_search\""));
}

#[test]
fn non_object_arguments_are_rejected_locally() {
    let client = LeanCtxClient::new("http://127.0.0.1:9").unwrap();
    let err = client
        .call_tool("t", Some(json!([1, 2, 3])), None::<&CallContext>)
        .unwrap_err();
    assert!(matches!(err, LeanCtxError::Config(_)));
}

#[test]
fn http_error_envelope_is_parsed() {
    let (base, server) = start_server(1);
    let client = LeanCtxClient::new(&base).unwrap();

    let err = client.manifest().unwrap_err();
    match err {
        LeanCtxError::Http(e) => {
            assert_eq!(e.status, 401);
            assert_eq!(e.error_code.as_deref(), Some("unauthorized"));
            assert_eq!(e.message, "invalid bearer token");
        }
        other => panic!("expected Http error, got {other:?}"),
    }

    server.join().unwrap();
}

#[test]
fn subscribe_events_streams_and_skips_heartbeats() {
    let (base, server) = start_server(1);
    let client = LeanCtxClient::new(&base).unwrap();

    let events: Vec<_> = client
        .subscribe_events(&EventQuery {
            workspace_id: Some("w".into()),
            channel_id: Some("c".into()),
            ..Default::default()
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, 1);
    assert_eq!(events[1].kind, "session_update");

    server.join().unwrap();
}
