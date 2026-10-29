// SPDX-License-Identifier: Apache-2.0
//! Local, network-free Hive status: the cockpit's `hive status --json`
//! shape and the human report behind `lean-ctx hive status`.

use chrono::{DateTime, Utc};

use super::auth::Session;
use super::config::HiveConfig;
use super::state::{ActivityRecord, HiveStore, InstanceRecord};
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, LeaseState};
use lean_ctx_protocol::hive::status::{
    HiveBusControlsInactiveV1, HiveCliStatusV1, HiveOrgPolicyStateV1, HiveStatusBusControlV1,
    HiveStatusDraftsV1, HiveStatusFilterPackV1, HiveStatusInstanceV1, HiveStatusOrgV1,
    HiveStatusPolicyV1, HiveStatusRequiredPackV1,
};

/// Everything `lean-ctx hive status` shows.
#[derive(Debug, Clone)]
pub struct HiveStatusReport {
    pub url: String,
    pub enabled: bool,
    pub instance: Option<InstanceRecord>,
    pub lease: Option<HiveLeaseV1>,
    pub lease_state: Option<LeaseState>,
    /// Why the stored enrollment cannot be used, if it cannot.
    pub problem: Option<String>,
    pub activity: ActivityRecord,
    /// Queued drafts `(outbox id, summary, last error)`.
    pub drafts_pending: Vec<super::knowledge::pull::DraftLine>,
    /// Drafts Hive refused for good; kept until removed.
    pub drafts_failed: Vec<super::knowledge::pull::DraftLine>,
    /// The last verified policy bundle (and why a newer one was refused).
    pub policy: super::policy::PolicyState,
    /// Per pack of `policy.required_filter_packs`, whether the installed
    /// filter set holds it as signed (the policy runtime's rule).
    pub required_installed: Vec<bool>,
    /// Why metrics are not sent (`None`: sent hourly, or no usable lease).
    pub metrics_off: Option<&'static str>,
    /// Engine metrics v2 (`None`: no usable lease).
    pub metrics_v2: Option<super::metrics_v2::upload::MetricsV2Status>,
    /// Why receipts are not sent (`None`: sent, or no usable lease).
    pub receipts_off: Option<&'static str>,
    /// Receipts waiting in the outbox.
    pub receipts_queued: usize,
    /// Bus controls in force (filled by the bus control module).
    pub bus_controls: Vec<HiveStatusBusControlV1>,
    /// Why recorded bus controls do not apply right now.
    pub bus_controls_inactive: Option<HiveBusControlsInactiveV1>,
    /// The Hive root is `[hive] root_key`, vouched for by nobody.
    pub unvouched_root: bool,
}

impl HiveStatusReport {
    pub fn connected(&self) -> bool {
        self.instance.is_some()
            && self.problem.is_none()
            && matches!(
                self.lease_state,
                Some(LeaseState::Valid | LeaseState::Grace)
            )
    }

    /// The cockpit shape: exactly six fields, nulls printed.
    pub fn cli_status(&self) -> HiveCliStatusV1 {
        let connected = self.connected();
        HiveCliStatusV1 {
            available: true,
            connected,
            instance: self.instance.as_ref().map(|i| HiveStatusInstanceV1 {
                id: i.instance_id.clone(),
                label: i.label.clone(),
            }),
            org: self.instance.as_ref().map(|i| HiveStatusOrgV1 {
                name: i.org_name.clone(),
            }),
            last_seen: self
                .activity
                .last_seen
                .as_deref()
                .and_then(|t| lean_ctx_protocol::UtcTimestamp::new(t).ok()),
            url: self.url.clone(),
            drafts: HiveStatusDraftsV1 {
                queued: u32::try_from(self.drafts_pending.len()).unwrap_or(u32::MAX),
                refused: u32::try_from(self.drafts_failed.len()).unwrap_or(u32::MAX),
            },
            policy: self.policy_view(),
            filter_packs: self
                .policy
                .filter_packs
                .iter()
                .map(|(pack_id, version)| HiveStatusFilterPackV1 {
                    pack_id: pack_id.clone(),
                    version: *version,
                })
                .collect(),
            policy_refusal: self.policy.last_refusal.clone(),
            web_url: Some(
                self.instance
                    .as_ref()
                    .and_then(|i| i.web_url.clone())
                    .unwrap_or_else(|| self.url.clone()),
            ),
            bus_controls: self.bus_controls.clone(),
            bus_controls_inactive: self.bus_controls_inactive,
            metrics_v2: self
                .metrics_v2
                .as_ref()
                .map(super::metrics_v2::upload::MetricsV2Status::wire),
        }
    }

    fn policy_view(&self) -> Option<HiveStatusPolicyV1> {
        let state = match self.policy.org_state.as_deref()? {
            "required" => HiveOrgPolicyStateV1::Required,
            "unpinned" => HiveOrgPolicyStateV1::Unpinned,
            _ => return None,
        };
        Some(HiveStatusPolicyV1 {
            version: self.policy.policy_version,
            state,
            verified_at: seconds(self.policy.verified_at.as_deref()?)?,
            not_after: self.policy.not_after.as_deref().and_then(seconds),
            enforced: self.policy.enforced,
            required_filter_packs: self
                .policy
                .required_filter_packs
                .iter()
                .enumerate()
                .map(|(i, r)| HiveStatusRequiredPackV1 {
                    pack_id: r.pack_id.clone(),
                    version: r.version,
                    installed: self.required_installed.get(i).copied().unwrap_or(false),
                })
                .collect(),
        })
    }
}

/// A timestamp in the status' second-precision `Z` form.
fn seconds(value: &str) -> Option<lean_ctx_protocol::UtcTimestamp> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .ok()?
        .with_timezone(&Utc);
    lean_ctx_protocol::UtcTimestamp::new(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok()
}

/// Build the report from local state at `local_now`. Never touches the network.
pub fn report(
    store: &HiveStore,
    config: &HiveConfig,
    local_now: DateTime<Utc>,
) -> HiveStatusReport {
    let instance = store.instance().ok().flatten();
    let url = instance
        .as_ref()
        .map(|i| i.url.clone())
        .or_else(|| config.origin().ok())
        .unwrap_or_else(|| lean_ctx_protocol::hive::HIVE_DEFAULT_URL.to_string());
    let mut report = HiveStatusReport {
        url: url.clone(),
        enabled: config.enabled.unwrap_or(true),
        instance: instance.clone(),
        lease: None,
        lease_state: None,
        problem: None,
        activity: store.activity(),
        drafts_pending: Vec::new(),
        drafts_failed: Vec::new(),
        policy: super::policy::load_state(store.data_dir()),
        required_installed: Vec::new(),
        metrics_off: None,
        metrics_v2: None,
        receipts_off: None,
        receipts_queued: super::outbox::Outbox::at(store.data_dir())
            .depth(super::outbox::CHANNEL_RECEIPTS)
            .pending,
        bus_controls: Vec::new(),
        bus_controls_inactive: None,
        unvouched_root: super::root_cert::unvouched(store, config),
    };
    (report.drafts_pending, report.drafts_failed) =
        super::knowledge::pull::draft_listing(store.data_dir());
    (report.bus_controls, report.bus_controls_inactive) =
        super::bus::control::status_view(store, local_now);
    report.required_installed = super::policy::required_packs_status(
        &report.policy.required_filter_packs,
        super::policy::load_filter_set(store.data_dir()).as_deref(),
    );
    if instance.is_none() {
        return report;
    }
    // A recorded revocation is the whole story: no lease is shown as valid.
    let revoked = instance
        .as_ref()
        .and_then(|i| super::revoked::recorded(store, i));
    let session = match revoked {
        Some(revoked) => Err(revoked),
        None => super::root_cert::trust_root(store, config, &url)
            .and_then(|root| Session::load(store.clone(), root)),
    };
    match session {
        Ok(session) => {
            report.lease_state = Some(session.lease.state_at(local_now));
            report.metrics_off =
                super::metrics::upload::status(store, config, &session.lease.lease);
            report.metrics_v2 = Some(super::metrics_v2::upload::status(
                store,
                config,
                &session.instance,
                &session.lease.lease,
                local_now,
            ));
            report.receipts_off =
                super::receipts::status(store, config, &session.instance, &session.lease.lease);
            report.lease = Some(session.lease.lease);
        }
        Err(error) => report.problem = Some(error.to_string()),
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_machine_reports_available_but_not_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let report = report(&store, &HiveConfig::default(), Utc::now());
        assert_eq!(
            serde_json::to_value(report.cli_status()).unwrap(),
            serde_json::to_value(HiveCliStatusV1::not_connected(None)).unwrap()
        );
    }
}
