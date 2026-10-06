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
