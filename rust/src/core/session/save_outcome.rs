// SPDX-License-Identifier: Apache-2.0
//! Commit acknowledgement without replacing edits made during background I/O.

use super::{PreparedSave, SessionState};
use crate::core::context_checkpoint::CanonicalSession;
use lean_ctx_protocol::Sha256Digest;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(super) struct SaveOutcome {
    id: String,
    version: u32,
    expected: Option<Sha256Digest>,
    pub(super) primary_sha256: Option<String>,
    pub(super) committed: Option<Box<CanonicalSession>>,
    pub(super) result: Result<(), String>,
}

impl SaveOutcome {
    pub(super) fn new(prepared: &PreparedSave) -> Self {
        Self {
            id: prepared.id.clone(),
            version: prepared.version,
            expected: prepared.expected_storage_digest.clone(),
            primary_sha256: None,
            committed: None,
            result: Ok(()),
        }
    }
}

impl SessionState {
    pub(super) fn note_save_failure(&mut self) {
        self.last_save_failed = true;
        self.stats.unsaved_changes = self
            .stats
            .unsaved_changes
            .max(super::state::BATCH_SAVE_INTERVAL);
    }

    pub(super) fn acknowledge_save(&mut self, outcome: SaveOutcome) -> Result<(), String> {
        if self.id != outcome.id {
            return outcome.result;
        }
        let same_base = self
            .canonical_checkpoint
            .as_ref()
            .and_then(|binding| binding.storage_digest.as_ref())
            == outcome.expected.as_ref();
        if same_base && let Some(committed) = outcome.committed {
            // Includes a primary commit followed by a pointer-write failure.
            // Update only the binding; pending task/findings/etc remain untouched.
            self.canonical_checkpoint = Some(committed);
        }
        if outcome.result.is_err() {
            self.note_save_failure();
        } else if same_base && self.version == outcome.version {
            self.stats.unsaved_changes = 0;
            self.last_save_failed = false;
            self.last_flush = Some(std::time::Instant::now());
        }
        outcome.result
    }

    /// Flush the shared live owner without holding its state lock during disk I/O.
    /// The per-owner lane covers preparation through acknowledgement, not state.
    pub async fn save_shared(owner: Arc<RwLock<Self>>) -> Result<(), String> {
        let gate = owner.read().await.save_gate.clone();
        let lane = tokio::time::timeout(std::time::Duration::from_secs(5), gate.lock()).await;
        let Ok(_lane) = lane else {
            owner.write().await.note_save_failure();
            return Err("session save lane unavailable".into());
        };
        let (id, prepared) = {
            let mut session = owner.write().await;
            if !Arc::ptr_eq(&gate, &session.save_gate) {
                return Ok(()); // The scheduled owner was replaced, not the new session.
            }
            let prepared = session
                .prepare_save()
                .inspect_err(|_| session.note_save_failure())?;
            (session.id.clone(), prepared)
        };
        if let Ok(outcome) =
            tokio::task::spawn_blocking(move || prepared.write_to_disk_with_policy(false)).await
        {
            owner.write().await.acknowledge_save(outcome)
        } else {
            let mut session = owner.write().await;
            if session.id == id {
                session.note_save_failure();
            }
            Err("session save worker unavailable; verify committed state before retrying".into())
        }
    }

    pub(crate) async fn save_shared_logged(owner: Arc<RwLock<Self>>) {
        if let Err(error) = Self::save_shared(owner).await {
            tracing::warn!(%error, "session persistence failed; unsaved state retained");
        }
    }
}
