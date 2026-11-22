// SPDX-License-Identifier: Apache-2.0
//! LeanCTX | OSS apps: engine modules that nothing in the engine uses at
//! runtime, split upward out of the Engine (crate split step 1).
//!
//! The moved modules keep their engine paths: the crate root glob-imports the
//! engine and `core` re-exports `lean_ctx::core::*` next to the moved modules,
//! so `crate::core::X`, `crate::test_env` and `super::X` resolve unchanged and
//! each move stays a pure rename.

#[allow(unused_imports)]
use lean_ctx::*;

pub mod core {
    pub use lean_ctx::core::*;

    pub mod dashboard_pro;
}
