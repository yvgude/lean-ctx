// SPDX-License-Identifier: Apache-2.0
use std::time::{Duration, Instant};

use super::*;

const INTERVAL: Duration = Duration::from_secs(300);

/// The plan state the tests' machine has (see [`plan_state`]).
const PLAN: &str = "\"team\"|[\"sync\"]|2026-11-17T10:00:00Z|";

/// The link state the tests' machine has (see [`link_state`]).
const LINK: &str = "inst-1|https://cloud.example|2026-11-17T09:00:00Z|";

/// [`super::note`] on a machine whose plan state is [`PLAN`] and whose link
/// state is [`LINK`].
fn note(
    record: &VaultRecord,
    trigger: Trigger,
    result: Result<&super::super::place::VaultSync, &CloudError>,
    runs: &mut BTreeMap<String, VaultRun>,
    status: &mut SyncStatus,
    pending: Option<u64>,
) -> Option<Duration> {
    let plan = || PLAN.to_string();
    let link = || LINK.to_string();
    let states = ParkStates {
        plan: &plan,
        link: &link,
    };
    super::note(record, trigger, result, runs, status, pending, &states)
}

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

/// #270: Instance A's fact was written at 09:19:08 and reached Cloud at
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

/// #270: Instance B's daemon got Cloud's `log.advanced` hint for A's fact,
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
/// and Cloud's `log.advanced` for A's fact came 1 s later; the 30 s spacing
/// held it. It now pulls 2 s after the startup pull, within the vault's
/// shared budget.
#[test]
fn after_a_restart_a_bus_hint_pulls_within_two_seconds_of_the_startup_pull() {
    let dir = tempfile::tempdir().unwrap();
    let vault = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
    let (t0, wall) = (Instant::now(), lean_ctx_time::UtcDateTime::now());
    let wall_at = |millis: i64| wall + lean_ctx_time::Duration::milliseconds(millis);
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
    let now = lean_ctx_time::UtcDateTime::now();
    let status = SyncStatus {
        interval_s: 300,
        last_pass_at: Some(stamp(now)),
        heartbeat_at: Some(stamp(now - lean_ctx_time::Duration::seconds(30))),
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
    assert!(!back.running_at(now + lean_ctx_time::Duration::seconds(200)));
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

/// Review 05.11.: a vault no longer shared left `cloud.sync` on the next
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
    let offline = CloudError::Offline("down".into());
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
    let now = lean_ctx_time::UtcDateTime::now();
    let admit = |at| super::super::budget::admit_sync(dir.path(), &record.vault_id, false, at);
    assert_eq!(admit(now), super::super::budget::Admit::Go);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let offline = CloudError::Offline("down".into());
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
    let later = now + lean_ctx_time::Duration::hours(1);
    assert_eq!(admit(later), super::super::budget::Admit::Go);
}

/// Review of #270: a 429 waits exactly as long as Cloud's `Retry-After`
/// says, not the generic backoff (30 s at first, far shorter than a
/// minute-long rate limit, or far longer than a short one).
#[test]
fn a_rate_limit_waits_for_clouds_retry_after() {
    let record = VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    };
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let limited = CloudError::RateLimited {
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
    let at = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(
        status.vaults[0].last_sync_at.as_deref().unwrap(),
    )
    .unwrap();
    let next = lean_ctx_time::OffsetDateTime::parse_from_rfc3339(
        status.vaults[0].next_attempt_at.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(
        (next - at).num_seconds(),
        90,
        "the status shows LeanCTX Cloud's wait"
    );
    // Without a Retry-After, the exponential backoff (±20 % jitter).
    let unsaid = CloudError::RateLimited { retry_after: None };
    let wait = failure_wait(&unsaid, 1, lean_ctx_time::UtcDateTime::now());
    assert!(wait >= Duration::from_secs(24) && wait <= Duration::from_secs(36));
    // Review of 5!233 (a): a Retry-After of hours waits `WAIT_MAX` at most.
    let hours = CloudError::RateLimited {
        retry_after: Some(6 * 3600),
    };
    assert_eq!(
        failure_wait(&hours, 1, lean_ctx_time::UtcDateTime::now()),
        WAIT_MAX
    );
    assert!(failure_wait(&unsaid, 40, lean_ctx_time::UtcDateTime::now()) <= WAIT_MAX);
}

/// An hourly upload allowance used up waits as long as LeanCTX Cloud says.
#[test]
fn an_upload_rate_limit_waits_for_its_retry_after() {
    let limited = CloudError::UploadRateLimited {
        retry_after: Some(1200),
        per_hour: Some(256 << 20),
    };
    assert_eq!(
        failure_wait(&limited, 1, lean_ctx_time::UtcDateTime::now()),
        Duration::from_mins(20)
    );
    assert_eq!(
        failure_wait(&limited, 9, lean_ctx_time::UtcDateTime::now()),
        Duration::from_mins(20),
        "not the backoff of the ninth failure"
    );
    let unsaid = CloudError::UploadRateLimited {
        retry_after: None,
        per_hour: None,
    };
    let wait = failure_wait(&unsaid, 1, lean_ctx_time::UtcDateTime::now());
    assert!(wait >= Duration::from_secs(24) && wait <= Duration::from_secs(36));
}

/// Storage full parks the vault for [`PARK_RECHECK`], keeps the park
/// through other failures, and a success (in the thread, or a manual sync
/// the thread adopts) ends it.
#[test]
fn storage_full_parks_the_vault_until_a_sync_succeeds() {
    let record = VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    };
    let vault = super::super::place::place_id(&record.vault_id);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let full = CloudError::StorageFull(Box::new(crate::error::StorageFull {
        limit: Some(1 << 30),
        used: Some(1 << 30),
        next_step: None,
    }));
    let wait = note(
        &record,
        Trigger::Local,
        Err(&full),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(
        wait,
        Some(PARK_RECHECK),
        "not the 30 s backoff, not WAIT_MAX"
    );
    assert!(status.vaults[0].parked && runs[&vault].parked);
    assert!(row_parked(&status, &vault));
    let line = park_line(&record, &full);
    assert!(
        line.starts_with(
            "background sync: @acme/app is paused: this workspace's LeanCTX Cloud storage is full (1 GiB of 1 GiB)"
        ),
        "{line}"
    );
    assert!(
        line.ends_with(
            "Run `lean-ctx cloud sync` once storage has room (it is also tried again every 6 h)"
        ),
        "{line}"
    );
    // Offline at the recheck: still parked, still the long wait.
    let offline = CloudError::Offline("down".into());
    let wait = note(
        &record,
        Trigger::Periodic,
        Err(&offline),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(PARK_RECHECK));
    assert!(status.vaults[0].parked);
    assert_eq!(
        status.vaults[0].code.as_deref(),
        Some("storage_full"),
        "the park's reason stays"
    );
    // The row round-trips.
    let text = serde_json::to_string(&status).unwrap();
    assert!(text.contains("\"parked\":true"));
    assert_eq!(serde_json::from_str::<SyncStatus>(&text).unwrap(), status);
    // A manual sync succeeded and unparked the row: the thread adopts it
    // and the vault is due again at once.
    let now = Instant::now();
    let run = runs.get_mut(&vault).unwrap();
    adopt_park(run, None, now);
    assert!(!run.parked && run.due.retry_at.is_none() && run.failures == 0);
    // Parked by another process: the thread holds it as long.
    adopt_park(run, row_park(&status, &vault), now);
    assert_eq!(run.due.retry_at, Some(now + PARK_RECHECK));
    // A success in the thread unparks; an unparked row writes no `parked`.
    let done = super::super::place::VaultSync::default();
    note(
        &record,
        Trigger::Periodic,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    assert!(!status.vaults[0].parked && !runs[&vault].parked);
    assert!(
        serde_json::to_string(&status)
            .expect("status serializes")
            .contains("\"parked\":false")
    );
    assert_eq!(status.vaults[0].code, None, "a success clears the code");
}

fn record() -> VaultRecord {
    VaultRecord {
        vault_id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        ns: "acme".into(),
        name: "app".into(),
        sealed: false,
        roots: Vec::new(),
    }
}

/// Cloud contact findings B: a `rate_limited` answered again and again
/// waits longer each time (the backoff of its failures), with the cloud's
/// `Retry-After` as the floor, up to `WAIT_MAX`.
#[test]
fn a_rate_limit_in_a_row_backs_off_exponentially() {
    let short = CloudError::RateLimited {
        retry_after: Some(5),
    };
    let now = lean_ctx_time::UtcDateTime::now();
    let first = failure_wait(&short, 1, now);
    assert!(
        first >= Duration::from_secs(24) && first <= Duration::from_secs(36),
        "{first:?}"
    );
    let fourth = failure_wait(&short, 4, now);
    assert!(
        fourth >= Duration::from_secs(192) && fourth <= Duration::from_secs(288),
        "doubles: {fourth:?}"
    );
    assert!(failure_wait(&short, 30, now) >= Duration::from_secs(BACKOFF_MAX.as_secs() * 4 / 5));
    assert!(failure_wait(&short, 30, now) <= WAIT_MAX);
    // Repeated notes of one vault grow its wait.
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let waits: Vec<Duration> = (0..5)
        .map(|_| {
            note(
                &record(),
                Trigger::Periodic,
                Err(&short),
                &mut runs,
                &mut status,
                None,
            )
            .unwrap()
        })
        .collect();
    assert!(waits[4] > waits[0] * 8, "{waits:?}");
    let row = &status.vaults[0];
    assert_eq!(row.code.as_deref(), Some("rate_limited"));
    assert_eq!(row.retry_after_s, Some(5));
    assert!(!row.parked, "a rate limit is waited out, not parked");
}

/// Cloud contact findings B, D: `plan_required` parks the vault like a
/// full storage, with no retry by itself (no `next_attempt_at`), until
/// the plan state changes; the status names it as `code`.
#[test]
fn plan_required_parks_the_vault_until_the_plan_changes() {
    let vault = super::super::place::place_id(&record().vault_id);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let need = CloudError::from_response(
        402,
        &serde_json::json!({"error": {"code": "plan_required", "message": "x"}}),
    );
    assert!(matches!(need, CloudError::PlanRequired(_)), "{need:?}");
    let wait = note(
        &record(),
        Trigger::Local,
        Err(&need),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(PLAN_PARK), "no backoff loop");
    let row = &status.vaults[0];
    assert!(row.parked && runs[&vault].parked);
    assert_eq!(row.code.as_deref(), Some("plan_required"));
    assert_eq!(row.next_attempt_at, None, "no attempt is planned");
    assert_eq!(row.link_park, None, "a plan park is no link park");
    assert_eq!(
        row.plan_park.as_deref(),
        Some(PLAN),
        "the park keeps its plan state"
    );
    assert_eq!(row_park(&status, &vault), Some(PLAN_PARK));
    let text = serde_json::to_string(&status).unwrap();
    assert!(text.contains("\"code\":\"plan_required\""), "{text}");
    assert!(text.contains("\"retry_after_s\":null"), "{text}");
    assert_eq!(serde_json::from_str::<SyncStatus>(&text).unwrap(), status);
    assert!(park_line(&record(), &need).contains("when the plan changes"));
    // Another process's thread holds it as long.
    let now = Instant::now();
    let mut other = VaultRun::default();
    adopt_park(&mut other, row_park(&status, &vault), now);
    assert_eq!(other.due.retry_at, Some(now + PLAN_PARK));
    // A storage park is not lifted by a plan change.
    let mut storage = status.clone();
    storage.vaults[0].code = Some("storage_full".into());
    storage.vaults[0].plan_park = None;
    assert!(!unpark_plan(&mut storage, "another plan"));
    assert!(storage.vaults[0].parked);
    // The same plan state keeps the park.
    assert!(!unpark_plan(&mut status, PLAN));
    assert!(status.vaults[0].parked);
    // A reconnect does not lift a plan park.
    assert!(!unpark_link(&mut status, "another link"));
    assert!(status.vaults[0].parked);
    // The plan state changed: unparked, due again at once.
    assert!(unpark_plan(&mut status, "another plan"));
    assert!(!status.vaults[0].parked && status.vaults[0].plan_park.is_none());
    adopt_park(
        runs.get_mut(&vault).unwrap(),
        row_park(&status, &vault),
        now,
    );
    assert!(!runs[&vault].parked && runs[&vault].due.retry_at.is_none());
    assert!(!unpark_plan(&mut status, "another plan"), "once");
}

/// Review of 5!443 (1): the plan park was compared with the thread's
/// in-memory plan state, taken when the thread started, and the plan state
/// had no part that changes with a new lease. A daemon restarted after the
/// plan became valid, or a Team trial that became a paid Team (a new lease
/// with the same plan and rights), kept the vault parked for 30 days. The
/// park now stores the plan state it was taken under.
#[test]
fn a_plan_park_ends_with_a_new_lease_of_the_same_plan_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    let mut shared = record();
    shared.roots = vec![checkout.path().to_string_lossy().into_owned()];
    let vault = super::super::place::place_id(&shared.vault_id);
    let mut map = VaultMap::default();
    map.vaults
        .insert("github.com/acme/app".into(), shared.clone());
    let lease = |received_at: &str| crate::state::LeaseRecord {
        envelope: serde_json::json!({"lease": {"plan": "team", "rights": ["sync"]}}),
        offset_seconds: 0,
        received_at: received_at.into(),
    };
    store
        .write_config(crate::state::LEASE_FILE, &lease("2026-11-17T10:00:00Z"))
        .unwrap();
    // The trial ended: the sync meets `plan_required` and parks the vault.
    let need = CloudError::from_response(
        402,
        &serde_json::json!({"error": {"code": "plan_required", "message": "x"}}),
    );
    let wait = note_outside(&store, &shared, Trigger::Local, Err(&need));
    assert_eq!(wait, Some(PLAN_PARK));
    // A fresh thread (the daemon restarted) on the first look of a pass.
    let fresh_pass = || {
        let mut status = load_status(store.data_dir());
        let unparked = unpark_plan(&mut status, &plan_state(&store));
        let mut runs = BTreeMap::new();
        let mark = |_: &VaultRecord, _: &str| Mark::default();
        let due = plan_pass(
            &map,
            &mut runs,
            &mut status,
            &BTreeMap::new(),
            Instant::now(),
            INTERVAL,
            true,
            &mark,
        );
        (unparked, due.len())
    };
    // Nothing changed: still parked, not due.
    assert_eq!(fresh_pass(), (false, 0));
    assert!(row_parked(&load_status(store.data_dir()), &vault));
    // Paid: a new lease with the same plan and rights arrived before the
    // daemon started again. Its first pass unparks the vault, due at once.
    store
        .write_config(crate::state::LEASE_FILE, &lease("2026-11-18T09:00:00Z"))
        .unwrap();
    assert_eq!(fresh_pass(), (true, 1));
}

/// The plan state a `plan_required` park waits for changes with the
/// lease's plan and with the workspace lock.
#[test]
fn the_plan_state_follows_the_lease_plan_and_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    let none = plan_state(&store);
    let lease = |plan: &str| crate::state::LeaseRecord {
        envelope: serde_json::json!({"lease": {"plan": plan, "rights": ["sync"]}}),
        offset_seconds: 0,
        received_at: "2026-11-17T10:00:00Z".into(),
    };
    store
        .write_config(crate::state::LEASE_FILE, &lease("personal"))
        .unwrap();
    let personal = plan_state(&store);
    assert_ne!(personal, none);
    assert_eq!(plan_state(&store), personal, "stable");
    store
        .write_config(crate::state::LEASE_FILE, &lease("team"))
        .unwrap();
    let team = plan_state(&store);
    assert_ne!(team, personal);
    store.record_locked(Some("trial_ended"));
    assert_ne!(plan_state(&store), team);
}

/// Review of the field's 2'206 vault retries: a `404` was noted as a plain
/// `error` and retried every backoff, forever. It is `not_found` now; a
/// `401` is `needs_login`; a `404 feature_disabled` and every other code
/// stay as they were.
#[test]
fn a_404_is_named_not_found_and_other_codes_stay() {
    let unknown = CloudError::from_response(
        404,
        &serde_json::json!({"error": {"code": "not_found", "message": "unknown instance"}}),
    );
    assert!(matches!(unknown, CloudError::NotFound), "{unknown:?}");
    assert_eq!(code_of(&unknown), "not_found");
    let bare = CloudError::from_response(404, &serde_json::json!("<html>"));
    assert!(
        matches!(bare, CloudError::Server { status: 404, .. }),
        "{bare:?}"
    );
    assert!(is_unknown(&bare));
    assert_eq!(code_of(&bare), "not_found");
    let switched_off = CloudError::FeatureDisabled { feature: None };
    assert!(
        !is_unknown(&switched_off),
        "a switched-off feature is no 404"
    );
    assert_eq!(code_of(&switched_off), "error");
    let server = CloudError::Server {
        status: 500,
        code: "unknown".into(),
    };
    assert_eq!(code_of(&server), "error");
    assert_eq!(code_of(&CloudError::Unauthorized), "needs_login");
    assert_eq!(code_of(&CloudError::Offline("x".into())), "offline");
    assert_eq!(
        code_of(&CloudError::RateLimited { retry_after: None }),
        "rate_limited"
    );
}

/// Let every counted `404` of `status` lie further back than
/// [`NOT_FOUND_SPACING`], as if the next one came much later.
fn age_404s(status: &mut SyncStatus) {
    for row in &mut status.vaults {
        if row.not_found_at.is_some() {
            row.not_found_at = Some("2000-01-01T00:00:00Z".into());
        }
    }
}

/// Note `error` [`NOT_FOUND_PARK`] times, each spaced as a counted `404`.
fn three_404s(
    error: &CloudError,
    runs: &mut BTreeMap<String, VaultRun>,
    status: &mut SyncStatus,
) -> Option<Duration> {
    let mut wait = None;
    for _ in 0..NOT_FOUND_PARK {
        age_404s(status);
        wait = note(&record(), Trigger::Periodic, Err(error), runs, status, None);
    }
    wait
}

/// Review 84: a snapshot revision that rotated mid-clone (`clone_place`
/// retries once, then reports `404 not_found`) or a single `404` from a
/// proxy during a Cloud deploy does not park: it is `not_found` and backs
/// off like any other failure. A burst of `404`s within
/// [`NOT_FOUND_SPACING`] counts once.
#[test]
fn snapshot_rotation_and_a_single_edge_404_do_not_park() {
    let vault = super::super::place::place_id(&record().vault_id);
    let bare = CloudError::from_response(404, &serde_json::json!("<html>"));
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    // The rotation's `404 not_found`.
    let wait = note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::NotFound),
        &mut runs,
        &mut status,
        None,
    )
    .expect("a failure waits");
    assert!(wait <= WAIT_MAX && wait < PARK_RECHECK, "{wait:?}");
    let row = &status.vaults[0];
    assert!(!row.parked && !runs[&vault].parked);
    assert_eq!(row.code.as_deref(), Some("error"), "not_found only parked");
    assert_eq!(row.not_found_streak, 1);
    assert!(row.not_found_at.is_some());
    assert_eq!(row.link_park, None);
    assert!(row.next_attempt_at.is_some(), "the backoff is planned");
    assert_eq!(row_park(&status, &vault), None);
    // An edge `404` and another rotation right after: one signal, not three.
    for error in [&bare, &CloudError::NotFound, &bare] {
        note(
            &record(),
            Trigger::Periodic,
            Err(error),
            &mut runs,
            &mut status,
            None,
        );
    }
    let row = &status.vaults[0];
    assert!(!row.parked);
    assert_eq!(row.not_found_streak, 1, "a burst counts once");
    // Another failure kind breaks the streak, a success clears it.
    age_404s(&mut status);
    note(
        &record(),
        Trigger::Periodic,
        Err(&bare),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(status.vaults[0].not_found_streak, 2);
    note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::Offline("down".into())),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(status.vaults[0].not_found_streak, 0);
    assert_eq!(status.vaults[0].not_found_at, None);
    note(
        &record(),
        Trigger::Periodic,
        Err(&bare),
        &mut runs,
        &mut status,
        None,
    );
    let done = super::super::place::VaultSync::default();
    note(
        &record(),
        Trigger::Periodic,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    let row = &status.vaults[0];
    assert_eq!(
        (row.not_found_streak, row.not_found_at.as_deref()),
        (0, None)
    );
    let text = serde_json::to_string(&status).expect("status serializes");
    assert!(!text.contains("not_found"), "{text}");
}

/// Review 84: [`NOT_FOUND_PARK`] `404`s in a row, spaced, park the vault as
/// `not_found` with the link state; the park is rechecked after
/// [`PARK_RECHECK`] (6 h), also by a fresh thread, and another failure
/// keeps it and its reason.
#[test]
fn three_spaced_404s_park_and_are_rechecked_after_6_h() {
    let vault = super::super::place::place_id(&record().vault_id);
    let bare = CloudError::from_response(404, &serde_json::json!("<html>"));
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    // Two do not park yet.
    for error in [&CloudError::NotFound, &bare] {
        age_404s(&mut status);
        note(
            &record(),
            Trigger::Periodic,
            Err(error),
            &mut runs,
            &mut status,
            None,
        );
        assert!(!status.vaults[0].parked);
    }
    age_404s(&mut status);
    let wait = note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::NotFound),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(PARK_RECHECK), "no backoff loop");
    assert_eq!(PARK_RECHECK, Duration::from_hours(6));
    let row = &status.vaults[0];
    assert!(row.parked && runs[&vault].parked);
    assert_eq!(row.code.as_deref(), Some("not_found"));
    assert_eq!(row.not_found_streak, NOT_FOUND_PARK);
    assert_eq!(row.link_park.as_deref(), Some(LINK));
    assert_eq!(row.plan_park, None);
    assert!(row.next_attempt_at.is_some(), "the recheck is planned");
    assert_eq!(row_park(&status, &vault), Some(PARK_RECHECK));
    let text = serde_json::to_string(&status).expect("status serializes");
    assert!(text.contains("\"code\":\"not_found\""), "{text}");
    assert!(text.contains("\"parked\":true"), "{text}");
    assert_eq!(
        serde_json::from_str::<SyncStatus>(&text).expect("status parses"),
        status
    );
    // Not retried before the recheck, not on a purge; due once it passed.
    let now = Instant::now();
    let run = runs.get_mut(&vault).expect("the vault has a run");
    for later in [WAIT_MAX, BACKOFF_MAX * 4, Duration::from_mins(359)] {
        assert_eq!(decide(&run.due, now + later, INTERVAL), None, "{later:?}");
    }
    run.due.urgent = Some(Trigger::BusPurge);
    assert_eq!(decide(&run.due, now + WAIT_MAX, INTERVAL), None, "purge");
    run.due.urgent = None;
    assert!(
        decide(
            &run.due,
            now + PARK_RECHECK + INTERVAL + BURST_WINDOW + MIN_SPACING,
            INTERVAL
        )
        .is_some()
    );
    // A fresh thread holds it exactly as long.
    let mut other = VaultRun::default();
    adopt_park(&mut other, row_park(&status, &vault), now);
    assert_eq!(other.due.retry_at, Some(now + PARK_RECHECK));
    // An offline recheck keeps the park and its reason.
    let wait = note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::Offline("down".into())),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(PARK_RECHECK));
    let row = &status.vaults[0];
    assert!(row.parked);
    assert_eq!(row.code.as_deref(), Some("not_found"), "the reason stays");
    assert_eq!(row.link_park.as_deref(), Some(LINK));
    // The same link state and a plan change keep it.
    assert!(!unpark_link(&mut status, LINK));
    assert!(!unpark_plan(&mut status, "another plan"));
    assert!(status.vaults[0].parked);
    // Deterministic: the same failures write the same row (but the times).
    let mut again = SyncStatus::default();
    three_404s(&CloudError::NotFound, &mut BTreeMap::new(), &mut again);
    let mut parked = SyncStatus::default();
    three_404s(&CloudError::NotFound, &mut BTreeMap::new(), &mut parked);
    again.vaults[0]
        .last_sync_at
        .clone_from(&parked.vaults[0].last_sync_at);
    again.vaults[0]
        .not_found_at
        .clone_from(&parked.vaults[0].not_found_at);
    again.vaults[0]
        .next_attempt_at
        .clone_from(&parked.vaults[0].next_attempt_at);
    assert_eq!(again, parked);
}

/// Review 84 (pairs with p43 !608): a reconnect or another configured
/// Cloud (another link state) ends a `not_found` park, due again at once,
/// and clears `code`, `error` and the streak, so the app shows no stale
/// reason; a successful sync ends it too.
#[test]
fn a_not_found_park_ends_with_a_reconnect_or_a_success_and_clears_its_reason() {
    let vault = super::super::place::place_id(&record().vault_id);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    three_404s(&CloudError::NotFound, &mut runs, &mut status);
    assert!(status.vaults[0].parked);
    let parked = status.clone();
    // Reconnected: unparked, its reason gone, due again at once.
    assert!(unpark_link(
        &mut status,
        "inst-2|https://cloud.example|2026-11-19T08:00:00Z|"
    ));
    let row = &status.vaults[0];
    assert!(!row.parked && row.link_park.is_none() && row.next_attempt_at.is_none());
    assert_eq!(row.code, None, "code not_found is cleared");
    assert_eq!(row.error, None, "the error text is cleared");
    assert_eq!(row.failures, 0);
    assert_eq!(
        (row.not_found_streak, row.not_found_at.as_deref()),
        (0, None)
    );
    let text = serde_json::to_string(&status).expect("status serializes");
    assert!(
        !text.contains("not_found") && text.contains("\"parked\":false"),
        "{text}"
    );
    let now = Instant::now();
    let run = runs.get_mut(&vault).expect("the vault has a run");
    adopt_park(run, row_park(&status, &vault), now);
    assert!(!run.parked && run.due.retry_at.is_none() && run.failures == 0);
    assert!(
        decide(
            &run.due,
            now + INTERVAL + BURST_WINDOW + MIN_SPACING,
            INTERVAL
        )
        .is_some()
    );
    assert!(!unpark_link(&mut status, "another"), "once");
    // A success (a manual sync) unparks it too and clears the link state.
    let mut status = parked;
    let done = super::super::place::VaultSync::default();
    note(
        &record(),
        Trigger::Manual,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    let row = &status.vaults[0];
    assert!(!row.parked && row.link_park.is_none() && row.code.is_none());
    let text = serde_json::to_string(&status).expect("status serializes");
    assert!(
        !text.contains("link_park") && text.contains("\"parked\":false"),
        "{text}"
    );
}

/// Review 84, one rule with p43 !608: a `401` holds the background sync of
/// the vault (no backoff loop) and marks its link `needs_login`, which is no
/// park (`parked` stays `false`); a login or reconnect (another link state)
/// or a successful sync clears it. Review 84 R1: the hold is rechecked
/// after `LOGIN_RECHECK_FIRST` (6 h), then every `LOGIN_RECHECK` (24 h), so
/// a short Cloud auth outage does not stop every machine for good.
#[test]
fn a_401_marks_the_link_needs_login_until_a_login() {
    let vault = super::super::place::place_id(&record().vault_id);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let wait = note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::Unauthorized),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(wait, Some(LOGIN_RECHECK_FIRST), "no backoff loop");
    let row = &status.vaults[0];
    assert!(!row.parked, "needs_login is no park");
    assert!(row.needs_login);
    assert_eq!(row.code.as_deref(), Some("needs_login"));
    assert_eq!(row.link_park.as_deref(), Some(LINK));
    assert!(row.next_attempt_at.is_some(), "the recheck is planned");
    assert!(runs[&vault].parked, "the thread holds it");
    assert_eq!(row_park(&status, &vault), Some(LOGIN_RECHECK_FIRST));
    let text = serde_json::to_string(&status).expect("status serializes");
    assert!(text.contains("\"code\":\"needs_login\""), "{text}");
    assert!(text.contains("\"parked\":false"), "{text}");
    assert!(text.contains("\"needs_login\":true"), "{text}");
    // Not tried before the recheck, not by a fresh thread either.
    let now = Instant::now();
    let run = runs.get_mut(&vault).expect("the vault has a run");
    for later in [WAIT_MAX, Duration::from_hours(5)] {
        assert_eq!(decide(&run.due, now + later, INTERVAL), None, "{later:?}");
    }
    assert!(
        decide(&run.due, now + LOGIN_RECHECK_FIRST, INTERVAL).is_some(),
        "rechecked after 6 h"
    );
    let mut other = VaultRun::default();
    adopt_park(&mut other, row_park(&status, &vault), now);
    assert_eq!(other.due.retry_at, Some(now + LOGIN_RECHECK_FIRST));
    // A manual sync that fails offline keeps the reason.
    note(
        &record(),
        Trigger::Manual,
        Err(&CloudError::Offline("down".into())),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(status.vaults[0].code.as_deref(), Some("needs_login"));
    let line = park_line(&record(), &CloudError::Unauthorized);
    assert!(line.contains("after 6 h, then every 24 h"), "{line}");
    assert!(line.contains("lean-ctx cloud connect"), "{line}");
    assert!(!line.contains("login"), "{line}");
    // The same link keeps it; a login (a new enrollment) clears it.
    assert!(!unpark_link(&mut status, LINK));
    let held = status.clone();
    assert!(unpark_link(
        &mut status,
        "inst-1|https://cloud.example|2026-11-19T08:00:00Z|"
    ));
    let row = &status.vaults[0];
    assert_eq!((row.code.as_deref(), row.error.as_deref()), (None, None));
    assert!(row.link_park.is_none() && !row.parked && !row.needs_login);
    assert_eq!(row_park(&status, &vault), None);
    // A successful sync clears it as well.
    let mut status = held;
    let done = super::super::place::VaultSync::default();
    note(
        &record(),
        Trigger::Manual,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    assert_eq!(status.vaults[0].code, None);
    assert!(!status.vaults[0].needs_login);
    assert_eq!(row_park(&status, &vault), None);
    // A `401` replaces a storage park: the vault needs a login, not room.
    let mut status = SyncStatus::default();
    let full = CloudError::StorageFull(Box::new(crate::error::StorageFull {
        limit: Some(1 << 30),
        used: Some(1 << 30),
        next_step: None,
    }));
    note(
        &record(),
        Trigger::Periodic,
        Err(&full),
        &mut BTreeMap::new(),
        &mut status,
        None,
    );
    assert!(status.vaults[0].parked);
    note(
        &record(),
        Trigger::Manual,
        Err(&CloudError::Unauthorized),
        &mut BTreeMap::new(),
        &mut status,
        None,
    );
    let row = &status.vaults[0];
    assert!(!row.parked && row.needs_login);
    assert_eq!(row.code.as_deref(), Some("needs_login"));
}

/// Review 84 R1: a `needs_login` recheck that Cloud refuses again waits
/// `LOGIN_RECHECK` (24 h), not the first 6 h again and not for good; a
/// recheck that succeeds (the auth outage is over) ends the hold.
#[test]
fn a_needs_login_recheck_backs_off_to_a_day_and_heals_on_success() {
    let vault = super::super::place::place_id(&record().vault_id);
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let refused = |runs: &mut BTreeMap<String, VaultRun>, status: &mut SyncStatus| {
        note(
            &record(),
            Trigger::Periodic,
            Err(&CloudError::Unauthorized),
            runs,
            status,
            None,
        )
    };
    assert_eq!(
        refused(&mut runs, &mut status),
        Some(LOGIN_RECHECK_FIRST),
        "the first 401"
    );
    for _ in 0..2 {
        assert_eq!(
            refused(&mut runs, &mut status),
            Some(LOGIN_RECHECK),
            "a recheck refused again"
        );
        let row = &status.vaults[0];
        assert!(row.needs_login && !row.parked);
        assert!(row.next_attempt_at.is_some());
    }
    let now = Instant::now();
    let due = &runs[&vault].due;
    assert_eq!(decide(due, now + Duration::from_hours(23), INTERVAL), None);
    assert!(decide(due, now + LOGIN_RECHECK, INTERVAL).is_some());
    let done = super::super::place::VaultSync::default();
    let wait = note(
        &record(),
        Trigger::Periodic,
        Ok(&done),
        &mut runs,
        &mut status,
        Some(0),
    );
    assert_eq!(wait, None);
    let row = &status.vaults[0];
    assert!(!row.needs_login && row.code.is_none() && row.link_park.is_none());
    assert_eq!(row_park(&status, &vault), None);
    // A later `401` starts with the short recheck again.
    assert_eq!(refused(&mut runs, &mut status), Some(LOGIN_RECHECK_FIRST));
}

/// The link state a `not_found` park and `needs_login` wait for changes
/// with a reconnect (a new enrollment) and with the configured Cloud, and
/// with nothing else; a fresh thread (a restarted daemon) compares against
/// the stored one.
#[test]
fn the_link_state_follows_a_reconnect_and_the_configured_cloud() {
    let dir = tempfile::tempdir().expect("temp dir");
    let checkout = tempfile::tempdir().expect("temp dir");
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    let config = CloudConfig::default();
    let none = link_state(&store, &config);
    let enroll = |id: &str, enrolled_at: &str| crate::state::InstanceRecord {
        instance_id: id.into(),
        org_id: "org".into(),
        org_name: "Acme".into(),
        label: "laptop".into(),
        url: "https://cloud.example".into(),
        org_salt_id: "salt".into(),
        enrolled_at: enrolled_at.into(),
        web_url: None,
        org_kind: None,
        company_name: None,
    };
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &enroll("inst-1", "2026-11-17T09:00:00Z"),
        )
        .expect("instance written");
    let first = link_state(&store, &config);
    assert_ne!(first, none);
    assert_eq!(link_state(&store, &config), first, "stable");
    // A lease or a plan change is not a link change.
    store.record_locked(Some("trial_ended"));
    assert_eq!(link_state(&store, &config), first);
    // Another configured Cloud.
    let moved = CloudConfig {
        url: Some("https://other.example".into()),
        ..CloudConfig::default()
    };
    assert_ne!(link_state(&store, &moved), first);
    // A fresh thread on a held status: same link, still held and not due.
    let mut shared = record();
    shared.roots = vec![checkout.path().to_string_lossy().into_owned()];
    let vault = super::super::place::place_id(&shared.vault_id);
    let mut map = VaultMap::default();
    map.vaults
        .insert("github.com/acme/app".into(), shared.clone());
    let plan = || PLAN.to_string();
    let link = || first.clone();
    let states = ParkStates {
        plan: &plan,
        link: &link,
    };
    let mut status = SyncStatus::default();
    super::note(
        &shared,
        Trigger::Local,
        Err(&CloudError::Unauthorized),
        &mut BTreeMap::new(),
        &mut status,
        None,
        &states,
    );
    let fresh_pass = |status: &mut SyncStatus| {
        let unparked = unpark_link(status, &link_state(&store, &config));
        let mut runs = BTreeMap::new();
        let mark = |_: &VaultRecord, _: &str| Mark::default();
        let due = plan_pass(
            &map,
            &mut runs,
            status,
            &BTreeMap::new(),
            Instant::now(),
            INTERVAL,
            true,
            &mark,
        );
        (unparked, due.len())
    };
    assert_eq!(fresh_pass(&mut status), (false, 0));
    assert!(row_parked(&status, &vault));
    // Reconnected (a fresh device link writes a new enrollment).
    store
        .write_config(
            crate::state::INSTANCE_FILE,
            &enroll("inst-2", "2026-11-19T08:00:00Z"),
        )
        .expect("instance written");
    assert_eq!(fresh_pass(&mut status), (true, 1));
}

/// A plan park and a `not_found` park replace each other: the newer
/// reason sets its own state and clears the other, so only the state it
/// waits for unparks it.
#[test]
fn a_plan_park_and_a_not_found_park_replace_each_other() {
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    let plan_required = || {
        CloudError::from_response(
            402,
            &serde_json::json!({"error": {"code": "plan_required", "message": "no plan"}}),
        )
    };
    assert!(matches!(plan_required(), CloudError::PlanRequired(_)));
    note(
        &record(),
        Trigger::Periodic,
        Err(&plan_required()),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(status.vaults[0].plan_park.as_deref(), Some(PLAN));
    // A single 404 keeps the plan park and its reason.
    note(
        &record(),
        Trigger::Manual,
        Err(&CloudError::NotFound),
        &mut runs,
        &mut status,
        None,
    );
    assert_eq!(status.vaults[0].code.as_deref(), Some("plan_required"));
    // Three in a row replace it.
    let wait = three_404s(&CloudError::NotFound, &mut runs, &mut status);
    assert_eq!(wait, Some(PARK_RECHECK));
    let row = &status.vaults[0];
    assert!(row.parked);
    assert_eq!(row.code.as_deref(), Some("not_found"));
    assert_eq!(row.link_park.as_deref(), Some(LINK));
    assert_eq!(row.plan_park, None);
    assert!(row.next_attempt_at.is_some());
    assert!(
        !unpark_plan(&mut status, "another plan"),
        "no plan park left"
    );
    assert!(status.vaults[0].parked);
    // plan_required replaces the not_found park.
    note(
        &record(),
        Trigger::Manual,
        Err(&plan_required()),
        &mut runs,
        &mut status,
        None,
    );
    let row = &status.vaults[0];
    assert!(row.parked);
    assert_eq!(row.code.as_deref(), Some("plan_required"));
    assert_eq!(row.plan_park.as_deref(), Some(PLAN));
    assert_eq!(row.link_park, None);
    assert_eq!(row.next_attempt_at, None);
    assert!(
        !unpark_link(&mut status, "another link"),
        "no link park left"
    );
    assert!(unpark_plan(&mut status, "another plan"));
    assert!(!status.vaults[0].parked);
}

/// Review 84 nit: a process that is not the daemon (a push on write) ends
/// a hold whose link state this machine no longer has before it looks at
/// the park, and writes the status file only then.
#[test]
fn a_push_on_write_sees_a_login_at_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
    let config = CloudConfig::default();
    let id = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
    let row = |link: &str| VaultSyncStatus {
        vault_id: super::super::place::place_id(id),
        code: Some(NEEDS_LOGIN.into()),
        needs_login: true,
        error: Some("refused".into()),
        link_park: Some(link.into()),
        ..VaultSyncStatus::default()
    };
    let held = |link: &str| SyncStatus {
        vaults: vec![row(link)],
        ..SyncStatus::default()
    };
    // Under this machine's link state: still held.
    save_status(store.data_dir(), &held(&link_state(&store, &config)));
    release_holds(&store, &config);
    assert!(is_parked(store.data_dir(), id), "needs_login holds a push");
    // Taken under another one (a login since): released.
    save_status(store.data_dir(), &held("inst-0|old|x|"));
    release_holds(&store, &config);
    assert!(!is_parked(store.data_dir(), id));
    let row = &load_status(store.data_dir()).vaults[0];
    assert_eq!((row.code.as_deref(), row.error.as_deref()), (None, None));
    // Nothing held (the common push): no status file is written.
    let file = sync_dir(store.data_dir()).join(STATUS_FILE);
    std::fs::remove_file(&file).expect("status file removed");
    release_holds(&store, &config);
    assert!(!file.exists(), "no hold, no write");
}

/// A push on write skips a parked vault; the daemon's recheck or a manual
/// sync takes it along.
#[test]
fn a_parked_vault_is_seen_by_every_process() {
    let dir = tempfile::tempdir().unwrap();
    let id = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
    assert!(!is_parked(dir.path(), id));
    let status = SyncStatus {
        vaults: vec![VaultSyncStatus {
            vault_id: super::super::place::place_id(id),
            parked: true,
            ..VaultSyncStatus::default()
        }],
        ..SyncStatus::default()
    };
    save_status(dir.path(), &status);
    assert!(is_parked(dir.path(), id));
    assert!(!is_parked(dir.path(), "another"));
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
/// the daemon sync that vault after the debounce, with no manual `cloud
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
    // Cloud off / not C0 2.x: a change is noted, nothing syncs.
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

/// The keys the app (p43 !608) reads from `lean-ctx cloud sync status
/// --json`: `parked` and `needs_login` are always written as booleans,
/// `code` `not_found` only with `parked: true`, `needs_login: true` with
/// `code` `needs_login` and a `next_attempt_at` (its recheck).
#[test]
fn a_status_row_names_the_keys_the_app_reads() {
    let keys = |row: &VaultSyncStatus| {
        let value = serde_json::to_value(row).expect("row serializes");
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        (value, keys)
    };
    let base = [
        "code",
        "error",
        "failures",
        "last_ok_at",
        "last_sync_at",
        "last_trigger",
        "needs_login",
        "next_attempt_at",
        "parked",
        "pending",
        "pushed",
        "retry_after_s",
        "stored",
        "vault",
        "vault_id",
    ];
    let (value, found) = keys(&VaultSyncStatus::default());
    assert_eq!(found, base, "an idle row");
    assert_eq!(value["parked"], serde_json::json!(false));
    assert_eq!(value["needs_login"], serde_json::json!(false));
    // A 401.
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::Unauthorized),
        &mut runs,
        &mut status,
        None,
    );
    let (value, found) = keys(&status.vaults[0]);
    assert!(found.contains(&"link_park".to_string()), "{found:?}");
    assert_eq!(value["parked"], serde_json::json!(false));
    assert_eq!(value["needs_login"], serde_json::json!(true));
    assert_eq!(value["code"], serde_json::json!("needs_login"));
    assert!(value["next_attempt_at"].is_string());
    // A single 404: no `not_found` yet; three: parked `not_found`.
    let (mut runs, mut status) = (BTreeMap::new(), SyncStatus::default());
    note(
        &record(),
        Trigger::Periodic,
        Err(&CloudError::NotFound),
        &mut runs,
        &mut status,
        None,
    );
    let (value, _) = keys(&status.vaults[0]);
    assert_eq!(value["parked"], serde_json::json!(false));
    assert_ne!(value["code"], serde_json::json!("not_found"));
    three_404s(&CloudError::NotFound, &mut runs, &mut status);
    let (value, _) = keys(&status.vaults[0]);
    assert_eq!(value["parked"], serde_json::json!(true));
    assert_eq!(value["needs_login"], serde_json::json!(false));
    assert_eq!(value["code"], serde_json::json!("not_found"));
    assert!(value["next_attempt_at"].is_string());
}

/// bd 40060: the background pass drops an unlinked root and keeps the
/// others; a damaged opt-out store holds every root back.
#[test]
fn unlink_drops_the_root_from_the_background_pass() {
    let data = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let roots = vec![
        a.path().to_string_lossy().into_owned(),
        b.path().to_string_lossy().into_owned(),
    ];
    let mut map = VaultMap::default();
    map.vaults.insert(
        "s1".into(),
        VaultRecord {
            vault_id: "v1".into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: roots.clone(),
        },
    );
    crate::unlinked::add(data.path(), a.path()).unwrap();
    let mut kept = map.clone();
    super::without_unlinked(data.path(), &mut kept);
    assert_eq!(kept.vaults["s1"].roots, roots[1..].to_vec());

    std::fs::write(data.path().join(crate::unlinked::FILE), "[").unwrap();
    super::without_unlinked(data.path(), &mut map);
    assert!(map.vaults["s1"].roots.is_empty());
}
