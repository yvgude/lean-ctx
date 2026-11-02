// SPDX-License-Identifier: Apache-2.0
//! Local inputs of engine metrics v2: the time-ordered store readers they
//! build on (`collect`) and the Hive use counters they sample
//! ([`hive_use`]).

pub(crate) mod collect;
pub mod hive_use;
