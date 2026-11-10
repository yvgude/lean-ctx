// SPDX-License-Identifier: Apache-2.0
//! The input check's named hook (pz21 §1.9, D7): two thin wrappers over the
//! real release functions, for the two cases no registered tool produces —
//! a bound result that cannot carry an extra text block, and an error whose
//! data carries an untrusted object.
//!
//! They add no capability: called on a given result inside an open call,
//! they can only fence it, taint the session or withhold it, exactly as the
//! release point does for every tool. Outside a call they apply the
//! detector step only. The locked target pins that behaviour; nothing else
//! may be added here.

use rmcp::model::CallToolResult;

/// Release `result` of `tool` through the real release point.
#[doc(hidden)]
#[must_use]
pub fn release_result(tool: &str, result: CallToolResult) -> CallToolResult {
    crate::server::policy_guard::release_result(tool, result)
}

/// Release `error` through the real error channel of the release point.
#[doc(hidden)]
#[must_use]
pub fn release_error(error: rmcp::ErrorData) -> rmcp::ErrorData {
    crate::server::policy_guard::release_error(error)
}
