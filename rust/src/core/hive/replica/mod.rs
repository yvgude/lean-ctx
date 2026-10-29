// SPDX-License-Identifier: Apache-2.0
//! The engine's offline replica of a place (Hive v4 V4, SPEC-ENGINE §3.4):
//! an encrypted SQLite store per place, the cursor into the server log, the
//! queue of own events not yet in that log. Folding events and the sync
//! routes follow the C0 2.0 wire shapes.

pub mod converted;
pub mod crypt;
pub mod fold;
pub mod history;
pub mod log;
pub mod store;

pub use store::{Opened, Replica};
