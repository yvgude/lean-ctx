// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud gateway key --json`: a short-lived key for running
//! sessions on models from LeanCTX, paid from the team's balance, for a local
//! reader such as LeanCTX Code (`POST /v1/gateway/key`).
//!
//! The key reaches only the reader on stdout; it is never written to disk,
//! cached or logged. A used-up balance, a full set of live keys, a refusal
//! and a cloud without the gateway are answers (`state`), not errors, so the
//! reader can show the right next step.

use serde::Serialize;

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::config::CloudConfig;
use lean_ctx_cloud::gateway::{GatewayAnswer, GatewayKey};

use super::Failure;

/// The local-read kind of this document.
pub(crate) const KIND: &str = "cloud.gateway.key";

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct GatewayDoc {
    /// `false`: the cloud does not offer the gateway (not configured or older).
    pub available: bool,
    /// `ready`, `stopped`, `too_many_keys`, `forbidden` or `not_configured`.
    pub state: &'static str,
    /// The key; only when `ready`.
    pub gateway: Option<GatewayKey>,
    /// `top_up` when a top-up helps a stopped balance.
    pub next_step: Option<&'static str>,
    /// Seconds after which asking again may help (`stopped` and
    /// `too_many_keys` only).
    pub retry_after_s: Option<u64>,
}

pub(crate) fn build(answer: GatewayAnswer) -> GatewayDoc {
    let doc = |available, state| GatewayDoc {
        available,
        state,
        gateway: None,
        next_step: None,
        retry_after_s: None,
    };
    match answer {
        GatewayAnswer::Ready(key) => GatewayDoc {
            gateway: Some(key),
            ..doc(true, "ready")
        },
        GatewayAnswer::Stopped {
            top_up,
            retry_after_s,
        } => GatewayDoc {
            next_step: top_up.then_some("top_up"),
            retry_after_s,
            ..doc(true, "stopped")
        },
        GatewayAnswer::TooManyKeys { retry_after_s } => GatewayDoc {
            retry_after_s,
            ..doc(true, "too_many_keys")
        },
        GatewayAnswer::Forbidden => doc(true, "forbidden"),
        GatewayAnswer::NotConfigured => doc(false, "not_configured"),
    }
}

pub(crate) fn run(config: &CloudConfig) {
    super::emit(KIND, || {
        let store = CloudStore::open().map_err(Failure::from)?;
        let answer = lean_ctx_cloud::gateway::request(&store, config).map_err(Failure::from)?;
        Ok(build(answer))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_key_reaches_the_reader() {
        let doc = serde_json::to_value(build(GatewayAnswer::Ready(GatewayKey {
            base_url: "https://gateway.example/v1".into(),
            key: "gk_abc123".into(),
            expires_at: "2026-11-18T13:00:00Z".into(),
        })))
        .expect("json");
        assert_eq!(doc["available"], true);
        assert_eq!(doc["state"], "ready");
        assert_eq!(doc["gateway"]["key"], "gk_abc123");
        assert_eq!(doc["gateway"]["expires_at"], "2026-11-18T13:00:00Z");
        assert!(doc["next_step"].is_null());
    }

    #[test]
    fn a_stopped_balance_names_the_top_up() {
        let doc = serde_json::to_value(build(GatewayAnswer::Stopped {
            top_up: true,
            retry_after_s: Some(60),
        }))
        .expect("json");
        assert_eq!(doc["state"], "stopped");
        assert_eq!(doc["next_step"], "top_up");
        assert_eq!(doc["retry_after_s"], 60);
        assert!(doc["gateway"].is_null());
    }

    #[test]
    fn a_full_set_of_keys_is_not_a_stopped_balance() {
        let doc = serde_json::to_value(build(GatewayAnswer::TooManyKeys {
            retry_after_s: Some(30),
        }))
        .expect("json");
        assert_eq!(
            doc,
            serde_json::json!({
                "available": true,
                "state": "too_many_keys",
                "gateway": null,
                "next_step": null,
                "retry_after_s": 30
            })
        );
        let doc = serde_json::to_value(build(GatewayAnswer::TooManyKeys {
            retry_after_s: None,
        }))
        .expect("json");
        assert_eq!(doc["state"], "too_many_keys");
        assert!(doc["next_step"].is_null());
        assert!(doc["retry_after_s"].is_null());
    }

    #[test]
    fn a_cloud_without_the_gateway_reads_as_not_available() {
        let doc = serde_json::to_value(build(GatewayAnswer::NotConfigured)).expect("json");
        assert_eq!(doc["available"], false);
        assert_eq!(doc["state"], "not_configured");
        let doc = serde_json::to_value(build(GatewayAnswer::Forbidden)).expect("json");
        assert_eq!(doc["available"], true);
        assert_eq!(doc["state"], "forbidden");
    }

    #[test]
    fn the_gateway_key_is_listed_in_capabilities() {
        assert!(super::super::capabilities::KINDS.contains(&KIND));
        assert!(
            super::super::capabilities::COMMANDS
                .iter()
                .any(|(group, verbs)| *group == "cloud.gateway" && verbs.contains(&"key"))
        );
    }
}
