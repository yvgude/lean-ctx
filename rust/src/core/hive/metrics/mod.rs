// SPDX-License-Identifier: Apache-2.0
//! What is left of the retired daily Cockpit metrics for Hive (the 1.x
//! `POST /v1/metrics`, owner decision 2026-10-31): the time-ordered store
//! readers engine metrics v2 build on (`collect`), the Hive use counters
//! they sample ([`hive_use`]) and the reason `hive status` names for the
//! retired upload ([`upload`]).

pub(crate) mod collect;
pub mod hive_use;
pub(crate) mod local_state;
pub mod upload;
