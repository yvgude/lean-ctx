// SPDX-License-Identifier: Apache-2.0

//! Fail-closed conversion adapters between `ContextCheckpointV1` and the
//! deployed legacy wire formats.
//!
//! Each legacy format keeps its own wire owner.  This module never re-implements
//! a legacy encoder: it constructs the deployed types, serializes them through
//! their own `serde` contracts, and re-parses the result with the deployed
//! parser before returning it.  The checkpoint side is always read-only.
//!
//! The adapters start from the typed projection plans in
//! [`super::projections`]: a plan names the source fields a target can hold and
//! the target-owned inputs a caller must supply.  An adapter materializes a
//! target **only** when every caller-supplied input for that target is present
//! and validates; nothing is defaulted, guessed, or back-filled.  Timestamps,
//! policy identities, ledger and package contents, git and ROI state,
//! signatures, keys, and trust are never fabricated.  Where a target cannot be
//! materialized without an owner outside this slice, the adapter returns an
//! explicit typed refusal naming that dependency instead of a plausible-looking
//! payload.
//!
//! Reverse conversion (legacy payload back into a checkpoint) is admitted only
//! under a caller-authenticated anchor checkpoint, and only when the legacy
//! payload carries nothing the checkpoint would silently drop.

use serde_json::Value;

use lean_ctx_protocol::ContextCheckpointV1;

use crate::core::ccp_session_bundle::{
    CcpSessionBundleV1, PolicyIdentityV1 as SessionPolicyIdentityV1,
    ProjectIdentityV1 as SessionProjectIdentityV1, SessionExcerptV1,
};
use crate::core::context_snapshot::types::{
    ContextSnapshotV1, GitAnchorV1, SnapshotLedgerV1, SnapshotLineageV1, SnapshotProjectV1,
    SnapshotRoiV1, SnapshotSessionV1,
};
use crate::core::handoff_ledger::{
    HandoffLedgerV1, KnowledgeExcerpt, SessionExcerpt as HandoffSessionExcerpt, ToolCallsSummary,
};
use crate::core::handoff_transfer_bundle::{
    ArtifactsExcerptV1, HandoffTransferBundleV1, ProjectIdentityV1 as HandoffProjectIdentityV1,
};
use crate::core::session::{Decision, Finding, ProgressEntry, SessionStats, TaskInfo};

use super::projections::{
    ContextCheckpointProjectionErrorV1, ContextCheckpointProjectionInputV1,
    ContextCheckpointProjectionLossReasonV1, ContextCheckpointProjectionLossV1,
    ContextCheckpointProjectionResultV1, ContextCheckpointProjectionTargetV1,
    ContextCheckpointProjections,
};
use crate::core::contracts::{
    CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION, CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
    CONTEXT_PACKAGE_V2_SCHEMA_VERSION, CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION,
    HANDOFF_LEDGER_V1_SCHEMA_VERSION, HANDOFF_TRANSFER_BUNDLE_V1_SCHEMA_VERSION,
};

/// Schema version of this adapter request/result contract.
pub const CONTEXT_CHECKPOINT_ADAPTER_SCHEMA_VERSION: u32 = 1;

/// Maximum encoded legacy payload this slice will emit or accept.
pub const MAX_CONTEXT_CHECKPOINT_LEGACY_PAYLOAD_BYTES: usize = 350_000;

/// Maximum encoded `.ctxpkg` carrier envelope this slice will inspect.
pub const MAX_CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_BYTES: usize = 262_144;

/// Byte length of a lowercase hexadecimal content hash from the deployed hasher.
const LEGACY_HASH_HEX_LENGTH: usize = 64;

/// Maximum byte length of a bounded legacy label.
const MAX_LEGACY_LABEL_BYTES: usize = 64;

/// Deployed producer cap on Session Bundle `next_steps`.
const SESSION_BUNDLE_MAX_NEXT_STEPS: usize = 25;

/// Deployed producer cap on Handoff ledger decisions.
const HANDOFF_LEDGER_MAX_DECISIONS: usize = 10;

/// Deployed producer cap on Handoff ledger findings and next steps.
const HANDOFF_LEDGER_MAX_LIST: usize = 20;

/// Deployed cap on the Context Snapshot session slice lists.
const SNAPSHOT_MAX_SESSION_LIST: usize = 64;

/// Deployed producer cap on a serialized Session Bundle.
const SESSION_BUNDLE_MAX_PAYLOAD_BYTES: usize = 250_000;
const MAX_CONTEXT_CHECKPOINT_LEGACY_LOSSES: usize = 128;
const MAX_CONTEXT_CHECKPOINT_REQUIRED_INPUTS: usize = 16;

macro_rules! validated_deserialize {
    ($target:ident { $($field:ident : $field_type:ty),+ $(,)? }) => {
        impl<'de> Deserialize<'de> for $target {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Wire {
                    $($field: $field_type),+
                }

                let Wire { $($field),+ } = Wire::deserialize(deserializer)?;
                let value = Self { $($field),+ };
                value.validate().map_err(|error| {
                    DeError::custom(format!("{error:?}"))
                })?;
                Ok(value)
            }
        }
    };
}

mod adoption;
mod context_snapshot;
mod contract;
mod ctxpkg;
mod forward;
mod handoff_bundle;
mod inputs;
mod session_bundle;
#[cfg(test)]
mod tests;
mod validation;
mod wire;

pub use adoption::*;
pub use contract::*;
pub use forward::*;
pub use inputs::*;

use context_snapshot::materialize_context_snapshot;
use ctxpkg::materialize_ctxpkg;
use forward::{refusal, required_inputs_from_plan, residual_losses};
use handoff_bundle::materialize_handoff_bundle;
use session_bundle::materialize_session_bundle;
use validation::{
    projection_input_field_path, projection_input_target, require_arity, require_capacity,
    validate_optional_label, validate_project_identity, validate_sorted_unique_timestamps,
    validate_timestamp,
};
use wire::{
    chrono_timestamp, compression_rate, encode_owner, parse_canonical_json, parse_json_object,
    payload_from_value, progress_percent, source_identity, validate_carrier_envelope,
    validate_legacy_target_value,
};
