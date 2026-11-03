// SPDX-License-Identifier: Apache-2.0

//! The composition root of the `lean-ctx` binary: CLI dispatch, setup and
//! uninstall, doctor and status, the local cockpit (dashboard) and the HTTP
//! server. It sits above the Engine library (`lean_ctx`, package
//! `lean-ctx-engine`), so an edit here re-checks this crate only, not the
//! Engine.

#![recursion_limit = "256"]
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod cli;
pub mod dashboard;
pub mod doctor;
#[cfg(feature = "http-server")]
pub mod http_server;
pub mod proxy_setup;
pub mod setup;
pub mod status;
pub mod token_report;
pub mod uninstall;
