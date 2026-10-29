// SPDX-License-Identifier: Apache-2.0

use super::*;

struct TelemetryEnvGuard(Option<std::ffi::OsString>);

impl TelemetryEnvGuard {
    fn disable() -> Self {
        let previous = std::env::var_os("LEAN_CTX_TELEMETRY");
        crate::test_env::set_var("LEAN_CTX_TELEMETRY", "off");
        Self(previous)
    }
}

impl Drop for TelemetryEnvGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => crate::test_env::set_var("LEAN_CTX_TELEMETRY", value),
            None => crate::test_env::remove_var("LEAN_CTX_TELEMETRY"),
        }
    }
}

fn tool_counts(batch: &TelemetryBatchV2) -> (u64, u64) {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::ToolUsageAggregate(metrics) => {
                Some((metrics.calls, metrics.failures))
            }
            _ => None,
        })
        .expect("tool aggregate")
}

fn occurrence_count(batch: &TelemetryBatchV2, name: &str) -> Option<u64> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::SetupCompleted(metrics) if name == "setup_completed" => {
                Some(metrics.count)
            }
            TelemetryEventV2::IntegrationDetected(metrics) if name == "integration_detected" => {
                Some(metrics.count)
            }
            TelemetryEventV2::CheckoutStarted(metrics) if name == "checkout_started" => {
                Some(metrics.count)
            }
            _ => None,
        })
}

fn version_transition(batch: &TelemetryBatchV2) -> Option<(u16, u16)> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::VersionUpgrade(metrics) => {
                Some((metrics.from_major, metrics.to_major))
            }
            _ => None,
        })
}

fn sync_counts(batch: &TelemetryBatchV2) -> Option<(u64, u64, u64)> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::SyncAggregate(metrics) => {
                Some((metrics.attempts, metrics.successes, metrics.failures))
            }
            _ => None,
        })
}

fn autopilot_counts(batch: &TelemetryBatchV2) -> Option<(u64, u64, u64)> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::AutopilotAggregate(metrics) => {
                Some((metrics.admitted, metrics.denied, metrics.fallback))
            }
            _ => None,
        })
}

fn autopilot_fallback_counts(batch: &TelemetryBatchV2) -> Option<(u64, u64, u64)> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::AutopilotFallbackAggregate(metrics) => {
                Some((metrics.admitted, metrics.denied, metrics.fallback))
            }
            _ => None,
        })
}

fn error_count(batch: &TelemetryBatchV2, category: ErrorCategory) -> Option<u64> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::ErrorCategoryAggregate(metrics) if metrics.category == category => {
                Some(metrics.count)
            }
            _ => None,
        })
}

#[test]
fn daily_batch_is_typed_bounded_and_contains_no_runtime_content() {
    let batch = build_daily_heartbeat(
        "550e8400-e29b-41d4-a716-446655440000".into(),
        "a".repeat(64),
        "2026-09-09".into(),
        DistributionChannel::Cargo,
        ClientFamily::Codex,
    )
    .expect("valid batch");
    let json = serde_json::to_string(&batch).expect("serialize batch");
    assert!(batch.validate().is_ok());
    for forbidden in [
        "prompt",
        "source_code",
        "context_content",
        "file_path",
        "filename",
        "command",
        "argument",
        "stdout",
        "stderr",
        "repository_url",
        "task_text",
        "issue_text",
        "error_message",
        "api_key",
    ] {
        assert!(
            !json.contains(forbidden),
            "forbidden key leaked: {forbidden}"
        );
    }
}

#[test]
fn malformed_identity_is_rejected_before_send() {
    assert!(
        build_daily_heartbeat(
            "raw-user-id".into(),
            "a".repeat(64),
            "2026-09-09".into(),
            DistributionChannel::Unknown,
            ClientFamily::Other,
        )
        .is_err()
    );
}

#[test]
#[serial_test::serial]
fn prepare_is_two_phase_and_preview_does_not_advance_state() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    // A fresh state has no baseline, so the first batch carries every failure
    // earlier tests left in this process's global counter.
    let prior_failures = crate::core::telemetry::global_metrics()
        .daily_telemetry_snapshot()
        .tool_failures;
    crate::core::telemetry::global_metrics().record_tool_call(2_000, true);

    let preview = preview_daily_batch().expect("preview");
    assert!(!state_path().expect("state path").exists());
    assert_eq!(tool_counts(&preview).1, prior_failures);

    let pending = prepare_daily_batch().expect("prepare");
    crate::core::telemetry::global_metrics().record_tool_call(4_000, false);
    assert_eq!(prepare_daily_batch().expect("retry"), pending);
    assert_eq!(preview_daily_batch().expect("pending preview"), pending);

    let mut wrong = pending.clone();
    wrong.events[0].app_version.push_str("-different");
    assert!(acknowledge_daily_batch(&wrong).is_err());
    assert_eq!(prepare_daily_batch().expect("still pending"), pending);

    let (calls, failures) = tool_counts(&pending);
    acknowledge_daily_batch(&pending).expect("acknowledge");
    // Same-day batches carry the day's cumulative totals, never deltas.
    let next = preview_daily_batch().expect("next preview");
    assert_eq!(tool_counts(&next), (calls + 1, failures + 1));
}

#[test]
#[serial_test::serial]
fn preview_fails_fast_during_send_then_preserves_concurrent_counts() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_sync_result(false).expect("record included failure");
    let lease = begin_daily_send().expect("begin send");
    let error = preview_daily_batch().expect_err("preview must not race a send");
    assert!(error.contains("send is in progress"));
    record_sync_result(true).expect("record concurrent success");
    lease.commit().expect("commit included failure");
    let preview = preview_daily_batch().expect("preview after send");
    assert_eq!(sync_counts(&preview), Some((2, 1, 1)));
    assert!(today_is_unsent());
}

#[test]
#[serial_test::serial]
fn preview_gives_up_on_held_sidecar_lock_and_recovers() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    record_sync_result(true).expect("record result");
    {
        let path = one_shot_path().expect("sidecar path");
        let lock = open_sidecar_lock(&path).expect("open lock");
        lock.lock_exclusive().expect("hold writer lock");
        let error = preview_daily_batch().expect_err("preview wait for a held writer is bounded");
        assert!(error.contains("cannot lock one-shot state"));
    }
    assert_eq!(
        sync_counts(&preview_daily_batch().expect("preview after writer exits")),
        Some((1, 1, 0))
    );
}

#[test]
fn histogram_edges_are_bounded_and_deterministic() {
    let histogram = single_observation_histogram(51, &[10, 50, 100], MAX_COUNT + 1);
    assert_eq!(histogram.upper_bounds, vec![10, 50, 100]);
    assert_eq!(histogram.counts, vec![0, 0, MAX_COUNT]);
    let capped = single_observation_histogram(101, &[10, 50, 100], 1);
    assert_eq!(capped.counts, vec![0, 0, 1]);

    let saturated = bounded_histogram_delta(&[MAX_COUNT; 9], &[0; 9]);
    assert_eq!(saturated.iter().sum::<u64>(), MAX_COUNT);
    assert_eq!(saturated[0], MAX_COUNT);
    assert!(saturated[1..].iter().all(|count| *count == 0));
}

#[test]
#[serial_test::serial]
fn send_lease_blocks_purge_until_send_finishes() {
    // Judged on outcomes, not on how fast a thread is scheduled: the earlier
    // threaded version failed on a loaded macOS runner when purge had already
    // hit its bounded lock timeout before the test started its 50 ms window.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let lease = begin_daily_send().expect("begin send");
    let state = state_path().expect("state path");
    assert!(
        state.exists(),
        "the send lease persists the aggregate state"
    );

    let error = purge_local_state().expect_err("purge must not run during an in-flight send");
    assert!(
        error.contains("timed out"),
        "unexpected purge error: {error}"
    );
    assert!(
        state.exists(),
        "a refused purge must leave the state intact"
    );

    drop(lease);
    purge_local_state().expect("purge succeeds once the send finished");
    assert!(!state.exists(), "purge removes the aggregate state");
}

/// Two fixed buckets and the clock values that name them. Admission and day
/// attribution are decided by the pinned clock, so none of the tests below can
/// shift meaning across a real UTC midnight.
const BUCKET: &str = "2026-03-01";
const NEXT_BUCKET: &str = "2026-03-02";
/// 2026-03-01T00:00:00Z.
const T0: i64 = 1_772_323_200;
const NEXT_T0: i64 = T0 + 86_400;

fn sidecar() -> OneShotState {
    load_one_shots_at(&one_shot_path().expect("sidecar path")).expect("sidecar")
}

fn day_state(day: &str) -> Option<DayTotals> {
    sidecar().days.get(day).cloned()
}

fn today_is_unsent() -> bool {
    day_state(&current_send_bucket()).is_some_and(|day| day.unsent())
}

fn attempts_today() -> u32 {
    load_state().expect("state").attempts_in_bucket
}

fn event_buckets(batch: &TelemetryBatchV2) -> BTreeSet<String> {
    batch
        .events
        .iter()
        .map(|event| event.timestamp_bucket.clone())
        .collect()
}

/// Take a lease under the pinned clock and acknowledge it, reporting whether
/// this sender was admitted.
fn admit_and_commit() -> bool {
    match begin_daily_send() {
        Ok(lease) => {
            lease.commit().expect("commit admitted batch");
            true
        }
        Err(error) => {
            // A loser that times out on the send lock (seen on a loaded
            // windows-latest runner) was not admitted either.
            assert!(
                error.contains("not due") || error.contains("lock timed out"),
                "{error}"
            );
            false
        }
    }
}

#[test]
#[serial_test::serial]
fn same_day_resends_are_spaced_and_carry_cumulative_totals() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);

    record_sync_result(true).expect("record morning sync");
    let lease = begin_daily_send().expect("first lease");
    // The bucket that decided admission is the bucket that stamps the payload.
    assert_eq!(lease.batch().events[0].timestamp_bucket, BUCKET);
    lease.commit().expect("commit first send");
    assert_eq!(last_sent_bucket().as_deref(), Some(BUCKET));
    assert!(!today_is_unsent());

    // A caller that evaluated "due" before the commit above still arrives
    // here. The refusal happens under the lock and is inert: nothing frozen,
    // no attempt consumed.
    record_sync_result(false).expect("record afternoon sync");
    let error = begin_daily_send().err().expect("resend is spaced");
    assert!(error.contains("not due"), "{error}");
    assert!(load_state().expect("state").pending.is_none());
    assert_eq!(attempts_today(), 1);

    // Once due, the same day is re-sent with its cumulative totals; the
    // server replaces the day's row, so repeats never double count.
    let _clock = TestClockGuard::set(BUCKET, T0 + RESEND_INTERVAL_SECS);
    let lease = begin_daily_send().expect("due resend");
    assert_eq!(lease.batch().events[0].timestamp_bucket, BUCKET);
    assert_eq!(sync_counts(lease.batch()), Some((2, 1, 1)));
    lease.commit().expect("commit resend");
    assert!(!today_is_unsent());
    assert_eq!(attempts_today(), 2);
}

#[test]
#[serial_test::serial]
fn an_up_to_date_day_is_refused_without_consuming_an_attempt() {
    // A send folds this process's live counters, and non-serial tests (MCP
    // tool calls) move them at any moment. A round in which they moved proves
    // nothing about "up to date", so only an undisturbed round is judged.
    for _ in 0..50 {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let before = current_checkpoint();
        let _clock = TestClockGuard::set(BUCKET, T0);
        begin_daily_send()
            .expect("first lease")
            .commit()
            .expect("commit first send");

        let _clock = TestClockGuard::set(BUCKET, T0 + MAX_SEND_INTERVAL_SECS);
        let second = begin_daily_send();
        if current_checkpoint() != before {
            continue;
        }
        let error = second.err().expect("nothing new to send");
        assert!(error.contains("already up to date"), "{error}");
        assert_eq!(attempts_today(), 1);
        assert!(load_state().expect("state").pending.is_none());
        return;
    }
    panic!("process counters never held still for one round");
}

#[test]
#[serial_test::serial]
fn the_next_bucket_is_still_admitted() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    begin_daily_send()
        .expect("first lease")
        .commit()
        .expect("commit first send");

    // Attempts and spacing are per day: the new day starts fresh.
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    let lease = begin_daily_send().expect("next bucket is admitted");
    assert_eq!(lease.batch().events[0].timestamp_bucket, NEXT_BUCKET);
    assert_eq!(attempts_today(), 1);
}

#[test]
#[serial_test::serial]
fn an_acknowledged_bucket_still_retries_its_frozen_pending_batch() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);

    // Freeze a payload and abandon it the way a crash between the network send
    // and the acknowledgement does: the lease drops, `pending` survives.
    let pending = begin_daily_send().expect("first lease").batch().clone();

    let path = state_path().expect("state path");
    let mut state = load_state().expect("load state");
    state.last_sent_bucket = Some(BUCKET.to_string());
    write_state(&path, &state).expect("seed acknowledged bucket");

    // The frozen payload is handed back verbatim once the retry is due --
    // at-least-once delivery of the exact checkpointed batch is deliberate.
    let _clock = TestClockGuard::set(BUCKET, T0 + RETRY_BACKOFF_SECS);
    let lease = begin_daily_send().expect("pending must still retry");
    assert_eq!(lease.batch(), &pending);
}

#[test]
#[serial_test::serial]
fn a_pending_retry_keeps_its_own_bucket_across_a_day_boundary() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);

    let pending = begin_daily_send().expect("first lease").batch().clone();
    assert_eq!(pending.events[0].timestamp_bucket, BUCKET);

    // The day rolls over before the retry. The frozen payload is returned
    // unchanged -- it is not re-stamped with the new day.
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    let lease = begin_daily_send().expect("pending retry");
    assert_eq!(lease.batch(), &pending);
    assert_eq!(lease.batch().events[0].timestamp_bucket, BUCKET);
}

#[test]
#[serial_test::serial]
fn two_callers_contending_over_one_bucket_admit_exactly_one_batch() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    // Pinned once, before either sender starts, so both read the same clock.
    let _clock = TestClockGuard::set(BUCKET, T0);
    assert!(last_sent_bucket().is_none(), "bucket must start unsent");

    // The barrier makes the interleaving deterministic instead of hoping the
    // scheduler produces it: both senders are past any caller-side precheck
    // before either one reaches the lock, and both name the same bucket.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let contender_barrier = std::sync::Arc::clone(&barrier);
    let contender = std::thread::spawn(move || {
        contender_barrier.wait();
        admit_and_commit()
    });
    barrier.wait();
    let here = admit_and_commit();
    let there = contender.join().expect("contending sender");

    assert_eq!(
        usize::from(here) + usize::from(there),
        1,
        "exactly one sender may send within one spacing interval"
    );
    assert_eq!(last_sent_bucket().as_deref(), Some(BUCKET));
    assert!(load_state().expect("state").pending.is_none());
}

#[test]
#[serial_test::serial]
fn failed_sends_back_off_exponentially() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    // Dropping a lease without commit is a failed network send.
    drop(begin_daily_send().expect("first attempt"));

    let _clock = TestClockGuard::set(BUCKET, T0 + RETRY_BACKOFF_SECS - 1);
    assert!(
        begin_daily_send()
            .err()
            .expect("backoff")
            .contains("not due")
    );
    let second = T0 + RETRY_BACKOFF_SECS;
    let _clock = TestClockGuard::set(BUCKET, second);
    drop(begin_daily_send().expect("second attempt after 60s"));

    let _clock = TestClockGuard::set(BUCKET, second + RETRY_BACKOFF_SECS);
    assert!(
        begin_daily_send()
            .err()
            .expect("doubled")
            .contains("not due")
    );
    let _clock = TestClockGuard::set(BUCKET, second + 2 * RETRY_BACKOFF_SECS);
    drop(begin_daily_send().expect("third attempt after 120s"));
    assert_eq!(attempts_today(), 3);
}

#[test]
#[serial_test::serial]
fn a_clock_that_moved_backwards_does_not_stall_sending() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0 + 3_600);
    begin_daily_send()
        .expect("first send")
        .commit()
        .expect("commit");
    record_sync_result(true).expect("new activity");
    let _clock = TestClockGuard::set(BUCKET, T0);
    begin_daily_send()
        .expect("earlier clock is treated as due")
        .commit()
        .expect("commit");
}

#[test]
#[serial_test::serial]
fn periodic_sends_leave_one_attempt_of_the_daily_cap_for_exit() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let mut now = T0;
    // The spacing stays flat: every send of the day is due half an hour after
    // the previous one, the forty-seventh as much as the second.
    for _ in 0..DAILY_SEND_CAP - 1 {
        let _clock = TestClockGuard::set(BUCKET, now);
        record_sync_result(true).expect("new activity");
        begin_daily_send()
            .expect("periodic send within cap")
            .commit()
            .expect("commit");
        now += RESEND_INTERVAL_SECS;
    }
    let _clock = TestClockGuard::set(BUCKET, now);
    record_sync_result(true).expect("final activity");
    let error = begin_daily_send().err().expect("periodic cap reached");
    assert!(error.contains("limit reached"), "{error}");

    // The exit send still delivers the last activity of the day.
    let lease = begin_send(SendTrigger::Exit).expect("exit keeps the last attempt");
    assert_eq!(
        sync_counts(lease.batch()),
        Some((u64::from(DAILY_SEND_CAP), u64::from(DAILY_SEND_CAP), 0))
    );
    lease.commit().expect("commit exit send");
    const {
        assert!(
            DAILY_SEND_CAP < 60,
            "must stay below the server's daily limit"
        );
    };
    record_sync_result(true).expect("activity after the cap");
    let _clock = TestClockGuard::set(BUCKET, now + MAX_SEND_INTERVAL_SECS);
    let error = begin_send(SendTrigger::Exit).err().expect("hard cap");
    assert!(error.contains("limit reached"), "{error}");
}

#[test]
#[serial_test::serial]
fn an_acknowledged_day_is_resent_every_half_hour_not_later() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let mut now = T0;
    for _ in 0..5 {
        let _clock = TestClockGuard::set(BUCKET, now);
        record_sync_result(true).expect("new activity");
        begin_daily_send().expect("due").commit().expect("commit");
        now += RESEND_INTERVAL_SECS;
    }
    // A minute early is refused without consuming an attempt; on the half hour
    // the sixth send is due, where a doubling backoff would wait two hours.
    let _clock = TestClockGuard::set(BUCKET, now - 60);
    record_sync_result(true).expect("new activity");
    let error = begin_daily_send().err().expect("not due a minute early");
    assert!(error.contains("not due"), "{error}");
    assert_eq!(attempts_today(), 5);
    let _clock = TestClockGuard::set(BUCKET, now);
    begin_daily_send()
        .expect("due on the half hour")
        .commit()
        .expect("commit");
    assert_eq!(attempts_today(), 6);
}

#[test]
#[serial_test::serial]
fn exit_sends_use_a_short_flat_spacing() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    begin_daily_send()
        .expect("first send")
        .commit()
        .expect("commit");
    record_sync_result(true).expect("new activity");

    let _clock = TestClockGuard::set(BUCKET, T0 + EXIT_RESEND_INTERVAL_SECS);
    assert!(
        begin_daily_send()
            .err()
            .expect("periodic waits longer")
            .contains("not due")
    );
    begin_send(SendTrigger::Exit)
        .expect("exit send is due")
        .commit()
        .expect("commit");
}

#[test]
#[serial_test::serial]
fn a_closed_day_is_sent_under_its_own_bucket_after_rollover() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    {
        let _clock = TestClockGuard::set(BUCKET, T0);
        record_sync_result(true).expect("record before midnight");
    }
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    record_sync_result(false).expect("record after midnight");

    let lease = begin_daily_send().expect("send after rollover");
    let batch = lease.batch().clone();
    assert_eq!(batch.events[0].timestamp_bucket, NEXT_BUCKET);
    assert_eq!(
        event_buckets(&batch),
        BTreeSet::from([BUCKET.to_string(), NEXT_BUCKET.to_string()])
    );
    let closed_sync = batch.events.iter().find_map(|envelope| {
        match (&envelope.event, envelope.timestamp_bucket.as_str()) {
            (TelemetryEventV2::SyncAggregate(metrics), BUCKET) => {
                Some((metrics.attempts, metrics.successes, metrics.failures))
            }
            _ => None,
        }
    });
    assert_eq!(closed_sync, Some((1, 1, 0)));
    assert_eq!(sync_counts(&batch), Some((1, 0, 1)));
    batch.validate().expect("valid multi-day batch");

    lease.commit().expect("commit");
    // An acknowledged closed day has nothing left to send and is dropped.
    assert!(day_state(BUCKET).is_none());
    assert!(!today_is_unsent());
}

#[test]
#[serial_test::serial]
fn many_unsent_closed_days_stay_within_the_batch_and_retention_bounds() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    let closed: Vec<String> = (1..=12).map(|day| format!("2026-02-{day:02}")).collect();
    let future = "2026-03-05";
    with_locked_one_shots(|mut state| {
        for day in closed.iter().map(String::as_str).chain([future]) {
            day_totals(&mut state, day).sync.attempts = 1;
            day_totals(&mut state, day).sync.successes = 1;
        }
        Ok((state, ()))
    })
    .expect("seed closed days");
    // Any later write normalizes the sidecar.
    record_sync_result(true).expect("record today");

    // Only the newest closed days are retained.
    let retained: Vec<String> = sidecar()
        .days
        .keys()
        .filter(|day| day.as_str() < NEXT_BUCKET)
        .cloned()
        .collect();
    assert_eq!(retained, closed[closed.len() - RETAINED_CLOSED_DAYS..]);

    let lease = begin_daily_send().expect("send");
    let batch = lease.batch().clone();
    assert!(batch.events.len() <= MAX_BATCH_EVENTS);
    assert_eq!(batch.events[0].timestamp_bucket, NEXT_BUCKET);
    let buckets = event_buckets(&batch);
    assert!(!buckets.contains(future), "future days are never sent");
    assert!(buckets.iter().all(|day| day.as_str() <= NEXT_BUCKET));
    batch.validate().expect("valid batch");
    lease.commit().expect("commit");
    for day in &buckets {
        assert!(day_state(day).is_none_or(|day| !day.unsent()), "{day}");
    }
}

#[test]
#[serial_test::serial]
fn legacy_queue_is_migrated_into_today() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let path = one_shot_path().expect("sidecar path");
    ensure_parent(&path).expect("sidecar dir");
    let mut legacy = OneShotState {
        installation_id: installation_id::get_or_create().expect("identity"),
        ..OneShotState::default()
    };
    legacy.queued.sync = SyncMetrics {
        attempts: 2,
        successes: 1,
        failures: 1,
    };
    let bytes = serde_json::to_vec(&legacy).expect("encode legacy sidecar");
    crate::core::atomic_fs::try_atomic_write(&path, &bytes, None).expect("write legacy sidecar");

    assert_eq!(
        sync_counts(&preview_daily_batch().expect("preview")),
        Some((2, 1, 1))
    );
    record_sync_result(true).expect("record after migration");
    let migrated = sidecar();
    assert!(migrated.queued.is_empty());
    assert_eq!(migrated.days[BUCKET].totals.sync.attempts, 3);
}

#[test]
#[serial_test::serial]
fn a_corrupt_config_refuses_collection_and_sending() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let config = crate::core::config::Config::path().expect("config path");
    std::fs::create_dir_all(config.parent().expect("config dir")).expect("config dir");
    std::fs::write(&config, "telemetry = [not toml").expect("write corrupt config");
    assert!(!telemetry_collection_eligible());
    record_sync_result(true).expect("refused recording is a no-op");
    assert!(!one_shot_path().expect("sidecar path").exists());
    assert_eq!(
        crate::core::telemetry_send::send_telemetry(SendTrigger::Exit),
        None,
        "a corrupt config must never become default-on"
    );
    assert!(!state_path().expect("state path").exists());
}

#[test]
#[serial_test::serial]
fn send_aborts_on_a_bounded_wait_for_the_aggregate_lock() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let path = state_path().expect("state path");
    ensure_parent(&path).expect("state dir");

    // A contending operation must not wait for the entire network request.
    let blocker = open_state_lock(&path).expect("open state lock");
    blocker.lock_exclusive().expect("hold aggregate lock");

    // Monotonic elapsed time, not wall-clock date: unaffected by any rollover.
    let started = std::time::Instant::now();
    let error = begin_daily_send().err().expect("must not wait forever");
    let waited = started.elapsed();
    assert!(error.contains("aggregate"), "{error}");
    assert!(error.contains("timed out"), "{error}");
    assert!(
        waited >= SEND_LOCK_TIMEOUT,
        "returned before the bound: {waited:?}"
    );
    // Generous headroom: a loaded machine must not fail this, while an
    // unbounded wait never finishes at all.
    assert!(
        waited < SEND_LOCK_TIMEOUT * 10,
        "wait was not bounded: {waited:?}"
    );

    drop(blocker);
    begin_daily_send().expect("lock is usable once the holder exits");
}

#[test]
#[serial_test::serial]
fn send_aborts_on_a_bounded_wait_for_the_nested_one_shot_lock() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let sidecar = one_shot_path().expect("sidecar path");
    ensure_parent(&sidecar).expect("sidecar dir");

    // The sidecar lock is taken while the aggregate lock is already held, so an
    // unbounded wait here pins both locks rather than one.
    let blocker = open_sidecar_lock(&sidecar).expect("open sidecar lock");
    blocker.lock_exclusive().expect("hold one-shot lock");

    let started = std::time::Instant::now();
    let error = begin_daily_send()
        .err()
        .expect("nested wait is bounded too");
    let waited = started.elapsed();
    assert!(error.contains("one-shot"), "{error}");
    assert!(error.contains("timed out"), "{error}");
    assert!(
        waited < SEND_LOCK_TIMEOUT * 10,
        "wait was not bounded: {waited:?}"
    );

    // The aggregate lock was released along with the failed attempt, so the
    // next sender is not left locked out by the abort itself.
    drop(blocker);
    begin_daily_send().expect("both locks free again");
}

#[test]
#[serial_test::serial]
fn contended_purge_and_rotation_leave_state_and_callbacks_untouched() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let lease = begin_daily_send().expect("lease");
    let path = state_path().expect("path");
    let original = std::fs::read(&path).expect("state");
    let called = std::cell::Cell::new(false);
    let operation = || {
        called.set(true);
        Ok(())
    };
    let started = std::time::Instant::now();
    assert!(
        purge_local_state_then(operation)
            .unwrap_err()
            .contains("timed out")
    );
    assert!(
        rotate_identity_state_then(operation)
            .unwrap_err()
            .contains("timed out")
    );
    assert!(started.elapsed() < SEND_LOCK_TIMEOUT * 10);
    assert!(!called.get());
    assert_eq!(std::fs::read(path).expect("preserved state"), original);
    drop(lease);
    purge_local_state_then(operation).expect("purge after release");
    assert!(called.get());
}

#[test]
#[serial_test::serial]
fn contended_record_and_ack_preserve_counters_and_exact_pending_retry() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_sync_result(true).expect("initial event");
    let lease = begin_daily_send().expect("lease");
    let pending = lease.batch().clone();
    let path = one_shot_path().expect("path");
    let original = std::fs::read(&path).expect("sidecar");
    let blocker = open_sidecar_lock(&path).expect("lock");
    blocker.lock_exclusive().expect("hold sidecar");
    let started = std::time::Instant::now();
    assert!(record_sync_result(false).unwrap_err().contains("timed out"));
    assert!(lease.commit().unwrap_err().contains("timed out"));
    assert!(started.elapsed() < SEND_LOCK_TIMEOUT * 10);
    assert_eq!(std::fs::read(path).expect("preserved sidecar"), original);
    drop(blocker);
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    let retry = begin_daily_send().expect("retry");
    assert_eq!(retry.batch(), &pending);
    retry.commit().expect("ack after release");
    assert_eq!(last_sent_bucket().as_deref(), Some(BUCKET));
    assert_eq!(sync_counts(&preview_daily_batch().expect("preview")), None);
}

#[test]
#[serial_test::serial]
fn a_busy_ledger_does_not_erase_the_pending_version_upgrade() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let record = crate::core::telemetry_ledger::HeartbeatRecord {
        timestamp: "2026-03-01T00:00:00Z".into(),
        installation_id: String::new(),
        version: "3.10.1".into(),
        os: String::new(),
        arch: String::new(),
        schema_version: 2,
        event_names: vec![],
        payload_hash: "a".repeat(64),
        endpoint: String::new(),
        status: "success".into(),
    };
    crate::core::telemetry_ledger::append(&record).unwrap();
    let ledger = crate::core::paths::state_dir()
        .unwrap()
        .join("telemetry_heartbeats.jsonl");
    let blocker = open_state_lock(&ledger).unwrap();
    blocker.lock_exclusive().unwrap();
    assert!(
        record_current_version_value("4.0.0")
            .unwrap_err()
            .contains("timed out")
    );
    assert!(
        !one_shot_path().unwrap().exists(),
        "failed observation must not persist"
    );
    drop(blocker);
    record_current_version_value("4.0.0").unwrap();
    assert_eq!(
        version_transition(&preview_daily_batch().unwrap()),
        Some((3, 4))
    );
}

#[test]
#[serial_test::serial]
fn one_shots_are_deduplicated_and_ack_only_included_watermarks() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_current_version_value("3.9.20").expect("seed major");
    assert_eq!(
        version_transition(&preview_daily_batch().expect("preview")),
        None
    );

    record_setup_completion(vec!["Claude Code".into(), "Claude Code".into()])
        .expect("record setup");
    let first = prepare_daily_batch().expect("prepare first");
    assert_eq!(occurrence_count(&first, "setup_completed"), Some(1));
    assert_eq!(occurrence_count(&first, "integration_detected"), Some(1));

    record_setup_completion(vec!["Claude Code".into(), "Codex".into()])
        .expect("record later integration");
    record_current_version_value("4.0.0").expect("record upgrade");
    assert_eq!(prepare_daily_batch().expect("retry exact"), first);
    acknowledge_daily_batch(&first).expect("ack first");

    // Day totals are cumulative: each same-day send restates the whole day.
    let second = prepare_daily_batch().expect("prepare second");
    assert_eq!(occurrence_count(&second, "setup_completed"), Some(1));
    assert_eq!(occurrence_count(&second, "integration_detected"), Some(2));
    assert_eq!(version_transition(&second), Some((3, 4)));
    record_current_version_value("5.0.0").expect("record next upgrade while pending");
    acknowledge_daily_batch(&second).expect("ack second");

    // Several upgrades in one day collapse into one transition.
    let third = prepare_daily_batch().expect("prepare residual upgrade");
    assert_eq!(version_transition(&third), Some((3, 5)));
    acknowledge_daily_batch(&third).expect("ack residual upgrade");

    record_current_version_value("2.0.0").expect("ignore downgrade");
    let final_preview = preview_daily_batch().expect("final preview");
    assert_eq!(occurrence_count(&final_preview, "setup_completed"), Some(1));
    assert_eq!(
        occurrence_count(&final_preview, "integration_detected"),
        Some(2)
    );
    assert_eq!(version_transition(&final_preview), Some((3, 5)));
    assert!(!today_is_unsent());
}

#[test]
#[serial_test::serial]
fn setup_recording_does_not_wait_for_network_send_lease() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let lease = begin_daily_send().expect("begin send");
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(record_setup_completion(vec!["codex".into()]))
            .expect("report setup recording");
    });
    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("setup recording must not wait for network lease")
        .expect("setup recording succeeded");
    drop(lease);
    worker.join().expect("setup worker");
}

#[test]
#[serial_test::serial]
fn setup_recording_respects_environment_opt_out() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _telemetry = TelemetryEnvGuard::disable();
    record_setup_completion(vec!["codex".into()]).expect("opted-out recording is a no-op");
    record_sync_result(true).expect("opted-out sync recording is a no-op");
    record_autopilot_decisions(1, 1).expect("opted-out decision recording is a no-op");
    record_autopilot_fallback().expect("opted-out fallback recording is a no-op");
    record_checkout_started().expect("opted-out checkout recording is a no-op");
    record_error_category(ErrorCategory::Internal).expect("opted-out error recording is a no-op");
    assert!(!one_shot_path().expect("one-shot path").exists());
    let preview = preview_daily_batch().expect("preview");
    assert_eq!(occurrence_count(&preview, "setup_completed"), None);
    assert_eq!(occurrence_count(&preview, "integration_detected"), None);
    assert_eq!(sync_counts(&preview), None);
    assert_eq!(autopilot_counts(&preview), None);
    assert_eq!(autopilot_fallback_counts(&preview), None);
    assert_eq!(occurrence_count(&preview, "checkout_started"), None);
    assert_eq!(error_count(&preview, ErrorCategory::Internal), None);
}

#[test]
#[serial_test::serial]
fn error_categories_are_typed_durable_and_ack_only_the_pending_snapshot() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    for category in ERROR_CATEGORIES {
        record_error_category(category).expect("record error category");
    }
    let first = prepare_daily_batch().expect("prepare first");
    for category in ERROR_CATEGORIES {
        assert_eq!(error_count(&first, category), Some(1));
    }

    record_error_category(ErrorCategory::Timeout).expect("record concurrent timeout");
    assert_eq!(prepare_daily_batch().expect("retry"), first);
    acknowledge_daily_batch(&first).expect("ack first");

    let residual = preview_daily_batch().expect("residual preview");
    assert_eq!(error_count(&residual, ErrorCategory::Timeout), Some(2));
    for category in ERROR_CATEGORIES {
        if category != ErrorCategory::Timeout {
            assert_eq!(error_count(&residual, category), Some(1));
        }
    }
    assert!(today_is_unsent());
    let json = serde_json::to_string(&residual).expect("serialize telemetry");
    assert!(!json.contains("error message"));
    assert!(!json.contains("stack"));
}

#[test]
#[serial_test::serial]
fn error_category_counter_saturates_at_schema_bound() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    with_locked_one_shots(|mut state| {
        day_totals(&mut state, BUCKET).error_categories[7] = MAX_COUNT;
        Ok((state, ()))
    })
    .expect("seed saturated counter");
    record_error_category(ErrorCategory::Internal).expect("saturated recorder is a no-op");
    assert_eq!(
        error_count(
            &preview_daily_batch().expect("preview"),
            ErrorCategory::Internal
        ),
        Some(MAX_COUNT)
    );
}

#[test]
#[serial_test::serial]
fn autopilot_results_are_durable_and_ack_only_the_pending_snapshot() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_autopilot_decisions(2, 1).expect("record decisions");
    let first = prepare_daily_batch().expect("prepare first");
    assert_eq!(autopilot_counts(&first), Some((2, 1, 0)));

    record_autopilot_decisions(1, 2).expect("record concurrent decisions");
    record_autopilot_fallback().expect("record concurrent fallback");
    assert_eq!(prepare_daily_batch().expect("retry"), first);
    acknowledge_daily_batch(&first).expect("ack first");

    let residual = preview_daily_batch().expect("residual preview");
    assert_eq!(autopilot_counts(&residual), Some((3, 3, 1)));
    assert_eq!(autopilot_fallback_counts(&residual), Some((0, 0, 1)));
}

#[test]
#[serial_test::serial]
fn checkout_starts_are_durable_and_ack_only_the_pending_snapshot() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_checkout_started().expect("record checkout");
    let first = prepare_daily_batch().expect("prepare first");
    assert_eq!(occurrence_count(&first, "checkout_started"), Some(1));

    record_checkout_started().expect("record concurrent checkout");
    assert_eq!(prepare_daily_batch().expect("retry"), first);
    acknowledge_daily_batch(&first).expect("ack first");
    assert_eq!(
        occurrence_count(
            &preview_daily_batch().expect("residual preview"),
            "checkout_started"
        ),
        Some(2)
    );
}

#[test]
#[serial_test::serial]
fn saturated_autopilot_counters_remain_bounded() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    with_locked_one_shots(|mut state| {
        let today = day_totals(&mut state, BUCKET);
        today.autopilot = DecisionMetrics {
            admitted: MAX_COUNT,
            denied: MAX_COUNT,
            fallback: MAX_COUNT,
        };
        today.autopilot_fallback.fallback = MAX_COUNT;
        today.checkout_started = MAX_COUNT;
        Ok((state, ()))
    })
    .expect("seed saturated autopilot counts");

    record_autopilot_decisions(1, 1).expect("saturated decision recorder is a no-op");
    record_autopilot_fallback().expect("saturated fallback recorder is a no-op");
    record_checkout_started().expect("saturated checkout recorder is a no-op");
    let preview = preview_daily_batch().expect("preview");
    assert_eq!(
        autopilot_counts(&preview),
        Some((MAX_COUNT, MAX_COUNT, MAX_COUNT))
    );
    assert_eq!(autopilot_fallback_counts(&preview), Some((0, 0, MAX_COUNT)));
    assert_eq!(
        occurrence_count(&preview, "checkout_started"),
        Some(MAX_COUNT)
    );
}

#[test]
#[serial_test::serial]
fn sync_results_are_durable_and_ack_only_the_pending_snapshot() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_sync_result(true).expect("record success");
    record_sync_result(false).expect("record failure");
    let first = prepare_daily_batch().expect("prepare first");
    assert_eq!(sync_counts(&first), Some((2, 1, 1)));

    record_sync_result(true).expect("record concurrent success");
    assert_eq!(prepare_daily_batch().expect("retry"), first);
    acknowledge_daily_batch(&first).expect("ack first");
    assert_eq!(
        sync_counts(&preview_daily_batch().expect("residual preview")),
        Some((3, 2, 1))
    );
    assert!(today_is_unsent());
}

#[test]
#[serial_test::serial]
fn retry_after_sidecar_ack_crash_does_not_double_subtract_one_shots() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_sync_result(true).expect("record included sync result");
    record_autopilot_decisions(1, 0).expect("record included decision");
    let pending = prepare_daily_batch().expect("prepare pending batch");
    let aggregate_path = state_path().expect("aggregate path");
    let state = load_state_at(&aggregate_path).expect("load pending state");
    let pending_state = state.pending.expect("pending batch");
    let acknowledgement_id =
        pending_acknowledgement_id(&pending_state).expect("compute acknowledgement id");
    let included = pending_state.included_days;

    // Simulate a crash after the sidecar acknowledgement was persisted but
    // before the aggregate pending marker was cleared.
    acknowledge_one_shots_at(
        &one_shot_path().expect("one-shot path"),
        &included,
        &acknowledgement_id,
    )
    .expect("persist sidecar acknowledgement");
    record_sync_result(false).expect("record result after interrupted commit");
    record_autopilot_decisions(0, 1).expect("record decision after interrupted commit");

    acknowledge_daily_batch(&pending).expect("retry acknowledgement");
    let residual = preview_daily_batch().expect("residual preview");
    assert_eq!(sync_counts(&residual), Some((2, 1, 1)));
    assert_eq!(autopilot_counts(&residual), Some((1, 1, 0)));
    assert!(today_is_unsent());
}

#[test]
#[serial_test::serial]
fn distinct_pending_instances_with_identical_metrics_are_each_acknowledged() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_sync_result(true).expect("record first result");
    let first = prepare_daily_batch().expect("prepare first batch");
    let first_state = load_state().expect("load first state");
    let first_pending = first_state.pending.expect("first pending");
    let first_id = pending_acknowledgement_id(&first_pending).expect("first id");
    acknowledge_daily_batch(&first).expect("ack first batch");

    record_sync_result(true).expect("record identical second result");
    let second = prepare_daily_batch().expect("prepare second batch");
    let second_state = load_state().expect("load second state");
    let second_pending = second_state.pending.expect("second pending");
    let second_id = pending_acknowledgement_id(&second_pending).expect("second id");
    assert_ne!(first_id, second_id);
    acknowledge_daily_batch(&second).expect("ack second batch");
    assert_eq!(
        sync_counts(&preview_daily_batch().expect("final preview")),
        Some((2, 2, 0))
    );
    assert!(!today_is_unsent());
}

#[test]
#[serial_test::serial]
fn saturated_sync_counter_remains_cross_field_consistent() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    with_locked_one_shots(|mut state| {
        day_totals(&mut state, BUCKET).sync = SyncMetrics {
            attempts: MAX_COUNT,
            successes: MAX_COUNT,
            failures: 0,
        };
        Ok((state, ()))
    })
    .expect("seed saturated sync counts");

    record_sync_result(false).expect("saturated recorder is a no-op");
    assert_eq!(
        sync_counts(&preview_daily_batch().expect("preview")),
        Some((MAX_COUNT, MAX_COUNT, 0))
    );
}

#[test]
#[serial_test::serial]
fn identity_rotation_requeues_installation_scoped_facts() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_setup_completion(vec!["codex".into(), "claude".into()]).expect("record setup");
    let first = prepare_daily_batch().expect("prepare first");
    acknowledge_daily_batch(&first).expect("ack first");
    record_sync_result(true).expect("record old-identity sync");
    record_autopilot_decisions(1, 1).expect("record old-identity decisions");
    record_autopilot_fallback().expect("record old-identity fallback");
    record_checkout_started().expect("record old-identity checkout");
    record_error_category(ErrorCategory::Internal).expect("record old-identity error");

    rotate_identity_state_then(|| Ok(())).expect("rotate state");
    let replay = preview_daily_batch().expect("preview replay");
    assert_eq!(occurrence_count(&replay, "setup_completed"), Some(1));
    assert_eq!(occurrence_count(&replay, "integration_detected"), Some(2));
    assert_eq!(sync_counts(&replay), None);
    assert_eq!(autopilot_counts(&replay), None);
    assert_eq!(autopilot_fallback_counts(&replay), None);
    assert_eq!(occurrence_count(&replay, "checkout_started"), None);
    assert_eq!(error_count(&replay, ErrorCategory::Internal), None);
}

#[test]
#[serial_test::serial]
fn failed_identity_rotation_does_not_requeue_old_identity_facts() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_setup_completion(vec!["codex".into()]).expect("record setup");
    let first = prepare_daily_batch().expect("prepare first");
    acknowledge_daily_batch(&first).expect("ack first");

    assert!(rotate_identity_state_then::<()>(|| Err("reset failed".into())).is_err());
    let unchanged = preview_daily_batch().expect("preview unchanged state");
    assert_eq!(occurrence_count(&unchanged, "setup_completed"), Some(1));
    assert_eq!(
        occurrence_count(&unchanged, "integration_detected"),
        Some(1)
    );
    assert!(!today_is_unsent());
}

#[test]
#[serial_test::serial]
fn legacy_pending_batch_is_discarded_after_identity_change() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let stale = prepare_daily_batch().expect("prepare old identity batch");
    let stale_id = batch_installation_id(&stale).to_string();
    let path = state_path().expect("state path");
    let legacy_state = load_state_at(&path).expect("load state");
    let mut legacy_json = serde_json::to_value(legacy_state).expect("serialize legacy state");
    legacy_json
        .as_object_mut()
        .expect("aggregate state object")
        .remove("installation_id");
    let bytes = serde_json::to_vec(&legacy_json).expect("encode legacy JSON");
    crate::core::atomic_fs::try_atomic_write(&path, &bytes, None)
        .expect("write legacy state without identity field");

    let current_id = installation_id::reset().expect("rotate identity directly");
    assert_ne!(current_id, stale_id);
    let current = preview_daily_batch().expect("preview current identity");
    assert_ne!(current, stale);
    assert!(
        current
            .events
            .iter()
            .all(|event| event.installation_id == current_id)
    );
}

#[test]
#[serial_test::serial]
fn stale_sidecar_discards_sync_but_requeues_setup_after_identity_change() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    record_setup_completion(vec!["codex".into()]).expect("record setup");
    let first = prepare_daily_batch().expect("prepare first");
    acknowledge_daily_batch(&first).expect("ack setup");
    record_sync_result(true).expect("record old-identity sync");
    record_autopilot_decisions(1, 1).expect("record old-identity decisions");
    record_autopilot_fallback().expect("record old-identity fallback");
    record_checkout_started().expect("record old-identity checkout");
    record_error_category(ErrorCategory::Internal).expect("record old-identity error");

    installation_id::reset().expect("simulate successful reset before sidecar cleanup");
    let current = preview_daily_batch().expect("preview rebound sidecar");
    assert_eq!(occurrence_count(&current, "setup_completed"), Some(1));
    assert_eq!(occurrence_count(&current, "integration_detected"), Some(1));
    assert_eq!(sync_counts(&current), None);
    assert_eq!(autopilot_counts(&current), None);
    assert_eq!(autopilot_fallback_counts(&current), None);
    assert_eq!(occurrence_count(&current, "checkout_started"), None);
    assert_eq!(error_count(&current, ErrorCategory::Internal), None);
}

mod tool_counts;
