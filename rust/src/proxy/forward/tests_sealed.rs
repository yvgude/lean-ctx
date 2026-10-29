// SPDX-License-Identifier: Apache-2.0
//! Provider-sealed items through the real Anthropic route against a loopback
//! stand-in: bound to the destination that produced them, to the context they
//! were produced for, to the authority that admitted the producing request
//! and to the sources that request was derived from.

#[allow(clippy::wildcard_imports)]
use super::*;

const GOVERNED: &str = "[context_gateway]\nmode = \"governed\"\n";
const SEALED_A: &str = "c2VhbGVkLWJ5LXRoZS1wcm92aWRlci1B";
const SEALED_B: &str = "c2VhbGVkLWJ5LXRoZS1wcm92aWRlci1C";

/// A hang guard for one turn through a real route, not a latency bound:
/// under a full parallel suite on a loaded machine a turn (admission, receipt
/// with sidecar and fsynced audit anchor, loopback exchange) took longer than
/// the 5 s it used to be. What the tests prove does not depend on it.
const ROUTE_BOUND: Duration = Duration::from_secs(60);

/// A loopback upstream that answers one connection per entry with that
/// content type and body, and returns every request body it received.
async fn responding_upstream(
    responses: Vec<(&'static str, Vec<u8>)>,
) -> (String, JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback upstream");
    let address = listener.local_addr().expect("loopback address");
    let server = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for (content_type, body) in responses {
            // Not a latency bound either: between two connections a test
            // runs refused turns, each sealing a receipt (sidecar, fsynced
            // anchor); under a full parallel suite that took longer than 2 s, and
            // 15 s ran out on the loaded gate host (25.10.: 502 in two tests).
            let accepted = tokio::time::timeout(Duration::from_secs(30), listener.accept()).await;
            let Ok(Ok((mut stream, _))) = accepted else {
                break;
            };
            bodies.push(read_request_body(&mut stream).await);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        }
        bodies
    });
    (format!("http://{address}"), server)
}

/// An Anthropic message whose content carries one signed thinking block per
/// signature.
fn thinking_message(signatures: &[&str]) -> (&'static str, Vec<u8>) {
    let blocks: Vec<serde_json::Value> = signatures
        .iter()
        .map(|signature| {
            serde_json::json!({"type": "thinking", "thinking": "plan", "signature": signature})
        })
        .collect();
    let message = serde_json::json!({
        "id": "msg_1", "type": "message", "role": "assistant", "content": blocks
    });
    ("application/json", serde_json::to_vec(&message).unwrap())
}

fn empty_reply() -> (&'static str, Vec<u8>) {
    ("application/json", b"{}".to_vec())
}

fn opening_turn() -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16,
        "messages": [{"role": "user", "content": "hello"}]
    })
}

/// The next turn: the assistant's signed thinking block is sent back.
fn returning_turn(signature: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test", "max_tokens": 16,
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan", "signature": signature},
                {"type": "text", "text": "hi"}
            ]},
            {"role": "user", "content": "continue"}
        ]
    })
}

/// One Anthropic turn through the real handler; the response body is read
/// to its end, as a client does, so a streamed response is fully relayed.
async fn anthropic_turn(
    upstream: &str,
    body: &serde_json::Value,
    authorization: Option<&str>,
) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let request = request
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let response = tokio::time::timeout(
        ROUTE_BOUND,
        crate::proxy::anthropic::handler(State(proxy_test_state(upstream)), request),
    )
    .await
    .expect("bounded route")
    .expect("an answer or a typed refusal");
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, usize::MAX).await.expect("response body");
    Response::from_parts(parts, Body::from(body))
}

fn assert_sealed_refusal(response: &Response, code: &str, case: &str) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{case}");
    let codes = response
        .headers()
        .get(crate::proxy::admitted::REASON_CODES_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(codes.contains(code), "{case}: {codes}");
}

/// Under governed mode a sealed item leaves only as what the same
/// destination produced: returned unchanged it is forwarded byte for byte
/// and its one receipt states it, with its digest, as provider material
/// returned to its origin and not inspected. A changed byte, an item no
/// response carried, and the same item sent to another upstream are refused.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_returns_only_unchanged_and_only_to_the_destination_that_produced_it() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    crate::proxy::sealed_origin::forget_sealed_items_for_test();
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    assert_eq!(
        anthropic_turn(&upstream, &opening_turn(), None)
            .await
            .status(),
        StatusCode::OK
    );

    let mut changed = SEALED_A.to_owned();
    changed.replace_range(..1, "d");
    for (case, signature) in [
        ("one byte changed", changed.as_str()),
        ("never seen", SEALED_B),
    ] {
        let refused = anthropic_turn(&upstream, &returning_turn(signature), None).await;
        assert_sealed_refusal(&refused, "egress.sealed_origin_unknown", case);
    }

    let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other = format!("http://{}", elsewhere.local_addr().unwrap());
    let refused = anthropic_turn(&other, &returning_turn(SEALED_A), None).await;
    assert_sealed_refusal(&refused, "egress.sealed_origin_unknown", "another upstream");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), elsewhere.accept())
            .await
            .is_err(),
        "an item of another destination must not be sent there"
    );

    let returned = returning_turn(SEALED_A);
    let forwarded = anthropic_turn(&upstream, &returned, None).await;
    assert_eq!(forwarded.status(), StatusCode::OK);
    let bodies = server.await.expect("upstream");
    assert_eq!(
        bodies.len(),
        2,
        "only the opening and the valid return arrived"
    );
    assert_eq!(bodies[1], serde_json::to_vec(&returned).unwrap());
    let stated: Vec<_> = proxy_receipts()
        .into_iter()
        .filter(|receipt| has_reason(receipt, "egress.sealed_returned_to_origin"))
        .collect();
    assert_eq!(stated.len(), 1, "one receipt states the returned item");
    assert!(has_reason(&stated[0], "coverage.uninspected"));
    assert_eq!(stated[0].final_context, Some(sha256(&bodies[1])));
    assert!(
        stated[0].decisions.iter().any(|decision| decision.object
            == crate::core::context_admission::receipt_store::keyed::object_key()
                .expect("receipt key")
                .object(&sha256(SEALED_A.as_bytes()))),
        "the receipt names the sealed item by its digest"
    );
}

/// The same holds when the destination streams: the signature arrives in
/// deltas and is known once its block has stopped.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_of_a_streamed_response_can_be_returned() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    let (head, tail) = SEALED_A.split_at(7);
    let stream = format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\
         \"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}}}\n\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\
         \"delta\":{{\"type\":\"signature_delta\",\"signature\":\"{head}\"}}}}\n\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\
         \"delta\":{{\"type\":\"signature_delta\",\"signature\":\"{tail}\"}}}}\n\n\
         data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         data: {{\"type\":\"message_stop\"}}\n\n"
    );
    let (upstream, server) = responding_upstream(vec![
        ("text/event-stream", stream.into_bytes()),
        empty_reply(),
    ])
    .await;
    let mut opening = opening_turn();
    opening["stream"] = serde_json::Value::Bool(true);
    assert_eq!(
        anthropic_turn(&upstream, &opening, None).await.status(),
        StatusCode::OK
    );
    let returned = returning_turn(SEALED_A);
    assert_eq!(
        anthropic_turn(&upstream, &returned, None).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        server.await.expect("upstream")[1],
        serde_json::to_vec(&returned).unwrap()
    );
}

/// The register is bounded in count and age. What it no longer holds is of
/// unknown origin again.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn evicted_or_expired_sealed_item_is_refused() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    {
        let _bounds = crate::proxy::sealed_origin::bound_for_test(1, Duration::from_mins(1));
        let (upstream, server) =
            responding_upstream(vec![thinking_message(&[SEALED_A, SEALED_B]), empty_reply()]).await;
        let _ = anthropic_turn(&upstream, &opening_turn(), None).await;
        let evicted = anthropic_turn(&upstream, &returning_turn(SEALED_A), None).await;
        assert_sealed_refusal(&evicted, "egress.sealed_origin_unknown", "evicted");
        let kept = anthropic_turn(&upstream, &returning_turn(SEALED_B), None).await;
        assert_eq!(kept.status(), StatusCode::OK, "the newest entry is kept");
        assert_eq!(server.await.expect("upstream").len(), 2);
    }
    let _bounds = crate::proxy::sealed_origin::bound_for_test(8, Duration::ZERO);
    let (upstream, _server) = responding_upstream(vec![thinking_message(&[SEALED_A])]).await;
    let _ = anthropic_turn(&upstream, &opening_turn(), None).await;
    let expired = anthropic_turn(&upstream, &returning_turn(SEALED_A), None).await;
    assert_sealed_refusal(&expired, "egress.sealed_origin_unknown", "expired");
}

/// A member that only shares the sealed member's name is inspected under
/// governed mode like any other content.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn namesake_of_a_sealed_member_is_inspected_under_governed_mode() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    let key = example_access_key();
    let (upstream, server) = responding_upstream(vec![empty_reply()]).await;
    let request = serde_json::json!({
        "model": "claude-test", "max_tokens": 16,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "hello", "signature": format!("deploy with {key}")}
        ]}]
    });
    let response = anthropic_turn(&upstream, &request, None).await;
    let arrived = server.await.expect("upstream");
    assert!(
        arrived
            .iter()
            .all(|body| !String::from_utf8_lossy(body).contains(key)),
        "the namesake member left uninspected (status {})",
        response.status()
    );
    assert!(
        response.status() == StatusCode::BAD_REQUEST || arrived.len() == 1,
        "masked and forwarded, or refused — never silently dropped"
    );
}

/// A register hit proves origin, not the right to reuse: another principal
/// and a policy epoch that changed since the response are refused as
/// "authority lapsed", distinct from unknown origin; the unchanged context
/// is the control.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_not_reusable_by_another_principal_or_after_a_policy_change() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    let one = Some("Bearer caller-one");
    let _ = anthropic_turn(&upstream, &opening_turn(), one).await;

    let other = anthropic_turn(
        &upstream,
        &returning_turn(SEALED_A),
        Some("Bearer caller-two"),
    )
    .await;
    assert_sealed_refusal(
        &other,
        "egress.sealed_authority_lapsed",
        "another principal",
    );

    let same = anthropic_turn(&upstream, &returning_turn(SEALED_A), one).await;
    assert_eq!(same.status(), StatusCode::OK, "unchanged context");
    assert_eq!(server.await.expect("upstream").len(), 2);

    write_gateway_config("[context_gateway]\nmode = \"sovereign\"\n");
    let after_change = anthropic_turn(&upstream, &returning_turn(SEALED_A), one).await;
    assert_sealed_refusal(
        &after_change,
        "egress.sealed_authority_lapsed",
        "policy epoch changed since the response",
    );
}

/// Holds the policy-override lock without fixing a policy, so the project
/// pack on disk is the live policy for the duration of a test.
struct LivePolicy {
    _guard: crate::core::policy::runtime::TestOverrideGuard,
}

fn write_pack(root: &std::path::Path) {
    let directory = root.join(".lean-ctx");
    std::fs::create_dir_all(&directory).expect("policy directory");
    std::fs::write(
        directory.join("policy.toml"),
        "name = \"sealed\"\nversion = \"1.0.0\"\ndescription = \"sealed item test\"\n",
    )
    .expect("project policy");
}

/// One Anthropic turn of a proxy whose operator configured `project` as its
/// project (`[proxy] project_root`). That is the only way the proxy gets a
/// project (`proxy::request_project`): a request scope around the handler
/// names none, and is not how a request reaches the proxy.
async fn anthropic_turn_in(
    project: &std::path::Path,
    upstream: &str,
    body: &serde_json::Value,
) -> Response {
    write_gateway_config(&format!(
        "{GOVERNED}\n[proxy]\nproject_root = '{}'\n",
        project.display()
    ));
    anthropic_turn(upstream, body, None).await
}

/// The project is part of the context: an item produced for one project is
/// not reusable from another, nor after the project's policy was removed;
/// within its own, unchanged project it is returned to its origin.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_bound_to_the_project_it_was_produced_for() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _live = LivePolicy {
        _guard: crate::core::policy::runtime::lock_test_override(),
    };
    write_gateway_config(GOVERNED);
    let project_a = tempfile::tempdir().expect("project A");
    let project_b = tempfile::tempdir().expect("project B");
    write_pack(project_a.path());
    write_pack(project_b.path());
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    let opening = anthropic_turn_in(project_a.path(), &upstream, &opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK);

    let cross = anthropic_turn_in(project_b.path(), &upstream, &returning_turn(SEALED_A)).await;
    assert_sealed_refusal(&cross, "egress.sealed_authority_lapsed", "another project");

    let own = anthropic_turn_in(project_a.path(), &upstream, &returning_turn(SEALED_A)).await;
    assert_eq!(own.status(), StatusCode::OK, "own, unchanged project");

    std::fs::remove_dir_all(project_a.path().join(".lean-ctx")).expect("remove the policy");
    let removed = anthropic_turn_in(project_a.path(), &upstream, &returning_turn(SEALED_A)).await;
    assert_sealed_refusal(
        &removed,
        "egress.sealed_authority_lapsed",
        "the project policy was removed",
    );
    assert_eq!(
        server.await.expect("upstream").len(),
        2,
        "only the opening turn and the return within its own project arrived"
    );
}

/// Restores the process's active role when dropped.
struct RoleReset;

impl Drop for RoleReset {
    fn drop(&mut self) {
        let _ = crate::core::roles::set_active_role("coder");
    }
}

/// The role is part of the context under a project policy: an item produced
/// under one role is known to the destination, but a request under another
/// role does not hold the authority it was produced under.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_not_reusable_under_another_role() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _live = LivePolicy {
        _guard: crate::core::policy::runtime::lock_test_override(),
    };
    write_gateway_config(GOVERNED);
    let project = tempfile::tempdir().expect("project");
    write_pack(project.path());
    let _reset = RoleReset;
    crate::core::roles::set_active_role("coder").expect("role A");
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    let opening = anthropic_turn_in(project.path(), &upstream, &opening_turn()).await;
    assert_eq!(opening.status(), StatusCode::OK);

    crate::core::roles::set_active_role("reviewer").expect("role B");
    let other_role = anthropic_turn_in(project.path(), &upstream, &returning_turn(SEALED_A)).await;
    assert_sealed_refusal(
        &other_role,
        "egress.sealed_authority_lapsed",
        "another role",
    );

    crate::core::roles::set_active_role("coder").expect("role A again");
    let same_role = anthropic_turn_in(project.path(), &upstream, &returning_turn(SEALED_A)).await;
    assert_eq!(same_role.status(), StatusCode::OK, "the producing role");
    assert_eq!(server.await.expect("upstream").len(), 2);
}

/// The person a gateway key resolved to is part of the context: the same
/// item, destination and credential, but another person, is not a reuse.
#[cfg(feature = "enterprise")]
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn sealed_item_is_not_reusable_by_another_gateway_person() {
    async fn turn_as(person: &str, upstream: &str, body: &serde_json::Value) -> Response {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        request
            .extensions_mut()
            .insert(crate::proxy::gateway_identity::GatewayTags {
                person: Some(person.to_owned()),
                team: None,
                project: None,
            });
        tokio::time::timeout(
            ROUTE_BOUND,
            crate::proxy::anthropic::handler(State(proxy_test_state(upstream)), request),
        )
        .await
        .expect("bounded route")
        .expect("an answer or a typed refusal")
    }
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    let (upstream, server) =
        responding_upstream(vec![thinking_message(&[SEALED_A]), empty_reply()]).await;
    let opening = turn_as("ada", &upstream, &opening_turn()).await;
    let _ = to_bytes(opening.into_body(), usize::MAX).await;

    let other = turn_as("grace", &upstream, &returning_turn(SEALED_A)).await;
    assert_sealed_refusal(&other, "egress.sealed_authority_lapsed", "another person");
    let same = turn_as("ada", &upstream, &returning_turn(SEALED_A)).await;
    assert_eq!(same.status(), StatusCode::OK, "the producing person");
    assert_eq!(server.await.expect("upstream").len(), 2);
}

const GEMINI_ROUTE: &str = "/v1beta/models/gemini-pro:streamGenerateContent";

/// One Gemini turn through the real handler, the response read to its end.
async fn gemini_turn(upstream: &str, body: &serde_json::Value) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri(GEMINI_ROUTE)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let response = tokio::time::timeout(
        ROUTE_BOUND,
        crate::proxy::google::handler(State(proxy_test_state(upstream)), request),
    )
    .await
    .expect("bounded route")
    .expect("an answer or a typed refusal");
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, usize::MAX).await.expect("response body");
    Response::from_parts(parts, Body::from(body))
}

fn gemini_opening() -> serde_json::Value {
    serde_json::json!({"contents": [{"role": "user", "parts": [{"text": "hello"}]}]})
}

/// The array Gemini answers a non-SSE stream call with.
fn gemini_array(signature: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!([
        {"candidates": [{"content": {"role": "model", "parts": [{"text": "one moment"}]}}]},
        {"candidates": [{"content": {"role": "model", "parts": [
            {"functionCall": {"name": "lookup", "args": {"q": "weather"}},
             "thoughtSignature": signature}
        ]}}]}
    ]))
    .unwrap()
}

/// The next Gemini turn: the model's function call comes back with its
/// thought signature.
fn gemini_returning(signature: &str) -> serde_json::Value {
    serde_json::json!({"contents": [
        {"role": "user", "parts": [{"text": "hello"}]},
        {"role": "model", "parts": [
            {"functionCall": {"name": "lookup", "args": {"q": "weather"}},
             "thoughtSignature": signature}
        ]},
        {"role": "user", "parts": [
            {"functionResponse": {"name": "lookup", "response": {"ok": true}}}
        ]}
    ]})
}

/// Gemini's array-shaped stream answer carries thought signatures too: one
/// that arrived in a complete answer can be returned to that destination
/// under governed mode; a tampered one, one sent elsewhere and one from an
/// answer that was cut short are of unknown origin.
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn gemini_thought_signature_of_an_array_response_can_be_returned() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _mode = proxy_mode("cache");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    write_gateway_config(GOVERNED);
    crate::proxy::sealed_origin::forget_sealed_items_for_test();
    let (upstream, server) = responding_upstream(vec![
        ("application/json", gemini_array(SEALED_A)),
        empty_reply(),
    ])
    .await;
    assert_eq!(
        gemini_turn(&upstream, &gemini_opening()).await.status(),
        StatusCode::OK
    );

    let tampered = gemini_turn(&upstream, &gemini_returning(SEALED_B)).await;
    assert_sealed_refusal(&tampered, "egress.sealed_origin_unknown", "tampered");

    let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other = format!("http://{}", elsewhere.local_addr().unwrap());
    let moved = gemini_turn(&other, &gemini_returning(SEALED_A)).await;
    assert_sealed_refusal(&moved, "egress.sealed_origin_unknown", "another upstream");

    let returned = gemini_returning(SEALED_A);
    assert_eq!(
        gemini_turn(&upstream, &returned).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        server.await.expect("upstream")[1],
        serde_json::to_vec(&returned).unwrap()
    );

    // An answer cut short inside the array records nothing.
    let mut cut = gemini_array(SEALED_B);
    cut.truncate(cut.len() - 4);
    let (upstream, _server) = responding_upstream(vec![("application/json", cut)]).await;
    let _ = gemini_turn(&upstream, &gemini_opening()).await;
    let after_cut = gemini_turn(&upstream, &gemini_returning(SEALED_B)).await;
    assert_sealed_refusal(&after_cut, "egress.sealed_origin_unknown", "partial answer");
}

#[path = "sealed_source_authority_tests.rs"]
mod source_authority;
