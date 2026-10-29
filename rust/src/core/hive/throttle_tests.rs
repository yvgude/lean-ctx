// SPDX-License-Identifier: Apache-2.0
//! Throttles of quota and policy refusals (Release B, API §7.2, §8.1).

use serde_json::json;

use super::*;

fn store() -> (tempfile::TempDir, HiveStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    (dir, store)
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn fixture_quota() -> HiveError {
    let body: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes/quota_exceeded.error.json"
    ))
    .unwrap();
    HiveError::from_response(429, &body)
}

#[test]
fn a_monthly_quota_holds_sync_for_retry_after_and_says_why() {
    let (_dir, store) = store();
    let now = at("2026-10-31T00:00:00Z");
    let error = fixture_quota();
    assert!(error.is_transient(), "a monthly quota is waited out");
    assert_eq!(
        error.to_string(),
        "this workspace has used all its monthly transfer (53687091200 of 53687091200); \
         nothing is charged for more; uploads wait in the local outbox until 2026-11-01T00:00:00Z"
    );
    let back = note(&store, Scope::Sync, error.clone(), now);
    assert_eq!(back, error, "the error passes unchanged");
    let held = holding(&store, Scope::Sync, now).expect("sync held");
    assert_eq!(held.until, "2026-11-01T00:00:00Z", "retry_after_s 86400");
    assert_eq!(held.scopes, ["sync"]);
    assert!(
        holding(&store, Scope::Bus, now).is_none(),
        "bus messages are another quota"
    );
    assert!(check(&store, Scope::Sync, now).is_err());
    assert!(
        check(&store, Scope::Sync, now)
            .unwrap_err()
            .to_string()
            .starts_with("waiting for Hive until 2026-11-01T00:00:00Z: this workspace has used all its monthly transfer")
    );
    // A bus publish that meets transfer too holds both.
    let _ = note(&store, Scope::Bus, error.clone(), now);
    assert_eq!(
        holding(&store, Scope::Bus, now).unwrap().scopes,
        ["bus", "sync"]
    );
    let lines = status_lines(&store, now);
    assert_eq!(lines, status_lines(&store, now), "deterministic");
    assert_eq!(lines.len(), 1, "one throttle per quota");
    assert!(
        lines[0].starts_with("Throttled:    bus, sync until 2026-11-01T00:00:00Z: this workspace")
    );
    // Over at its end: nothing holds, nothing is shown.
    let later = at("2026-11-01T00:00:01Z");
    assert!(holding(&store, Scope::Sync, later).is_none());
    assert!(status_lines(&store, later).is_empty());
}

#[test]
fn without_retry_after_a_monthly_quota_waits_for_reset_at_or_the_next_month() {
    let now = at("2026-12-15T10:00:00Z");
    let quota = |extra: serde_json::Value| {
        let mut body = json!({"error": {"code": "quota_exceeded", "message": "x",
            "resource": "bus_messages", "limit": 250000, "used": 250000}});
        for (k, v) in extra.as_object().unwrap() {
            body["error"][k] = v.clone();
        }
        HiveError::from_response(429, &body)
    };
    let (_, t) = of_error(
        Some(Scope::Bus),
        &quota(json!({"reset_at": "2027-01-01T00:00:00Z"})),
        now,
    )
    .unwrap();
    assert_eq!(t.until, "2027-01-01T00:00:00Z");
    let (key, t) = of_error(Some(Scope::Bus), &quota(json!({})), now).unwrap();
    assert_eq!(key, "quota:bus_messages");
    assert_eq!(
        t.until, "2027-01-01T00:00:00Z",
        "the calendar month resets (UTC)"
    );
    // Never a busy loop, never longer than 32 days.
    let (_, t) = of_error(Some(Scope::Bus), &quota(json!({"retry_after_s": 1})), now).unwrap();
    assert_eq!(t.until, "2026-12-15T10:01:00Z");
    let (_, t) = of_error(
        Some(Scope::Bus),
        &quota(json!({"retry_after_s": 99_999_999u64})),
        now,
    )
    .unwrap();
    assert_eq!(t.until, "2027-01-16T10:00:00Z");
}

#[test]
fn a_cooling_pro_slot_holds_connect_and_points_to_the_web_recovery() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    let error = HiveError::from_response(
        409,
        &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": "registered_engines",
            "limit": 3, "used": 3, "reset_at": "2026-11-02T08:00:00.000Z", "cooldown_days": 7}}),
    );
    assert!(!error.is_transient(), "waiting alone is days, not a retry");
    assert_eq!(
        error.to_string(),
        format!(
            "this workspace has used all its engine slots (3 of 3); nothing is charged for more; \
             a revoked engine's slot frees at 2026-11-02T08:00:00Z (7-day cool-down), or {}",
            crate::core::hive::error::RECOVER_HINT
        )
    );
    let _ = note(&store, Scope::Connect, error, now);
    let held = holding(&store, Scope::Connect, now).unwrap();
    assert_eq!(held.until, "2026-11-02T08:00:00Z");
    assert!(status_lines(&store, now)[0].contains("Settings › Engines › Recover"));
    clear_connect(&store);
    assert!(holding(&store, Scope::Connect, now).is_none());
}

#[test]
fn counted_quotas_without_a_reset_are_shown_not_retried() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    for resource in ["private_vaults", "registered_engines"] {
        let error = HiveError::from_response(
            409,
            &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": resource,
                "limit": 2, "used": 2, "reset_at": null}}),
        );
        assert!(!error.is_transient(), "{resource}");
        assert_eq!(error.retry_after_at(now), None);
        assert!(
            of_error(Some(Scope::Sync), &error, now).is_none(),
            "{resource}"
        );
        let _ = note(&store, Scope::Sync, error, now);
    }
    assert!(status_lines(&store, now).is_empty());
}

#[test]
fn a_policy_refusal_holds_its_path_for_a_while_and_a_success_ends_it() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    let error = HiveError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x",
            "member": "sharing.task_claim_min_role", "level": "workspace"}}),
    );
    assert!(!error.is_transient());
    assert_eq!(
        error.to_string(),
        "Hive policy refuses this (sharing.task_claim_min_role, set at the workspace level); \
         it is not retried until an admin changes the policy in Hive"
    );
    let _ = note(&store, Scope::Tasks, error, now);
    assert_eq!(
        holding(&store, Scope::Tasks, now).unwrap().until,
        "2026-10-26T12:15:00Z"
    );
    assert!(
        holding(&store, Scope::Sync, now).is_none(),
        "only the refused path"
    );
    passed(&store, Scope::Tasks);
    assert!(holding(&store, Scope::Tasks, now).is_none());
    // A consent refusal names the action a person takes.
    let consent = HiveError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x",
            "member": "sharing.consent_required_for", "level": "org", "action": "trust_boundary"}}),
    );
    assert_eq!(
        consent.to_string(),
        "Hive policy refuses this (sharing.consent_required_for, set at the org level); \
         a person does this on the Hive web (trust boundary)"
    );
    // Values that are no wire tokens never reach a terminal.
    let odd = HiveError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x", "member": "x\u{1b}[31m"}}),
    );
    assert_eq!(
        odd.to_string(),
        "Hive policy refuses this; it is not retried until an admin changes the policy in Hive"
    );
}

/// A huge `retry_after_s` (an HTTP 429 body or a quota built by hand) is
/// clamped before any time arithmetic: the longest wait, never a panic.
#[test]
fn a_huge_retry_after_holds_for_the_longest_wait_and_never_panics() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    let longest = stamp(now + chrono::Duration::seconds(MAX_WAIT_S));
    for huge in [
        u64::MAX,
        i64::MAX.unsigned_abs(),
        i64::MAX.unsigned_abs() / 1000,
    ] {
        let http = HiveError::from_response(
            429,
            &json!({"error": {"code": "quota_exceeded", "message": "x",
                "resource": "bus_messages", "retry_after_s": huge}}),
        );
        let back = note_on(&store, None, http, now);
        assert!(matches!(back, HiveError::QuotaExceeded(_)));
        let held = holding(&store, Scope::Bus, now).expect("bus held");
        assert_eq!(held.until, longest, "from_response, {huge}");

        let by_hand = HiveError::QuotaExceeded(Box::new(crate::core::hive::error::Quota {
            status: 429,
            resource: "transfer_bytes".into(),
            limit: None,
            used: None,
            reset_at: None,
            retry_after: Some(huge),
            cooldown_days: None,
        }));
        let (_, throttle) = of_error(Some(Scope::Sync), &by_hand, now).expect("a throttle");
        assert_eq!(throttle.until, longest, "of_error, {huge}");
    }
}

/// E2E-FINAL: a used-up month held sync until its end even after the usage
/// was lowered. A quota throttle now lets one call through every hour; its
/// success ends the throttle, a new refusal records it again; a newer plan
/// ends every quota throttle; policy holds never probe.
#[test]
fn a_quota_throttle_probes_hourly_and_ends_on_success_or_a_new_plan() {
    let (_dir, store) = store();
    let now = at("2026-10-10T12:00:00Z");
    let month = HiveError::from_response(
        429,
        &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": "transfer_bytes"}}),
    );
    note_on(&store, None, month.clone(), now);
    assert!(quota_holds(&store, Scope::Sync, now));
    assert!(check(&store, Scope::Sync, now).is_err(), "held");
    let soon = now + chrono::Duration::seconds(QUOTA_PROBE_S - 1);
    assert!(
        check(&store, Scope::Sync, soon).is_err(),
        "no probe within the hour"
    );
    let hour = now + chrono::Duration::seconds(QUOTA_PROBE_S);
    assert!(
        check(&store, Scope::Sync, hour).is_ok(),
        "one probe after an hour"
    );
    assert!(
        check(&store, Scope::Sync, hour).is_err(),
        "the next probe is an hour later"
    );
    // The probe was refused again: held, next probe an hour on.
    note_on(&store, None, month.clone(), hour);
    assert!(check(&store, Scope::Sync, hour).is_err());
    // A probe that passed ends it.
    let later = hour + chrono::Duration::seconds(QUOTA_PROBE_S);
    assert!(check(&store, Scope::Sync, later).is_ok());
    passed(&store, Scope::Sync);
    assert!(
        holding(&store, Scope::Sync, later).is_none(),
        "the quota is back"
    );
    // A newer plan ends quota throttles, never policy holds.
    note_on(&store, None, month, later);
    let policy = HiveError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x", "member": "sharing.bus"}}),
    );
    note(&store, Scope::Bus, policy, later);
    plan_changed(&store);
    assert!(!quota_holds(&store, Scope::Sync, later));
    assert!(holding(&store, Scope::Sync, later).is_none());
    assert!(holding(&store, Scope::Bus, later).is_some(), "policy stays");
    // A policy hold never lets a probe through, however long it waits.
    assert!(
        check(
            &store,
            Scope::Bus,
            later + chrono::Duration::seconds(POLICY_HOLD_S - 1)
        )
        .is_err()
    );
}
