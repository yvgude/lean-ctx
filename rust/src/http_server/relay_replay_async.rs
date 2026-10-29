// SPDX-License-Identifier: Apache-2.0

//! Async boundary for the durable relay ledger; never run SQLite on a reactor.

use std::path::PathBuf;
use std::sync::Arc;

use super::relay_replay::{RelayReplayStore, Reservation, StoreError, StoreLimits};
use crate::core::a2a::relay::RelayRecordV1;

#[derive(Debug)]
pub(super) struct AsyncRelayReplayStore {
    root: PathBuf,
    retention_seconds: i64,
    store: tokio::sync::OnceCell<Arc<RelayReplayStore>>,
}

impl AsyncRelayReplayStore {
    pub(super) fn new(root: &str, retention_seconds: i64) -> Self {
        Self {
            root: PathBuf::from(root),
            retention_seconds,
            store: tokio::sync::OnceCell::new(),
        }
    }

    async fn store(&self) -> Result<Arc<RelayReplayStore>, StoreError> {
        // Initialization includes schema/integrity checks. Cache the successful
        // handle, not individual outcomes; failures remain fail-closed/retryable.
        self.store
            .get_or_try_init(|| async {
                let root = self.root.clone();
                let retention = self.retention_seconds;
                tokio::task::spawn_blocking(move || {
                    let store = RelayReplayStore::new(root, StoreLimits::default(), retention)?;
                    tracing::debug!(
                        database = ?store.database_path().file_name(),
                        "relay replay store initialized"
                    );
                    Ok(Arc::new(store))
                })
                .await
                .map_err(|error| join_error(&error))?
            })
            .await
            .map(Arc::clone)
    }

    /// Call only after verifying the origin/hop signatures and scope policy.
    pub(super) async fn reserve(
        &self,
        storage_id: &str,
        record: &RelayRecordV1,
        fingerprint: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Reservation, StoreError> {
        let store = self.store().await?;
        let storage_id = storage_id.to_string();
        let origin = record.origin.clone();
        let tenant = record.tenant_id.clone();
        let project = record.project_id.clone();
        let fingerprint = fingerprint.to_string();
        let expires_at = record.expires_at.timestamp();
        tokio::task::spawn_blocking(move || {
            store.reserve(
                &storage_id,
                &origin,
                &tenant,
                &project,
                &fingerprint,
                expires_at,
                now.timestamp(),
            )
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    pub(super) async fn complete(&self, storage_id: &str, lease: &str) -> Result<(), StoreError> {
        self.transition(storage_id, lease, true).await
    }

    /// Complete a task delivery and keep its response for duplicate deliveries.
    ///
    /// The durable store holds delivery acknowledgements only. A locally
    /// produced response is one by construction; a forwarded response is the
    /// next peer's unverified bytes and is checked here (see
    /// [`is_delivery_acknowledgement`]). Anything else completes the delivery
    /// WITHOUT a stored response: the duplicate then gets the typed
    /// `task_response_not_retained` answer and is never executed again.
    pub(super) async fn complete_with_response(
        &self,
        storage_id: &str,
        lease: &str,
        body: String,
    ) -> Result<(), StoreError> {
        let store = self.store().await?;
        let storage_id = storage_id.to_owned();
        let lease = lease.to_owned();
        tokio::task::spawn_blocking(move || {
            // Size violations keep the store's own typed error.
            let bounded = !body.is_empty() && body.len() <= MAX_TASK_RESPONSE_BYTES;
            if bounded && !is_delivery_acknowledgement(&body) {
                tracing::warn!(
                    bytes = body.len(),
                    "relay task response is not a delivery acknowledgement; completed without it"
                );
                return store.complete(&storage_id, &lease);
            }
            store.complete_with_response(&storage_id, &lease, &body)
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    /// Caller must authenticate the record before requesting cached bytes.
    pub(super) async fn completed_response(
        &self,
        storage_id: &str,
        record: &RelayRecordV1,
        fingerprint: &str,
    ) -> Result<Option<String>, StoreError> {
        let store = self.store().await?;
        let storage_id = storage_id.to_owned();
        let origin = record.origin.clone();
        let tenant = record.tenant_id.clone();
        let project = record.project_id.clone();
        let fingerprint = fingerprint.to_owned();
        tokio::task::spawn_blocking(move || {
            store.completed_response(&storage_id, &origin, &tenant, &project, &fingerprint)
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    pub(super) async fn release(&self, storage_id: &str, lease: &str) -> Result<(), StoreError> {
        self.transition(storage_id, lease, false).await
    }

    async fn transition(
        &self,
        storage_id: &str,
        lease: &str,
        completed: bool,
    ) -> Result<(), StoreError> {
        let store = self.store().await?;
        let storage_id = storage_id.to_string();
        let lease = lease.to_string();
        tokio::task::spawn_blocking(move || {
            if completed {
                store.complete(&storage_id, &lease)
            } else {
                store.release(&storage_id, &lease)
            }
        })
        .await
        .map_err(|error| join_error(&error))?
    }
}

fn join_error(error: &tokio::task::JoinError) -> StoreError {
    StoreError::Unavailable(format!("relay replay task failed: {error}"))
}

/// The store's own response bound (`RelayReplayStore::complete_with_response`).
const MAX_TASK_RESPONSE_BYTES: usize = 64 * 1024;

/// Longest identifier an acknowledgement may carry (a signature is 128 hex).
const MAX_ACKNOWLEDGEMENT_FIELD_BYTES: usize = 256;

/// The acceptance reply of `handlers::accept_remote_task`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskAcceptance {
    status: String,
    content_type: String,
    task_id: String,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ACKNOWLEDGEMENT_FIELD_BYTES
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

/// Whether a task response is one of the two delivery acknowledgements the
/// receiver produces — and therefore carries no tool or context content:
///
/// - the acceptance reply: `status` (`accepted` | `duplicate`),
///   `content_type` (`a2a_task`) and the caller's own `task_id`;
/// - a signed task status (`SignedTaskStatusV1`): routing identifiers, a
///   digest, a closed task state, two timestamps, a key id and a signature.
///
/// Both are closed shapes (unknown fields are rejected) and every string in
/// them is a bounded identifier without whitespace. A stored acknowledgement
/// has no source whose rights could change, so it needs no source authority.
fn is_delivery_acknowledgement(body: &str) -> bool {
    if let Ok(accepted) = serde_json::from_str::<TaskAcceptance>(body) {
        return matches!(accepted.status.as_str(), "accepted" | "duplicate")
            && accepted.content_type == "a2a_task"
            && identifier(&accepted.task_id);
    }
    crate::core::a2a::task_response::SignedTaskStatusV1::from_json(body).is_ok_and(|status| {
        [
            &status.sender,
            &status.recipient,
            &status.tenant_id,
            &status.project_id,
            &status.task_id,
            &status.request_digest,
            &status.key_id,
            &status.signature,
        ]
        .into_iter()
        .all(|value| identifier(value))
    })
}

#[cfg(test)]
pub(super) fn test_record(now: chrono::DateTime<chrono::Utc>) -> RelayRecordV1 {
    RelayRecordV1::new_signed(
        "delivery",
        "origin",
        "recipient",
        "tenant",
        "project",
        crate::core::a2a_transport::TransportContentType::EvidenceBundle,
        lean_ctx_protocol::DataClassification::Internal,
        now + chrono::Duration::hours(24),
        4,
        now,
        &crate::core::a2a::relay::test_origin_key("origin"),
        "test-channel-secret",
        b"payload",
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_status() -> String {
        use crate::core::a2a::task::{
            TASK_ACTION_GET, TaskControlDescriptorV1, TaskState, TaskStatusV1,
        };
        let now = chrono::Utc::now();
        let mut request = TaskControlDescriptorV1::new(
            "origin",
            "recipient",
            "tenant",
            "project",
            TASK_ACTION_GET,
            "task-1",
            &format!("sha256:{}", "a".repeat(64)),
            now,
            now + chrono::Duration::minutes(5),
            "nonce-1",
            None,
            "grant",
            "origin-key",
        );
        request.sign(&crate::core::a2a::relay::test_origin_key("origin"));
        let reply = crate::core::a2a::task_response::SignedTaskStatusV1::sign_status(
            &request,
            TaskStatusV1 {
                schema_version: 1,
                state: TaskState::Created,
                timestamp: now,
            },
            "server-key",
            &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
            now,
        )
        .expect("signed response");
        serde_json::to_string_pretty(&reply).expect("signed response JSON")
    }

    /// Pins the shape of everything the relay response store may hold: the
    /// two acknowledgements the receiver produces, and nothing that could
    /// carry tool or context content.
    #[test]
    fn only_delivery_acknowledgements_are_storable_task_responses() {
        for status in ["accepted", "duplicate"] {
            let acceptance = serde_json::json!({
                "status": status,
                "content_type": "a2a_task",
                "task_id": "task-1",
            })
            .to_string();
            assert!(is_delivery_acknowledgement(&acceptance), "{status}");
        }
        let signed = signed_status();
        assert!(is_delivery_acknowledgement(&signed));

        let mut with_result: serde_json::Value = serde_json::from_str(&signed).expect("JSON");
        with_result["result"] = serde_json::json!("fn private() {}");
        let mut prose_identifier: serde_json::Value = serde_json::from_str(&signed).expect("JSON");
        prose_identifier["task_id"] = serde_json::json!("contents of the private file");
        let mut oversized: serde_json::Value = serde_json::from_str(&signed).expect("JSON");
        oversized["key_id"] = serde_json::json!("k".repeat(MAX_ACKNOWLEDGEMENT_FIELD_BYTES + 1));
        for content in [
            "fn private() {}".to_owned(),
            serde_json::json!({"status": "completed", "output": "fn private() {}"}).to_string(),
            serde_json::json!({
                "status": "accepted", "content_type": "a2a_task", "task_id": "task-1",
                "artifacts": ["fn private() {}"],
            })
            .to_string(),
            serde_json::json!({
                "status": "accepted", "content_type": "a2a_task",
                "task_id": "contents of the private file",
            })
            .to_string(),
            serde_json::json!({"status": "received", "content_type": "a2a_task", "task_id": "t"})
                .to_string(),
            with_result.to_string(),
            prose_identifier.to_string(),
            oversized.to_string(),
        ] {
            assert!(!is_delivery_acknowledgement(&content), "{content}");
        }
    }

    /// A forwarded response that carries content completes the delivery
    /// without being stored: the duplicate finds the completion and no bytes
    /// (the handler answers `task_response_not_retained`), and the delivery is
    /// never reserved — executed — again. An acknowledgement is stored and
    /// returned byte-exactly.
    #[tokio::test]
    async fn content_response_is_not_stored_and_the_delivery_stays_completed() {
        let root = tempfile::tempdir().expect("root");
        let store = AsyncRelayReplayStore::new(
            root.path().to_str().expect("UTF-8 path"),
            super::super::relay_replay::MIN_RETENTION_SECONDS,
        );
        let now = chrono::Utc::now();
        let record = test_record(now);
        let fingerprint = "f".repeat(64);
        let content =
            serde_json::json!({"status": "completed", "output": "fn private() {}"}).to_string();
        let acknowledgement = signed_status();
        for (id, body, stored) in [
            ("a".repeat(64), content, None),
            (
                "b".repeat(64),
                acknowledgement.clone(),
                Some(acknowledgement),
            ),
        ] {
            let Reservation::Reserved(lease) = store
                .reserve(&id, &record, &fingerprint, now)
                .await
                .expect("reserve")
            else {
                panic!("new reservation required")
            };
            store
                .complete_with_response(&id, &lease, body)
                .await
                .expect("completed");
            assert_eq!(
                store
                    .completed_response(&id, &record, &fingerprint)
                    .await
                    .expect("lookup"),
                stored
            );
            assert_eq!(
                store
                    .reserve(&id, &record, &fingerprint, now)
                    .await
                    .expect("duplicate"),
                Reservation::Completed,
                "a completed delivery is never executed again"
            );
        }
    }
}
