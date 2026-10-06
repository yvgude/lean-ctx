// SPDX-License-Identifier: Apache-2.0

//! Safe, deliberately lossy projection from live continuation state into a
//! point-in-time Context Snapshot.
//!
//! The two contracts are not aliases: checkpoint source identities are not
//! filesystem paths, and checkpoint references are not snapshot lineage
//! events. Those fields therefore remain empty instead of being mislabeled.

use lean_ctx_protocol::context_checkpoint::{ContextCheckpointProjectionV1, ContextCheckpointV1};
use sha2::{Digest, Sha256};

use super::context_snapshot::{ContextSnapshotV1, types::SnapshotSessionV1};

/// Projects only semantics shared safely by both version-one contracts.
pub(crate) struct ContextSnapshotProjectionV1;

impl ContextCheckpointProjectionV1 for ContextSnapshotProjectionV1 {
    type Projection = ContextSnapshotV1;
    type Error = String;

    fn project(&self, checkpoint: &ContextCheckpointV1) -> Result<ContextSnapshotV1, String> {
        checkpoint.validate().map_err(|error| error.to_string())?;

        let mut snapshot = ContextSnapshotV1::new(
            checkpoint.created_at.as_str().to_owned(),
            checkpoint.engine_version.as_str().to_owned(),
        );
        snapshot.parent_id = checkpoint
            .identity
            .parent_checkpoint_id
            .as_ref()
            .map(|id| id.as_str().to_owned());
        snapshot.project.identity_hash = Some(scoped_project_identity(checkpoint));
        snapshot.session = Some(SnapshotSessionV1 {
            session_id: checkpoint
                .live_state
                .session_state
                .as_ref()
                .map(|session| session.identity.session_id.as_str().to_owned()),
            task: Some(checkpoint.live_state.task.title.as_str().to_owned()),
            decisions: checkpoint
                .live_state
                .decisions
                .iter()
                .map(|decision| decision.statement.as_str().to_owned())
                .collect(),
            // A checkpoint source_id is intentionally not a machine path.
            files_touched: Vec::new(),
            progress_pct: Some(progress_percent(
                checkpoint.live_state.progress.completed_steps,
                checkpoint.live_state.progress.total_steps,
            )),
        });
        Ok(snapshot)
    }
}

fn scoped_project_identity(checkpoint: &ContextCheckpointV1) -> String {
    let mut digest = Sha256::new();
    digest.update(b"leanctx/context-checkpoint/snapshot-project/v1\0");
    for component in [
        checkpoint.lineage.tenant_id.as_str(),
        checkpoint.lineage.workspace_id.as_str(),
        checkpoint.lineage.project_id.as_str(),
    ] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn progress_percent(completed: u32, total: u32) -> u8 {
    // Domain validation guarantees total > 0 and completed <= total.
    ((u64::from(completed) * 100) / u64::from(total)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_projection_is_bounded_and_deterministic() {
        assert_eq!(progress_percent(0, 3), 0);
        assert_eq!(progress_percent(1, 3), 33);
        assert_eq!(progress_percent(3, 3), 100);
        assert_eq!(progress_percent(u32::MAX, u32::MAX), 100);
    }
}
