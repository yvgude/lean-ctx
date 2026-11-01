// SPDX-License-Identifier: Apache-2.0
//! The retired daily `POST /v1/metrics` upload: only the reason `hive
//! status` names for it is left. Hive 1.x is retired (owner decision
//! 2026-10-31) and its route is gone; engine metrics v2 (hourly, gated)
//! carry the same figures.
//!
//! The reason still follows `[hive] metrics_upload`, the lease's
//! `metrics.upload` right, the organization's switch in the lease
//! (`org_settings.metrics_upload`, absent = on) and an `upload_disabled`
//! answer recorded under the current lease, so the status line keeps
//! saying who switched it off.

use super::local_state::{self, UploadState};
use crate::core::hive::HiveStore;
use crate::core::hive::config::HiveConfig;
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, HiveRight};

/// Why metrics are not sent; never `None` since the upload is retired.
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

/// Why metrics are not sent, for `hive status` (local only).
pub fn status(store: &HiveStore, config: &HiveConfig, lease: &HiveLeaseV1) -> Option<&'static str> {
    blocked_by(
        config,
        lease,
        &local_state::load_state_at(&store.data_dir().join("metrics")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        assert!(blocked_by(&on, &allowed, &refused).is_some_and(|r| r.contains("refused")));
    }
}
