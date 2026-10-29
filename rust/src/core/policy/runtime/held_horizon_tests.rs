// SPDX-License-Identifier: Apache-2.0
use super::{PublicationAuthority, REQUEST_PROJECT, TestPolicyOverride, refresh};
use crate::core::policy::org::Lifetime;
use chrono::{DateTime, Utc};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const MINIMAL_PACK: &str = "name = 'held-horizon'\nversion = '1.0.0'\ndescription = 'test'\n";

#[derive(Clone, Copy)]
enum Phase {
    Open,
    Closing,
    Fresh,
}

fn wait_until_horizon(horizon: DateTime<Utc>) {
    let cap = Instant::now() + Duration::from_secs(10);
    while Utc::now() < horizon {
        assert!(Instant::now() < cap, "the original horizon did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_original_horizon_passing_in_the_closing_refresh_withholds_the_result_despite_a_later_fresh_answer()
 {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(crate::core::policy::load(MINIMAL_PACK).unwrap()));
    let root = tempfile::tempdir().unwrap();
    let project = crate::core::pathutil::canonicalize_secure(root.path()).unwrap();
    let h = Rc::new(Cell::new(Utc::now() + chrono::Duration::seconds(60)));
    let phase = Rc::new(Cell::new(Phase::Open));
    let open_calls = Rc::new(Cell::new(0_u32));
    let closing_calls = Rc::new(Cell::new(0_u32));
    let fresh_calls = Rc::new(Cell::new(0_u32));
    let deadline = Instant::now() + Duration::from_mins(1);
    let _hook = refresh::answer_hook_for_test({
        let h = Rc::clone(&h);
        let phase = Rc::clone(&phase);
        let open_calls = Rc::clone(&open_calls);
        let closing_calls = Rc::clone(&closing_calls);
        let fresh_calls = Rc::clone(&fresh_calls);
        move |lifetime| {
            let original = h.get();
            let later = original + chrono::Duration::hours(1);
            let horizon = match phase.get() {
                Phase::Open => {
                    open_calls.set(open_calls.get() + 1);
                    original
                }
                Phase::Closing => {
                    closing_calls.set(closing_calls.get() + 1);
                    wait_until_horizon(original);
                    later
                }
                Phase::Fresh => {
                    fresh_calls.set(fresh_calls.get() + 1);
                    later
                }
            };
            *lifetime = Some(Lifetime::for_test(Some(horizon), deadline));
        }
    });

    REQUEST_PROJECT.sync_scope(RefCell::new(Some(project)), || {
        let mut positive_authority = None;
        let positive = super::with_source_view(|| {
            positive_authority = Some(PublicationAuthority::capture().unwrap());
            phase.set(Phase::Fresh);
            42
        });
        assert_eq!(positive.unwrap(), 42);
        positive_authority.unwrap().verify().unwrap();
        assert!(open_calls.get() >= 1);
        assert!(fresh_calls.get() >= 1);

        h.set(Utc::now() + chrono::Duration::seconds(3));
        phase.set(Phase::Open);
        closing_calls.set(0);
        let operation_calls = Cell::new(0_u32);
        let mut held_authority = None;
        let negative = super::with_source_view(|| {
            held_authority = Some(PublicationAuthority::capture().unwrap());
            operation_calls.set(operation_calls.get() + 1);
            phase.set(Phase::Closing);
            42
        });
        assert_eq!(
            negative.unwrap_err(),
            "source policy view changed during operation"
        );
        assert_eq!(operation_calls.get(), 1);
        assert!(closing_calls.get() >= 1);
        assert!(Utc::now() >= h.get());
        assert!(held_authority.unwrap().verify().is_err());
    });
}

#[cfg(feature = "http-server")]
fn write_gateway_config(text: &str) {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(config.join("config.toml"), text).expect("gateway config");
}

#[cfg(feature = "http-server")]
#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn the_original_horizon_passing_after_the_first_send_proof_sends_nothing() {
    use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
    use crate::proxy::admitted::{AdmittedBody, Payload, REASON_CODES_HEADER, RequestHead, admit};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(crate::core::policy::load(MINIMAL_PACK).unwrap()));
    write_gateway_config("[context_gateway]\nmode = \"developer\"\n");
    let root = tempfile::tempdir().unwrap();
    let project = crate::core::pathutil::canonicalize_secure(root.path()).unwrap();
    let h = Rc::new(Cell::new(Utc::now() + chrono::Duration::seconds(60)));
    let phase = Rc::new(Cell::new(Phase::Open));
    let open_calls = Rc::new(Cell::new(0_u32));
    let closing_calls = Rc::new(Cell::new(0_u32));
    let fresh_calls = Rc::new(Cell::new(0_u32));
    let deadline = Instant::now() + Duration::from_mins(1);
    let _hook = refresh::answer_hook_for_test({
        let h = Rc::clone(&h);
        let phase = Rc::clone(&phase);
        let open_calls = Rc::clone(&open_calls);
        let closing_calls = Rc::clone(&closing_calls);
        let fresh_calls = Rc::clone(&fresh_calls);
        move |lifetime| {
            let original = h.get();
            let later = original + chrono::Duration::hours(1);
            let horizon = match phase.get() {
                Phase::Open => {
                    open_calls.set(open_calls.get() + 1);
                    original
                }
                Phase::Closing => {
                    closing_calls.set(closing_calls.get() + 1);
                    wait_until_horizon(original);
                    later
                }
                Phase::Fresh => {
                    fresh_calls.set(fresh_calls.get() + 1);
                    later
                }
            };
            *lifetime = Some(Lifetime::for_test(Some(horizon), deadline));
        }
    });

    REQUEST_PROJECT
        .scope(RefCell::new(Some(project)), async {
            let document = serde_json::json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hello"}]
            });
            let bytes = serde_json::to_vec(&document).unwrap();
            let original_bytes = bytes.len();
            let client = reqwest::Client::new();
            let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_a = format!("http://{}", listener_a.local_addr().unwrap());
            let target_a = EgressTarget {
                provider: "Anthropic",
                model: None,
                upstream_base: &upstream_a,
            };
            let admission_a = admit(
                Payload::Document(&document),
                &target_a,
                TrustedDestinationBinding::default(),
            )
            .expect("the minimal document must be admitted");
            let body_a = AdmittedBody::bind(
                admission_a,
                bytes.clone(),
                &upstream_a,
                &upstream_a,
                RequestHead::new(reqwest::Method::POST, []),
                original_bytes,
                None,
            )
            .await
            .expect("the positive admitted body must bind");
            let accepted_a = Cell::new(0_u32);
            let serve_a = async {
                let (mut stream, _) = listener_a.accept().await.unwrap();
                accepted_a.set(accepted_a.get() + 1);
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "a request header must arrive");
                    assert!(request.len() + read <= 65_536, "bounded fixture header");
                    request.extend_from_slice(&chunk[..read]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
            };
            let (positive, ()) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(body_a.send(&client, 1, |request| request), serve_a)
            })
            .await
            .expect("the positive loopback exchange must finish");
            assert!(positive.is_ok_and(|attempt| {
                attempt
                    .response
                    .is_ok_and(|response| response.status().is_success())
            }));
            assert_eq!(accepted_a.get(), 1);
            assert!(open_calls.get() >= 1);

            let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_b = format!("http://{}", listener_b.local_addr().unwrap());
            let target_b = EgressTarget {
                provider: "Anthropic",
                model: None,
                upstream_base: &upstream_b,
            };
            h.set(Utc::now() + chrono::Duration::seconds(3));
            phase.set(Phase::Open);
            fresh_calls.set(0);
            let admission_b = admit(
                Payload::Document(&document),
                &target_b,
                TrustedDestinationBinding::default(),
            )
            .expect("the same minimal document must be admitted");
            let body_b = AdmittedBody::bind(
                admission_b,
                bytes,
                &upstream_b,
                &upstream_b,
                RequestHead::new(reqwest::Method::POST, []),
                original_bytes,
                None,
            )
            .await
            .expect("the negative admitted body must bind");
            let built = Cell::new(0_u32);
            let negative = body_b
                .send(&client, 1, |request| {
                    built.set(built.get() + 1);
                    phase.set(Phase::Fresh);
                    wait_until_horizon(h.get());
                    request
                })
                .await;
            let Err(refusal) = negative else {
                panic!("the original expired horizon must stop the send")
            };
            assert_eq!(
                refusal.response("Anthropic").headers()[REASON_CODES_HEADER],
                "policy.changed"
            );
            assert_eq!(built.get(), 1);
            assert!(fresh_calls.get() >= 1);
            assert!(Utc::now() >= h.get());
            let accepted_b = Cell::new(0_u32);
            let unexpected_accept = tokio::time::timeout(Duration::from_millis(200), async {
                let connection = listener_b.accept().await;
                if connection.is_ok() {
                    accepted_b.set(accepted_b.get() + 1);
                }
                connection
            })
            .await;
            assert!(unexpected_accept.is_err());
            assert_eq!(accepted_b.get(), 0);
        })
        .await;
}
