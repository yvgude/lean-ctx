//! Context Snapshot (`CONTEXT_SNAPSHOT_V1`) — the git-anchored, signed, temporal
//! artifact behind the Context Time Machine (GL epic #1022, Phase 0 #1023).
//!
//! A snapshot is a distilled, typed, signed projection of the live context
//! stores at a point in time:
//!
//! - **git anchor** — commit / branch / dirty state ([`GitAnchorV1`])
//! - **lineage** — what entered the window, from Context IR ([`SnapshotLineageV1`])
//! - **ledger** — why it was there, with Φ-scores ([`SnapshotLedgerV1`])
//! - **ROI** — token savings at that moment ([`SnapshotRoiV1`])
//! - **session** — the task/decisions behind it ([`SnapshotSessionV1`])
//!
//! Snapshots chain via [`ContextSnapshotV1::parent_id`] into an append-only
//! timeline. The id is content-addressed (BLAKE3 of the canonical body, see
//! [`digest`]) and the signature (ed25519, see [`signing`]) is computed over it.
//!
//! This module is the **contract** (Phase 0): the types, the deterministic
//! id/signing semantics, and their tests. The builder that fills snapshots from
//! live stores and the append-only timeline index land in Phase 1 (#1024).

pub mod builder;
pub mod digest;
pub mod publish;
pub mod restore;
pub mod signing;
pub mod timeline;
pub mod types;

pub use builder::{SnapshotOptions, build, create};
pub use digest::{canonical_body, compute_id, finalize_id};
pub use publish::{ImportOutcome, PublishOptions, PublishOutcome, import, publish};
pub use restore::{GitRestore, RestoreOptions, RestoreOutcome, SessionMerge, restore};
pub use signing::{sign_snapshot, verify_snapshot};
pub use timeline::{
    TimelineEntry, head_id, load_entries, read_snapshot, resolve_id, snapshots_dir, write_snapshot,
};
pub use types::{
    ContextSnapshotV1, GitAnchorV1, MAX_SNAPSHOT_LEDGER_ITEMS, MAX_SNAPSHOT_LINEAGE_ITEMS,
    MAX_SNAPSHOT_SESSION_LIST, SnapshotLedgerItemV1, SnapshotLedgerV1, SnapshotLineageItemV1,
    SnapshotLineageV1, SnapshotProjectV1, SnapshotRoiV1, SnapshotSessionV1, SnapshotSignatureV1,
};

#[cfg(test)]
mod tests {
    use super::types::*;
    use crate::core::contracts::CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION;

    /// A fully-populated snapshot exercising every field for roundtrip coverage.
    fn full_snapshot() -> ContextSnapshotV1 {
        ContextSnapshotV1 {
            schema_version: CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
            snapshot_id: "id".repeat(32),
            parent_id: Some("p".repeat(64)),
            created_at: "2026-06-28T12:00:00Z".into(),
            lean_ctx_version: "3.9.0".into(),
            git: GitAnchorV1 {
                commit: Some("abc1234".into()),
                branch: Some("feat/context-time-machine".into()),
                dirty: true,
            },
            project: SnapshotProjectV1 {
                root_hash: Some("r".repeat(64)),
                identity_hash: Some("i".repeat(64)),
            },
            roi: SnapshotRoiV1 {
                input_tokens: 1000,
                output_tokens: 200,
                tokens_saved: 800,
                compression_rate: 0.444_44,
            },
            lineage: SnapshotLineageV1 {
                items_recorded: 42,
                items: vec![SnapshotLineageItemV1 {
                    seq: 1,
                    kind: "read".into(),
                    tool: "ctx_read".into(),
                    path: Some("src/main.rs".into()),
                    input_tokens: 500,
                    output_tokens: 13,
                    compression_ratio: 0.974,
                    content_hash: Some("c".repeat(64)),
                }],
            },
            ledger: SnapshotLedgerV1 {
                window_size: 8000,
                total_tokens_sent: 1200,
                total_tokens_saved: 800,
                items: vec![SnapshotLedgerItemV1 {
                    path: "src/main.rs".into(),
                    state: "pinned".into(),
                    phi: Some(0.91),
                    sent_tokens: 13,
                    original_tokens: 500,
                }],
            },
            session: Some(SnapshotSessionV1 {
                session_id: Some("sess-1".into()),
                task: Some("Implement Context Time Machine".into()),
                decisions: vec!["Use ed25519 for snapshot signing".into()],
                files_touched: vec!["rust/src/core/context_snapshot/mod.rs".into()],
                progress_pct: Some(20),
            }),
            signature: Some(SnapshotSignatureV1 {
                algorithm: "ed25519".into(),
                public_key: "a".repeat(64),
                value: "b".repeat(128),
            }),
        }
    }

    #[test]
    fn serde_roundtrip_preserves_every_field() {
        let original = full_snapshot();
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: ContextSnapshotV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn new_uses_current_schema_version_and_empty_slices() {
        let snap = ContextSnapshotV1::new("2026-06-28T12:00:00Z".into(), "3.9.0".into());
        assert_eq!(snap.schema_version, CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION);
        assert!(snap.snapshot_id.is_empty());
        assert!(snap.signature.is_none());
        assert!(snap.lineage.items.is_empty());
        assert!(snap.ledger.items.is_empty());
    }

    #[test]
    fn sign_then_serde_roundtrip_still_verifies() {
        use ed25519_dalek::SigningKey;
        let mut snap = ContextSnapshotV1::new("2026-06-28T12:00:00Z".into(), "3.9.0".into());
        super::sign_snapshot(&mut snap, &SigningKey::from_bytes(&[9u8; 32])).expect("sign");

        let json = serde_json::to_string(&snap).expect("serialize");
        let restored: ContextSnapshotV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snap, restored);
        assert!(super::verify_snapshot(&restored).expect("verify"));
    }
}
