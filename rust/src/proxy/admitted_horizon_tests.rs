// SPDX-License-Identifier: Apache-2.0
//! The admitted send path under a held policy horizon: a body admitted and
//! bound while the signed horizon is ahead leaves once; when the caller's own
//! request building outlasts the original horizon, nothing is sent.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::admitted::{AdmittedBody, Payload, REASON_CODES_HEADER, RequestHead, admit};
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
use crate::core::policy::runtime::held_horizon_tests::{
    HorizonClock, MINIMAL_PACK, Phase, wait_until_horizon,
};
use crate::core::policy::runtime::{REQUEST_PROJECT, TestPolicyOverride};

fn write_gateway_config(text: &str) {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(config.join("config.toml"), text).expect("gateway config");
}

#[tokio::test]
#[serial_test::serial(cache_telemetry)]
async fn the_original_horizon_passing_after_the_first_send_proof_sends_nothing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(crate::core::policy::load(MINIMAL_PACK).unwrap()));
    write_gateway_config("[context_gateway]\nmode = \"developer\"\n");
    let root = tempfile::tempdir().unwrap();
    let project = crate::core::pathutil::canonicalize_secure(root.path()).unwrap();
    let clock = HorizonClock::new();
    let deadline = Instant::now() + Duration::from_mins(1);
    let _hook = clock.arm(deadline);

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
            assert!(clock.open_calls.get() >= 1);

            let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_b = format!("http://{}", listener_b.local_addr().unwrap());
            let target_b = EgressTarget {
                provider: "Anthropic",
                model: None,
                upstream_base: &upstream_b,
            };
            clock.h.set(Utc::now() + chrono::Duration::seconds(3));
            clock.phase.set(Phase::Open);
            clock.fresh_calls.set(0);
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
                    clock.phase.set(Phase::Fresh);
                    wait_until_horizon(clock.h.get());
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
            assert!(clock.fresh_calls.get() >= 1);
            assert!(Utc::now() >= clock.h.get());
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
