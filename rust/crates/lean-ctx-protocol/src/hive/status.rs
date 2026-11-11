// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx hive status --json`: the one shape the local cockpit reads.
//!
//! It is built from local state only (no network) and prints `available:
//! true` whenever the engine has the Hive connector, connected or not.
//! The six fields of 1.0 are always printed; 1.2 adds `drafts`, `policy`,
//! `filter_packs` and `policy_refusal` (readers treat them as "not
//! reported" when an older engine leaves them out).

use serde::{Deserialize, Serialize};

use super::HIVE_DEFAULT_URL;
use crate::UtcTimestamp;

/// Instance shown in the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusInstanceV1 {
    pub id: String,
    pub label: String,
}

/// Organization shown in the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusOrgV1 {
    pub name: String,
}

/// Session drafts not yet in Hive (1.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusDraftsV1 {
    /// Queued in the outbox, sent with the next sync.
    pub queued: u32,
    /// Refused by Hive for good; kept on this machine.
    pub refused: u32,
}

/// An active filter pack (1.2). Bundles verify as a whole, so every listed
/// pack is active; a refused bundle leaves the previous list in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusFilterPackV1 {
    pub pack_id: String,
    pub version: u64,
}

/// What a bus control asks of an agent (1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveBusControlActionV1 {
    Hold,
    Stop,
    Veto,
    #[serde(other)]
    Unknown,
}

/// A bus control in force on this machine (1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusBusControlV1 {
    /// The agent it targets; `null`: every agent of this instance.
    pub target_agent: Option<String>,
    pub action: HiveBusControlActionV1,
    /// What a veto names (a lease or handoff reference).
    pub target_ref: Option<String>,
    pub reason: String,
    pub issued_by: String,
    pub since: UtcTimestamp,
}

/// Why recorded bus controls do not apply right now (1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveBusControlsInactiveV1 {
    NotEnrolled,
    LeaseLapsed,
    #[serde(other)]
    Unknown,
}

/// Output of `lean-ctx hive status --json`. `null` fields are printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveCliStatusV1 {
    /// The engine build has the Hive connector.
    pub available: bool,
    /// Enrolled and the lease is not lapsed.
    pub connected: bool,
    pub instance: Option<HiveStatusInstanceV1>,
    pub org: Option<HiveStatusOrgV1>,
    /// Last successful exchange with Hive.
    pub last_seen: Option<UtcTimestamp>,
    /// Hive web origin (the configured `[hive] url`, default hosted).
    pub url: String,
    /// 1.2.
    #[serde(default)]
    pub drafts: HiveStatusDraftsV1,
    /// 1.2.
    #[serde(default)]
    pub filter_packs: Vec<HiveStatusFilterPackV1>,
    /// 1.2: why the newest policy bundle was refused (the last verified one stays).
    #[serde(default)]
    pub policy_refusal: Option<String>,
    /// 1.3: origin of the Hive web app ("Open Hive"): the one enrollment
    /// named, else `url`.
    #[serde(default)]
    pub web_url: Option<String>,
    /// 1.5: bus controls in force. Left out when empty, so readers of
    /// earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_controls: Vec<HiveStatusBusControlV1>,
    /// 1.5: why recorded controls do not apply; left out when they do or
    /// none are recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_controls_inactive: Option<HiveBusControlsInactiveV1>,
    /// Engine metrics v2 (telemetry SPEC §4); left out when not connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_v2: Option<HiveStatusMetricsV2>,
    /// 1.6: the project here was resolved to a vault its link file or its
    /// identity no longer backs. Left out when they agree, so readers of
    /// earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_mismatch: Option<HiveStatusVaultMismatchV1>,
    /// 1.7: this machine carries a managed configuration
    /// (`leanctx.managed-config/v1`). Left out when it carries none, so
    /// readers of earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<HiveStatusManagedV1>,
}

/// `managed` of `hive status --json` (1.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusManagedV1 {
    pub carrier: super::managed::ManagedCarrierKindV1,
    /// Where the carrier lives (a path, or `HKLM\…`).
    pub location: String,
    /// The members in force, in schema order; empty when `error` is set.
    pub members: Vec<HiveStatusManagedMemberV1>,
    /// `managed_digest` of the members; `null` when `error` is set.
    pub digest: Option<String>,
    /// Why the carrier was refused: the engine fails closed (Hive calls
    /// and, with nothing to go by, `hive.*` settings refuse).
    pub error: Option<String>,
}

/// One managed member (`hive.url`, …) and its value as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusManagedMemberV1 {
    pub member: String,
    pub value: String,
}

/// `vault_mismatch` of `hive status --json` (1.6). That resolution is not
/// used: the next connect or sync resolves the project again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusVaultMismatchV1 {
    /// The checkout root on this machine.
    pub project_root: String,
    /// `ns/name` this machine resolved it to.
    pub resolved: String,
    /// `ns/name` its link file names; `null` when the identity changed.
    pub linked: Option<String>,
    /// `link` or `identity`.
    pub reason: String,
}

/// `metrics_v2` of `hive status --json` (telemetry SPEC §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiveStatusMetricsV2 {
    /// The workspace's sharing state as Hive last answered it (or the plan
    /// default before the first answer).
    pub sharing: super::metrics_v2::SharingStateV2,
    pub last_sent_at: Option<String>,
    pub buffered_buckets: u64,
    pub dropped_buckets: u64,
    /// `false` while engine metrics are not released: nothing is shared,
    /// whatever `sharing` says. Older engines omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<bool>,
}

impl HiveCliStatusV1 {
    /// Status of an engine that was never connected.
    pub fn not_connected(url: Option<&str>) -> Self {
        Self {
            available: true,
            connected: false,
            instance: None,
            org: None,
            last_seen: None,
            url: url.unwrap_or(HIVE_DEFAULT_URL).to_string(),
            drafts: HiveStatusDraftsV1::default(),
            filter_packs: Vec::new(),
            policy_refusal: None,
            web_url: Some(url.unwrap_or(HIVE_DEFAULT_URL).to_string()),
            bus_controls: Vec::new(),
            bus_controls_inactive: None,
            metrics_v2: None,
            vault_mismatch: None,
            managed: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_connected_prints_nulls() {
        assert_eq!(
            serde_json::to_string(&HiveCliStatusV1::not_connected(None)).unwrap(),
            r#"{"available":true,"connected":false,"instance":null,"org":null,"last_seen":null,"url":"https://hive.leanctx.com","drafts":{"queued":0,"refused":0},"filter_packs":[],"policy_refusal":null,"web_url":"https://hive.leanctx.com"}"#
        );
    }

    #[test]
    fn bus_controls_appear_only_when_present() {
        let mut status = HiveCliStatusV1::not_connected(None);
        let quiet = serde_json::to_string(&status).unwrap();
        assert!(!quiet.contains("bus_controls"));
        status.bus_controls_inactive = Some(HiveBusControlsInactiveV1::LeaseLapsed);
        assert!(
            serde_json::to_string(&status)
                .unwrap()
                .contains(r#""bus_controls_inactive":"lease_lapsed""#)
        );
    }

    #[test]
    fn a_1_0_status_still_reads() {
        let old = r#"{"available":true,"connected":false,"instance":null,"org":null,"last_seen":null,"url":"https://hive.leanctx.com"}"#;
        let status: HiveCliStatusV1 = serde_json::from_str(old).unwrap();
        assert_eq!(
            status,
            HiveCliStatusV1 {
                web_url: None,
                ..HiveCliStatusV1::not_connected(None)
            }
        );
    }
}
