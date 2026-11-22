// SPDX-License-Identifier: Apache-2.0
//! The protocol's contract tests. src/lib.rs compiles this file as module
//! `protocol` of the unit-test binary, so a test is
//! `protocol::<file stem>::<test>`. Add new tests as modules here, not as new
//! tests/*.rs files (`autotests = false`).

mod cloud_fixtures;
mod cloud_v2_manifest;
mod k2_review_schema;
mod model_v1;
mod receipts_v2_contract;
