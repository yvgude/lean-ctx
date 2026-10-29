// SPDX-License-Identifier: Apache-2.0
//! C0 2.0 events on the engine (Hive v4 V4.1, SPEC-ENGINE §3.3): ids,
//! the hybrid logical clock, signing and verification, and building events
//! from what the capture gate let through. Worker and alias keys come from
//! `core/hive/workers` (ENGINE-2).

pub mod build;
pub mod hlc;
pub mod ids;
pub mod sealed_body;
pub mod sign;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "code_anchor_vectors_tests.rs"]
mod code_anchor_vectors;
