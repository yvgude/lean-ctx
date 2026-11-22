// SPDX-License-Identifier: Apache-2.0
//! `[cloud]` configuration.
//!
//! ```toml
//! [cloud]
//! # enabled = true          # unset: on while this machine is connected
//! url = "https://cloud.leanctx.com"
//! # root_key = "<base64>"   # self-hosted Cloud: its root public key
//! auto_capture = true       # 3.11.5 name `auto_draft` is read as an alias
//! pull_interval_minutes = 5  # 1..=60
//! metrics_upload = true
//! receipts_upload = true
//! bus = "auto"              # auto | off
//! vault_labels = true
//! presence_names = true     # false: no "working on" line for your team
//!
//! [cloud.worker]             # stricter local caps on the opt-in budget
//! # max_tasks_per_day = 3
//! # max_tokens_per_day = 200000
//! # max_usd_micros_per_day = 2000000
//! ```
//!
//! The Cloud connector reads this section and re-exports these types as
//! its own config. It also registers what a `config set`
//! of a `cloud.` key applies at once ([`register_off_switches`]).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use lean_ctx_protocol::cloud::CLOUD_DEFAULT_URL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudBusMode {
    #[default]
    Auto,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudConfig {
    /// `None`: on while this machine is connected. `false` pauses all Cloud
    /// traffic without disconnecting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Cloud origin; any `https` URL for a self-hosted Cloud.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Root public key (base64 Ed25519) of a self-hosted Cloud.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_key: Option<String>,
    /// Run the capture gate on session learnings (3.11.5: `auto_draft`,
    /// still read as an alias, `core::config::renamed`).
    pub auto_capture: bool,
    /// Minutes between scheduler passes; [`CloudConfig::pull_interval`]
    /// clamps it to [`PULL_INTERVAL_BOUNDS`].
    pub pull_interval_minutes: u32,
    pub metrics_upload: bool,
    pub receipts_upload: bool,
    pub bus: CloudBusMode,
    pub vault_labels: bool,
    /// Team presence names: your team sees what your agents are working on
    /// (the status line each agent sets itself, with paths and secrets
    /// removed) and this machine's short name as `machine_name`. Sent only
    /// to a Cloud that shows them to members of the same team; `false`
    /// keeps presence to role, state and the Instance's short name, which
    /// every presence carries (never a domain).
    pub presence_names: bool,
    /// The user agreed to install the proprietary Intelligence runtime that
    /// the lease licenses (`cloud connect` asks, `cloud runtime install`
    /// records it). Nothing installs it without this consent.
    pub runtime_consent: bool,
    /// Worker mode: local caps that only tighten the server opt-in budget.
    pub worker: CloudWorkerConfig,
    /// mz6: origin of the Cloud models gateway that serves the subscription
    /// models (`leanctx` provider). `None`: `https://models.<cloud host>`.
    /// Honoured only when its host is the Cloud host or a direct subdomain
    /// of it (loopback for tests); any other value is refused and the
    /// instance token is never sent (`core::cloud_models::models_origin`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_url: Option<String>,
    /// mz6 owner question Q3 (open): whether Anthropic-shaped agents may
    /// reach the subscription models through `shape-xlat` routing aliases.
    /// Default `false`: OpenAI-shaped agents only, the rel2 launch scope;
    /// `true` has no effect until Q3 is decided (routing refuses aliases to
    /// `leanctx`).
    pub models_anthropic_agents: bool,
    /// How the bus treats a sender presenting as the Queen that is no
    /// registered Queen session: `warn` (unset: pass, log, mark) or
    /// `enforce` (refuse); any other value counts as `enforce`. Kept as
    /// text so a wrong value never breaks the whole config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queen_identity: Option<String>,
    /// What remains of the pre-3.12 `[cloud]` table (hosted account sync):
    /// the legacy opt-in the loader turns into `telemetry.enabled`. Read,
    /// never written; the config migration retires the key.
    #[serde(skip_serializing)]
    pub contribute_enabled: bool,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            url: None,
            root_key: None,
            auto_capture: true,
            pull_interval_minutes: DEFAULT_PULL_INTERVAL_MINUTES,
            metrics_upload: true,
            receipts_upload: true,
            bus: CloudBusMode::Auto,
            vault_labels: true,
            presence_names: true,
            runtime_consent: false,
            worker: CloudWorkerConfig::default(),
            models_url: None,
            models_anthropic_agents: false,
            queen_identity: None,
            contribute_enabled: false,
        }
    }
}

/// Default minutes between scheduler passes (SPEC-ENGINE §3.16).
pub const DEFAULT_PULL_INTERVAL_MINUTES: u32 = 5;
/// Allowed `pull_interval_minutes` (SPEC-ENGINE §3.16).
pub const PULL_INTERVAL_BOUNDS: std::ops::RangeInclusive<u32> = 1..=60;

impl CloudConfig {
    /// `pull_interval_minutes` within [`PULL_INTERVAL_BOUNDS`].
    pub fn pull_interval(&self) -> u32 {
        self.pull_interval_minutes
            .clamp(*PULL_INTERVAL_BOUNDS.start(), *PULL_INTERVAL_BOUNDS.end())
    }
}

/// Applies the `[cloud]` off switches to the connector's local state.
pub type OffSwitches = fn(&CloudConfig);

static OFF_SWITCHES: OnceLock<OffSwitches> = OnceLock::new();

/// Register what a `config set` of a `cloud.` key applies at once (the
/// connector stops its local recording when a switch is off). The first
/// registration wins.
pub fn register_off_switches(apply: OffSwitches) {
    let _ = OFF_SWITCHES.set(apply);
}

/// Apply the off switches in `config`. Without a registration there is no
/// connector state to stop, so nothing happens.
pub(crate) fn apply_off_switches(config: &CloudConfig) {
    if let Some(apply) = OFF_SWITCHES.get() {
        apply(config);
    }
}

/// `[cloud.worker]`: stricter local caps on top of the server's budget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudWorkerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tasks_per_day: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_per_day: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd_micros_per_day: Option<u64>,
}

/// `[cloud]` in `lean-ctx config schema` / `config validate`.
pub fn schema_section() -> crate::core::config::schema::SectionSchema {
    use crate::core::config::schema::{KeySchema, SectionSchema};
    let defaults = CloudConfig::default();
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
                "Pause all LeanCTX Cloud traffic with false; unset means on while this machine is connected.",
                None,
            ),
        ),
        (
            "url",
            key(
                "string",
                serde_json::json!(CLOUD_DEFAULT_URL),
                "LeanCTX Cloud origin (https; a self-hosted LeanCTX Cloud server may use any https URL).",
                None,
            ),
        ),
        (
            "root_key",
            key(
                "string",
                serde_json::Value::Null,
                "Root public key (base64 Ed25519) of a self-hosted LeanCTX Cloud server.",
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
                "Minutes between LeanCTX Cloud sync passes (1-60; values outside are clamped).",
                None,
            ),
        ),
        (
            "metrics_upload",
            key(
                "bool",
                serde_json::json!(defaults.metrics_upload),
                "Send hourly instance metrics to LeanCTX Cloud (counts only, including full re-fetches and unregulated program names; never content).",
                None,
            ),
        ),
        (
            "receipts_upload",
            key(
                "bool",
                serde_json::json!(defaults.receipts_upload),
                "Upload context receipts to LeanCTX Cloud.",
                None,
            ),
        ),
        (
            "bus",
            key(
                "string",
                serde_json::json!("auto"),
                "Relay the agent bus through LeanCTX Cloud (auto) or keep it local (off).",
                Some(&["auto", "off"]),
            ),
        ),
        (
            "queen_identity",
            KeySchema {
                env_override: Some("LEAN_CTX_CLOUD_QUEEN_IDENTITY".into()),
                ..key(
                    "string",
                    serde_json::json!("warn"),
                    "A bus sender presenting as the app's coordinator session that is no registered coordinator session: warn (pass, log in bus/queen-warnings.json, mark for the receiver) or enforce (refuse); any other value counts as enforce. Enforce only once queen-sessions.json lists every coordinator session.",
                    Some(&["warn", "enforce"]),
                )
            },
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
            "presence_names",
            key(
                "bool",
                serde_json::json!(defaults.presence_names),
                "Show your team what your agents are working on: the status line each agent sets itself, with paths and secrets removed (only through a server that shows it to your team alone); false keeps presence to role, state and this machine's short name.",
                None,
            ),
        ),
        (
            "runtime_consent",
            key(
                "bool",
                serde_json::json!(defaults.runtime_consent),
                "You agreed to install the proprietary Intelligence runtime your plan licenses (set by `cloud connect` or `cloud runtime install`).",
                None,
            ),
        ),
        (
            "models_url",
            key(
                "string",
                serde_json::Value::Null,
                "Origin of the LeanCTX Cloud models gateway for the leanctx provider; unset means https://models.<cloud host>. Only the LeanCTX Cloud host or a direct subdomain of it is honoured.",
                None,
            ),
        ),
        (
            "models_anthropic_agents",
            key(
                "bool",
                serde_json::json!(defaults.models_anthropic_agents),
                "Let Anthropic-shaped agents reach the subscription models through shape-xlat routing aliases; false means OpenAI-shaped agents only. Owner question Q3 is open: true takes effect only once it is decided.",
                None,
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
        description: "LeanCTX Cloud: workspace knowledge, policies and the shared agent bus."
            .into(),
        keys: keys
            .into_iter()
            .map(|(name, key)| (name.to_string(), key))
            .collect(),
    }
}
