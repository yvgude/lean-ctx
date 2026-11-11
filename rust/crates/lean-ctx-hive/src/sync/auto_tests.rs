// SPDX-License-Identifier: Apache-2.0
use std::time::{Duration, Instant};

use super::*;

const INTERVAL: Duration = Duration::from_secs(300);

fn at(base: Instant, secs: u64) -> Instant {
    base + Duration::from_secs(secs)
}

#[test]
fn a_vault_never_tried_syncs_at_once_then_waits_for_the_interval() {
    let t0 = Instant::now();
    let mut due = Due::default();
    assert_eq!(decide(&due, t0, INTERVAL), Some(Trigger::Periodic));
    due.last_attempt = Some(t0);
    assert_eq!(decide(&due, at(t0, 299), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 300), INTERVAL), Some(Trigger::Periodic));
}

fn ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

#[test]
fn a_local_change_syncs_after_the_debounce_and_the_spacing() {
    let t0 = Instant::now();
    let mut due = Due {
        last_attempt: Some(t0),
        ..Due::default()
    };
    // Changed at 40 s: quiet until 41 s.
    due.last_change = Some(at(t0, 40));
    due.first_change = Some(at(t0, 40));
    assert_eq!(decide(&due, ms(t0, 40_500), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 41), INTERVAL), Some(Trigger::Local));
    // Changed right after an attempt: the spacing holds it.
    due.last_change = Some(ms(t0, 100));
    due.first_change = Some(ms(t0, 100));
    assert_eq!(decide(&due, ms(t0, 1_500), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 2), INTERVAL), Some(Trigger::Local));
}

/// #270: Instance A's fact was written at 09:19:08 and reached Hive at
/// 09:19:40 — the 30 s spacing after the vault's previous sync held it.
/// A change 3 s after a sync now goes out 1 s later, not at 30 s.
#[test]
fn a_fact_written_soon_after_a_sync_goes_out_within_two_seconds() {
    let t0 = Instant::now();
    let due = Due {
        last_attempt: Some(t0),
        full_at: Some(t0 + BURST_EVERY),
        last_change: Some(at(t0, 3)),
        first_change: Some(at(t0, 3)),
        ..Due::default()
    };
    assert_eq!(decide(&due, at(t0, 4), INTERVAL), Some(Trigger::Local));
}

#[test]
fn a_stream_of_changes_syncs_at_the_latest_after_the_hold() {
    let t0 = Instant::now();
    let mut due = Due {
        last_attempt: Some(t0),
        first_change: Some(at(t0, 10)),
        ..Due::default()
    };
    // A write every half second never leaves a quiet second.
    for half in 20..30 {
        let now = ms(t0, half * 500);
        due.last_change = Some(now);
        assert_eq!(decide(&due, now, INTERVAL), None, "{half}");
    }
    due.last_change = Some(at(t0, 15));
    assert_eq!(decide(&due, at(t0, 15), INTERVAL), Some(Trigger::Local));
}

#[test]
fn a_burst_is_paced_per_vault() {
    let t0 = Instant::now();
    let mut due = Due::default();
    // Six attempts within 2.5 s spend the burst.
    for i in 0..BURST_MAX as u64 {
        due.attempted(ms(t0, i * 500));
    }
    due.urgent = Some(Trigger::Bus);
    assert_eq!(decide(&due, ms(t0, 9_999), INTERVAL), None, "budget spent");
    // One turn comes back per `BURST_EVERY`, not when the first attempt
    // has left the whole window (5!233 e2e: A held for up to 60 s).
    assert_eq!(decide(&due, at(t0, 10), INTERVAL), Some(Trigger::Bus));
    due.attempted(at(t0, 10));
    due.urgent = Some(Trigger::Bus);
    assert_eq!(decide(&due, ms(t0, 19_999), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 20), INTERVAL), Some(Trigger::Bus));
    // A purge never waits for the budget.
    due.urgent = Some(Trigger::BusPurge);
    assert_eq!(decide(&due, at(t0, 12), INTERVAL), Some(Trigger::BusPurge));
    // A quiet vault's bucket is full again: one turn spent.
    due.attempted(at(t0, 200));
    assert_eq!(due.full_at, Some(at(t0, 200) + BURST_EVERY));
}

/// Review of 5!235: rebuilt from the last minute's attempts, the daemon's
/// bucket refilled as the first burst aged out, and a flood of hints got a
/// second full burst every minute. The kept state holds it to the burst
/// plus one turn per `BURST_EVERY` over any length of time.
#[test]
fn a_flood_of_hints_keeps_the_burst_rate_for_ten_minutes() {
    let t0 = Instant::now();
    let mut due = Due::default();
    let mut goes = 0u64;
    for step in 0..6_000 {
        let now = ms(t0, step * 100);
        due.urgent = Some(Trigger::Bus);
        if decide(&due, now, INTERVAL).is_some() {
            due.attempted(now);
            goes += 1;
        }
    }
    assert!(
        goes <= BURST_MAX as u64 + 600 / BURST_EVERY.as_secs(),
        "{goes}"
    );
}

/// e2e `offline.b-change-reaches-a` on 7983df1239: A's daemon tried six
/// times in 8 s (connect, periodic, local, bus, local, bus) and then held
/// B's `log.advanced` hint until the first try was 60 s old, past the
/// e2e's 45 s. The hint now goes out within `BURST_EVERY`.
#[test]
fn a_bus_hint_after_a_spent_burst_waits_one_turn_not_a_minute() {
    let t0 = Instant::now();
    let mut due = Due::default();
    for millis in [0, 2_543, 4_798, 5_655, 6_932, 7_944] {
        due.attempted(ms(t0, millis));
    }
    due.urgent = Some(Trigger::Bus);
    assert_eq!(decide(&due, ms(t0, 10_400), INTERVAL), Some(Trigger::Bus));
}

/// #270: Instance B's daemon got Hive's `log.advanced` hint for A's fact,
/// but held it for the 30 s spacing after B's own last sync.
#[test]
fn a_bus_hint_waits_only_for_the_spacing_and_a_purge_for_nothing() {
    let t0 = Instant::now();
    let mut due = Due {
        last_attempt: Some(t0),
        full_at: Some(t0 + BURST_EVERY),
        urgent: Some(Trigger::Bus),
        ..Due::default()
    };
    assert_eq!(decide(&due, at(t0, 1), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 3), INTERVAL), Some(Trigger::Bus));
    due.urgent = Some(Trigger::BusPurge);
    assert_eq!(decide(&due, ms(t0, 100), INTERVAL), Some(Trigger::BusPurge));
}

/// #270 E2E `offline.b-catches-up` (29.7 s): B restarted, pulled at once,
/// and Hive's `log.advanced` for A's fact came 1 s later; the 30 s spacing
/// held it. It now pulls 2 s after the startup pull, within the vault's
/// shared budget.
#[test]
fn after_a_restart_a_bus_hint_pulls_within_two_seconds_of_the_startup_pull() {
    let dir = tempfile::tempdir().unwrap();
    let vault = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
    let (t0, wall) = (Instant::now(), Utc::now());
    let wall_at = |millis: i64| wall + chrono::Duration::milliseconds(millis);
    let budget =
        |millis| super::super::budget::admit_sync(dir.path(), vault, false, wall_at(millis));
    // Startup: a vault never tried syncs at once.
    let mut due = Due::default();
    assert_eq!(decide(&due, t0, INTERVAL), Some(Trigger::Periodic));
    assert_eq!(budget(0), super::super::budget::Admit::Go);
    due.attempted(t0);
    // The hint 1 s later waits only for the spacing: the thread sleeps until
    // the spacing ends, not a whole poll more.
    due.urgent = Some(Trigger::Bus);
    let hinted = at(t0, 1);
    assert_eq!(decide(&due, hinted, INTERVAL), None);
    let woke = hinted + next_wake([&due], hinted);
    let pulled = woke.duration_since(t0);
    assert!(
        pulled <= Duration::from_secs(2),
        "pulled {pulled:?} after the startup pull"
    );
    assert_eq!(decide(&due, woke, INTERVAL), Some(Trigger::Bus));
    assert_eq!(
        budget(i64::try_from(pulled.as_millis()).unwrap()),
        super::super::budget::Admit::Go,
        "inside the shared budget"
    );
}

#[test]
fn the_thread_sleeps_a_poll_when_nothing_waits_for_the_spacing() {
    let t0 = Instant::now();
    let idle = Due {
        last_attempt: Some(t0),
        ..Due::default()
    };
    assert_eq!(next_wake([&idle], ms(t0, 500)), POLL);
    let changed = Due {
        last_change: Some(t0),
        ..Due::default()
    };
    assert_eq!(
        next_wake([&changed], ms(t0, 400)),
        Duration::from_millis(600)
    );
    let backing_off = Due {
        last_attempt: Some(t0),
        urgent: Some(Trigger::Bus),
        retry_at: Some(at(t0, 60)),
        ..Due::default()
    };
    assert_eq!(next_wake([&backing_off], ms(t0, 1000)), POLL);
}

#[test]
fn a_backoff_holds_every_trigger() {
    let t0 = Instant::now();
    let due = Due {
        urgent: Some(Trigger::BusPurge),
        last_change: Some(t0),
        retry_at: Some(at(t0, 60)),
        ..Due::default()
    };
    assert_eq!(decide(&due, at(t0, 59), INTERVAL), None);
    assert_eq!(decide(&due, at(t0, 60), INTERVAL), Some(Trigger::BusPurge));
}

#[test]
fn backoff_doubles_with_jitter_and_stops_at_the_cap() {
    // No jitter offset at the middle of the range (±20 % around 1.0).
    assert_eq!(backoff(1, 500), Duration::from_secs(30));
    assert_eq!(backoff(2, 500), Duration::from_secs(60));
    assert_eq!(backoff(3, 500), Duration::from_secs(120));
    assert_eq!(backoff(40, 500), BACKOFF_MAX);
    assert_eq!(backoff(1, 0), Duration::from_secs(24));
    assert_eq!(backoff(1, 999), Duration::from_millis(35_970));
    assert!(backoff(40, 999) < BACKOFF_MAX + BACKOFF_MAX / 4);
}

#[test]
fn only_vault_places_are_nudged() {
    assert_eq!(
        vault_of_place("vault:3f1c2b6a8d4e4a7b9c105e2f6a7b8c01").as_deref(),
        Some("3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01")
    );
    assert_eq!(vault_of_place("vault:"), None);
    assert_eq!(vault_of_place("org:acme"), None);
}

#[test]
fn a_vault_without_a_local_checkout_is_not_synced() {
    let dir = tempfile::tempdir().unwrap();
    let mut record = VaultRecord {
        vault_id: "v".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: vec![dir.path().join("gone").to_string_lossy().into_owned()],
    };
    assert_eq!(root_of(&record), None);
    record.roots.push(dir.path().to_string_lossy().into_owned());
    assert_eq!(
        root_of(&record),
        Some(dir.path().to_string_lossy().into_owned())
    );
}

#[test]
fn status_round_trips_and_shows_a_stale_thread_as_stopped() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_status(dir.path()), SyncStatus::default());
    let now = Utc::now();
    let status = SyncStatus {
        interval_s: 300,
        last_pass_at: Some(stamp(now)),
        heartbeat_at: Some(stamp(now - chrono::Duration::seconds(30))),
        vaults: vec![VaultSyncStatus {
            vault_id: "v".into(),
            vault: "@acme/app".into(),
            pending: 2,
            ..VaultSyncStatus::default()
        }],
    };
    save_status(dir.path(), &status);
    let back = load_status(dir.path());
    assert_eq!(back, status);
    assert!(back.running_at(now));
    assert!(!back.running_at(now + chrono::Duration::seconds(200)));
    assert!(!SyncStatus::default().running_at(now));
}

fn shared(ids: &[&str]) -> VaultMap {
    let mut map = VaultMap::default();
    for id in ids {
        map.vaults.insert(
            format!("github.com/acme/{id}"),
            VaultRecord {
                vault_id: (*id).into(),
                ns: "acme".into(),
                name: (*id).into(),
                sealed: false,
                roots: Vec::new(),
            },
        );
    }
    map
}

#[test]
fn a_manual_sync_row_survives_the_threads_next_write() {
    let row = |id: &str, at: &str| VaultSyncStatus {
        vault_id: id.into(),
        last_sync_at: Some(at.into()),
        ..VaultSyncStatus::default()
    };
    let mut mine = SyncStatus {
        vaults: vec![
            row("a", "2026-11-05T10:00:00Z"),
            row("b", "2026-11-05T10:00:00Z"),
        ],
        ..SyncStatus::default()
    };
    let disk = SyncStatus {
        vaults: vec![
            row("a", "2026-11-05T10:05:00Z"),
            row("b", "2026-11-05T09:00:00Z"),
            row("c", "2026-11-05T10:01:00Z"),
        ],
        ..SyncStatus::default()
    };
    merge_manual(&mut mine, &disk, &shared(&["a", "b", "c"]));
    let at: Vec<_> = mine
        .vaults
        .iter()
        .map(|v| (v.vault_id.as_str(), v.last_sync_at.as_deref().unwrap()))
        .collect();
    assert_eq!(
        at,
        [
            ("a", "2026-11-05T10:05:00Z"),
            ("b", "2026-11-05T10:00:00Z"),
            ("c", "2026-11-05T10:01:00Z")
        ]
    );
}

/// Review 05.11.: a vault no longer shared left `hive.sync` on the next
/// write, but the merge with the file brought its row back for good.
#[test]
fn a_vault_no_longer_shared_leaves_the_status_for_good() {
    let row = |id: &str| VaultSyncStatus {
        vault_id: id.into(),
        last_sync_at: Some("2026-11-05T10:00:00Z".into()),
        ..VaultSyncStatus::default()
    };
    let disk = SyncStatus {
        vaults: vec![row("a"), row("gone")],
        ..SyncStatus::default()
    };
    let mut mine = SyncStatus {
        vaults: vec![row("a"), row("gone")],
        ..SyncStatus::default()
    };
    merge_manual(&mut mine, &disk, &shared(&["a"]));
    let ids: Vec<_> = mine.vaults.iter().map(|v| v.vault_id.as_str()).collect();
    assert_eq!(ids, ["a"]);
    // The next write keeps it out, even though the old file still has it.
    merge_manual(&mut mine, &disk, &shared(&["a"]));
    assert_eq!(mine.vaults.len(), 1);
}

#[test]
fn a_failed_sync_backs_off_and_a_success_clears_it() {
    let record = VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    };
    let mut runs = BTreeMap::new();
    let mut status = SyncStatus::default();
    let offline = HiveError::Offline("down".into());
    note(
        &record,
        Trigger::Bus,
        Err(&offline),
        &mut runs,
        &mut status,
        Some(3),
    );
    note(
        &record,
        Trigger::Periodic,
        Err(&offline),
        &mut runs,
        &mut status,
        None,
    );
    let row = &status.vaults[0];
    assert_eq!(row.vault, "@acme/app");
    assert_eq!(row.failures, 2);
    assert_eq!(row.pending, 3, "an unreadable count keeps the last one");
    assert!(row.error.is_some() && row.next_attempt_at.is_some());
    assert!(runs[&record.vault_id].due.retry_at.is_some());
    let done = super::super::place::VaultSync {
        pushed: 3,
        ..Default::default()
    };
    note(
        &record,
        Trigger::Local,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    let row = &status.vaults[0];
    assert_eq!((row.failures, row.pending, row.pushed), (0, 0, 3));
    assert_eq!(row.last_trigger.as_deref(), Some("local"));
    assert!(row.error.is_none() && row.last_ok_at.is_some());
    assert!(runs[&record.vault_id].due.retry_at.is_none());
}

/// A failure noted by one process (here a push on write) holds the vault
/// for every other one through the shared budget.
#[test]
fn a_vault_backing_off_is_held_for_every_process() {
    let dir = tempfile::tempdir().unwrap();
    let record = VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    };
    let now = Utc::now();
    let admit = |at| super::super::budget::admit_sync(dir.path(), &record.vault_id, false, at);
    assert_eq!(admit(now), super::super::budget::Admit::Go);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let offline = HiveError::Offline("down".into());
    let wait = note(
        &record,
        Trigger::Local,
        Err(&offline),
        &mut runs,
        &mut status,
        None,
    );
    super::super::budget::settle(dir.path(), &record.vault_id, wait, now);
    assert_eq!(admit(now), super::super::budget::Admit::Backoff);
    let later = now + chrono::Duration::hours(1);
    assert_eq!(admit(later), super::super::budget::Admit::Go);
}

/// Review of #270: a 429 waits exactly as long as Hive's `Retry-After`
/// says, not the generic backoff (30 s at first, far shorter than a
/// minute-long rate limit, or far longer than a short one).
#[test]
fn a_rate_limit_waits_for_hives_retry_after() {
    let record = VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    };
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let limited = HiveError::RateLimited {
        retry_after: Some(90),
    };
    let wait = note(
        &record,
        Trigger::Local,
        Err(&limited),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(Duration::from_secs(90)));
    let at =
        chrono::DateTime::parse_from_rfc3339(status.vaults[0].last_sync_at.as_deref().unwrap())
            .unwrap();
    let next =
        chrono::DateTime::parse_from_rfc3339(status.vaults[0].next_attempt_at.as_deref().unwrap())
            .unwrap();
    assert_eq!(
        (next - at).num_seconds(),
        90,
        "the status shows Hive's wait"
    );
    // Without a Retry-After, the exponential backoff (±20 % jitter).
    let unsaid = HiveError::RateLimited { retry_after: None };
    let wait = failure_wait(&unsaid, 1, Utc::now());
    assert!(wait >= Duration::from_secs(24) && wait <= Duration::from_secs(36));
    // Review of 5!233 (a): a Retry-After of hours waits `WAIT_MAX` at most.
    let hours = HiveError::RateLimited {
        retry_after: Some(6 * 3600),
    };
    assert_eq!(failure_wait(&hours, 1, Utc::now()), WAIT_MAX);
    assert!(failure_wait(&unsaid, 40, Utc::now()) <= WAIT_MAX);
}

/// Review of #270: two processes noting a sync at once keep both rows.
#[test]
fn concurrent_notes_of_two_processes_keep_both_rows() {
    let dir = tempfile::tempdir().unwrap();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let data = dir.path().to_path_buf();
            std::thread::spawn(move || {
                with_status(&data, |status| {
                    std::thread::sleep(Duration::from_millis(5));
                    status.vaults.push(VaultSyncStatus {
                        vault_id: format!("v-{i}"),
                        ..VaultSyncStatus::default()
                    });
                });
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(load_status(dir.path()).vaults.len(), 8);
}

#[test]
fn the_cli_and_the_thread_never_sync_one_vault_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock_vault(dir.path(), "v-1", true).unwrap();
    assert!(held.is_some());
    assert!(lock_vault(dir.path(), "v-1", false).unwrap().is_none());
    assert!(lock_vault(dir.path(), "v-2", false).unwrap().is_some());
    drop(held);
    assert!(lock_vault(dir.path(), "v-1", false).unwrap().is_some());
}

/// Review of 5!233 (c): the test above failed now and then because a
/// parallel test forked a child (`git`) between the lock's open and its
/// drop; the child's copy of the descriptor kept the `flock` after the
/// drop closed ours. A clone of the descriptor stands in for that child
/// here, deterministically: the drop must still free the vault.
#[test]
fn a_dropped_lock_frees_the_vault_while_a_child_keeps_the_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock_vault(dir.path(), "v-1", true).unwrap().unwrap();
    let child = held.0.try_clone().unwrap();
    drop(held);
    assert!(lock_vault(dir.path(), "v-1", false).unwrap().is_some());
    drop(child);
}

/// k2 E2E: a fact remembered locally (the knowledge journal moves) makes
/// the daemon sync that vault after the debounce, with no manual `hive
/// sync`; an unchanged vault waits for the interval; an unshared project
/// (not in projects.json) never syncs.
#[test]
fn a_local_knowledge_write_syncs_its_vault_without_a_manual_sync() {
    let checkout = tempfile::tempdir().unwrap();
    let unshared = tempfile::tempdir().unwrap();
    let mut map = VaultMap::default();
    map.vaults.insert(
        "github.com/acme/app".into(),
        VaultRecord {
            vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            ns: "acme".into(),
            name: "app".into(),
            sealed: false,
            roots: vec![checkout.path().to_string_lossy().into_owned()],
        },
    );
    let lsn = std::cell::Cell::new(4u64);
    let looked = std::cell::RefCell::new(Vec::new());
    let mark = |_: &VaultRecord, root: &str| {
        looked.borrow_mut().push(root.to_owned());
        Mark {
            knowledge: lsn.get(),
            replica: None,
        }
    };
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let none = BTreeMap::new();
    let t0 = Instant::now();
    let plan = |runs: &mut BTreeMap<String, VaultRun>, status: &mut SyncStatus, at| {
        plan_pass(&map, runs, status, &none, at, INTERVAL, true, &mark)
            .into_iter()
            .map(|(_, _, trigger)| trigger)
            .collect::<Vec<_>>()
    };
    // Start: the first sync, then the baseline holds.
    assert_eq!(plan(&mut runs, &mut status, t0), [Trigger::Periodic]);
    let record = map.vaults.values().next().unwrap().clone();
    let done = super::super::place::VaultSync::default();
    note(
        &record,
        Trigger::Periodic,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    assert!(plan(&mut runs, &mut status, at(t0, 40)).is_empty());
    // `ctx_knowledge remember` moves the journal: synced 1 s later.
    lsn.set(5);
    assert!(
        plan(&mut runs, &mut status, at(t0, 41)).is_empty(),
        "debounce"
    );
    assert_eq!(plan(&mut runs, &mut status, at(t0, 42)), [Trigger::Local]);
    // Only the shared checkout was ever looked at.
    let unshared = unshared.path().to_string_lossy().into_owned();
    assert!(looked.borrow().iter().all(|root| *root != unshared));
    // Hive off / not C0 2.x: a change is noted, nothing syncs.
    note(
        &record,
        Trigger::Local,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    lsn.set(6);
    assert!(
        plan_pass(
            &map,
            &mut runs,
            &mut status,
            &none,
            at(t0, 50),
            INTERVAL,
            false,
            &mark
        )
        .is_empty()
    );
}

/// e2e `knowledge.visible-on-b` then `offline.b-kept-local` BLOCKED: a fact
/// remembered while the daemon's sync ran is not in that sync's capture
/// (knowledge is loaded as it starts). The pass then took the moved journal
/// head as its new baseline, so no later pass saw the write until the 5 min
/// interval. The baseline keeps the head the sync started from.
#[test]
fn a_fact_remembered_during_a_sync_is_synced_after_it() {
    let checkout = tempfile::tempdir().unwrap();
    let mut map = VaultMap::default();
    map.vaults.insert(
        "github.com/acme/app".into(),
        VaultRecord {
            vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            ns: "acme".into(),
            name: "app".into(),
            sealed: false,
            roots: vec![checkout.path().to_string_lossy().into_owned()],
        },
    );
    let replica = Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(9));
    let lsn = std::cell::Cell::new(4u64);
    let mark = |_: &VaultRecord, _: &str| Mark {
        knowledge: lsn.get(),
        replica,
    };
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let none = BTreeMap::new();
    let t0 = Instant::now();
    let plan = |runs: &mut BTreeMap<String, VaultRun>, status: &mut SyncStatus, at| {
        plan_pass(&map, runs, status, &none, at, INTERVAL, true, &mark)
            .into_iter()
            .map(|(_, _, trigger)| trigger)
            .collect::<Vec<_>>()
    };
    assert_eq!(plan(&mut runs, &mut status, t0), [Trigger::Periodic]);
    let record = map.vaults.values().next().unwrap().clone();
    // The pass: the journal at 4 as the sync starts, a fact remembered
    // while it runs (5), the sync writes the replica.
    let before = Mark {
        knowledge: 4,
        replica: None,
    };
    lsn.set(5);
    note(
        &record,
        Trigger::Periodic,
        Ok(&super::super::place::VaultSync::default()),
        &mut runs,
        &mut status,
        Some(0),
    );
    let id = super::super::place::place_id(&record.vault_id);
    runs.get_mut(&id).unwrap().mark = Some(before.after_sync(&mark(&record, "")));
    // The sync's own replica write is no change; the fact is.
    assert_eq!(
        Mark {
            knowledge: 4,
            replica: None
        }
        .after_sync(&Mark {
            knowledge: 4,
            replica
        }),
        Mark {
            knowledge: 4,
            replica
        }
    );
    assert!(
        plan(&mut runs, &mut status, at(t0, 40)).is_empty(),
        "debounce"
    );
    assert_eq!(plan(&mut runs, &mut status, at(t0, 41)), [Trigger::Local]);
}
