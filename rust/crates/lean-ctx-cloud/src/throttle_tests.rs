// SPDX-License-Identifier: Apache-2.0
//! Throttles of quota and policy refusals (Release B, API §7.2, §8.1).

use serde_json::json;

use super::*;

fn store() -> (tempfile::TempDir, CloudStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    (dir, store)
}

fn at(s: &str) -> lean_ctx_time::UtcDateTime {
    lean_ctx_time::OffsetDateTime::parse_from_rfc3339(s)
        .unwrap()
        .to_utc()
}

fn fixture_quota() -> CloudError {
    let body: serde_json::Value = serde_json::from_str(include_str!(
        "../../../crates/lean-ctx-protocol/tests/fixtures/cloud-v2/routes/quota_exceeded.error.json"
    ))
    .unwrap();
    CloudError::from_response(429, &body)
}

#[test]
fn a_monthly_quota_holds_sync_for_retry_after_and_says_why() {
    let (_dir, store) = store();
    let now = at("2026-10-31T00:00:00Z");
    let error = fixture_quota();
    assert!(error.is_transient(), "a monthly quota is waited out");
    assert_eq!(
        error.to_string(),
        "this workspace has used all its monthly transfer (40 GiB of 40 GiB); \
         nothing is charged for more; uploads wait in the local outbox until 2026-11-01T00:00:00Z; \
         Team gives every seat its own allowance"
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
            .starts_with("waiting for LeanCTX Cloud until 2026-11-01T00:00:00Z: this workspace has used all its monthly transfer")
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
        CloudError::from_response(429, &body)
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
    let error = CloudError::from_response(
        409,
        &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": "registered_instances",
            "limit": 3, "used": 3, "reset_at": "2026-11-02T08:00:00.000Z", "cooldown_days": 7}}),
    );
    assert!(!error.is_transient(), "waiting alone is days, not a retry");
    assert_eq!(
        error.to_string(),
        format!(
            "all hosts the plan includes are registered (3 of 3); nothing is charged for more; \
             a revoked host stays registered until 2026-11-02T08:00:00Z (7-day cool-down), or {}",
            crate::error::RECOVER_HINT
        )
    );
    let _ = note(&store, Scope::Connect, error, now);
    let held = holding(&store, Scope::Connect, now).unwrap();
    assert_eq!(held.until, "2026-11-02T08:00:00Z");
    assert!(status_lines(&store, now)[0].contains("Hosts › Recover the slot now"));
    clear_connect(&store);
    assert!(holding(&store, Scope::Connect, now).is_none());
}

#[test]
fn counted_quotas_without_a_reset_are_shown_not_retried() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    for resource in ["private_vaults", "registered_instances"] {
        let error = CloudError::from_response(
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
    let error = CloudError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x",
            "member": "sharing.task_claim_min_role", "level": "workspace"}}),
    );
    assert!(!error.is_transient());
    assert_eq!(
        error.to_string(),
        "LeanCTX Cloud policy refuses this (sharing.task_claim_min_role, set at the workspace level); \
         it is not retried until an admin changes the policy in LeanCTX Cloud"
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
    let consent = CloudError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x",
            "member": "sharing.consent_required_for", "level": "org", "action": "trust_boundary"}}),
    );
    assert_eq!(
        consent.to_string(),
        "LeanCTX Cloud policy refuses this (sharing.consent_required_for, set at the org level); \
         a person does this on the LeanCTX Cloud web (trust boundary)"
    );
    // Values that are no wire tokens never reach a terminal.
    let odd = CloudError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x", "member": "x\u{1b}[31m"}}),
    );
    assert_eq!(
        odd.to_string(),
        "LeanCTX Cloud policy refuses this; it is not retried until an admin changes the policy in LeanCTX Cloud"
    );
}

/// A huge `retry_after_s` (an HTTP 429 body or a quota built by hand) is
/// clamped before any time arithmetic: the longest wait, never a panic.
#[test]
fn a_huge_retry_after_holds_for_the_longest_wait_and_never_panics() {
    let (_dir, store) = store();
    let now = at("2026-10-26T12:00:00Z");
    let longest = stamp(now + lean_ctx_time::Duration::seconds(MAX_WAIT_S));
    for huge in [
        u64::MAX,
        i64::MAX.unsigned_abs(),
        i64::MAX.unsigned_abs() / 1000,
    ] {
        let http = CloudError::from_response(
            429,
            &json!({"error": {"code": "quota_exceeded", "message": "x",
                "resource": "bus_messages", "retry_after_s": huge}}),
        );
        let back = note_on(&store, None, http, now);
        assert!(matches!(back, CloudError::QuotaExceeded(_)));
        let held = holding(&store, Scope::Bus, now).expect("bus held");
        assert_eq!(held.until, longest, "from_response, {huge}");

        let by_hand = CloudError::QuotaExceeded(Box::new(crate::error::Quota {
            status: 429,
            resource: "transfer_bytes".into(),
            limit: None,
            used: None,
            reset_at: None,
            retry_after: Some(huge),
            cooldown_days: None,
            next_step: None,
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
    let month = CloudError::from_response(
        429,
        &json!({"error": {"code": "quota_exceeded", "message": "x", "resource": "transfer_bytes"}}),
    );
    note_on(&store, None, month.clone(), now);
    assert!(quota_holds(&store, Scope::Sync, now));
    assert!(check(&store, Scope::Sync, now).is_err(), "held");
    let soon = now + lean_ctx_time::Duration::seconds(QUOTA_PROBE_S - 1);
    assert!(
        check(&store, Scope::Sync, soon).is_err(),
        "no probe within the hour"
    );
    let hour = now + lean_ctx_time::Duration::seconds(QUOTA_PROBE_S);
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
    let later = hour + lean_ctx_time::Duration::seconds(QUOTA_PROBE_S);
    assert!(check(&store, Scope::Sync, later).is_ok());
    passed(&store, Scope::Sync);
    assert!(
        holding(&store, Scope::Sync, later).is_none(),
        "the quota is back"
    );
    // A newer plan ends quota throttles, never policy holds.
    note_on(&store, None, month, later);
    let policy = CloudError::from_response(
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
            later + lean_ctx_time::Duration::seconds(POLICY_HOLD_S - 1)
        )
        .is_err()
    );
}

fn rate_limited(retry_after: Option<u64>) -> CloudError {
    let mut body = json!({"error": {"code": "rate_limited", "message": "Too many requests."}});
    if let Some(s) = retry_after {
        body["error"]["retry_after_s"] = s.into();
    }
    CloudError::from_response(429, &body)
}

fn held_for(store: &CloudStore, now: UtcDateTime) -> Option<u64> {
    match token_hold(store, now) {
        Some(CloudError::RateLimited { retry_after }) => retry_after,
        _ => None,
    }
}

/// Cloud contact findings A: the token mint hold doubles with each 429 in
/// a row up to an hour, never ends before `Retry-After`, and a mint ends it.
#[test]
fn a_token_mint_429_backs_off_exponentially_with_retry_after_as_floor() {
    assert_eq!(rate_wait(1, None), 60);
    assert_eq!(rate_wait(2, None), 120);
    assert_eq!(rate_wait(3, Some(30)), 240, "the backoff is longer");
    assert_eq!(rate_wait(4, Some(900)), 900, "Retry-After is the floor");
    assert_eq!(rate_wait(7, None), RATE_MAX_WAIT_S, "capped at an hour");
    assert_eq!(rate_wait(u32::MAX, None), RATE_MAX_WAIT_S);
    assert_eq!(
        rate_wait(1, Some(7200)),
        7200,
        "the floor wins over the cap"
    );
    assert_eq!(rate_wait(1, Some(u64::MAX)), RATE_FLOOR_MAX_S, "bounded");

    let (dir, store) = store();
    let now = at("2026-11-17T10:00:00Z");
    assert_eq!(held_for(&store, now), None);
    let back = note_token_limit(&store, rate_limited(Some(30)), now);
    assert!(matches!(
        back,
        CloudError::RateLimited {
            retry_after: Some(30)
        }
    ));
    assert_eq!(held_for(&store, now), Some(60));
    assert!(
        holding(&store, Scope::Sync, now).is_none(),
        "the token hold is no sync quota"
    );
    // Ended: nothing is held, but the next 429 doubles the wait.
    let later = now + lean_ctx_time::Duration::seconds(61);
    assert_eq!(held_for(&store, later), None);
    assert!(status_lines(&store, later).is_empty());
    note_token_limit(&store, rate_limited(None), later);
    assert_eq!(held_for(&store, later), Some(120));
    let again = later + lean_ctx_time::Duration::seconds(121);
    note_token_limit(&store, rate_limited(None), again);
    assert_eq!(held_for(&store, again), Some(240));
    // Persisted: another reader of throttles.json sees the same hold.
    let reopened = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    assert_eq!(held_for(&reopened, again), Some(240));
    // Other errors are left alone.
    let offline = note_token_limit(&store, CloudError::Offline("x".into()), again);
    assert!(matches!(offline, CloudError::Offline(_)));
    assert_eq!(held_for(&store, again), Some(240));
    // A minted token ends the hold and its backoff step.
    token_minted(&store);
    assert_eq!(held_for(&store, again), None);
    note_token_limit(&store, rate_limited(None), again);
    assert_eq!(held_for(&store, again), Some(60));
    // Long after the last hold the step starts again.
    let much_later = again + lean_ctx_time::Duration::seconds(60 + RATE_MEMORY_S);
    note_token_limit(&store, rate_limited(None), much_later);
    assert_eq!(held_for(&store, much_later), Some(60));
}

/// Review of 5!443 (2): a token mint hold was honoured however far ahead
/// its `until` lay. One written while the clock ran ahead (a boot before
/// NTP, a VM resume), or a clock set back, blocked every cloud call for the
/// size of the jump, and nothing could mint to clear it. A hold with more
/// than [`RATE_FLOOR_MAX_S`] left is stale: it holds nothing, is not shown,
/// and the next 429 starts the backoff again.
#[test]
fn a_token_mint_hold_far_in_the_future_is_stale() {
    let (_dir, store) = store();
    let now = at("2026-11-17T10:00:00Z");
    // The longest hold that is written still holds, to the second.
    note_token_limit(&store, rate_limited(Some(u64::MAX)), now);
    assert_eq!(
        held_for(&store, now),
        Some(u64::try_from(RATE_FLOOR_MAX_S).unwrap())
    );
    token_minted(&store);
    // Written 30 days ahead of `now`'s clock, with a few 429s in a row.
    let ahead = now + lean_ctx_time::Duration::days(30);
    for _ in 0..3 {
        note_token_limit(&store, rate_limited(Some(3600)), ahead);
    }
    assert!(held_for(&store, ahead).is_some(), "under its own clock");
    assert_eq!(held_for(&store, now), None, "stale: no hold");
    assert!(
        status_lines(&store, now).is_empty(),
        "{:?}",
        status_lines(&store, now)
    );
    // The next 429 drops it and starts at the first step.
    note_token_limit(&store, rate_limited(None), now);
    assert_eq!(held_for(&store, now), Some(60));
}
