// SPDX-License-Identifier: Apache-2.0
//! Token-plan step 2, locked acceptance (spec/step2-compact-locked).
//!
//! The agent's compaction directive belongs to the app session that armed
//! it. One proxy carries many sessions (`x-leanctx-session-id`); a summary
//! one session armed must never reach another session's request, and a
//! request that names no session gets nothing (fail closed). `ctx_session
//! compact` says "armed" only when a proxy actually carries the calling
//! session; otherwise it says so plainly.
//!
//! Everything here goes through the public surfaces a person uses: the agent
//! arms and restores with the `ctx_session` MCP tool (its app session comes
//! from `LEAN_CTX_SESSION_ID`, as the launching app sets it), requests go
//! through the real Anthropic route to a local upstream that records what
//! actually arrives. Nothing here names the directive store, its file
//! layout, its constants or the stub format; a compaction is recognised by
//! the summary text reaching the upstream and the compacted first turn not
//! reaching it.
//!
//! The TTL rows need a clock and live in `compact_session_ttl_tests.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::ProxyState;

/// The app-session header the launching app sends to the proxy.
pub(super) const SESSION_HEADER: &str = "x-leanctx-session-id";
/// The app-session variable the launching app sets for the agent's MCP server.
const SESSION_ENV: &str = "LEAN_CTX_SESSION_ID";
/// Unique text in the first (compactable) turn of every test conversation.
pub(super) const FIRST_TURN_MARKER: &str = "first-turn-marker-q7c41";
/// A hang guard for one turn through the real route, not a latency bound.
const ROUTE_BOUND: Duration = Duration::from_secs(60);

/// A session id no other test (and no earlier run of this one) uses: the
/// proxy's per-session usage totals are process-wide.
pub(super) fn fresh_session(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("step2-{tag}-{nanos}")
}

/// One request as the upstream received it.
#[derive(Debug, Clone)]
pub(super) struct Arrived {
    /// Header lines, names lowercased.
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: String,
}

impl Arrived {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// A local upstream that answers every connection with an Anthropic message
/// (with measured usage, as the real API does) and records each request.
pub(super) struct Upstream {
    pub(super) url: String,
    arrived: Arc<Mutex<Vec<Arrived>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Upstream {
    /// Every request received so far, in order.
    pub(super) fn arrived(&self) -> Vec<Arrived> {
        self.arrived
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The most recent request.
    pub(super) fn last(&self) -> Arrived {
        self.arrived()
            .last()
            .cloned()
            .expect("a request reached the upstream")
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The assistant reply the upstream sends; `signature` adds a signed
/// thinking block (a provider-sealed item).
fn reply(signature: Option<&str>) -> Vec<u8> {
    let mut content = Vec::new();
    if let Some(signature) = signature {
        content.push(json!({"type": "thinking", "thinking": "plan", "signature": signature}));
    }
    content.push(json!({"type": "text", "text": "ok"}));
    serde_json::to_vec(&json!({
        "id": "msg_step2", "type": "message", "role": "assistant",
        "model": "claude-test", "content": content,
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 120, "output_tokens": 4}
    }))
    .unwrap()
}

pub(super) async fn spawn_upstream() -> Upstream {
    spawn_upstream_signing(None).await
}

/// Like [`spawn_upstream`], every reply carrying a signed thinking block.
pub(super) async fn spawn_upstream_signing(signature: Option<&'static str>) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let arrived = Arc::new(Mutex::new(Vec::new()));
    let sink = arrived.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let request = Box::pin(read_request(&mut socket)).await;
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request);
            let body = reply(signature);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        }
    });
    Upstream {
        url: format!("http://{address}"),
        arrived,
        task,
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Arrived {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 16384];
    let (head_end, length) = loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break (buf.len(), 0);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            break (end, length);
        }
    };
    while buf.len() < head_end + 4 + length {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end.min(buf.len())]).to_string();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let body_start = (head_end + 4).min(buf.len());
    Arrived {
        headers,
        body: String::from_utf8_lossy(&buf[body_start..]).to_string(),
    }
}

fn proxy_state(upstream: &str) -> ProxyState {
    let (_tx, rx) = tokio::sync::watch::channel(Arc::new(lean_ctx::core::config::Upstreams {
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
    }
}

/// One Anthropic turn through the real route. `session` is the app-session
/// header; `credential` the client's `authorization` value (OAuth/subscription
/// clients send a bearer token).
pub(super) async fn turn_with(
    upstream: &Upstream,
    session: Option<&str>,
    credential: &str,
    extra_headers: &[(&str, &str)],
    body: &Value,
) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", credential)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if let Some(session) = session {
        request = request.header(SESSION_HEADER, session);
    }
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let response = tokio::time::timeout(
        ROUTE_BOUND,
        super::anthropic::handler(State(proxy_state(&upstream.url)), request),
    )
    .await
    .expect("the route answers")
    .expect("an answer, not a handler error");
    let status = response.status();
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    status
}

pub(super) async fn turn(upstream: &Upstream, session: Option<&str>, body: &Value) -> StatusCode {
    turn_with(
        upstream,
        session,
        "Bearer sk-ant-api03-step2-test",
        &[],
        body,
    )
    .await
}

/// A conversation with a long compactable first stretch (one plain user turn
/// carrying [`FIRST_TURN_MARKER`] and four tool cycles) followed by four
/// plain user turns, so any `keep_recent_turns` the tool allows still leaves
/// a span to compact. `owner` makes the opening unique: the proxy keys what
/// it already forwarded (prefix replay, determinism guard) by a conversation's
/// opening, and real sessions never share one; a shared opening would let one
/// session's uncompacted forward veto another's compaction.
pub(super) fn conversation(owner: &str) -> Value {
    let mut messages = vec![json!({
        "role": "user",
        "content": format!("{FIRST_TURN_MARKER} ({owner}): investigate the failing deploy in depth")
    })];
    for i in 0..4 {
        messages.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": format!("toolu_{i}"), "name": "ctx_shell",
             "input": {"command": "ls"}}
        ]}));
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("toolu_{i}"),
             "content": format!("listing {i}: {}", "src/main.rs ".repeat(40))}
        ]}));
    }
    for i in 2..=5 {
        messages.push(json!({"role": "user", "content": format!("turn {i}")}));
    }
    json!({"model": "claude-test", "max_tokens": 64, "messages": messages})
}

/// The request reached the upstream compacted with `summary`.
pub(super) fn carries(arrived: &Arrived, summary: &str) -> bool {
    arrived.body.contains(summary)
}

/// The request reached the upstream with its full original history.
pub(super) fn untouched(arrived: &Arrived) -> bool {
    arrived.body.contains(FIRST_TURN_MARKER)
}

/// `LEAN_CTX_SESSION_ID` set (or removed) until dropped. The caller holds
/// the test environment lock (through `isolated_data_dir()`).
struct SessionEnv {
    previous: Option<std::ffi::OsString>,
}

impl SessionEnv {
    fn enter(session: Option<&str>) -> Self {
        let previous = std::env::var_os(SESSION_ENV);
        match session {
            Some(session) => lean_ctx::test_env::set_var(SESSION_ENV, session),
            None => lean_ctx::test_env::remove_var(SESSION_ENV),
        }
        Self { previous }
    }
}

impl Drop for SessionEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => lean_ctx::test_env::set_var(SESSION_ENV, previous),
            None => lean_ctx::test_env::remove_var(SESSION_ENV),
        }
    }
}

/// `ctx_session` called by the agent of app session `session` (its MCP
/// server started with `LEAN_CTX_SESSION_ID`), returning the reply text.
pub(super) async fn ctx_session_as(session: Option<&str>, arguments: Value) -> String {
    let _env = SessionEnv::enter(session);
    let project = tempfile::tempdir().expect("project dir");
    let engine = lean_ctx_server::engine::ContextEngine::with_project_root(project.path());
    engine
        .call_tool_text("ctx_session", Some(arguments))
        .await
        .expect("ctx_session answers")
}

/// The agent of `session` arms a compaction with `summary`.
pub(super) async fn arm(session: Option<&str>, summary: &str) -> String {
    ctx_session_as(
        session,
        json!({"action": "compact", "value": summary, "keep_recent_turns": 4}),
    )
    .await
}

/// The agent of `session` restores (undoes its compaction).
pub(super) async fn restore(session: Option<&str>) -> String {
    ctx_session_as(session, json!({"action": "restore"})).await
}

/// The reply tells the agent plainly that no proxy carries its session.
fn says_not_carried(reply: &str) -> bool {
    let lower = reply.to_ascii_lowercase();
    [
        "no proxy",
        "no lean-ctx proxy",
        "not proxied",
        "not carried",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// The reply claims the compaction is armed.
fn claims_armed(reply: &str) -> bool {
    let lower = reply.to_ascii_lowercase();
    lower.contains("compaction armed")
        || (lower.contains("armed") && !lower.contains("not armed") && !says_not_carried(reply))
}

/// The proxy has carried `session` once (one measured turn).
pub(super) async fn carried(upstream: &Upstream, session: &str) {
    let opening = json!({"model": "claude-test", "max_tokens": 16,
        "messages": [{"role": "user", "content": "hello"}]});
    assert_eq!(
        turn(upstream, Some(session), &opening).await,
        StatusCode::OK
    );
}

/// A private data/config dir and the community policy, held for the test.
pub(super) fn isolated_env() -> (
    lean_ctx::core::data_dir::IsolatedDataDir,
    lean_ctx::core::policy::runtime::TestPolicyOverride,
) {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    let policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    (data, policy)
}

// ---------------------------------------------------------------------------
// B1: one directive per session, keyed by the session header, fail closed.
// ---------------------------------------------------------------------------

/// B1-1 [fails on 736c264170: the directive is global, B receives A's
/// summary]. A arms; B's request behind the same proxy arrives exactly as B
/// sent it while A's own request arrives compacted with A's summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn b1_summary_armed_by_one_session_never_reaches_another() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let (a, b) = (fresh_session("b1a"), fresh_session("b1b"));
    carried(&upstream, &a).await;
    carried(&upstream, &b).await;
    let summary = "SUMMARY-B1-ONLY-FOR-SESSION-A";
    arm(Some(&a), summary).await;

    let (of_a, of_b) = (conversation(&a), conversation(&b));
    assert_eq!(turn(&upstream, Some(&b), &of_b).await, StatusCode::OK);
    let at_b = upstream.last();
    assert!(
        !carries(&at_b, summary),
        "B received A's summary: {}",
        at_b.body
    );
    assert!(
        untouched(&at_b),
        "B's history must arrive in full: {}",
        at_b.body
    );

    assert_eq!(turn(&upstream, Some(&a), &of_a).await, StatusCode::OK);
    let at_a = upstream.last();
    assert!(
        carries(&at_a, summary),
        "A's own request carries its summary"
    );
    assert!(!untouched(&at_a), "A's compacted span is replaced");

    // After A's compaction is in use (boundary pinned), B is still untouched.
    assert_eq!(turn(&upstream, Some(&b), &of_b).await, StatusCode::OK);
    let at_b = upstream.last();
    assert!(
        !carries(&at_b, summary) && untouched(&at_b),
        "{}",
        at_b.body
    );
}

/// B1-2 [fails on 736c264170: the global directive applies to any request].
/// A request without the session header is nobody's: an armed directive is
/// never applied to it — before and after its owner used it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn b1_request_without_session_header_is_never_compacted() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let a = fresh_session("b1anon");
    carried(&upstream, &a).await;
    let summary = "SUMMARY-B1-ANON-MUST-NOT-SEE";
    arm(Some(&a), summary).await;

    let anonymous = conversation(&fresh_session("b1nobody"));
    assert_eq!(turn(&upstream, None, &anonymous).await, StatusCode::OK);
    let anon = upstream.last();
    assert!(
        !carries(&anon, summary) && untouched(&anon),
        "{}",
        anon.body
    );

    assert_eq!(
        turn(&upstream, Some(&a), &conversation(&a)).await,
        StatusCode::OK
    );
    assert!(
        carries(&upstream.last(), summary),
        "control: the owner gets it"
    );

    // An empty or malformed header names no session either.
    for header in ["", "   ", "bad id with spaces"] {
        let body = conversation(&fresh_session("b1malformed"));
        assert_eq!(turn(&upstream, Some(header), &body).await, StatusCode::OK);
        let arrived = upstream.last();
        assert!(
            !carries(&arrived, summary) && untouched(&arrived),
            "header {header:?}: {}",
            arrived.body
        );
    }
}

/// B1-3 [fails on 736c264170: restore clears the one global directive].
/// Another session's restore has nothing to undo and leaves A's compaction
/// in place; A's own restore undoes it losslessly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn b1_restore_by_another_session_leaves_the_owners_compaction() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let (a, b) = (fresh_session("b1ra"), fresh_session("b1rb"));
    carried(&upstream, &a).await;
    carried(&upstream, &b).await;
    let summary = "SUMMARY-B1-RESTORE-OWNER-A";
    arm(Some(&a), summary).await;
    let body = conversation(&a);

    restore(Some(&b)).await;
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    assert!(
        carries(&upstream.last(), summary),
        "B's restore must not undo A's compaction"
    );

    restore(Some(&a)).await;
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    let after = upstream.last();
    assert!(
        !carries(&after, summary) && untouched(&after),
        "{}",
        after.body
    );
}

/// MIN-INTERVAL-1 [fails on 736c264170: the anti-obsession guard is global,
/// so B's compact is refused because A armed one]. Re-arming within the
/// minimum interval never stacks summaries for A, and is a per-session
/// guard: it never stops B from arming its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn min_interval_holds_per_session_and_never_blocks_another_session() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let (a, b) = (fresh_session("mia"), fresh_session("mib"));
    carried(&upstream, &a).await;
    carried(&upstream, &b).await;
    let (first, second, of_b) = (
        "SUMMARY-MI-A-FIRST",
        "SUMMARY-MI-A-SECOND",
        "SUMMARY-MI-B-OWN",
    );
    arm(Some(&a), first).await;
    arm(Some(&a), second).await;
    arm(Some(&b), of_b).await;

    assert_eq!(
        turn(&upstream, Some(&a), &conversation(&a)).await,
        StatusCode::OK
    );
    let at_a = upstream.last();
    assert!(
        carries(&at_a, first) || carries(&at_a, second),
        "A stays compacted: {}",
        at_a.body
    );
    assert!(
        at_a.body.matches(first).count() <= 1 && at_a.body.matches(second).count() <= 1,
        "one compaction per session, never stacked: {}",
        at_a.body
    );
    assert!(!carries(&at_a, of_b), "A never carries B's summary");

    assert_eq!(
        turn(&upstream, Some(&b), &conversation(&b)).await,
        StatusCode::OK
    );
    let at_b = upstream.last();
    assert!(
        carries(&at_b, of_b),
        "B's own compaction is armed: {}",
        at_b.body
    );
    assert!(
        !carries(&at_b, first) && !carries(&at_b, second),
        "{}",
        at_b.body
    );
}

// ---------------------------------------------------------------------------
// B3: an honest compact reply.
// ---------------------------------------------------------------------------

/// B3-1 [fails on 736c264170: it answers "Compaction armed" for a session no
/// proxy has carried]. Asked by a session the proxy never carried — while
/// the proxy does carry another — the reply says plainly that no proxy
/// carries it and does not claim "armed"; asked by a carried session it
/// says armed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn b3_compact_says_armed_only_for_a_session_a_proxy_carries() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let (seen, never) = (fresh_session("b3seen"), fresh_session("b3never"));
    carried(&upstream, &seen).await;

    let reply = arm(Some(&never), "SUMMARY-B3-NEVER-PROXIED").await;
    assert!(
        !reply.to_ascii_lowercase().contains("compaction armed") && !claims_armed(&reply),
        "claims armed for an uncarried session: {reply}"
    );
    assert!(
        says_not_carried(&reply),
        "must say no proxy carries it: {reply}"
    );

    let reply = arm(Some(&seen), "SUMMARY-B3-CARRIED").await;
    assert!(claims_armed(&reply), "a carried session is armed: {reply}");
    assert!(!says_not_carried(&reply), "{reply}");
}

/// B3-2 [fails on 736c264170: armed without any session, then applied to
/// every request]. Without an app session id there is nothing a proxy could
/// key the directive by: the reply does not claim armed, and no request
/// (with or without a session header) receives the summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn b3_compact_without_a_session_id_is_never_armed_nor_applied() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let other = fresh_session("b3other");
    carried(&upstream, &other).await;
    let summary = "SUMMARY-B3-NO-SESSION";

    let reply = arm(None, summary).await;
    assert!(
        !reply.to_ascii_lowercase().contains("compaction armed") && !claims_armed(&reply),
        "claims armed without a session: {reply}"
    );

    for session in [None, Some(other.as_str())] {
        let body = conversation(session.unwrap_or("b3-anonymous"));
        assert_eq!(turn(&upstream, session, &body).await, StatusCode::OK);
        let arrived = upstream.last();
        assert!(
            !carries(&arrived, summary) && untouched(&arrived),
            "{session:?}: {}",
            arrived.body
        );
    }
}

// ---------------------------------------------------------------------------
// Regression guards (pass on 736c264170 and must keep passing).
// ---------------------------------------------------------------------------

const SEALED: &str = "c2VhbGVkLXN0ZXAyLW9hdXRoLXJvdGF0aW9u";
const OAUTH_ONE: &str = "Bearer sk-ant-oat01-step2-first-access-token";
const OAUTH_TWO: &str = "Bearer sk-ant-oat01-step2-refreshed-access-token";
const OAUTH_BETA: &str = "oauth-2025-04-20,claude-code-20250219";

/// GUARD-OAUTH-GOVERNED [passes on 736c264170]. A subscription (OAuth)
/// client of a governed proxy: its bearer token and beta flags reach the
/// upstream byte for byte, also after the token is refreshed
/// mid-conversation, and the signed thinking block returned after the
/// rotation is admitted and forwarded (#2177: a rotated credential is not a
/// new caller). An armed compaction changes none of that, and governed
/// protection keeps the conversation whole: a summary has no recovery copy,
/// so under protection it never replaces the span.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn guard_oauth_governed_headers_and_sealed_rotation_pass_unchanged() {
    let _iso = isolated_env();
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("governed config");
    let upstream = spawn_upstream_signing(Some(SEALED)).await;
    let a = fresh_session("oauth");
    let beta = [("anthropic-beta", OAUTH_BETA)];

    let opening = json!({"model": "claude-test", "max_tokens": 16,
        "messages": [{"role": "user", "content": "hello"}]});
    assert_eq!(
        turn_with(&upstream, Some(&a), OAUTH_ONE, &beta, &opening).await,
        StatusCode::OK
    );
    let first = upstream.last();
    assert_eq!(first.header("authorization"), Some(OAUTH_ONE));
    assert_eq!(first.header("anthropic-beta"), Some(OAUTH_BETA));
    assert!(
        first.header("x-api-key").is_none(),
        "no credential invented"
    );

    let summary = "SUMMARY-OAUTH-GOVERNED";
    arm(Some(&a), summary).await;

    // The client refreshed its token; the assistant's signed thinking block
    // from the opening turn comes back in the recent turns.
    let mut body = conversation(&a);
    let messages = body["messages"].as_array_mut().unwrap();
    let at = messages.len() - 1;
    messages.insert(
        at,
        json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "plan", "signature": SEALED},
            {"type": "text", "text": "ok"}
        ]}),
    );
    assert_eq!(
        turn_with(&upstream, Some(&a), OAUTH_TWO, &beta, &body).await,
        StatusCode::OK,
        "a refreshed OAuth token is admitted"
    );
    let rotated = upstream.last();
    assert_eq!(rotated.header("authorization"), Some(OAUTH_TWO));
    assert_eq!(rotated.header("anthropic-beta"), Some(OAUTH_BETA));
    assert!(
        rotated.header("x-api-key").is_none(),
        "no credential invented"
    );
    assert!(rotated.body.contains(SEALED), "the sealed item is returned");
    assert!(
        !carries(&rotated, summary) && untouched(&rotated),
        "governed protection keeps the conversation whole: {}",
        rotated.body
    );
}

/// GUARD-OAUTH-ROTATION [passes on 736c264170]. Outside protection, the
/// session's compaction keeps applying when its OAuth token is refreshed:
/// it belongs to the session, never to the credential. Headers pass
/// unchanged on both sides of the rotation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn guard_oauth_rotation_keeps_the_sessions_compaction() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let a = fresh_session("oauthrot");
    let beta = [("anthropic-beta", OAUTH_BETA)];
    let opening = json!({"model": "claude-test", "max_tokens": 16,
        "messages": [{"role": "user", "content": "hello"}]});
    assert_eq!(
        turn_with(&upstream, Some(&a), OAUTH_ONE, &beta, &opening).await,
        StatusCode::OK
    );
    let summary = "SUMMARY-OAUTH-ROTATION";
    arm(Some(&a), summary).await;

    let body = conversation(&a);
    for credential in [OAUTH_ONE, OAUTH_TWO] {
        assert_eq!(
            turn_with(&upstream, Some(&a), credential, &beta, &body).await,
            StatusCode::OK
        );
        let arrived = upstream.last();
        assert_eq!(arrived.header("authorization"), Some(credential));
        assert_eq!(arrived.header("anthropic-beta"), Some(OAUTH_BETA));
        assert!(
            arrived.header("x-api-key").is_none(),
            "no credential invented"
        );
        assert!(
            carries(&arrived, summary),
            "{credential}: the session's compaction applies: {}",
            arrived.body
        );
    }
}

/// GUARD-CACHE-CONTROL [passes on 736c264170] (#448). Compaction never
/// touches the client's cached prefix: the system prompt and the cached
/// first message arrive byte-identical with their `cache_control` markers,
/// and the compaction lands after them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn guard_cache_control_markers_survive_compaction() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let a = fresh_session("cc");
    carried(&upstream, &a).await;
    let summary = "SUMMARY-CACHE-CONTROL";
    arm(Some(&a), summary).await;

    let mut body = conversation(&a);
    let system = json!([{"type": "text", "text": "You are a coding agent. Stable system prompt.",
        "cache_control": {"type": "ephemeral"}}]);
    let cached_first = json!({"role": "user", "content": [
        {"type": "text", "text": "Project rules: keep the diff small.",
         "cache_control": {"type": "ephemeral"}}
    ]});
    body["system"] = system.clone();
    body["messages"]
        .as_array_mut()
        .unwrap()
        .insert(0, cached_first.clone());

    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    let arrived = upstream.last().json();
    assert_eq!(arrived["system"], system, "cached system prompt untouched");
    assert_eq!(
        arrived["messages"][0], cached_first,
        "cached first message untouched, marker included"
    );
    let text = arrived.to_string();
    assert!(
        text.contains(summary),
        "compaction applied after the prefix"
    );
    assert!(
        !text.contains(FIRST_TURN_MARKER),
        "the live span was compacted"
    );
}
