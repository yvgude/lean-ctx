// SPDX-License-Identifier: Apache-2.0
//! `POST /v1/metrics`: the Cockpit totals of this device's Hive projects.
//!
//! Sent only while all of these allow it: `[hive] metrics_upload`, the
//! lease's `metrics.upload` right, the organization's switch in the lease
//! (`org_settings.metrics_upload`, absent = on) and no `upload_disabled`
//! answer under the current lease. Uploads replace per `(device, date,
//! project)`, so nothing is queued: a later run rebuilds from local data.
//! Projects are those Hive resolved for this machine (`projects.json`);
//! any other root is not reported.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::local_state::{self, UploadState};
use super::{BACKFILL_DAYS, RECENT_DAYS};
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::vault::{VaultMap, project_identity};
use crate::core::hive::{HiveError, HiveStore};
use lean_ctx_protocol::hive::HiveUploadKind;
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, HiveRight};
use lean_ctx_protocol::hive::telemetry::{METRICS_SCHEMA, MetricsAckV1, MetricsUploadV1};
use lean_ctx_protocol::hive::vault::stable_identity;

/// What a metrics run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsRun {
    /// Not due yet (hourly).
    NotDue,
    /// Not allowed now; the reason is shown in `hive status`.
    Off(&'static str),
    /// Sent: day entries Hive stored.
    Sent { days: u64 },
    /// Nothing with activity to send.
    Empty,
}

/// Why metrics are not sent, or `None` when everything allows them.
pub(crate) fn blocked_by(
    config: &HiveConfig,
    lease: &HiveLeaseV1,
    state: &UploadState,
) -> Option<&'static str> {
    if !config.metrics_upload {
        Some("switched off on this engine ([hive] metrics_upload = false)")
    } else if !lease.grants(HiveRight::MetricsUpload) {
        Some("the lease does not include metrics.upload")
    } else if !lease.org_allows_metrics_upload() {
        Some("switched off by the workspace")
    } else if state.disabled_under_lease.as_deref() == Some(lease.lease_id.as_str()) {
        Some("switched off by the workspace (Hive refused the upload)")
    } else {
        // K2 review (C1): the daily upload is retired. It ignored the
        // "Share engine metrics" rule and the Personal confirmation;
        // engine metrics v2 (hourly, gated) carry the same figures.
        Some(RETIRED)
    }
}

/// Why the daily upload no longer runs.
pub(crate) const RETIRED: &str = "replaced by hourly engine metrics (see Engine metrics)";

fn metrics_dir(store: &HiveStore) -> std::path::PathBuf {
    store.data_dir().join("metrics")
}

/// The Hive project id of `root`: a checkout Hive resolved, or a root with
/// the same stable identity as one.
fn project_id_of(map: &VaultMap, root: &Path) -> Option<String> {
    let text = root.to_string_lossy();
    if let Some(record) = map
        .vaults
        .values()
        .find(|record| record.roots.iter().any(|known| *known == text))
    {
        return Some(record.vault_id.clone());
    }
    let stable = stable_identity(&project_identity(root)?);
    map.vaults
        .get(&stable)
        .map(|record| record.vault_id.clone())
}

/// One run at `now`; `force` skips the hourly debounce (`hive sync`).
pub fn run(
    session: &Session,
    config: &HiveConfig,
    now: DateTime<Utc>,
    force: bool,
) -> Result<MetricsRun, HiveError> {
    let dir = metrics_dir(&session.store);
    let mut state = local_state::load_state_at(&dir);
    let lease = &session.lease.lease;
    // A newer lease that allows the upload again lifts an earlier refusal.
    if state
        .disabled_under_lease
        .as_deref()
        .is_some_and(|id| id != lease.lease_id && lease.org_allows_metrics_upload())
    {
        state.disabled_under_lease = None;
    }
    if let Some(reason) = blocked_by(config, lease, &state) {
        return Ok(MetricsRun::Off(reason));
    }
    if !super::due(state.last_run, now.timestamp(), force) {
        return Ok(MetricsRun::NotDue);
    }
    let map = VaultMap::load(session.store.data_dir());
    let ids = |root: &Path| project_id_of(&map, root);
    let days = if state.backfill_done {
        RECENT_DAYS
    } else {
        BACKFILL_DAYS
    };
    let uploads = super::build_uploads(&dir, &ids, days, config.vault_labels, now)
        .map_err(HiveError::Local)?;
    let mut stored = 0u64;
    for upload in &uploads {
        let body = serde_json::to_value(upload).map_err(|e| HiveError::Local(e.to_string()))?;
        // The builder's contract rules are stricter; this is the wire shape.
        let wire: MetricsUploadV1 =
            serde_json::from_value(body.clone()).map_err(|e| HiveError::Local(e.to_string()))?;
        debug_assert_eq!(wire.schema, METRICS_SCHEMA);
        wire.validate()
            .map_err(|e| HiveError::Local(format!("metrics withheld: {e}")))?;
        match session.with_client(|client| client.post("/metrics", &body)) {
            Ok(answer) => {
                let ack: MetricsAckV1 = serde_json::from_value(answer)
                    .map_err(|e| HiveError::Protocol(format!("metrics ack: {e}")))?;
                stored += u64::from(ack.accepted);
            }
            Err(HiveError::UploadDisabled(HiveUploadKind::Metrics | HiveUploadKind::Unknown)) => {
                state.disabled_under_lease = Some(lease.lease_id.clone());
                local_state::store_state_at(&dir, &state)
                    .map_err(|e| HiveError::Local(e.to_string()))?;
                return Ok(MetricsRun::Off(
                    "switched off by the workspace (Hive refused the upload)",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    state.last_run = Some(now.timestamp());
    if !uploads.is_empty() {
        state.last_success = Some(now.timestamp());
        state.last_days = stored;
    }
    state.backfill_done = true;
    local_state::store_state_at(&dir, &state).map_err(|e| HiveError::Local(e.to_string()))?;
    Ok(if uploads.is_empty() {
        MetricsRun::Empty
    } else {
        MetricsRun::Sent { days: stored }
    })
}

/// Whether metrics would be sent now, for `hive status` (local only).
pub fn status(store: &HiveStore, config: &HiveConfig, lease: &HiveLeaseV1) -> Option<&'static str> {
    blocked_by(
        config,
        lease,
        &local_state::load_state_at(&metrics_dir(store)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hive::vault::VaultRecord;
    use lean_ctx_protocol::UtcTimestamp;
    use lean_ctx_protocol::hive::lease::{HiveOrgSettingsV1, HivePlan, LEASE_SCHEMA};

    fn lease(rights: Vec<HiveRight>, metrics: Option<bool>) -> HiveLeaseV1 {
        HiveLeaseV1 {
            schema: LEASE_SCHEMA.into(),
            lease_id: "lse_1".into(),
            org_id: "org_acme".into(),
            instance_id: "inst_1".into(),
            plan: HivePlan::Team,
            seats_limit: 25,
            rights,
            policy_signer_keys: vec![],
            issued_at: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
            expires_at: UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
            grace_until: UtcTimestamp::new("2026-10-27T12:00:00Z").unwrap(),
            server_time: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
            org_settings: metrics.map(|metrics_upload| HiveOrgSettingsV1 {
                metrics_upload,
                receipts_upload: true,
            }),
        }
    }

    #[test]
    fn engine_lease_and_organization_must_all_allow_metrics() {
        let on = HiveConfig::default();
        let state = UploadState::default();
        let allowed = lease(vec![HiveRight::MetricsUpload], None);
        assert_eq!(blocked_by(&on, &allowed, &state), Some(RETIRED));
        assert_eq!(
            blocked_by(
                &on,
                &lease(vec![HiveRight::MetricsUpload], Some(true)),
                &state
            ),
            Some(RETIRED)
        );
        let off = HiveConfig {
            metrics_upload: false,
            ..HiveConfig::default()
        };
        assert!(blocked_by(&off, &allowed, &state).is_some_and(|r| r.contains("this engine")));
        assert!(blocked_by(&on, &lease(vec![], None), &state).is_some_and(|r| r.contains("lease")));
        assert!(
            blocked_by(
                &on,
                &lease(vec![HiveRight::MetricsUpload], Some(false)),
                &state
            )
            .is_some_and(|r| r.contains("workspace"))
        );
        let refused = UploadState {
            disabled_under_lease: Some("lse_1".into()),
            ..UploadState::default()
        };
        assert!(blocked_by(&on, &allowed, &refused).is_some_and(|r| r.contains("refused")));
    }

    #[test]
    fn only_projects_hive_resolved_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("payments");
        std::fs::create_dir_all(&checkout).unwrap();
        let mut map = VaultMap::default();
        map.vaults.insert(
            "stable".into(),
            VaultRecord {
                vault_id: "0123456789abcdef0123456789abcdef".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![checkout.to_string_lossy().into_owned()],
            },
        );
        assert_eq!(
            project_id_of(&map, &checkout).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(project_id_of(&map, &dir.path().join("other")), None);
    }
}
