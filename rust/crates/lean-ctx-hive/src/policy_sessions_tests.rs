// SPDX-License-Identifier: Apache-2.0
use chrono::TimeZone as _;

use super::*;

fn install(hive_data: &Path, version: u64, not_after: &str) {
    let dir = effective_floor::floor_dir_in(hive_data);
    let floor = serde_json::json!({
        "schema": effective_floor::FLOOR_SCHEMA,
        "version": version, "digest": format!("sha256:{}", "a".repeat(64)),
        "not_after": not_after,
        "policy": {"effective": {"schema": "leanctx.hive.policy/v1", "security": {}, "context": {},
                                 "sharing": {}},
                   "vaults": []}
    });
    write_private(
        &dir,
        effective_floor::FLOOR_FILE,
        &serde_json::to_vec(&floor).unwrap(),
    )
    .unwrap();
    write_private(
        &dir,
        effective_floor::REQUIRED_FILE,
        format!(r#"{{"version":{version}}}"#).as_bytes(),
    )
    .unwrap();
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap()
}

#[test]
fn session_ids_follow_the_app_rule() {
    assert_eq!(session_id(" app-7f3a ").as_deref(), Some("app-7f3a"));
    assert_eq!(session_id("a.b_c:d-1").as_deref(), Some("a.b_c:d-1"));
    assert_eq!(session_id(""), None);
    assert_eq!(session_id("../x"), None);
    assert_eq!(session_id("a b"), None);
    assert_eq!(session_id(&"x".repeat(129)), None);
    assert!(session_id(&"x".repeat(128)).is_some());
}

#[test]
fn the_installed_policy_says_verified_none_invalid_or_expired() {
    let dir = tempfile::tempdir().unwrap();
    let none = installed(dir.path(), now());
    assert_eq!(
        (none.signature.as_str(), none.state.as_str()),
        ("none", "none")
    );
    install(dir.path(), 3, "2099-01-01T00:00:00Z");
    let ok = installed(dir.path(), now());
    assert_eq!(ok.version, Some(3));
    assert_eq!(
        (ok.signature.as_str(), ok.state.as_str()),
        ("verified", "in_force")
    );
    assert!(!ok.acked, "no ack recorded");
    install(dir.path(), 4, "2026-01-01T00:00:00Z");
    assert_eq!(installed(dir.path(), now()).state, "expired");
    std::fs::write(
        effective_floor::floor_dir_in(dir.path()).join(effective_floor::FLOOR_FILE),
        b"{damaged",
    )
    .unwrap();
    assert_eq!(installed(dir.path(), now()).signature, "invalid");
}

#[test]
fn a_new_policy_opens_a_new_row_and_rows_stay_bounded() {
    let policy = |version: u64| Installed {
        version: Some(version),
        digest: Some(format!("d{version}")),
        signature: "verified".into(),
        state: "in_force".into(),
        ..Installed::default()
    };
    let mut evidence = SessionEvidence::default();
    evidence.note(&policy(1), "r1", "t1");
    evidence.note(&policy(1), "r2", "t2");
    evidence.note(&policy(2), "r3", "t3");
    assert_eq!(evidence.applied.len(), 2);
    let first = &evidence.applied[0];
    assert_eq!(
        (
            first.receipts,
            first.first_at.as_str(),
            first.last_at.as_str()
        ),
        (2, "t1", "t2")
    );
    assert_eq!(first.receipt_ids, ["r1", "r2"]);
    for n in 0..100 {
        evidence.note(&policy(2), &format!("x{n}"), "t");
    }
    let last = evidence.applied.last().unwrap();
    assert_eq!(last.receipts, 101);
    assert_eq!(last.receipt_ids.len(), MAX_RECEIPT_IDS);
    assert_eq!(last.receipt_ids.last().map(String::as_str), Some("x99"));
    for version in 3..40 {
        evidence.note(&policy(version), "r", "t");
    }
    assert_eq!(evidence.applied.len(), MAX_ROWS);
    assert_eq!(evidence.applied.last().unwrap().version, Some(39));
}

#[test]
fn receipts_are_recorded_per_session_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), 7, "2099-01-01T00:00:00Z");
    record(dir.path(), "app-b", "r1", now());
    record(dir.path(), "app-a", "r2", now());
    record(dir.path(), "app-a", "r3", now());
    record(dir.path(), "../evil", "r4", now());
    let all = list(dir.path());
    let ids: Vec<_> = all.iter().map(|e| e.session_id.as_str()).collect();
    assert_eq!(ids, ["app-a", "app-b"]);
    let a = load(dir.path(), "app-a").unwrap();
    assert_eq!(a.applied.len(), 1);
    assert_eq!(a.applied[0].version, Some(7));
    assert_eq!(a.applied[0].receipts, 2);
    assert!(load(dir.path(), "app-c").is_none());
    assert!(load(dir.path(), "../evil").is_none());
}

#[test]
fn only_the_newest_session_files_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("s");
    for n in 0..5 {
        write_private(&sessions, &format!("s{n}.json"), b"{}").unwrap();
        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + n);
        std::fs::File::options()
            .write(true)
            .open(sessions.join(format!("s{n}.json")))
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    prune(&sessions, 3);
    let mut left: Vec<_> = std::fs::read_dir(&sessions)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["s2.json", "s3.json", "s4.json"]);
}
