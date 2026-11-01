// SPDX-License-Identifier: Apache-2.0
//! The company journey's held floor, through to the proxy's send path: a
//! signed policy whose horizon passes stops a kept policy, a source view and
//! an admitted body that would leave again. Fixtures and bundle contract as in
//! `core::intelligence_runtime`'s company journey tests: ignored by default,
//! failing when selected without `LEANCTX_COMPANY_JOURNEY_BUNDLE`.

use std::time::{Duration, Instant};

use crate::core::intelligence_runtime::tests::first_install_tests::company_journey_tests::{
    Endpoint, bundle_file, instant,
};
use crate::core::policy::content::{ReleaseCheck, evaluate_text, release_check};

#[test]
#[ignore = "needs the TEST fixture bundle named by LEANCTX_COMPANY_JOURNEY_BUNDLE"]
fn a_held_floor_denies_everything_once_its_horizon_has_passed() {
    let endpoint = Endpoint::new(Some("marker-earlier.json"));
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-short.json"),
    )
    .unwrap();
    let horizon = instant(endpoint.facts["short_not_after"].as_str().unwrap());
    // The bundle must be fresh enough for the view to start inside the
    // horizon; a stale one is a fixture error, not a result.
    assert!(
        chrono::Utc::now() + chrono::Duration::seconds(5) < horizon,
        "the fixture bundle's short horizon is spent; produce a fresh bundle"
    );
    let project = tempfile::tempdir().unwrap();
    // What a kept policy says, asked directly: shell, read, clean content
    // withheld, release refused.
    let says = |policy: &crate::core::policy::runtime::ActivePolicy| {
        (
            policy.tool_allowed("ctx_shell"),
            policy.tool_allowed("ctx_read"),
            evaluate_text("nothing to hide", policy).blocked,
            release_check("nothing to hide", policy) == ReleaseCheck::Refused,
        )
    };

    // A policy obtained outside any view and simply kept. Its request, with
    // its two-second bound, is over long before the signed horizon.
    let kept = crate::core::policy::runtime::for_project(project.path())
        .unwrap()
        .expect("the endpoint has an organization policy");
    // Measured from the handout: whatever happened before it, the request
    // that produced this policy ended at most two seconds after it began.
    let obtained = Instant::now();
    let twin = std::sync::Arc::clone(&kept);
    while obtained.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(chrono::Utc::now() < horizon);
    // Past the request's deadline, inside the horizon: still the signed floor.
    assert_eq!(says(&kept), (false, true, false, false));

    let observed = std::cell::RefCell::new(None);
    let held = crate::core::policy::runtime::with_explicit_project_source_view(
        project.path(),
        |authority| {
            let allowed = |tool: &str| {
                crate::core::policy::runtime::active().map(|policy| policy.tool_allowed(tool))
            };
            // Inside the horizon the signed floor applies: exactly its deny.
            let before = (allowed("ctx_shell"), allowed("ctx_read"));
            let from_view = crate::core::policy::runtime::active().unwrap();
            let view_before = says(&from_view);
            // Wait on the clock the product reads until the signed instant
            // is observed to have passed.
            while chrono::Utc::now() < horizon {
                std::thread::sleep(Duration::from_millis(50));
            }
            let after = (allowed("ctx_shell"), allowed("ctx_read"));
            *observed.borrow_mut() = Some((
                before,
                after,
                authority.verify().is_err(),
                view_before,
                // The very policy taken from the view before, not asked for again.
                says(&from_view),
            ));
        },
    );
    assert!(held.is_err());
    let (before, after, publication_refused, view_before, view_after) = observed.take().unwrap();
    assert_eq!(before, (Some(false), Some(true)));
    assert_eq!(after, (Some(false), Some(false)));
    assert!(publication_refused);
    assert_eq!(view_before, (false, true, false, false));
    assert_eq!(view_after, (false, false, true, true));
    // The same kept policy, and its clone, after the signed horizon.
    assert_eq!(says(&kept), (false, false, true, true));
    assert_eq!(says(&twin), (false, false, true, true));

    // The proxy's send path under the successor policy, whose horizon is
    // still ahead: a body admitted and bound under it leaves once; then the
    // caller's own request building outlasts the signed horizon, and the same
    // body does not leave again.
    std::fs::write(
        &endpoint.policy_path,
        bundle_file(&endpoint.bundle, "policy-late.json"),
    )
    .unwrap();
    let late = instant(endpoint.facts["late_not_after"].as_str().unwrap());
    assert!(
        chrono::Utc::now() + chrono::Duration::seconds(20) < late,
        "the fixture bundle's second horizon is spent; produce a fresh bundle"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (built, seen, refusal_codes, second_connection) = runtime.block_on(send_across(late));
    assert_eq!(built, 2, "both requests were really built");
    assert!(seen.contains("x-built: live"), "{seen}");
    assert_eq!(refusal_codes.as_deref(), Some("policy.changed"));
    assert!(
        !second_connection,
        "the ready upstream saw a second connection"
    );
    drop(endpoint);
}

/// Admit and bind an empty body under the current project policy, send it
/// once to an owned loopback upstream, then send the same body again with a
/// request decoration that returns only after `horizon` has been observed to
/// pass. Returns how many requests were built, the first request as the
/// upstream saw it, the reason codes of the second attempt's refusal (if it
/// was refused) and whether the upstream saw a second connection.
async fn send_across(
    horizon: chrono::DateTime<chrono::Utc>,
) -> (u32, String, Option<String>, bool) {
    use super::admitted::{AdmittedBody, Payload, REASON_CODES_HEADER, admit};
    use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let target = EgressTarget {
        provider: "Anthropic",
        model: None,
        upstream_base: &upstream,
    };
    let admission = admit(
        Payload::Opaque(0),
        &target,
        TrustedDestinationBinding::default(),
    )
    .unwrap_or_else(|_| panic!("an empty payload is admitted under the signed policy"));
    let head = super::admitted::RequestHead::new(
        reqwest::Method::POST,
        [(
            axum::http::HeaderName::from_static("x-built"),
            axum::http::HeaderValue::from_static("live"),
        )],
    );
    let body = AdmittedBody::bind(
        admission,
        b"{}".to_vec(),
        &upstream,
        &upstream,
        head,
        2,
        None,
    )
    .await
    .unwrap_or_else(|_| panic!("the admitted body binds to its own upstream"));
    let client = reqwest::Client::new();
    let built = std::cell::Cell::new(0_u32);

    let serve = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(read > 0, "the request ended before its headers");
            request.extend_from_slice(&buffer[..read]);
        }
        // Closed after the answer: a second dispatch would need a connection.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8_lossy(&request).to_lowercase()
    };
    let (first, seen) = tokio::join!(
        body.send(&client, 1, |request| {
            built.set(built.get() + 1);
            request
        }),
        serve
    );
    assert!(
        first.is_ok_and(|attempt| attempt
            .response
            .is_ok_and(|response| response.status().is_success())),
        "the live attempt was not sent"
    );

    let second = body
        .send(&client, 2, |request| {
            built.set(built.get() + 1);
            // The caller's own code, taking its time: until the signed
            // horizon is observed to have passed on the real clock.
            while chrono::Utc::now() < horizon {
                std::thread::sleep(Duration::from_millis(50));
            }
            request
        })
        .await;
    let refusal_codes = second.err().map(|refusal| {
        refusal.response("Anthropic").headers()[REASON_CODES_HEADER]
            .to_str()
            .unwrap()
            .to_owned()
    });
    let second_connection = tokio::time::timeout(Duration::from_millis(200), listener.accept())
        .await
        .is_ok();
    (built.get(), seen, refusal_codes, second_connection)
}
