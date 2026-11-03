// SPDX-License-Identifier: Apache-2.0
#![recursion_limit = "256"]
#![warn(clippy::undocumented_unsafe_blocks)]

//! Agent hook handlers and hook installers, above the Engine library
//! (`lean_ctx`): an edit here re-checks this crate and the crates above it,
//! not the Engine.

pub mod hook_handlers;
pub mod hooks;
