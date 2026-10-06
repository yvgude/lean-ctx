// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::a2a_transport::{AgentIdentityV1, TransportContentType, TransportEnvelopeV1};

#[tokio::test(flavor = "current_thread")]
async fn canceled_handoff_waiter_keeps_admission_until_worker_finishes() {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let permit = std::sync::Arc::new(semaphore.clone().try_acquire_owned().unwrap());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let waiter = tokio::spawn(run_admitted_handoff(permit, move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv();
        (StatusCode::OK, Json(serde_json::json!({"done": true})))
    }));
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(semaphore.available_permits(), 0);
    release_tx.send(()).unwrap();
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        semaphore.clone().acquire_owned(),
    )
    .await
    .unwrap()
    .unwrap();
    drop(recovered);
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test]
async fn relay_preserves_task_response_bytes_and_rejects_missing_response() {
    use crate::core::a2a::remote_transport::DeliveryReceipt;
    let raw = "{\n  \"signature\": \"opaque-downstream-signature\"\n}";
    let receipt = DeliveryReceipt {
        envelope_id: "relay-1".into(),
        delivered_at: chrono::Utc::now(),
        remote_status: 200,
        round_trip_ms: 1,
        unverified_task_response: Some(raw.into()),
    };
    let response = forwarded_delivery_response(receipt.clone(), &TransportContentType::A2ATask);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), raw.as_bytes());
    for unverified_task_response in [None, Some(String::new()), Some("x".repeat(64 * 1024 + 1))] {
        let response = forwarded_delivery_response(
            DeliveryReceipt {
                unverified_task_response,
                ..receipt.clone()
            },
            &TransportContentType::A2ATask,
        );
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
    let response = forwarded_delivery_response(receipt, &TransportContentType::A2AMessage);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "forwarded");
    assert_eq!(json["delivery_id"], "relay-1");
    assert!(json.get("signature").is_none());
}

#[tokio::test]
async fn relay_replay_errors_preserve_typed_status_and_hide_storage_details() {
    use super::super::relay_replay::StoreError;
    for (error, status, retry_after) in [
        (
            StoreError::CapacityExhausted {
                retry_after_seconds: 7,
            },
            StatusCode::TOO_MANY_REQUESTS,
            Some("7"),
        ),
        (
            StoreError::CapacityExhausted {
                retry_after_seconds: 0,
            },
            StatusCode::TOO_MANY_REQUESTS,
            Some("1"),
        ),
        (StoreError::Expired, StatusCode::BAD_REQUEST, None),
        (
            StoreError::InvalidInput("private-detail".into()),
            StatusCode::BAD_REQUEST,
            None,
        ),
        (
            StoreError::MigrationRequired("private-detail".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            None,
        ),
        (
            StoreError::LeaseMismatch,
            StatusCode::SERVICE_UNAVAILABLE,
            None,
        ),
        (
            StoreError::Unavailable("private-detail".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            None,
        ),
    ] {
        let response = relay_replay_error(&error);
        assert_eq!(response.status(), status);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .map(|value| value.to_str().unwrap()),
            retry_after
        );
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("private-detail"));
    }
}

#[tokio::test]
async fn relay_replay_survives_legacy_freshness_window() {
    use super::super::relay_replay::Reservation;
    use super::super::remote_replay::RemoteReplayGuard;
    let root = tempfile::tempdir().unwrap();
    let guard = RemoteReplayGuard::new(
        root.path().to_str().unwrap(),
        REMOTE_REPLAY_RETENTION_SECONDS,
    );
    let now = chrono::Utc::now();
    let record = super::super::relay_replay_async::test_record(now);
    let id = relay_storage_id(&record);
    let fingerprint = record.origin_fingerprint().unwrap();
    let Reservation::Reserved(generation) = guard
        .reserve_relay(&id, &record, &fingerprint, now)
        .await
        .unwrap()
    else {
        panic!("first delivery must reserve");
    };
    guard.complete_relay(&id, &generation).await.unwrap();
    let reopened = RemoteReplayGuard::new(
        root.path().to_str().unwrap(),
        REMOTE_REPLAY_RETENTION_SECONDS,
    );
    assert_eq!(
        reopened
            .reserve_relay(
                &id,
                &record,
                &fingerprint,
                now + chrono::Duration::hours(23)
            )
            .await
            .unwrap(),
        Reservation::Completed
    );
}

#[test]
fn relay_storage_identity_is_path_safe_and_scope_bound() {
    use crate::core::a2a::relay::RelayRecordV1;
    let now = chrono::Utc::now();
    let record = RelayRecordV1::new_signed(
        "../../outside\\windows:stream",
        "origin",
        "recipient",
        "tenant",
        "project",
        TransportContentType::ContextPackage,
        lean_ctx_protocol::DataClassification::Internal,
        now + chrono::Duration::minutes(1),
        4,
        now,
        &crate::core::a2a::relay::test_origin_key("origin"),
        "origin-secret",
        b"{}",
    )
    .unwrap();
    let key = relay_storage_id(&record);
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(key, relay_storage_id(&record));
    for field in ["origin", "tenant", "project", "delivery"] {
        let mut changed = record.clone();
        match field {
            "origin" => changed.origin.push('x'),
            "tenant" => changed.tenant_id.push('x'),
            "project" => changed.project_id.push('x'),
            _ => changed.delivery_id.push('x'),
        }
        assert_ne!(key, relay_storage_id(&changed));
    }
    let mut left = record.clone();
    left.origin = "a".into();
    left.tenant_id = "bc".into();
    let mut right = left.clone();
    right.origin = "ab".into();
    right.tenant_id = "c".into();
    assert_ne!(relay_storage_id(&left), relay_storage_id(&right));
}

fn signed_at(sent_at: chrono::DateTime<chrono::Utc>) -> TransportEnvelopeV1 {
    let mut envelope = TransportEnvelopeV1::new(
        AgentIdentityV1 {
            agent_id: "sender".into(),
            agent_type: "test".into(),
            daemon_fingerprint: "fingerprint".into(),
            capabilities: vec!["a2a_messaging".into()],
        },
        Some("recipient"),
        TransportContentType::A2AMessage,
        "{}".into(),
    );
    envelope.sent_at = sent_at;
    envelope
        .metadata
        .insert("tenant_id".into(), "tenant-a".into());
    envelope
        .metadata
        .insert("project_id".into(), "project-a".into());
    envelope.sign(b"secret").expect("sign envelope");
    envelope
}

fn authority_is_valid(
    envelope: &TransportEnvelopeV1,
    secret: &[u8],
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    valid_remote_authority(
        envelope,
        secret,
        Some("recipient"),
        Some("tenant-a"),
        Some("project-a"),
        now,
    )
}

#[test]
fn remote_authority_requires_valid_signature_recipient_and_freshness() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    let valid = signed_at(now);
    assert!(authority_is_valid(&valid, b"secret", now));
    assert!(!authority_is_valid(&valid, b"wrong", now));

    let mut no_recipient = signed_at(now);
    no_recipient.recipient = None;
    no_recipient.sign(b"secret").expect("resign");
    assert!(!authority_is_valid(&no_recipient, b"secret", now));

    let stale = signed_at(now - chrono::Duration::seconds(301));
    assert!(!authority_is_valid(&stale, b"secret", now));
    let future = signed_at(now + chrono::Duration::seconds(31));
    assert!(!authority_is_valid(&future, b"secret", now));

    let mut wrong_scope = signed_at(now);
    wrong_scope
        .metadata
        .insert("tenant_id".into(), "tenant-b".into());
    wrong_scope.sign(b"secret").expect("resign");
    assert!(!authority_is_valid(&wrong_scope, b"secret", now));

    let mut wrong_project = signed_at(now);
    wrong_project
        .metadata
        .insert("project_id".into(), "project-b".into());
    wrong_project.sign(b"secret").expect("resign");
    assert!(!authority_is_valid(&wrong_project, b"secret", now));

    let mut wrong_recipient = signed_at(now);
    wrong_recipient.recipient = Some("other-recipient".into());
    wrong_recipient.sign(b"secret").expect("resign");
    assert!(!authority_is_valid(&wrong_recipient, b"secret", now));
}

#[test]
fn legacy_delivery_alias_must_verify_as_the_original_envelope() {
    use crate::core::a2a_transport::{
        LEGACY_DELIVERY_ID_METADATA, LEGACY_DELIVERY_SENT_AT_METADATA,
    };

    let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    let original = signed_at(now);
    let original_signature = original.signature.clone().expect("signature");
    let mut retry = original.clone();
    retry.metadata.insert(
        LEGACY_DELIVERY_ID_METADATA.to_string(),
        original_signature.clone(),
    );
    retry.metadata.insert(
        LEGACY_DELIVERY_SENT_AT_METADATA.to_string(),
        original.sent_at.to_rfc3339(),
    );
    retry.sent_at += chrono::Duration::seconds(1);
    retry.sign(b"secret").expect("sign retry");
    assert_eq!(
        verified_legacy_delivery_id(&retry, b"secret"),
        Ok(Some(original_signature))
    );

    retry
        .metadata
        .insert(LEGACY_DELIVERY_ID_METADATA.to_string(), "a".repeat(64));
    retry.sign(b"secret").expect("sign malicious alias");
    assert_eq!(verified_legacy_delivery_id(&retry, b"secret"), Err(()));

    retry.metadata.remove(LEGACY_DELIVERY_SENT_AT_METADATA);
    retry.sign(b"secret").expect("sign incomplete bridge");
    assert_eq!(verified_legacy_delivery_id(&retry, b"secret"), Err(()));
}

#[tokio::test]
async fn remote_replay_guard_rejects_duplicates_expires_and_releases_failures() {
    use super::super::remote_replay::Reservation;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy();
    let guard =
        super::super::remote_replay::RemoteReplayGuard::new(&root, REMOTE_REPLAY_RETENTION_SECONDS);
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);

    let Reservation::Reserved(a_generation) = guard.reserve("sig-a", now).await.expect("reserve")
    else {
        panic!("first delivery must reserve");
    };
    assert_eq!(
        guard.reserve("sig-a", now).await.expect("in flight"),
        Reservation::InFlight
    );
    guard
        .complete("sig-a", a_generation)
        .await
        .expect("complete a");
    let restarted =
        super::super::remote_replay::RemoteReplayGuard::new(&root, REMOTE_REPLAY_RETENTION_SECONDS);
    assert_eq!(
        restarted
            .reserve("sig-a", now)
            .await
            .expect("persistent replay"),
        Reservation::Completed
    );
    assert_eq!(
        restarted
            .reserve("sig-a", now - chrono::Duration::seconds(1))
            .await
            .expect("clock rollback must preserve replay"),
        Reservation::Completed
    );
    assert!(matches!(
        guard
            .reserve(
                "sig-a",
                now + chrono::Duration::seconds(REMOTE_REPLAY_RETENTION_SECONDS + 1),
            )
            .await
            .expect("expired reserve"),
        Reservation::Reserved(_)
    ));

    let Reservation::Reserved(b_generation) = guard.reserve("sig-b", now).await.expect("reserve b")
    else {
        panic!("b must reserve");
    };
    let Reservation::Reserved(c_generation) = guard.reserve("sig-c", now).await.expect("reserve c")
    else {
        panic!("c must reserve");
    };
    guard
        .release("sig-b", b_generation)
        .await
        .expect("release b");
    assert!(matches!(
        guard.reserve("sig-b", now).await.expect("reserve b again"),
        Reservation::Reserved(_)
    ));
    assert_eq!(
        guard.reserve("sig-c", now).await.expect("c remains"),
        Reservation::InFlight
    );
    assert!(guard.release("sig-c", c_generation + 1).await.is_err());
    assert_eq!(
        guard.reserve("sig-c", now).await.expect("owned c remains"),
        Reservation::InFlight
    );
    assert!(matches!(
        guard
            .reserve(
                "sig-c",
                now + chrono::Duration::seconds(REMOTE_REPLAY_RETENTION_SECONDS * 2 + 2),
            )
            .await
            .expect("expired pending reservation can retry idempotently"),
        Reservation::Reserved(_)
    ));
}

#[tokio::test]
async fn replay_primary_and_legacy_alias_transition_atomically() {
    use super::super::remote_replay::Reservation;

    let dir = tempfile::tempdir().expect("tempdir");
    let guard = super::super::remote_replay::RemoteReplayGuard::new(
        dir.path().to_str().expect("root"),
        REMOTE_REPLAY_RETENTION_SECONDS,
    );
    let now = chrono::Utc::now();
    let Reservation::Reserved(generation) = guard
        .reserve_with_alias("stable-a", Some("legacy-a"), now)
        .await
        .expect("reserve pair")
    else {
        panic!("pair must reserve");
    };
    assert_eq!(
        guard.reserve("legacy-a", now).await.expect("alias pending"),
        Reservation::InFlight
    );
    guard
        .complete_with_alias("stable-a", Some("legacy-a"), generation)
        .await
        .expect("complete pair");
    assert_eq!(
        guard
            .reserve("stable-a", now)
            .await
            .expect("primary complete"),
        Reservation::Completed
    );
    assert_eq!(
        guard
            .reserve("legacy-a", now)
            .await
            .expect("alias complete"),
        Reservation::Completed
    );

    let Reservation::Reserved(generation) = guard
        .reserve_with_alias("stable-b", Some("legacy-b"), now)
        .await
        .expect("reserve second pair")
    else {
        panic!("second pair must reserve");
    };
    guard
        .release_with_alias("stable-b", Some("legacy-b"), generation)
        .await
        .expect("release pair");
    assert!(matches!(
        guard
            .reserve("stable-b", now)
            .await
            .expect("primary released"),
        Reservation::Reserved(_)
    ));
    assert!(matches!(
        guard
            .reserve("legacy-b", now)
            .await
            .expect("alias released"),
        Reservation::Reserved(_)
    ));
}

#[test]
fn handoff_files_publish_atomically_and_idempotently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.json");
    persist_handoff_file(&path, b"payload", Some("delivery")).expect("publish");
    assert_eq!(std::fs::read(&path).expect("read payload"), b"payload");
    assert!(!path.with_extension("json.tmp").exists());
    persist_handoff_file(&path, b"payload", Some("delivery")).expect("idempotent retry");
    assert!(persist_handoff_file(&path, b"different", Some("delivery")).is_err());
    assert_eq!(std::fs::read(&path).expect("original remains"), b"payload");
}

#[cfg(unix)]
#[test]
fn handoff_files_reject_symlink_destinations() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target");
    std::fs::write(&target, b"original").expect("target");
    let path = dir.path().join("delivery.json");
    symlink(&target, &path).expect("symlink");
    assert!(persist_handoff_file(&path, b"payload", Some("delivery")).is_err());
    assert_eq!(
        std::fs::read(&target).expect("target unchanged"),
        b"original"
    );
}
