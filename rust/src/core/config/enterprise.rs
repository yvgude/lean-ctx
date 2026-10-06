//! `[enterprise]` — connect a lean-ctx Runtime to a LeanCTX Enterprise Suite.
//!
//! When `gateway_url` is set, the proxy injects `x-leanctx-*` metadata headers
//! on every upstream request so the Suite can attribute context savings,
//! populate the economics ledger, and make routing decisions.

use serde::{Deserialize, Serialize};

/// Enterprise Suite connection configuration (`[enterprise]` in config.toml).
///
/// Example:
/// ```toml
/// [enterprise]
/// gateway_url = "https://api.leanctx.com"
/// instance_token = "lctx_inst_..."
/// instance_id = "runtime-macbook-yves"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EnterpriseConfig {
    /// Base URL of the LeanCTX Enterprise Suite Gateway.
    /// When set, all provider requests are routed through this gateway AND
    /// `x-leanctx-*` metadata headers are injected.
    /// Env override: `LEAN_CTX_ENTERPRISE_GATEWAY_URL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_url: Option<String>,

    /// Bearer token for authenticating this Runtime instance with the Suite.
    /// Env override: `LEAN_CTX_ENTERPRISE_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_token: Option<String>,

    /// Stable identifier for this Runtime instance (e.g. "macbook-yves",
    /// "ci-runner-3"). Sent as `x-leanctx-instance`. Auto-generated from
    /// hostname if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,

    /// Whether to inject `x-leanctx-*` headers even when NOT routing through
    /// the gateway (i.e. when using direct provider upstreams but still wanting
    /// to report metadata to a sidecar collector). Default: headers are only
    /// injected when `gateway_url` is set.
    #[serde(default)]
    pub always_inject_headers: bool,

    /// Disable the enterprise integration without removing the config block.
    #[serde(default)]
    pub disabled: bool,
}

impl EnterpriseConfig {
    /// Resolve effective gateway URL (config → env).
    pub fn effective_gateway_url(&self) -> Option<String> {
        self.effective_gateway_url_owned()
    }

    /// Resolve effective gateway URL with env override (owned).
    pub fn effective_gateway_url_owned(&self) -> Option<String> {
        if self.disabled {
            return None;
        }
        std::env::var("LEAN_CTX_ENTERPRISE_GATEWAY_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.gateway_url.clone())
    }

    /// Resolve the instance token (config → env).
    pub fn effective_token(&self) -> Option<String> {
        if self.disabled {
            return None;
        }
        std::env::var("LEAN_CTX_ENTERPRISE_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.instance_token.clone())
    }

    /// Resolve instance ID (config → hostname fallback).
    pub fn effective_instance_id(&self) -> String {
        if let Some(id) = &self.instance_id {
            return id.clone();
        }
        gethostname::gethostname()
            .into_string()
            .unwrap_or_else(|_| "unknown".to_owned())
    }

    /// Whether header injection is active for the current request.
    pub fn should_inject_headers(&self) -> bool {
        if self.disabled {
            return false;
        }
        self.always_inject_headers || self.gateway_url.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_blocks_everything() {
        let cfg = EnterpriseConfig {
            gateway_url: Some("https://api.leanctx.com".to_owned()),
            instance_token: Some("tok".to_owned()),
            disabled: true,
            ..Default::default()
        };
        assert_eq!(cfg.effective_gateway_url(), None);
        assert_eq!(cfg.effective_token(), None);
        assert!(!cfg.should_inject_headers());
    }

    #[test]
    fn headers_injected_when_gateway_set() {
        let cfg = EnterpriseConfig {
            gateway_url: Some("https://api.leanctx.com".to_owned()),
            ..Default::default()
        };
        assert!(cfg.should_inject_headers());
    }

    #[test]
    fn always_inject_without_gateway() {
        let cfg = EnterpriseConfig {
            always_inject_headers: true,
            ..Default::default()
        };
        assert!(cfg.should_inject_headers());
    }

    #[test]
    fn instance_id_fallback() {
        let cfg = EnterpriseConfig::default();
        let id = cfg.effective_instance_id();
        assert!(!id.is_empty());
    }
}
