// SPDX-License-Identifier: Apache-2.0
//! The protocol's contract tests as one test binary (target `protocol`): a
//! test is `<old file stem>::<test>`. Add new tests as modules here, not as
//! new tests/*.rs files (`autotests = false`).

mod hive_fixtures;
mod hive_v2_manifest;
mod k2_review_schema;
mod receipts_v2_contract;
