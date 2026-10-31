// SPDX-License-Identifier: Apache-2.0

//! Verified context-checkpoint lineage and canonical session storage.
//!
//! The lineage adapter only joins already verified protocol artifacts;
//! signing/trust, store, runtime/API/CLI and sync remain downstream owners of
//! their wire formats.

pub mod lineage;
mod lineage_v3;
mod session_storage;

pub(crate) use session_storage::CanonicalSession;
pub(crate) use session_storage::prepare_commit as prepare_session_checkpoint_commit;

pub use lineage::*;
pub(crate) use lineage_v3::{VerifiedContextCheckpointV3, verify_checkpoint_v3};
