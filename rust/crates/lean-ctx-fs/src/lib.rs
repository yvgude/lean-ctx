// SPDX-License-Identifier: Apache-2.0

//! Filesystem and environment foundation for LeanCTX | OSS.
//!
//! Policy-free building blocks the engine composes: crash-atomic writes,
//! bounded advisory file locks, BLAKE3 hashing, the stable store-name hasher, JSONC parsing, home and Codex
//! directory resolution, the managed-configuration carrier readers, the
//! engine's error types, a process liveness probe
//! and the process-wide test environment lock. The engine reaches each module
//! through a facade at its old `core::` path; this crate never depends on the
//! engine.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod atomic_fs;
pub mod confined_write;
pub mod env_lock;
pub mod error;
pub mod file_lock;
pub mod hasher;
pub mod home;
pub mod jsonc;
pub mod managed;
pub mod process;
pub mod stable_hash;
