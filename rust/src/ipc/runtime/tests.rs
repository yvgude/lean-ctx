// SPDX-License-Identifier: Apache-2.0
use super::*;
use lean_ctx_protocol::runtime_exchange::RUNTIME_EXCHANGE_VERSION;
use lean_ctx_protocol::runtime_frame::{decode_runtime_frame, encode_runtime_frame};
use lean_ctx_protocol::runtime_session::RuntimeRequestSession;
use serde_json::json;

// Published synthetic fixtures, not credentials or captured user content.
const KEY: [u8; 32] = [0x0b; 32];
const ORDINARY: RuntimeExchangeProfile = RuntimeExchangeProfile::ORDINARY;
const DIGEST: &str = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

fn request() -> RuntimeRequestV1 {
    serde_json::from_value(json!({
        "protocol_version": RUNTIME_EXCHANGE_VERSION,
        "session_id": "test-session", "request_id": "test-request",
        "sequence": 1, "deadline_unix_ms": unix_millis().unwrap() + 10_000,
        "input": "hello",
        "invocation": {
            "schema_version": 1, "invocation_id": "test-invocation",
            "engine": {"engine_id": "test-peer", "engine_version": "1.0.0"},
            "operation": {"capability_id": "source-read", "capability_version": "1.0.0"},
            "input_ref": "projection:test", "input_digest": DIGEST,
            "source_refs": ["projection:test"],
            "policy_admission": {"policy_ref": "policy:test", "decision": "admitted"}
        }
    }))
    .unwrap()
}

fn response(request: &RuntimeRequestV1) -> RuntimeResponseV1 {
    serde_json::from_value(json!({
        "protocol_version": RUNTIME_EXCHANGE_VERSION,
        "request_digest": request.digest().unwrap(), "output": "hello",
        "observation": {
            "schema_version": 1, "invocation_id": "test-invocation", "status": "succeeded",
            "output_ref": "projection:result", "output_digest": DIGEST,
            "source_lineage": ["projection:test"], "measurements": []
        }
    }))
    .unwrap()
}

fn address(directory: &tempfile::TempDir) -> DaemonAddr {
    #[cfg(unix)]
    {
        DaemonAddr::Unix(directory.path().join("ir.sock"))
    }
    #[cfg(windows)]
    {
        DaemonAddr::NamedPipe(format!(
            r"\\.\pipe\leanctx-ir-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        ))
    }
}

#[tokio::test]
async fn inherited_stream_keeps_frame_authentication_scope_and_deadlines() {
    for fault in 0..4 {
        let request = request();
        let (mut client, mut peer) = tokio::io::duplex(4096);
        let expected = &request;
        let server = async move {
            let frame = read_frame(&mut peer, ORDINARY).await.unwrap();
            let mut session =
                RuntimeRequestSession::new(&KEY, expected.session_id.clone(), &expected.invocation)
                    .unwrap();
            let mut received = session.accept(&frame, unix_millis().unwrap()).unwrap();
            if fault == 2 {
                received.sequence += 1;
            }
            if fault == 3 {
                return; // Truncated reply is an I/O error, never a fallback success.
            }
            let key = if fault == 1 { [7; 32] } else { KEY };
            let reply = encode_runtime_frame(
                &key,
                RuntimeFrameDirection::Response,
                &[13; 24],
                &response(&received).to_bytes(&received).unwrap(),
            )
            .unwrap();
            peer.write_all(&reply).await.unwrap();
        };
        let (result, ()) = tokio::join!(
            exchange_inherited(&mut client, &KEY, &request, ORDINARY),
            server
        );
        match fault {
            0 => assert!(result.unwrap() == response(&request)),
            1 => assert!(matches!(result, Err(RuntimeExchangeError::InvalidFrame))),
            2 => assert!(matches!(result, Err(RuntimeExchangeError::InvalidResponse))),
            _ => assert!(matches!(result, Err(RuntimeExchangeError::Io))),
        }
    }
    let mut request = request();
    let (mut client, mut peer) = tokio::io::duplex(4096);
    request.deadline_unix_ms = 1;
    assert!(matches!(
        exchange_inherited(&mut client, &KEY, &request, ORDINARY).await,
        Err(RuntimeExchangeError::InvalidRequest)
    ));
    request.deadline_unix_ms = unix_millis().unwrap() + 100;
    let (result, ()) = tokio::join!(
        exchange_inherited(&mut client, &KEY, &request, ORDINARY),
        async {
            read_frame(&mut peer, ORDINARY).await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    );
    assert!(matches!(
        result,
        Err(RuntimeExchangeError::DeadlineExceeded)
    ));
}

#[tokio::test]
async fn local_endpoint_authenticates_fragmented_response_and_rejects_faults() {
    for fault in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let address = address(&directory);
        let request = request();
        let expected = response(&request);
        // Fresh, test-owned endpoint only; never invoke cleanup on a global socket.
        #[cfg(unix)]
        let listener = match &address {
            DaemonAddr::Unix(path) => tokio::net::UnixListener::bind(path).unwrap(),
        };
        #[cfg(windows)]
        let mut listener = super::super::bind_listener(&address).unwrap();
        let server = async {
            #[cfg(unix)]
            let (mut stream, _) = listener.accept().await.unwrap();
            #[cfg(windows)]
            let mut stream = listener.accept_pipe().await.unwrap();
            let bytes = read_frame(&mut stream, ORDINARY).await.unwrap();
            let mut session =
                RuntimeRequestSession::new(&KEY, "test-session".to_owned(), &request.invocation)
                    .unwrap();
            let mut admitted = session.accept(&bytes, unix_millis().unwrap()).unwrap();
            assert!(admitted == request);
            if fault == 4 {
                admitted.sequence += 1;
            }
            let payload = if fault == 3 {
                b"{".to_vec()
            } else {
                response(&admitted).to_bytes(&admitted).unwrap()
            };
            let mut frame =
                encode_runtime_frame(&KEY, RuntimeFrameDirection::Response, &[0x0d; 24], &payload)
                    .unwrap();
            if fault == 1 {
                frame[12] ^= 1;
            }
            if fault == 2 {
                frame = bytes;
            }
            for chunk in frame.chunks(7) {
                stream.write_all(chunk).await.unwrap();
            }
        };
        let (result, ()) = tokio::join!(exchange(&address, &KEY, &request), server);
        match fault {
            0 => assert!(result.unwrap() == expected),
            1 | 2 => assert!(matches!(result, Err(RuntimeExchangeError::InvalidFrame))),
            _ => assert!(matches!(result, Err(RuntimeExchangeError::InvalidResponse))),
        }
    }
}

#[tokio::test]
async fn oversized_header_is_rejected_without_waiting_for_a_body() {
    for length in [0_u32, 1_048_577, u32::MAX] {
        let (mut reader, mut writer) = tokio::io::duplex(32);
        let mut header = b"LCTXIR02".to_vec();
        header.extend_from_slice(&length.to_be_bytes());
        writer.write_all(&header).await.unwrap();
        // Writer stays open: a body read would stall and fail this outer timeout.
        let result =
            tokio::time::timeout(Duration::from_secs(1), read_frame(&mut reader, ORDINARY))
                .await
                .unwrap();
        assert!(matches!(result, Err(RuntimeExchangeError::InvalidFrame)));
    }
}

#[tokio::test]
async fn verifier_response_headers_are_bounded_before_body_reads() {
    let profile = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    // One byte over the response ceiling, and a request-sized length: the
    // response direction refuses both from the header alone.
    for length in [
        profile.max_response_bytes() + 1,
        profile.max_request_bytes(),
    ] {
        let (mut reader, mut writer) = tokio::io::duplex(32);
        let mut header = b"LCTXIR02".to_vec();
        header.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
        writer.write_all(&header).await.unwrap();
        // Writer stays open: a body read would stall and fail this outer timeout.
        let result = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut reader, profile))
            .await
            .unwrap();
        assert!(matches!(result, Err(RuntimeExchangeError::InvalidFrame)));
    }
    // Exactly the response ceiling is read in full. Framing only: the bytes
    // are neither authenticated nor JSON here.
    let length = profile.max_response_bytes();
    let overhead = RUNTIME_FRAME_HEADER_BYTES + RUNTIME_FRAME_NONCE_BYTES + RUNTIME_FRAME_TAG_BYTES;
    let mut frame = b"LCTXIR02".to_vec();
    frame.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
    frame.resize(overhead + length, 0);
    let (mut reader, mut writer) = tokio::io::duplex(64 * 1024);
    let (written, read) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(writer.write_all(&frame), read_frame(&mut reader, profile))
    })
    .await
    .unwrap();
    written.unwrap();
    assert_eq!(read.unwrap(), frame);
}

#[tokio::test]
async fn inherited_verifier_stream_preserves_profile_through_wide_roundtrip() {
    use lean_ctx_protocol::runtime_exchange::{
        MAX_RUNTIME_DEADLINE_MS, MAX_RUNTIME_EXCHANGE_BYTES,
    };
    use sha2::{Digest, Sha256};
    let profile = RuntimeExchangeProfile::ORG_POLICY_VERIFIER;
    let digest = |bytes: &[u8]| format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    // The ordinary request and its reply first; then both are widened past
    // the ordinary megabyte, inside the verifier's own ceilings.
    let mut request = request();
    let mut reply = response(&request);
    request.input = "a".repeat(4 * MAX_RUNTIME_EXCHANGE_BYTES);
    request.invocation.input_digest = digest(request.input.as_bytes()).try_into().unwrap();
    assert!(request.to_bytes().is_err());
    let output = "b".repeat(2 * MAX_RUNTIME_EXCHANGE_BYTES);
    reply.observation.output_digest = Some(digest(output.as_bytes()).try_into().unwrap());
    reply.output = Some(output);
    // Megabytes are digested, sealed and parsed on both sides; a debug build
    // takes about ten seconds for that, so the request's ordinary ten
    // seconds, counted from before the widening, run out. The deadline starts
    // here, at the protocol's full budget.
    request.deadline_unix_ms = unix_millis().unwrap() + MAX_RUNTIME_DEADLINE_MS;
    reply.request_digest = request.digest_in(profile).unwrap();
    let (mut client, mut peer) = tokio::io::duplex(64 * 1024);
    let (expected, answer) = (&request, &reply);
    let server = async move {
        let frame = read_frame(&mut peer, profile).await.unwrap();
        let mut session = RuntimeRequestSession::new_in(
            &KEY,
            expected.session_id.clone(),
            &expected.invocation,
            profile,
        )
        .unwrap();
        let received = session.accept(&frame, unix_millis().unwrap()).unwrap();
        assert!(received == *expected);
        let frame = encode_runtime_frame_in(
            &KEY,
            RuntimeFrameDirection::Response,
            &[13; 24],
            &answer.to_bytes_in(&received, profile).unwrap(),
            profile,
        )
        .unwrap();
        peer.write_all(&frame).await.unwrap();
    };
    // Bounded by the request's own deadline: a client that refused before
    // writing would leave the server waiting for a header, so the test fails
    // here instead of hanging.
    let bound = Duration::from_millis(MAX_RUNTIME_DEADLINE_MS);
    let (result, ()) = tokio::time::timeout(bound, async {
        tokio::join!(
            exchange_inherited(&mut client, &KEY, &request, profile),
            server
        )
    })
    .await
    .unwrap();
    assert!(result.unwrap() == reply);
}

#[tokio::test]
async fn truncated_frames_fail_as_io_errors() {
    let bytes =
        encode_runtime_frame(&KEY, RuntimeFrameDirection::Response, &[0x0d; 24], b"{}").unwrap();
    for length in [0, 11, 12, bytes.len() - 1] {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(&bytes[..length]).await.unwrap();
        drop(writer);
        assert!(matches!(
            read_frame(&mut reader, ORDINARY).await,
            Err(RuntimeExchangeError::Io)
        ));
    }
}

#[tokio::test]
async fn stalled_peer_hits_single_deadline_and_invalid_input_never_connects() {
    let directory = tempfile::tempdir().unwrap();
    let address = address(&directory);
    let mut request = request();
    request.deadline_unix_ms = 1;
    assert!(matches!(
        exchange(&address, &KEY, &request).await,
        Err(RuntimeExchangeError::InvalidRequest)
    ));
    request.deadline_unix_ms = unix_millis().unwrap() + 150;
    #[cfg(unix)]
    let listener = match &address {
        DaemonAddr::Unix(path) => tokio::net::UnixListener::bind(path).unwrap(),
    };
    #[cfg(windows)]
    let mut listener = super::super::bind_listener(&address).unwrap();
    let server = async {
        #[cfg(unix)]
        let (mut stream, _) = listener.accept().await.unwrap();
        #[cfg(windows)]
        let mut stream = listener.accept_pipe().await.unwrap();
        read_frame(&mut stream, ORDINARY).await.unwrap();
        // Do not reply; the client's timeout must also close its connection.
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(exchange(&address, &KEY, &request), server)
    })
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(RuntimeExchangeError::DeadlineExceeded)
    ));
}

#[tokio::test]
async fn missing_endpoint_is_bounded_and_never_created() {
    let directory = tempfile::tempdir().unwrap();
    let address = address(&directory);
    let mut request = request();
    request.deadline_unix_ms = unix_millis().unwrap() + 150;
    let result = tokio::time::timeout(Duration::from_secs(5), exchange(&address, &KEY, &request))
        .await
        .unwrap();
    #[cfg(unix)]
    assert!(matches!(
        result,
        Err(RuntimeExchangeError::ConnectionUnavailable)
    ));
    #[cfg(windows)]
    assert!(matches!(
        result,
        Err(RuntimeExchangeError::DeadlineExceeded)
    ));
    assert!(!address.is_listening());
}

#[test]
fn deadline_precedes_late_inner_errors_and_remote_addresses_are_rejected() {
    let late = Instant::now() - Duration::from_secs(1);
    assert!(matches!(
        finish_exchange(late, Err(RuntimeExchangeError::Io)),
        Err(RuntimeExchangeError::DeadlineExceeded)
    ));
    let timely = Instant::now() + Duration::from_secs(10);
    assert!(matches!(
        finish_exchange(timely, Err(RuntimeExchangeError::Io)),
        Err(RuntimeExchangeError::Io)
    ));
    #[cfg(unix)]
    let invalid = DaemonAddr::Unix("relative.sock".into());
    #[cfg(windows)]
    let invalid = DaemonAddr::NamedPipe(r"\\untrusted-host\pipe\runtime".into());
    assert_eq!(
        validate_local_address(&invalid),
        Err(RuntimeExchangeError::InvalidEndpoint)
    );
}

#[tokio::test]
async fn untrusted_listener_never_receives_plaintext_and_each_send_has_fresh_nonce() {
    let directory = tempfile::tempdir().unwrap();
    let address = address(&directory);
    #[cfg(unix)]
    let listener = match &address {
        DaemonAddr::Unix(path) => tokio::net::UnixListener::bind(path).unwrap(),
    };
    #[cfg(windows)]
    let mut listener = super::super::bind_listener(&address).unwrap();
    let mut nonces = Vec::new();
    for _ in 0..2 {
        let request = request();
        let server = async {
            #[cfg(unix)]
            let (mut stream, _) = listener.accept().await.unwrap();
            #[cfg(windows)]
            let mut stream = listener.accept_pipe().await.unwrap();
            let frame = read_frame(&mut stream, ORDINARY).await.unwrap();
            for plaintext in [b"hello".as_slice(), b"projection:test".as_slice()] {
                assert!(
                    !frame
                        .windows(plaintext.len())
                        .any(|window| window == plaintext)
                );
            }
            assert!(
                decode_runtime_frame(&[0x0c; 32], RuntimeFrameDirection::Request, &frame).is_err()
            );
            let forged = encode_runtime_frame(
                &[0x0c; 32],
                RuntimeFrameDirection::Response,
                &[0x0e; 24],
                &response(&request).to_bytes(&request).unwrap(),
            )
            .unwrap();
            stream.write_all(&forged).await.unwrap();
            frame
                [RUNTIME_FRAME_HEADER_BYTES..RUNTIME_FRAME_HEADER_BYTES + RUNTIME_FRAME_NONCE_BYTES]
                .to_vec()
        };
        let (result, nonce) = tokio::join!(exchange(&address, &KEY, &request), server);
        assert!(matches!(result, Err(RuntimeExchangeError::InvalidFrame)));
        nonces.push(nonce);
    }
    assert_ne!(nonces[0], nonces[1]);
}
