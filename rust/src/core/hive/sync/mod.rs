// SPDX-License-Identifier: Apache-2.0
//! Engine sync with a place (Hive v4 V4, SPEC-ENGINE §3.4.3–§3.4.4):
//! verification of snapshot and tail records, the push of own events, and
//! the driver that clones, tails and pushes a replica.

pub mod driver;
pub mod place;
pub mod purge;
pub mod push;
pub mod records;
pub mod swarm;
pub mod writer;
