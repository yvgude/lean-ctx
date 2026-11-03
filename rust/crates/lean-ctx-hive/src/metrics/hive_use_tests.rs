// SPDX-License-Identifier: Apache-2.0

use super::*;

const PROJECT: &str = "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42";

type Rows = Vec<(String, u64, u64, u64, u64)>;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

/// The held tasks' usage over the UTC day starting at `day`.
fn day_tasks(
    data: &Path,
    day: &str,
    now: DateTime<Utc>,
    usage: &WindowUsage<'_>,
) -> Vec<MetricsTaskUsageV1> {
    let start = at(&format!("{day}T00:00:00Z"));
    window_tasks(
        data,
        PROJECT,
        start,
        start + chrono::Duration::days(1),
        now,
        usage,
    )
}

/// Hits and first-context samples land on their project and day.
#[test]
fn hits_and_first_context_times_make_the_day_counters() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let now = at("2026-10-22T12:00:00Z");
    record_at(data, PROJECT, now, Some(3), Some(400));
    record_at(data, PROJECT, now, None, Some(1200));
    record_at(data, PROJECT, now, Some(1), Some(800));
    record_at(data, "other", now, Some(9), None);
    assert_eq!(
        day_counters(data, PROJECT, "2026-10-22"),
        (4, vec![400, 1200, 800])
    );
    assert_eq!(day_counters(data, PROJECT, "2026-10-21"), (0, Vec::new()));
}

/// A claim's usage is what the host sessions recorded inside its window,
/// cut at the day's edges and at `now` while still held; tasks sort by
/// number and carry list price in micro-USD.
#[test]
fn claim_windows_bound_the_task_usage() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    claim_started(data, PROJECT, 9, at("2026-10-21T23:30:00Z"));
    claim_ended(data, PROJECT, 9, at("2026-10-22T00:30:00Z"));
    claim_started(data, PROJECT, 7, at("2026-10-22T10:00:00Z"));
    // A renewal does not open a second window.
    claim_started(data, PROJECT, 7, at("2026-10-22T10:05:00Z"));
    claim_started(data, "other", 1, at("2026-10-22T10:00:00Z"));
    let now = at("2026-10-22T11:00:00Z");
    let windows = std::cell::RefCell::new(Vec::new());
    let usage = |from: DateTime<Utc>, to: DateTime<Utc>| -> Rows {
        windows.borrow_mut().push((from, to));
        vec![("claude-opus-5-5".to_owned(), 1_000, 200, 0, 0)]
    };
    let tasks = day_tasks(data, "2026-10-22", now, &usage);
    assert_eq!(
        windows.into_inner(),
        [
            (at("2026-10-22T00:00:00Z"), at("2026-10-22T00:30:00Z")),
            (at("2026-10-22T10:00:00Z"), now),
        ]
    );
    let numbers: Vec<u64> = tasks.iter().map(|row| row.number).collect();
    assert_eq!(numbers, [7, 9]);
    assert_eq!(tasks[0].input_tokens, 1_000);
    assert_eq!(tasks[0].output_tokens, 200);

    // No usage inside the windows: no rows.
    let idle = |_: DateTime<Utc>, _: DateTime<Utc>| -> Rows { Vec::new() };
    assert!(day_tasks(data, "2026-10-22", now, &idle).is_empty());
}

/// M2 counts each minute once: claims held at the same time share its
/// usage, a claim ending in the minute the next one starts shares that
/// minute, and the shares add up to what the host recorded.
#[test]
fn overlapping_and_adjacent_claims_share_their_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    claim_started(data, PROJECT, 1, at("2026-10-22T10:00:00Z"));
    claim_started(data, PROJECT, 2, at("2026-10-22T10:00:10Z"));
    claim_ended(data, PROJECT, 1, at("2026-10-22T10:01:30Z"));
    claim_ended(data, PROJECT, 2, at("2026-10-22T10:01:40Z"));
    claim_started(data, PROJECT, 3, at("2026-10-22T10:01:50Z"));
    claim_ended(data, PROJECT, 3, at("2026-10-22T10:02:20Z"));
    let now = at("2026-10-22T11:00:00Z");
    // 1 001 input tokens in every minute.
    let usage = |from: DateTime<Utc>, to: DateTime<Utc>| -> Rows {
        let minutes = u64::try_from((to - from).num_minutes() + 1).unwrap();
        vec![("claude-opus-5-5".to_owned(), 1_001 * minutes, 0, 0, 0)]
    };
    let input: Vec<(u64, u64)> = day_tasks(data, "2026-10-22", now, &usage)
        .iter()
        .map(|row| (row.number, row.input_tokens))
        .collect();
    // 10:00 by 1 and 2; 10:01 by 1, 2 and 3; 10:02 by 3.
    assert_eq!(input, [(1, 501 + 334), (2, 500 + 334), (3, 333 + 1_001)]);
    let total: u64 = input.iter().map(|(_, n)| n).sum();
    assert_eq!(total, 3 * 1_001, "three minutes, each counted once");
}

/// C9: the engine measures an open claim's usage itself, at list price,
/// from the claim's start; an ended claim or idle hosts measure nothing.
#[test]
fn an_open_claim_is_measured_by_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    claim_started(data, PROJECT, 4, at("2026-10-22T10:00:00Z"));
    let now = at("2026-10-22T10:30:00Z");
    let windows = std::cell::RefCell::new(Vec::new());
    let usage = |from: DateTime<Utc>, to: DateTime<Utc>| -> Rows {
        windows.borrow_mut().push((from, to));
        vec![("claude-opus-5-5".to_owned(), 10_000, 2_000, 0, 0)]
    };
    let measured = claim_usage(data, PROJECT, 4, now, &usage).unwrap();
    assert_eq!(measured.tokens, 12_000);
    assert!(
        measured.usd_micros.is_some_and(|usd| usd > 0),
        "{measured:?}"
    );
    assert_eq!(*windows.borrow(), [(at("2026-10-22T10:00:00Z"), now)]);
    let idle = |_: DateTime<Utc>, _: DateTime<Utc>| -> Rows { Vec::new() };
    assert_eq!(claim_usage(data, PROJECT, 4, now, &idle), None);
    claim_ended(data, PROJECT, 4, now);
    assert_eq!(claim_usage(data, PROJECT, 4, now, &usage), None);
}

/// Days and ended claims older than the upload reach are dropped.
#[test]
fn old_days_and_claims_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let old = at("2026-08-01T12:00:00Z");
    record_at(data, PROJECT, old, Some(1), None);
    claim_started(data, PROJECT, 1, old);
    claim_ended(data, PROJECT, 1, old);
    let now = at("2026-10-22T12:00:00Z");
    record_at(data, PROJECT, now, Some(1), None);
    let state = load(data);
    assert_eq!(state.days.keys().collect::<Vec<_>>(), ["2026-10-22"]);
    assert!(state.claims.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(data.join("metrics").join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}
