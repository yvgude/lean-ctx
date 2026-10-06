// SPDX-License-Identifier: Apache-2.0

//! Typed `ContextCheckpointV1` projections.
//!
//! This slice names typed target plans, versions, and explicit semantic loss
//! records over the protocol checkpoint.  Its lineage adapter only joins
//! already verified protocol artifacts; signing/trust, branch/store, merge,
//! runtime/API/CLI, and sync remain downstream owners of their wire formats.

mod adapters;
mod branch_merge;
pub mod lineage;
mod lineage_v3;
mod migration;
mod projections;
mod session_storage;

pub(crate) use session_storage::prepare_commit as prepare_session_checkpoint_commit;
pub(crate) use session_storage::{CanonicalSession, PersonalCheckpointLineageV1};

pub use adapters::*;
pub use branch_merge::*;
pub use lineage::*;
pub(crate) use lineage_v3::{VerifiedContextCheckpointV3, verify_checkpoint_v3};
pub use migration::*;
pub use projections::*;
