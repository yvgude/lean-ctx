// SPDX-License-Identifier: Apache-2.0
//! The process-default rustls `CryptoProvider`.
//!
//! The dependency tree enables both rustls providers: aws-lc-rs (reqwest's
//! rustls feature, rustls' defaults) and ring (ureq). With two compiled in,
//! rustls cannot pick a process default, and any TLS client built from the
//! default (tokio-tungstenite's connector: the Hive bus relay, the Codex
//! ChatGPT passthrough) panics with "Could not automatically determine the
//! process-level CryptoProvider". reqwest and the update verifier name
//! aws-lc-rs explicitly; this installs the same one as the default.
//!
//! Called first in `main` (CLI, daemon, MCP server), and again right before
//! each WebSocket connect so library and test callers are covered too.
//! Idempotent: a provider already installed stays.

/// Install aws-lc-rs as the process-default rustls provider, once.
pub fn install_default() {
    #[cfg(feature = "http-server")]
    {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}
