// SPDX-License-Identifier: Apache-2.0
//! The local record of a revocation (C0 2.0 E12, `401 instance_revoked`).
//!
//! Once Hive says this instance was revoked, the engine records it and
//! fails closed: [`super::auth::Session::load`] answers the revocation for
//! that instance without contacting Hive, so sync, policy, metrics, the
//! scheduler and every CLI path stop at once instead of retrying (BAUPLAN
//! v1.1 §4.3: revocation stops future authorised access). `hive status`
//! shows it. A new `lean-ctx hive connect` clears it.

use serde::{Deserialize, Serialize};

use super::error::Revocation;
use super::state::{HiveStore, InstanceRecord};
use super::{HiveError, workers::canonical_instance_id};

pub(crate) const REVOKED_FILE: &str = "revoked.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RevokedRecord {
    /// Canonical id of the revoked instance.
    instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<u64>,
}

/// The revocation as this instance shows it: with its workspace's name.
fn named(instance: &InstanceRecord, mut r: Revocation) -> HiveError {
    r.workspace = Some(instance.org_name.clone()).filter(|n| !n.trim().is_empty());
    HiveError::Revoked(r)
}

/// Record `error` when it is a revocation of `instance` (a statement for
/// another instance id is not this engine's and is only passed on), and
/// return it named. Any other error passes unchanged.
pub(crate) fn note(store: &HiveStore, instance: &InstanceRecord, error: HiveError) -> HiveError {
    let HiveError::Revoked(r) = error else {
        return error;
    };
    let own = canonical_instance_id(&instance.instance_id);
    let ours = r
        .instance_id
        .as_deref()
        .is_none_or(|id| canonical_instance_id(id) == own);
    if ours {
        let record = RevokedRecord {
            instance_id: own,
            reason: r.reason.clone(),
            revoked_at: r.revoked_at,
        };
        if let Err(e) = store.write_config(REVOKED_FILE, &record) {
            tracing::warn!("hive: could not record the revocation: {e}");
        }
        // N1: a revoked engine records nothing more for Hive.
        super::receipts::stop_local_recording(store.data_dir());
    }
    named(instance, r)
}

/// The recorded revocation of `instance`, if any.
pub(crate) fn recorded(store: &HiveStore, instance: &InstanceRecord) -> Option<HiveError> {
    let record = store
        .read_config::<RevokedRecord>(REVOKED_FILE)
        .ok()
        .flatten()?;
    (record.instance_id == canonical_instance_id(&instance.instance_id)).then(|| {
        named(
            instance,
            Revocation {
                workspace: None,
                reason: record.reason,
                revoked_at: record.revoked_at,
                instance_id: Some(record.instance_id),
            },
        )
    })
}

/// A new enrolment starts without the old revocation.
pub(crate) fn clear(store: &HiveStore) -> Result<(), HiveError> {
    store.remove_config(REVOKED_FILE)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::hive::config::HiveConfig;

    const FIXTURE: &str = include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/instance_revoked.error.json"
    );

    fn instance(id: &str) -> InstanceRecord {
        InstanceRecord {
            instance_id: id.into(),
            org_id: "4b6d8f0a-2c4e-4f6a-8b0c-2e4f6a8c0e1a".into(),
            org_name: "acme".into(),
            label: "laptop".into(),
            url: "https://hive.example".into(),
            org_salt_id: "s".into(),
            enrolled_at: "2026-10-25T10:00:00Z".into(),
            web_url: None,
            org_kind: None,
            company_name: None,
        }
    }

    /// The C0 error body: one clear line, not "Hive answered 401 (unknown)".
    #[test]
    fn the_c0_revocation_body_is_one_clear_refusal() {
        let body: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let error = HiveError::from_response(401, &body);
        let HiveError::Revoked(r) = &error else {
            panic!("{error:?}");
        };
        assert_eq!(r.reason.as_deref(), Some("key_compromise"));
        assert_eq!(r.revoked_at, Some(1_792_800_500_000));
        assert!(!error.is_transient(), "never retried");
        assert_eq!(
            error.to_string(),
            "this engine was revoked by its workspace (key compromise); reconnect with \
             `lean-ctx hive connect` after an admin allows it"
        );
        // A 1.x Hive' unknown codes stay tolerated.
        let newer = HiveError::from_response(
            401,
            &json!({"error": {"code": "brand_new", "message": "x"}}),
        );
        assert!(matches!(newer, HiveError::Server { status: 401, .. }));
    }

    /// Recorded once, then every session load fails closed with it (so
    /// `hive status` shows it instead of a valid lease) until a connect.
    #[test]
    fn a_noted_revocation_closes_the_session_until_a_new_connect() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let own = instance("inst_8e3c0d5f2b4e4f609b8c1d2e3f4a5b6c");
        store
            .write_config(crate::core::hive::state::INSTANCE_FILE, &own)
            .unwrap();
        assert!(recorded(&store, &own).is_none());

        // A statement for another instance is not this engine's.
        let other = HiveError::Revoked(Revocation {
            instance_id: Some("11111111-2222-4333-8444-555555555555".into()),
            ..Revocation::default()
        });
        let _ = note(&store, &own, other);
        assert!(recorded(&store, &own).is_none());

        let body: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let shown = note(&store, &own, HiveError::from_response(401, &body));
        assert!(
            shown
                .to_string()
                .starts_with("this engine was revoked by acme")
        );
        let again = recorded(&store, &own).expect("recorded");
        assert_eq!(again, shown);

        // Session::load answers it without any network.
        let error = crate::core::hive::auth::Session::load(store.clone(), [7; 32])
            .err()
            .expect("closed");
        assert_eq!(error, shown);
        let status =
            crate::core::hive::status::report(&store, &HiveConfig::default(), chrono::Utc::now());
        assert!(!status.connected());
        assert!(status.lease.is_none(), "no valid lease shown");
        assert_eq!(status.problem.as_deref(), Some(shown.to_string().as_str()));

        // A new enrolment (another id, or the same after `clear`) is open.
        assert!(recorded(&store, &instance("inst_00000000000040008000000000000001")).is_none());
        clear(&store).unwrap();
        assert!(recorded(&store, &own).is_none());
    }

    /// K2 review 2 (N1): a recorded revocation stops every local recording
    /// for Hive at once, and `may_record` says no while it stands.
    #[test]
    fn a_revocation_stops_metrics_and_receipts_recording() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("d");
        let store = HiveStore::at(dir.path().join("c"), data.clone());
        let own = instance("inst_8e3c0d5f2b4e4f609b8c1d2e3f4a5b6c");
        store
            .write_config(crate::core::hive::state::INSTANCE_FILE, &own)
            .unwrap();
        crate::core::hive::metrics_v2::secrets::enable(&data).unwrap();
        crate::core::hive::state::write_private(&data, "receipts.enabled", b"1").unwrap();
        assert!(store.may_record());
        let body: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let _ = note(&store, &own, HiveError::from_response(401, &body));
        assert!(!crate::core::hive::metrics_v2::secrets::enabled(&data));
        assert!(!data.join("receipts.enabled").is_file());
        assert!(!store.may_record(), "revoked: nothing records");
    }
}
