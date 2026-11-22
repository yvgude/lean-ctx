// SPDX-License-Identifier: Apache-2.0
//! Local inputs of engine metrics v2: the time-ordered store readers they
//! build on (`collect`) and the Cloud use counters they sample
//! ([`cloud_use`]).

pub mod cloud_use;
pub(crate) mod collect;
