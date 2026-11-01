// SPDX-License-Identifier: Apache-2.0
//! Adaptive compression policy (OSS stub).
//!
//! The enterprise build replaces this with outcome-trained policies that learn
//! from Value Gate feedback. In OSS mode, a static default policy is used.

use serde::{Deserialize, Serialize};

/// Compression tuning knobs applied per-request based on task classification.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompressionPolicy {
    pub compress_tool_output: bool,
    pub log_compression_level: u8,
    pub code_preserve: bool,
}

impl Default for CompressionPolicy {
    fn default() -> Self {
        Self {
            compress_tool_output: true,
            log_compression_level: 2,
            code_preserve: true,
        }
    }
}

/// Returns the best compression policy for the given task class.
/// OSS: always returns the static default.
pub fn best_policy_for(_task_class: &str) -> CompressionPolicy {
    CompressionPolicy::default()
}

/// Selects the optimal policy (OSS: returns default).
pub fn select_policy(_task_class: &str) -> CompressionPolicy {
    CompressionPolicy::default()
}

/// Records a Value Gate outcome for policy feedback learning.
/// OSS: no-op (no feedback loop without enterprise).
pub fn record_value_gate_outcome(
    _task_class: String,
    _policy: CompressionPolicy,
    _accepted: bool,
    _savings_pct: f64,
) {
}

/// Recorded policy outcome for feedback learning.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PolicyOutcome {
    pub task_class: String,
    pub policy_used: CompressionPolicy,
    pub session_success: Option<bool>,
    pub savings_pct: f64,
}
