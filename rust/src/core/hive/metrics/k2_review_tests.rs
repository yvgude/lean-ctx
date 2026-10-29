// SPDX-License-Identifier: Apache-2.0
//! K2 review (gate G1): the C0 1.x `POST /v1/metrics` body still runs on
//! every scheduler tick next to engine metrics v2. Content injected into its
//! sources must not reach the body.
//!
//! Tests named `k2_finding_*` pin a defect the review found; they fail until
//! it is fixed.

use std::path::Path;

use chrono::{NaiveDate, TimeZone, Utc};

use crate::core::hive::config::HiveConfig;

const MARK: &str = "k2canary";
const VAULT: &str = "0123456789abcdef0123456789abcdef";

/// `device.label` is the machine's host name (`gethostname`), sent with
/// every upload whatever the configuration says.
#[test]
fn k2_finding_v1_device_label_is_the_host_name() {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let device = super::device("6f1d2c3b-4a5e-4f60-8a7b-9c0d1e2f3a4b".into()).unwrap();
    let body = serde_json::to_string(&device).unwrap();
    assert!(
        host.trim().is_empty() || !body.contains(host.trim()),
        "the host name {host:?} leaves the engine as device.label: {body}"
    );
}

fn poisoned_checkout(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let data = dir.join("data");
    let repo = dir.join(format!("{MARK}-merger-2027"));
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(data.join("sessions")).unwrap();
    let mut s = crate::core::session::SessionState::new();
    s.id = "sess_1".into();
    s.project_root = Some(repo.to_string_lossy().into_owned());
    s.started_at = Utc.with_ymd_and_hms(2026, 10, 26, 16, 0, 0).unwrap();
    s.stats.client = Some("claude-code".into());
    std::fs::write(
        data.join("sessions").join("sess_1.json"),
        serde_json::to_string(&s).unwrap(),
    )
    .unwrap();
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(&repo);
    let hash = crate::core::savings_ledger::hash_folder(&canonical.to_string_lossy());
    let line = serde_json::json!({
        "ts": "2026-10-26T16:05:00+00:00", "tool": "ctx_read", "baseline_tokens": 100,
        "actual_tokens": 10, "saved_tokens": 90, "repo_hash": hash,
    });
    std::fs::create_dir_all(data.join("savings")).unwrap();
    std::fs::write(
        data.join("savings").join("ledger.jsonl"),
        format!("{line}\n"),
    )
    .unwrap();
    (data, repo)
}

/// With the shipped default `[hive] vault_labels = true`, the checkout's
/// folder name goes out as `days[].vault.label`.
#[test]
fn k2_finding_v1_day_names_the_checkout_folder_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (data, _repo) = poisoned_checkout(dir.path());
    let ids = |_: &Path| Some(VAULT.to_string());
    let days = super::collect::collect(
        &super::collect::Sources {
            data: &data,
            home: None,
        },
        super::collect::Window::ending(NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(), 3),
        &ids,
        HiveConfig::default().vault_labels,
    );
    assert!(!days.is_empty(), "the poisoned day has activity");
    let body = serde_json::to_string(&days).unwrap();
    assert!(
        !body.contains(MARK),
        "the checkout folder name leaves the engine: {body}"
    );
}

/// Without labels the same day carries no folder name (the switch works).
#[test]
fn v1_day_without_labels_carries_no_folder_name() {
    let dir = tempfile::tempdir().unwrap();
    let (data, _repo) = poisoned_checkout(dir.path());
    let ids = |_: &Path| Some(VAULT.to_string());
    let days = super::collect::collect(
        &super::collect::Sources {
            data: &data,
            home: None,
        },
        super::collect::Window::ending(NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(), 3),
        &ids,
        false,
    );
    assert!(!days.is_empty());
    assert!(!serde_json::to_string(&days).unwrap().contains(MARK));
}
