// SPDX-License-Identifier: Apache-2.0
//! Whether engine metrics may be collected and sent for the connected
//! workspace, and which vaults are left out (SPEC §3, owner decisions).
//!
//! Every one of these must allow it, else nothing is collected, buffered
//! or sent and the buffer is dropped (K3):
//!
//! 1. `[hive] metrics_upload` (this engine);
//! 2. no recorded revocation of this engine (K16);
//! 3. the lease right `metrics.upload` and the workspace switch
//!    `org_settings.metrics_upload`;
//! 4. the policy rule "Share engine metrics"
//!    (`sharing.share_engine_metrics`) in the verified effective-policy
//!    bundle: `false` turns it off. No bundle, or an expired, damaged or
//!    inconsistent one counts as off (fail closed for egress);
//! 5. the workspace's sharing state (`GET /v1/metrics/sharing`, the
//!    `sharing` of every ack): `on`. Unknown yet: the plan default (Team and
//!    Enterprise on; Personal wait for the person's confirmation,
//!    `lean-ctx hive metrics confirm`, in a terminal);
//! 6. no `403` refusal under the policy version or lease in force.
//!
//! A vault whose own entry turns the rule off is left out of every slice;
//! no hour before the state's `since` is built.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::hive::config::HiveConfig;
use crate::core::hive::effective_floor;
use crate::core::hive::state::{HiveStore, InstanceRecord};
use lean_ctx_protocol::hive::effective_policy::EffectivePolicyV1;
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, HivePlan, HiveRight};
use lean_ctx_protocol::hive::metrics_v2::{
    PlanDefault, PolicyLevel, SharingStateV2, SharingValue, TimeStamp,
};

const MEMBER: &str = "share_engine_metrics";

/// Why engine metrics are off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffReason {
    /// `[hive] metrics_upload = false`.
    Engine,
    /// The lease does not include `metrics.upload`.
    Lease,
    /// The workspace switched metrics upload off.
    Workspace,
    /// This engine was revoked.
    Revoked,
    /// The policy rule is off.
    Policy,
    /// The policy bundle in force expired or is damaged.
    PolicyUnavailable,
    /// Personal: the person has not confirmed yet.
    NotConfirmed,
}

/// The decision for one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sharing {
    On {
        /// Vaults whose own policy turns the rule off.
        vaults_off: BTreeSet<String>,
        /// Work outside every vault may be sent (Personal workspace only).
        unattributed: bool,
        /// Version of the effective policy in force (0 = none).
        policy_version: u64,
        /// Start of the sharing period: no hour before it is built.
        since: Option<DateTime<Utc>>,
        /// Who set it: `None` = the plan default.
        level: Option<PolicyLevel>,
    },
    Off {
        reason: OffReason,
        level: Option<PolicyLevel>,
    },
}

impl Sharing {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On { .. })
    }

    /// `on (plan default)` or `off (<why>)` as `hive status` prints it.
    pub fn describe(&self) -> String {
        match self {
            Self::On { level, .. } => match level {
                Some(level) => format!("on (set by {})", level.label()),
                None => "on (plan default)".into(),
            },
            Self::Off { reason, level } => {
                let why = match reason {
                    OffReason::Engine => "this engine, [hive] metrics_upload = false",
                    OffReason::Lease => "the lease does not include metrics.upload",
                    OffReason::Workspace => "switched off by the workspace",
                    OffReason::Revoked => "this engine was revoked",
                    OffReason::Policy => "policy",
                    OffReason::PolicyUnavailable => "policy, no valid bundle in force",
                    OffReason::NotConfirmed => {
                        "waiting for your confirmation: run `lean-ctx hive metrics confirm` in a terminal"
                    }
                };
                match level {
                    Some(level) => format!("off ({why}, set by {})", level.label()),
                    None => format!("off ({why})"),
                }
            }
        }
    }

    /// The SPEC §4 `now:` words.
    pub fn now_words(&self) -> String {
        match self {
            Self::On { level, .. } => format!(
                "on, set by {}",
                level.map_or("plan default", PolicyLevel::label)
            ),
            Self::Off {
                reason: OffReason::NotConfirmed,
                ..
            } => "waiting for your confirmation, set by plan default".into(),
            Self::Off { level, .. } => format!(
                "off, set by {}",
                level.map_or("this engine", PolicyLevel::label)
            ),
        }
    }
}

/// Whether the plan waits for the person's confirmation by default.
pub fn needs_confirmation(plan: HivePlan) -> bool {
    !matches!(plan, HivePlan::Team | HivePlan::Enterprise)
}

/// The rule's value in one effective policy: `None` = no level sets it.
/// Read through JSON so it works whether the protocol types name the
/// member or keep it among the unknown members.
fn rule(policy: &EffectivePolicyV1) -> Option<bool> {
    serde_json::to_value(&policy.sharing)
        .ok()?
        .get(MEMBER)?
        .as_bool()
}

/// The policy part of the decision, from the installed floor under
/// `hive_data`: `Err(reason)` when it turns metrics off.
pub(crate) fn policy(
    hive_data: &Path,
    now: DateTime<Utc>,
) -> Result<(BTreeSet<String>, u64), OffReason> {
    let dir = effective_floor::floor_dir_in(hive_data);
    let (floor, required) =
        effective_floor::read(&dir).map_err(|_| OffReason::PolicyUnavailable)?;
    let Some(floor) = effective_floor::check(floor.as_deref(), required.as_deref())
        .map_err(|_| OffReason::PolicyUnavailable)?
    else {
        // K2 (L3): no verified bundle installed means no rule to send under.
        return Err(OffReason::PolicyUnavailable);
    };
    let expired = DateTime::parse_from_rfc3339(&floor.not_after)
        .map_or(true, |t| t.with_timezone(&Utc) <= now);
    if expired {
        return Err(OffReason::PolicyUnavailable);
    }
    if rule(&floor.policy.effective) == Some(false) {
        return Err(OffReason::Policy);
    }
    let vaults_off = floor
        .policy
        .vaults
        .iter()
        .filter(|v| rule(&v.effective) == Some(false))
        .map(|v| v.vault_id.clone())
        .collect();
    Ok((vaults_off, floor.version))
}

/// The decision from the local switches and the last sharing state Hive
/// answered (`None`: not asked yet, the plan default applies).
pub fn decide_with(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    sharing: Option<&SharingStateV2>,
    now: DateTime<Utc>,
) -> Sharing {
    let off = |reason, level| Sharing::Off { reason, level };
    if !config.metrics_upload {
        return off(OffReason::Engine, Some(PolicyLevel::Engine));
    }
    if crate::core::hive::revoked::recorded(store, instance).is_some() {
        return off(OffReason::Revoked, None);
    }
    if !lease.grants(HiveRight::MetricsUpload) {
        return off(OffReason::Lease, None);
    }
    if !lease.org_allows_metrics_upload() {
        return off(OffReason::Workspace, None);
    }
    // The workspace's answer first: a person who never confirmed is told so.
    let (since, level) = match sharing {
        Some(s) => match s.state {
            SharingValue::On => (
                s.since.as_ref().map(TimeStamp::at),
                s.policy.map(|p| p.level),
            ),
            SharingValue::Off => {
                return off(OffReason::Policy, s.policy.map(|p| p.level));
            }
            SharingValue::NeedsConfirmation => return off(OffReason::NotConfirmed, None),
        },
        None if needs_confirmation(lease.plan) => return off(OffReason::NotConfirmed, None),
        None => (None, None),
    };
    let (vaults_off, policy_version) = match policy(store.data_dir(), now) {
        Ok(found) => found,
        Err(OffReason::Policy) => {
            return off(
                OffReason::Policy,
                sharing.and_then(|s| s.policy).map(|p| p.level),
            );
        }
        Err(reason) => return off(reason, None),
    };
    let state = super::buffer::State::load(store.data_dir());
    if let Some((reason, level)) = state.refused(&lease.lease_id, policy_version) {
        return off(reason, level);
    }
    Sharing::On {
        vaults_off,
        unattributed: instance.org_kind.as_deref() == Some("personal"),
        policy_version,
        since,
        level,
    }
}

/// [`decide_with`] and the sharing state this engine last received.
pub fn decide(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> Sharing {
    let state = super::buffer::State::load(store.data_dir());
    let sharing = state.sharing.filter(|_| state.org_id == instance.org_id);
    decide_with(store, config, instance, lease, sharing.as_ref(), now)
}

/// The sharing state to report when Hive has not answered one yet.
pub fn plan_state(plan: HivePlan) -> SharingStateV2 {
    let default_on = !needs_confirmation(plan);
    SharingStateV2 {
        state: if default_on {
            SharingValue::On
        } else {
            SharingValue::NeedsConfirmation
        },
        plan_default: if default_on {
            PlanDefault::On
        } else {
            PlanDefault::Off
        },
        policy: None,
        confirmed_at: None,
        since: None,
    }
}
