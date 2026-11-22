// SPDX-License-Identifier: Apache-2.0
//! Local, network-free Cloud status: the cockpit's `cloud status --json`
//! shape and the human report behind `lean-ctx cloud status`.

use crate::CloudConfigExt as _;
use lean_ctx_time::UtcDateTime;

use super::auth::Session;
use super::config::CloudConfig;
use super::state::{ActivityRecord, CloudStore, InstanceRecord};
use lean_ctx_protocol::cloud::lease::{CloudLeaseV1, LeaseState};
use lean_ctx_protocol::cloud::status::{
    CloudBusControlsInactiveV1, CloudCliStatusV1, CloudStatusBusControlV1, CloudStatusDraftsV1,
    CloudStatusFilterPackV1, CloudStatusInstanceV1, CloudStatusOrgV1,
};

/// Everything `lean-ctx cloud status` shows.
#[derive(Debug, Clone)]
pub struct CloudStatusReport {
    pub url: String,
    pub enabled: bool,
    pub instance: Option<InstanceRecord>,
    pub lease: Option<CloudLeaseV1>,
    pub lease_state: Option<LeaseState>,
    /// Why the stored enrollment cannot be used, if it cannot.
    pub problem: Option<String>,
    pub activity: ActivityRecord,
    /// Queued drafts `(outbox id, summary, last error)`.
    pub drafts_pending: Vec<super::knowledge::pull::DraftLine>,
    /// Drafts Cloud refused for good; kept until removed.
    pub drafts_failed: Vec<super::knowledge::pull::DraftLine>,
    /// The last verified policy bundle (and why a newer one was refused).
    pub policy: super::policy::PolicyState,
    /// Engine metrics v2 (`None`: no usable lease).
    pub metrics_v2: Option<super::metrics_v2::upload::MetricsV2Status>,
    /// Why receipts are not sent (`None`: sent, or no usable lease).
    pub receipts_off: Option<&'static str>,
    /// Receipts waiting in the outbox.
    pub receipts_queued: usize,
    /// Bus controls in force (filled by the bus control module).
    pub bus_controls: Vec<CloudStatusBusControlV1>,
    /// Why recorded bus controls do not apply right now.
    pub bus_controls_inactive: Option<CloudBusControlsInactiveV1>,
    /// The Cloud root is `[cloud] root_key`, vouched for by nobody.
    pub unvouched_root: bool,
    /// Durable workers in the book (Cloud allows
    /// [`WORKER_CAP`](super::workers::attach::WORKER_CAP) per instance).
    pub workers: usize,
    /// Workers the book gave up that Cloud has not retired yet.
    pub workers_retiring: usize,
    /// The machine's managed configuration: each member and its carrier.
    pub managed: Option<lean_ctx_protocol::cloud::status::CloudStatusManagedV1>,
}

impl CloudStatusReport {
    pub fn connected(&self) -> bool {
        self.instance.is_some()
            && self.problem.is_none()
            && matches!(
                self.lease_state,
                Some(LeaseState::Valid | LeaseState::Grace)
            )
    }

    /// The cockpit shape: exactly six fields, nulls printed.
    pub fn cli_status(&self) -> CloudCliStatusV1 {
        let connected = self.connected();
        CloudCliStatusV1 {
            available: true,
            connected,
            instance: self.instance.as_ref().map(|i| CloudStatusInstanceV1 {
                id: i.instance_id.clone(),
                label: i.label.clone(),
            }),
            org: self.instance.as_ref().map(|i| CloudStatusOrgV1 {
                name: i.org_name.clone(),
            }),
            last_seen: self
                .activity
                .last_seen
                .as_deref()
                .and_then(|t| lean_ctx_protocol::UtcTimestamp::new(t).ok()),
            url: self.url.clone(),
            drafts: CloudStatusDraftsV1 {
                queued: u32::try_from(self.drafts_pending.len()).unwrap_or(u32::MAX),
                refused: u32::try_from(self.drafts_failed.len()).unwrap_or(u32::MAX),
            },
            filter_packs: self
                .policy
                .filter_packs
                .iter()
                .map(|(pack_id, version)| CloudStatusFilterPackV1 {
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
            // Per checkout: the CLI fills it for the project it runs in
            // ([`super::resolution::Stale::wire`]).
            vault_mismatch: None,
            managed: self.managed.clone(),
            // From the Cloud, not local state: the CLI fills them
            // ([`super::team_stop::apply`]).
            team_stop: None,
            role: None,
            org_slug: None,
            company: None,
            step_up_url: None,
        }
    }
}

/// Build the report from local state at `local_now`. Never touches the network.
pub fn report(
    store: &CloudStore,
    config: &CloudConfig,
    local_now: UtcDateTime,
) -> CloudStatusReport {
    let instance = store.instance().ok().flatten();
    let url = instance
        .as_ref()
        .map(|i| i.url.clone())
        .or_else(|| config.origin().ok())
        .unwrap_or_else(|| lean_ctx_protocol::cloud::CLOUD_DEFAULT_URL.to_string());
    let mut report = CloudStatusReport {
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
        metrics_v2: None,
        receipts_off: None,
        receipts_queued: super::outbox::Outbox::at(store.data_dir())
            .depth(super::outbox::CHANNEL_RECEIPTS)
            .pending,
        bus_controls: Vec::new(),
        bus_controls_inactive: None,
        unvouched_root: super::root_cert::unvouched(store, config),
        workers: 0,
        workers_retiring: 0,
        managed: lean_ctx::core::config::managed::current().map(|managed| managed.wire()),
    };
    if let Some(instance) = &instance {
        let id = &instance.instance_id;
        report.workers = super::workers::attach::list(store, id).map_or(0, |w| w.len());
        report.workers_retiring = super::workers::attach::retired(store, id).map_or(0, |r| r.len());
    }
    (report.drafts_pending, report.drafts_failed) =
        super::knowledge::pull::draft_listing(store.data_dir());
    (report.bus_controls, report.bus_controls_inactive) =
        super::bus::control::status_view(store, local_now);
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

/// When the one-step trial of `lease` ends, in second-precision `Z` form:
/// `None` without a running trial (or with a time that is not RFC 3339).
#[must_use]
pub fn trial_ends_at(lease: &CloudLeaseV1) -> Option<String> {
    super::error::wire_time(lease.trial_ends_at.as_deref())
}

/// `cloud status`'s trial line, e.g. `Trial ends 2026-12-02 10:00 UTC`.
#[must_use]
pub fn trial_line(lease: &CloudLeaseV1) -> Option<String> {
    let at = trial_ends_at(lease)?;
    Some(format!("Trial ends {} {} UTC", &at[..10], &at[11..16]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(trial_ends_at: Option<&str>) -> CloudLeaseV1 {
        let envelope: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../lean-ctx-protocol/tests/fixtures/cloud/lease_envelope.response.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut value = envelope["lease"].clone();
        if let Some(at) = trial_ends_at {
            value["trial_ends_at"] = at.into();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn trial_line_shows_only_while_a_trial_runs() {
        assert_eq!(trial_line(&lease(None)), None);
        assert_eq!(trial_ends_at(&lease(None)), None);
        let running = lease(Some("2026-12-02T11:00:00+01:00"));
        assert_eq!(
            trial_ends_at(&running).as_deref(),
            Some("2026-12-02T10:00:00Z")
        );
        assert_eq!(
            trial_line(&running).as_deref(),
            Some("Trial ends 2026-12-02 10:00 UTC")
        );
        assert_eq!(trial_line(&lease(Some("next week"))), None);
    }

    #[test]
    fn a_fresh_machine_reports_available_but_not_connected() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        let report = report(&store, &CloudConfig::default(), UtcDateTime::now());
        assert_eq!(
            serde_json::to_value(report.cli_status()).unwrap(),
            serde_json::to_value(CloudCliStatusV1::not_connected(None)).unwrap()
        );
    }
}
