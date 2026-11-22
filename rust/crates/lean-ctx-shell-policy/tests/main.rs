// SPDX-License-Identifier: Apache-2.0
//! The shell policy's integration tests as one test binary (target
//! `shell_policy`).
//!
//! Each tests/*.rs file used to be its own binary, and each one linked the
//! whole Engine. They are modules of this binary now, so a test is
//! `<old file stem>::<test>`. Add new integration tests as modules here, not
//! as new tests/*.rs files (`autotests = false`).
//!
//! gate_unregistered needs a process that never registered the policy: its
//! test runs in a process of its own (see fresh_process.rs, shared with the
//! Engine, server and composition-root test binaries).

mod fresh_process;

mod attack_scenarios;
mod gate_unregistered;
