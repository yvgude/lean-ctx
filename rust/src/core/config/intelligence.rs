// SPDX-License-Identifier: Apache-2.0
//! User-global opt-in and independent trust pins for the optional local runtime.

use serde::{Deserialize, Serialize};

/// Never merged from a project file, including trusted workspaces.
/// A public signing key is not a credential; the package cannot provide its pin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntelligenceRuntimeConfig {
    pub enabled: bool,
    pub accept_proprietary: bool,
    pub staging: bool,
    pub root: String,
    pub manifest_sha256: String,
    pub trust_key_hex: String,
    pub channel_url: String,
    pub channel_signature_url: String,
    pub channel_root_key_hex: String,
    /// Explicit user-global handoff to the private verifier, never project input.
    pub license_configuration: String,
    /// Let the planner apply a promoted read-strategy policy instead of only
    /// recording it (shadow). Security and explicit choices still win.
    pub context_policy_apply: bool,
}
