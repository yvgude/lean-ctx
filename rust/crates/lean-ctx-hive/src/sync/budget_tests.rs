// SPDX-License-Identifier: Apache-2.0
use super::super::auto::{BURST_EVERY, BURST_MAX};
use super::*;

const VAULT: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";

/// Review of #270: push on write ran up to 30 pushes a minute per MCP
/// process (2 s spacing only), and the daemon never counted them. Every
/// process now takes its turn from one budget per vault, whoever asks: a
/// burst of `BURST_MAX`, then one per `BURST_EVERY`.
#[test]
fn every_process_shares_one_burst_budget_per_vault() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Utc::now();
    let at = |secs: i64| t0 + chrono::Duration::seconds(secs);
    // Two MCP processes and the daemon, one attempt every 2 s between them.
    let goes: Vec<i64> = (0..30)
        .map(|i| i * 2)
        .filter(|s| admit(dir.path(), VAULT, false, at(*s)) == Admit::Go)
        .collect();
    assert_eq!(
        goes,
        [0, 2, 4, 6, 8, 10, 12, 20, 30, 40, 50],
        "one budget, not one per process: the burst, then one per 10 s"
    );
    assert_eq!(admit(dir.path(), VAULT, false, at(59)), Admit::Burst);
    // A purge passes the cap; another vault has its own budget.
    assert_eq!(admit(dir.path(), VAULT, true, at(59)), Admit::Go);
    assert_eq!(admit(dir.path(), "other", false, at(59)), Admit::Go);
    // The turns come back as the old attempts age.
    assert_eq!(admit(dir.path(), VAULT, false, at(71)), Admit::Go);
}

#[test]
fn a_backoff_holds_every_attempt_and_a_success_clears_it() {
    let mut budget = Budget::default();
    assert_eq!(budget.admit(false, 0), Admit::Go);
    budget.settle(Some(Duration::from_secs(90)), 1_000);
    assert_eq!(budget.admit(false, 90_000), Admit::Backoff);
    assert_eq!(budget.admit(true, 90_000), Admit::Backoff, "a purge too");
    assert_eq!(budget.admit(false, 91_000), Admit::Go);
    budget.settle(None, 92_000);
    assert_eq!(budget.not_before, None);
}

/// A burst budget stamped before the clock went back holds a vault for one
/// turn at most, as an empty bucket would, and the cut is kept.
#[test]
fn a_clock_set_back_holds_the_burst_budget_for_one_turn_at_most() {
    let mut budget = Budget {
        full_at: Some(10_000_000),
        not_before: None,
    };
    assert_eq!(budget.admit(false, 0), Admit::Burst);
    assert_eq!(budget.full_at, Some(window_ms()), "the cut is kept");
    let every = i64::try_from(BURST_EVERY.as_millis()).unwrap();
    assert_eq!(budget.admit(false, every - 1), Admit::Burst);
    assert_eq!(budget.admit(false, every), Admit::Go);
}

/// Review of 5!233 (a): a hold in the file is never longer than
/// `WAIT_MAX` from now. A wall clock stepped back by ten hours during a
/// backoff held every process for the ten hours as well.
#[test]
fn a_clock_set_back_holds_a_vault_for_the_wait_max_at_most() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Utc::now();
    let ten_hours = Duration::from_secs(10 * 3600);
    // The backoff was stamped before the clock went back ten hours.
    let stamped = Budget {
        full_at: None,
        not_before: Some((t0 + chrono::Duration::hours(10)).timestamp_millis()),
    };
    save(dir.path(), VAULT, &stamped);
    assert_eq!(admit(dir.path(), VAULT, false, t0), Admit::Backoff);
    let cut = load(dir.path(), VAULT).not_before.unwrap();
    assert_eq!(
        cut,
        t0.timestamp_millis() + wait_max_ms(),
        "the cut is kept"
    );
    // Asking again later does not move the hold along with the clock.
    let half = t0 + chrono::Duration::from_std(WAIT_MAX / 2).unwrap();
    assert_eq!(admit(dir.path(), VAULT, false, half), Admit::Backoff);
    assert_eq!(load(dir.path(), VAULT).not_before, Some(cut));
    let after = t0 + chrono::Duration::from_std(WAIT_MAX).unwrap();
    assert_eq!(admit(dir.path(), VAULT, false, after), Admit::Go);
    // A wait asked for at settle time (a long `Retry-After`) is cut too.
    settle(dir.path(), VAULT, Some(ten_hours), t0);
    assert_eq!(
        load(dir.path(), VAULT).not_before,
        Some(t0.timestamp_millis() + wait_max_ms())
    );
}

/// Review of 5!233 (b): a manual `hive sync` is never held, but it counts
/// against the burst cap of every other sync and sets the backoff.
#[test]
fn a_manual_sync_counts_against_the_burst_cap_without_being_held() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Utc::now();
    let at = |secs: i64| t0 + chrono::Duration::seconds(secs);
    for i in 0..BURST_MAX {
        record(dir.path(), VAULT, None, at(i64::try_from(i).unwrap()));
    }
    let full = |secs: i64| Some(at(secs).timestamp_millis());
    assert_eq!(load(dir.path(), VAULT).full_at, full(60), "the burst spent");
    assert_eq!(admit(dir.path(), VAULT, false, at(6)), Admit::Burst);
    // One more manual sync past the cap still runs and is recorded; it
    // fails and holds every other sync for its wait.
    record(dir.path(), VAULT, Some(Duration::from_secs(90)), at(11));
    assert_eq!(load(dir.path(), VAULT).full_at, full(70));
    assert_eq!(admit(dir.path(), VAULT, true, at(12)), Admit::Backoff);
    // A later manual success clears the hold; the window has moved on.
    record(dir.path(), VAULT, None, at(120));
    assert_eq!(admit(dir.path(), VAULT, false, at(121)), Admit::Go);
}

/// e2e `conflict.fact-converges` and `offline.b-change-reaches-a` on
/// 7983df1239 (5!233 + 5!234): a connect, the daemon's periodic, local
/// and bus triggers spent Instance A's six turns in 8 s, and the sliding
/// window then held A until the first of them was 60 s old. B's fact,
/// pushed 10 s into that hold, reached A only after the e2e's 45 s.
/// A spent burst now holds a vault for `BURST_EVERY` at most.
#[test]
fn a_spent_burst_holds_a_vault_for_one_turn_not_a_whole_window() {
    let mut budget = Budget::default();
    // Instance A's attempts in the run of 9 Nov 11:23:36 (ms after the
    // first): manual connect, periodic, local, bus, local, bus.
    for at in [0, 2_543, 4_798, 5_655, 6_932, 7_944] {
        assert_eq!(budget.admit(false, at), Admit::Go);
    }
    assert_eq!(
        budget.admit(false, 8_500),
        Admit::Burst,
        "the burst is spent"
    );
    // B's `log.advanced` hint 10.4 s in: the sliding window said Burst
    // until 60 s.
    assert_eq!(budget.admit(false, 10_400), Admit::Go);
    // A steady stream gets one turn per `BURST_EVERY`.
    let every = i64::try_from(BURST_EVERY.as_millis()).unwrap();
    assert_eq!(budget.admit(false, 2 * every - 1), Admit::Burst);
    assert_eq!(budget.admit(false, 2 * every), Admit::Go);
    assert_eq!(budget.admit(false, 3 * every - 1), Admit::Burst);
    assert_eq!(budget.admit(false, 3 * every), Admit::Go);
}

/// However often a vault is asked for, no more than the burst plus one
/// attempt per `BURST_EVERY` goes through, over any length of time. Review
/// of 5!235: rebuilt from the last minute's attempts, the bucket refilled
/// as the first burst aged out; a flood got 111 turns in 600 s, not 65.
#[test]
fn the_burst_budget_keeps_its_rate_under_a_flood() {
    let mut budget = Budget::default();
    let span = 600_000;
    let goes = (0..span)
        .step_by(100)
        .filter(|at| budget.admit(false, *at) == Admit::Go)
        .count();
    let every = i64::try_from(BURST_EVERY.as_millis()).unwrap();
    let steady = usize::try_from(span / every).unwrap();
    assert!(goes <= BURST_MAX + steady, "{goes}");
    assert_eq!(goes, BURST_MAX + steady - 1);
}
