// SPDX-License-Identifier: Apache-2.0

//! Outcome contracts, local signal adapters, and deterministic evaluation.
//!
//! The model lives in the `lean-ctx-workgraph` crate and is re-exported here
//! unchanged, so engine paths such as `crate::core::outcome::contracts` stay
//! stable.

pub use lean_ctx_workgraph::outcome::*;
