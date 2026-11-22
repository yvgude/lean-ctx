// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx cloud status --json`: the one shape the local cockpit reads.
//!
//! It is built from local state only (no network) and prints `available:
//! true` whenever the engine has the Cloud connector, connected or not.
//! The six fields of 1.0 are always printed; 1.2 adds `drafts`, `policy`,
//! `filter_packs` and `policy_refusal` (readers treat them as "not
//! reported" when an older engine leaves them out).

use serde::{Deserialize, Serialize};

use super::CLOUD_DEFAULT_URL;
use crate::UtcTimestamp;

/// Instance shown in the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusInstanceV1 {
    pub id: String,
    pub label: String,
}

/// Organization shown in the cockpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusOrgV1 {
    pub name: String,
}

/// Session drafts not yet in Cloud (1.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusDraftsV1 {
    /// Queued in the outbox, sent with the next sync.
    pub queued: u32,
    /// Refused by Cloud for good; kept on this machine.
    pub refused: u32,
}

/// An active filter pack (1.2). Bundles verify as a whole, so every listed
/// pack is active; a refused bundle leaves the previous list in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusFilterPackV1 {
    pub pack_id: String,
    pub version: u64,
}

/// What a bus control asks of an agent (1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudBusControlActionV1 {
    Hold,
    Stop,
    Veto,
    #[serde(other)]
    Unknown,
}

/// A bus control in force on this machine (1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusBusControlV1 {
    /// The agent it targets; `null`: every agent of this instance.
    pub target_agent: Option<String>,
    pub action: CloudBusControlActionV1,
    /// What a veto names (a lease or handoff reference).
    pub target_ref: Option<String>,
    pub reason: String,
    pub issued_by: String,
    pub since: UtcTimestamp,
}

/// Why recorded bus controls do not apply right now (1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudBusControlsInactiveV1 {
    NotEnrolled,
    LeaseLapsed,
    #[serde(other)]
    Unknown,
}

/// Output of `lean-ctx cloud status --json`. `null` fields are printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudCliStatusV1 {
    /// The engine build has the Cloud connector.
    pub available: bool,
    /// Enrolled and the lease is not lapsed.
    pub connected: bool,
    pub instance: Option<CloudStatusInstanceV1>,
    pub org: Option<CloudStatusOrgV1>,
    /// Last successful exchange with Cloud.
    pub last_seen: Option<UtcTimestamp>,
    /// Cloud web origin (the configured `[cloud] url`, default hosted).
    pub url: String,
    /// 1.2.
    #[serde(default)]
    pub drafts: CloudStatusDraftsV1,
    /// 1.2.
    #[serde(default)]
    pub filter_packs: Vec<CloudStatusFilterPackV1>,
    /// 1.2: why the newest policy bundle was refused (the last verified one stays).
    #[serde(default)]
    pub policy_refusal: Option<String>,
    /// 1.3: origin of the LeanCTX Cloud web app (its "Open" link): the one enrollment
    /// named, else `url`.
    #[serde(default)]
    pub web_url: Option<String>,
    /// 1.5: bus controls in force. Left out when empty, so readers of
    /// earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bus_controls: Vec<CloudStatusBusControlV1>,
    /// 1.5: why recorded controls do not apply; left out when they do or
    /// none are recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_controls_inactive: Option<CloudBusControlsInactiveV1>,
    /// Engine metrics v2 (telemetry SPEC §4); left out when not connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_v2: Option<CloudStatusMetricsV2>,
    /// 1.6: the project here was resolved to a vault its link file or its
    /// identity no longer backs. Left out when they agree, so readers of
    /// earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_mismatch: Option<CloudStatusVaultMismatchV1>,
    /// 1.7: this machine carries a managed configuration
    /// (`leanctx.managed-config/v1`). Left out when it carries none, so
    /// readers of earlier shapes see unchanged output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<CloudStatusManagedV1>,
    /// 1.8: the team stop (kill switch) as the Cloud last answered it. Left
    /// out when not connected or the Cloud does not offer it, so readers of
    /// earlier shapes see unchanged output; `null` (`Some(None)`) when
    /// connected but no answer is kept yet, so nothing reads as "running".
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub team_stop: Option<Option<CloudStatusTeamStopV1>>,
    /// 1.8: the linked person's acting role in the team (the Cloud's word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// 1.8: the team's slug (what `cloud stop --release --confirm` names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_slug: Option<String>,
    /// 1.8: the person's standing in the team's company, when the Cloud names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<CloudStatusCompanyV1>,
    /// 1.8: the browser page that confirms a release (admin, two-factor);
    /// only while the team is stopped and this person may release it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_up_url: Option<String>,
}

/// A member that is present, even as `null` (`Some(None)`); absent is `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `team_stop` of `cloud status --json` (1.8): the last answer the tick or
/// a `cloud stop` command kept; status never asks the Cloud. Unknown
/// members a newer engine adds are ignored when read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudStatusTeamStopV1 {
    pub active: bool,
    pub reason: Option<String>,
    /// When the stop began (RFC 3339, the Cloud's).
    pub since: Option<String>,
    /// Who stopped it (display name, else email).
    pub by: Option<String>,
    /// The Cloud's count of stops and releases.
    pub generation: u64,
    pub may_stop: bool,
    pub may_release: bool,
    /// `fresh`, or `stale` (older than two pull intervals, or of unknown
    /// age). No answer kept yet is `team_stop: null`, not an object.
    pub cache: String,
    /// Seconds since the Cloud answered; `null` when unknown.
    pub age_s: Option<u64>,
}

/// `company` of `cloud status --json` (1.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudStatusCompanyV1 {
    /// The person administers the company (stops and releases its teams).
    pub admin: bool,
}

/// `managed` of `cloud status --json` (1.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusManagedV1 {
    pub carrier: super::managed::ManagedCarrierKindV1,
    /// Where the carrier lives (a path, or `HKLM\…`).
    pub location: String,
    /// The members in force, in schema order; empty when `error` is set.
    pub members: Vec<CloudStatusManagedMemberV1>,
    /// `managed_digest` of the members; `null` when `error` is set.
    pub digest: Option<String>,
    /// Why the carrier was refused: the engine fails closed (Cloud calls
    /// and, with nothing to go by, `cloud.*` settings refuse).
    pub error: Option<String>,
}

/// One managed member (`cloud.url`, …) and its value as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusManagedMemberV1 {
    pub member: String,
    pub value: String,
}

/// `vault_mismatch` of `cloud status --json` (1.6). That resolution is not
/// used: the next connect or sync resolves the project again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusVaultMismatchV1 {
    /// The checkout root on this machine.
    pub project_root: String,
    /// `ns/name` this machine resolved it to.
    pub resolved: String,
    /// `ns/name` its link file names; `null` when the identity changed.
    pub linked: Option<String>,
    /// `link` or `identity`.
    pub reason: String,
}

/// `metrics_v2` of `cloud status --json` (telemetry SPEC §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusMetricsV2 {
    /// The workspace's sharing state as Cloud last answered it (or the plan
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

impl CloudCliStatusV1 {
    /// Status of an engine that was never connected.
    pub fn not_connected(url: Option<&str>) -> Self {
        Self {
            available: true,
            connected: false,
            instance: None,
            org: None,
            last_seen: None,
            url: url.unwrap_or(CLOUD_DEFAULT_URL).to_string(),
            drafts: CloudStatusDraftsV1::default(),
            filter_packs: Vec::new(),
            policy_refusal: None,
            web_url: Some(url.unwrap_or(CLOUD_DEFAULT_URL).to_string()),
            bus_controls: Vec::new(),
            bus_controls_inactive: None,
            metrics_v2: None,
            vault_mismatch: None,
            managed: None,
            team_stop: None,
            role: None,
            org_slug: None,
            company: None,
            step_up_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_connected_prints_nulls() {
        assert_eq!(
            serde_json::to_string(&CloudCliStatusV1::not_connected(None)).unwrap(),
            r#"{"available":true,"connected":false,"instance":null,"org":null,"last_seen":null,"url":"https://cloud.leanctx.com","drafts":{"queued":0,"refused":0},"filter_packs":[],"policy_refusal":null,"web_url":"https://cloud.leanctx.com"}"#
        );
    }

    #[test]
    fn bus_controls_appear_only_when_present() {
        let mut status = CloudCliStatusV1::not_connected(None);
        let quiet = serde_json::to_string(&status).unwrap();
        assert!(!quiet.contains("bus_controls"));
        status.bus_controls_inactive = Some(CloudBusControlsInactiveV1::LeaseLapsed);
        assert!(
            serde_json::to_string(&status)
                .unwrap()
                .contains(r#""bus_controls_inactive":"lease_lapsed""#)
        );
    }

    #[test]
    fn a_1_0_status_still_reads() {
        let old = r#"{"available":true,"connected":false,"instance":null,"org":null,"last_seen":null,"url":"https://cloud.leanctx.com"}"#;
        let status: CloudCliStatusV1 = serde_json::from_str(old).unwrap();
        assert_eq!(
            status,
            CloudCliStatusV1 {
                web_url: None,
                ..CloudCliStatusV1::not_connected(None)
            }
        );
    }
}
