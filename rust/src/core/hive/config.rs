// SPDX-License-Identifier: Apache-2.0
//! `[hive]` configuration.
//!
//! ```toml
//! [hive]
//! # enabled = true          # unset: on while this machine is connected
//! url = "https://hive.leanctx.com"
//! # root_key = "<base64>"   # self-hosted Hive: its root public key
//! auto_capture = true       # 3.11.5 name `auto_draft` is read as an alias
//! pull_interval_minutes = 5  # 1..=60
//! metrics_upload = true
//! receipts_upload = true
//! bus = "auto"              # auto | off
//! vault_labels = true
//!
//! [hive.worker]             # stricter local caps on the opt-in budget
//! # max_tasks_per_day = 3
//! # max_tokens_per_day = 200000
//! # max_usd_micros_per_day = 2000000
//! ```

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use super::HiveError;
use lean_ctx_protocol::hive::HIVE_DEFAULT_URL;

/// Root public keys of the hosted Hive (`hive.leanctx.com`), base64
/// Ed25519, the one that signs today first (C0 `root_keys.vectors.json`
/// `hosted`, RESOLUTIONS R55). Any other Hive needs `[hive] root_key`.
pub const HOSTED_ROOT_KEYS: &[&str] = &["gu/tH0HqqDVcjm+fbHoS05NodwN7eWqQ5N4qAxq+cV8="];

/// Hosted roots that no longer sign a key set (`retired`): a set signed
/// only by one of them is refused as `retired_root`.
pub const RETIRED_ROOT_KEYS: &[&str] = &[];

/// Key id (`hex(blake3(pk))[..16]`) of the hosted root while the offline
/// rotation V15.O9 is open (`pre_rotation`); `None` once it is done.
pub const PRE_ROTATION_ROOT_KEY_ID: Option<&str> = Some("b5406e1db468082e");

/// The compiled keys of `list`, decoded (a malformed entry fails the
/// vector test, never a connect).
pub fn compiled_root_keys(list: &[&str]) -> Vec<[u8; 32]> {
    list.iter()
        .filter_map(|k| decode_key(k, "root_key").ok())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveBusMode {
    #[default]
    Auto,
    Off,
}

/// How this engine grants a sealed vault key to a newly approved recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveSealedGrants {
    /// Show the recipient's fingerprint and grant only after the member
    /// confirmed it (the only guard against a dishonest Hive operator).
    #[default]
    Confirm,
    /// Grant to every recipient Hive lists (trusts Hive with the list).
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveConfig {
    /// `None`: on while this machine is connected. `false` pauses all Hive
    /// traffic without disconnecting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Hive origin; any `https` URL for a self-hosted Hive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Root public key (base64 Ed25519) of a self-hosted Hive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_key: Option<String>,
    /// Run the capture gate on session learnings (3.11.5: `auto_draft`,
    /// still read as an alias, `core::config::renamed`).
    pub auto_capture: bool,
    /// Minutes between scheduler passes; [`HiveConfig::pull_interval`]
    /// clamps it to [`PULL_INTERVAL_BOUNDS`].
    pub pull_interval_minutes: u32,
    pub metrics_upload: bool,
    pub receipts_upload: bool,
    pub bus: HiveBusMode,
    pub vault_labels: bool,
    /// The user agreed to install the proprietary Intelligence runtime that
    /// the lease licenses (`hive connect` asks, `hive runtime install`
    /// records it). Nothing installs it without this consent.
    pub runtime_consent: bool,
    /// Sealed repos: `confirm` (default) or `auto`.
    pub sealed_grants: HiveSealedGrants,
    /// Worker mode: local caps that only tighten the server opt-in budget.
    pub worker: super::worker::HiveWorkerConfig,
}

impl Default for HiveConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            url: None,
            root_key: None,
            auto_capture: true,
            pull_interval_minutes: DEFAULT_PULL_INTERVAL_MINUTES,
            metrics_upload: true,
            receipts_upload: true,
            bus: HiveBusMode::Auto,
            vault_labels: true,
            runtime_consent: false,
            sealed_grants: HiveSealedGrants::Confirm,
            worker: super::worker::HiveWorkerConfig::default(),
        }
    }
}

/// Default minutes between scheduler passes (SPEC-ENGINE §3.16).
pub const DEFAULT_PULL_INTERVAL_MINUTES: u32 = 5;
/// Allowed `pull_interval_minutes` (SPEC-ENGINE §3.16).
pub const PULL_INTERVAL_BOUNDS: std::ops::RangeInclusive<u32> = 1..=60;

impl HiveConfig {
    /// `pull_interval_minutes` within [`PULL_INTERVAL_BOUNDS`].
    pub fn pull_interval(&self) -> u32 {
        self.pull_interval_minutes
            .clamp(*PULL_INTERVAL_BOUNDS.start(), *PULL_INTERVAL_BOUNDS.end())
    }

    /// The configured origin, normalized and checked.
    pub fn origin(&self) -> Result<String, HiveError> {
        normalize_origin(self.url.as_deref().unwrap_or(HIVE_DEFAULT_URL))
    }

    /// The root key that signs this Hive' key set.
    pub fn root_key(&self, origin: &str) -> Result<[u8; 32], HiveError> {
        let encoded = match (&self.root_key, origin == HIVE_DEFAULT_URL) {
            (Some(key), _) => key.as_str(),
            (None, true) => HOSTED_ROOT_KEYS.first().copied().ok_or_else(|| {
                HiveError::Config(
                    "this build has no hosted Hive root key; set `[hive] root_key`".into(),
                )
            })?,
            (None, false) => {
                return Err(HiveError::Config(format!(
                    "set `[hive] root_key` to the root public key of {origin}"
                )));
            }
        };
        decode_key(encoded.trim(), "root_key")
    }
}

/// `[hive]` in `lean-ctx config schema` / `config validate`.
pub fn schema_section() -> crate::core::config::schema::SectionSchema {
    use crate::core::config::schema::{KeySchema, SectionSchema};
    let defaults = HiveConfig::default();
    let key = |ty: &str, default: serde_json::Value, description: &str, values: Option<&[&str]>| {
        KeySchema {
            ty: ty.into(),
            default,
            description: description.into(),
            values: values.map(|v| v.iter().map(|s| (*s).to_string()).collect()),
            env_override: None,
        }
    };
    let keys = [
        (
            "enabled",
            key(
                "bool",
                serde_json::Value::Null,
                "Pause all Hive traffic with false; unset means on while this machine is connected.",
                None,
            ),
        ),
        (
            "url",
            key(
                "string",
                serde_json::json!(HIVE_DEFAULT_URL),
                "Hive origin (https; a self-hosted Hive may use any https URL).",
                None,
            ),
        ),
        (
            "root_key",
            key(
                "string",
                serde_json::Value::Null,
                "Root public key (base64 Ed25519) of a self-hosted Hive.",
                None,
            ),
        ),
        (
            "auto_capture",
            key(
                "bool",
                serde_json::json!(defaults.auto_capture),
                "Run the capture gate on session learnings and share what passes (renamed from auto_draft).",
                None,
            ),
        ),
        (
            "pull_interval_minutes",
            key(
                "integer",
                serde_json::json!(defaults.pull_interval_minutes),
                "Minutes between Hive sync passes (1-60; values outside are clamped).",
                None,
            ),
        ),
        (
            "metrics_upload",
            key(
                "bool",
                serde_json::json!(defaults.metrics_upload),
                "Send hourly engine metrics to Hive (counts only, including full re-fetches and unregulated program names; never content).",
                None,
            ),
        ),
        (
            "receipts_upload",
            key(
                "bool",
                serde_json::json!(defaults.receipts_upload),
                "Upload context receipts to Hive.",
                None,
            ),
        ),
        (
            "bus",
            key(
                "string",
                serde_json::json!("auto"),
                "Relay the agent bus through Hive (auto) or keep it local (off).",
                Some(&["auto", "off"]),
            ),
        ),
        (
            "vault_labels",
            key(
                "bool",
                serde_json::json!(defaults.vault_labels),
                "Send checkout folder names as vault labels.",
                None,
            ),
        ),
        (
            "runtime_consent",
            key(
                "bool",
                serde_json::json!(defaults.runtime_consent),
                "You agreed to install the proprietary Intelligence runtime your plan licenses (set by `hive connect` or `hive runtime install`).",
                None,
            ),
        ),
        (
            "sealed_grants",
            key(
                "string",
                serde_json::json!("confirm"),
                "Sealed vaults: grant the vault key to a newly approved instance only after you confirmed its fingerprint (confirm), or to every instance Hive lists (auto).",
                Some(&["confirm", "auto"]),
            ),
        ),
        (
            "worker.max_tasks_per_day",
            key(
                "integer",
                serde_json::Value::Null,
                "Worker mode: at most this many claimed tasks per day on this machine; only tightens the opt-in budget set on the web.",
                None,
            ),
        ),
        (
            "worker.max_tokens_per_day",
            key(
                "integer",
                serde_json::Value::Null,
                "Worker mode: stop claiming once the day's results reported this many tokens; only tightens the opt-in budget.",
                None,
            ),
        ),
        (
            "worker.max_usd_micros_per_day",
            key(
                "integer",
                serde_json::Value::Null,
                "Worker mode: stop claiming once the day's results reported this cost (USD millionths); only tightens the opt-in budget.",
                None,
            ),
        ),
    ];
    SectionSchema {
        description: "LeanCTX Hive: workspace knowledge, policies and the shared agent bus.".into(),
        keys: keys
            .into_iter()
            .map(|(name, key)| (name.to_string(), key))
            .collect(),
    }
}

/// Decode a base64 Ed25519 public key.
pub(crate) fn decode_key(encoded: &str, what: &str) -> Result<[u8; 32], HiveError> {
    B64.decode(encoded)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| HiveError::Config(format!("{what} must be base64 of 32 bytes")))
}

/// `https://host[:port]` (or `http://localhost|127.0.0.1[:port]`), no path,
/// query, fragment or credentials, no trailing slash.
pub fn normalize_origin(url: &str) -> Result<String, HiveError> {
    let url = url.trim().trim_end_matches('/');
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| HiveError::Config(format!("{url:?} is not a URL")))?;
    if rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
        || rest.chars().any(char::is_whitespace)
    {
        return Err(HiveError::Config(
            "the Hive URL is an origin only: scheme, host and optional port".into(),
        ));
    }
    let host = rest.rsplit_once(':').map_or(rest, |(host, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            host
        } else {
            rest
        }
    });
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(format!("https://{rest}")),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => {
            Ok(format!("http://{rest}"))
        }
        _ => Err(HiveError::Config(
            "Hive is reached over https (http only on localhost)".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_https_or_loopback_http() {
        assert_eq!(
            normalize_origin("https://hive.example.com/").unwrap(),
            "https://hive.example.com"
        );
        assert_eq!(
            normalize_origin("http://127.0.0.1:8100").unwrap(),
            "http://127.0.0.1:8100"
        );
        assert!(normalize_origin("http://hive.example.com").is_err());
        assert!(normalize_origin("http://localhost.evil.com").is_err());
        assert!(normalize_origin("https://user:pw@hive.example.com").is_err());
        assert!(normalize_origin("https://hive.example.com/v1").is_err());
        assert!(normalize_origin("ftp://hive.example.com").is_err());
    }

    #[test]
    fn self_hosted_hive_needs_its_root_key() {
        let config = HiveConfig {
            url: Some("https://hive.acme.internal".into()),
            ..HiveConfig::default()
        };
        let origin = config.origin().unwrap();
        assert!(config.root_key(&origin).is_err());
        let config = HiveConfig {
            root_key: Some(B64.encode([7u8; 32])),
            ..config
        };
        assert_eq!(config.root_key(&origin).unwrap(), [7u8; 32]);
    }

    #[test]
    fn compiled_roots_match_the_contract_vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/root_keys.vectors.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let listed = |name: &str| -> Vec<(String, String)> {
            v[name]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let (id, pk) = (&e["key_id"], &e["public_key"]);
                    (id.as_str().unwrap().into(), pk.as_str().unwrap().into())
                })
                .collect()
        };
        let compiled = |list: &[&str]| -> Vec<(String, String)> {
            let keys = compiled_root_keys(list);
            assert_eq!(keys.len(), list.len(), "every compiled root decodes");
            keys.iter()
                .map(|k| {
                    let id = blake3::hash(k).to_hex()[..16].to_string();
                    (id, B64.encode(k))
                })
                .collect()
        };
        assert!(!HOSTED_ROOT_KEYS.is_empty(), "3.12.0 refuses an empty list");
        assert_eq!(compiled(HOSTED_ROOT_KEYS), listed("hosted"));
        assert_eq!(compiled(RETIRED_ROOT_KEYS), listed("retired"));
        assert_eq!(PRE_ROTATION_ROOT_KEY_ID, v["pre_rotation"].as_str());
        let origin = HiveConfig::default().origin().unwrap();
        assert_eq!(
            HiveConfig::default().root_key(&origin).unwrap(),
            compiled_root_keys(HOSTED_ROOT_KEYS)[0]
        );
    }

    #[test]
    fn defaults_match_the_documented_section() {
        let parsed: HiveConfig = toml::from_str("").unwrap();
        assert_eq!(parsed, HiveConfig::default());
        assert_eq!(parsed.pull_interval_minutes, 5);
        assert!(parsed.auto_capture);
        assert_eq!(parsed.bus, HiveBusMode::Auto);
    }

    #[test]
    fn the_pull_interval_is_clamped_to_its_bounds() {
        let at = |minutes| {
            HiveConfig {
                pull_interval_minutes: minutes,
                ..HiveConfig::default()
            }
            .pull_interval()
        };
        assert_eq!(at(0), 1);
        assert_eq!(at(15), 15);
        assert_eq!(at(600), 60);
    }
}
