// SPDX-License-Identifier: Apache-2.0
//! Token-plan step 2, locked acceptance: the lifetime of an armed directive.
//!
//! Pinned behaviour, not constants: an armed directive stops applying once
//! its TTL has passed, and re-arming within the minimum interval neither
//! stacks summaries nor discards the active one, while re-arming after it
//! replaces the summary. Time moves with the directive clock seam
//! `lean_ctx::core::compact_directive::TestClock` (same shape as
//! `core::archive::presented::TestClock`):
//!
//! - `TestClock::past_min_interval()` moves the directive clock one second
//!   past the minimum re-arm interval of every directive armed so far and
//!   stays inside their TTL;
//! - `TestClock::past_ttl()` moves it one second past the TTL of every
//!   directive armed so far.
//!
//! Both offsets are process-wide and reset when the guard drops; hold
//! `isolated_data_dir()` for as long as a guard lives. 736c264170 has no such
//! seam, so this file does not compile there (counted as failing).

use axum::http::StatusCode;
use lean_ctx::core::compact_directive::TestClock;

use super::compact_session_tests::{
    arm, carried, carries, conversation, fresh_session, isolated_env, spawn_upstream, turn,
    untouched,
};

/// TTL-1 [fails on 736c264170: no clock seam]. An armed directive applies
/// until its TTL has passed and never after: the owner's next request then
/// arrives with its full history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn ttl_expired_directive_no_longer_reaches_its_session() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let a = fresh_session("ttl");
    carried(&upstream, &a).await;
    let summary = "SUMMARY-TTL-EXPIRES";
    arm(Some(&a), summary).await;
    let body = conversation(&a);

    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    assert!(
        carries(&upstream.last(), summary),
        "control: armed and applied"
    );

    let _later = TestClock::past_ttl();
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    let expired = upstream.last();
    assert!(
        !carries(&expired, summary) && untouched(&expired),
        "an expired directive must not apply: {}",
        expired.body
    );
}

/// TTL-2 [fails on 736c264170: no clock seam]. Within the minimum interval a
/// second compact is refused or coalesced — the session stays compacted
/// with exactly one summary — and after the interval a new compact replaces
/// the old summary entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn min_interval_rearm_keeps_one_summary_then_replaces_after_it() {
    let _iso = isolated_env();
    let upstream = spawn_upstream().await;
    let a = fresh_session("rearm");
    carried(&upstream, &a).await;
    let (first, quick, later) = (
        "SUMMARY-REARM-FIRST",
        "SUMMARY-REARM-TOO-SOON",
        "SUMMARY-REARM-AFTER-INTERVAL",
    );
    arm(Some(&a), first).await;
    let body = conversation(&a);
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    assert!(
        carries(&upstream.last(), first),
        "control: armed and applied"
    );

    arm(Some(&a), quick).await;
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    let soon = upstream.last();
    assert!(
        carries(&soon, first) || carries(&soon, quick),
        "still compacted: {}",
        soon.body
    );
    assert!(
        soon.body.matches(first).count() <= 1 && soon.body.matches(quick).count() <= 1,
        "never stacked: {}",
        soon.body
    );

    let _clock = TestClock::past_min_interval();
    // The proxy keeps carrying the session.
    carried(&upstream, &a).await;
    arm(Some(&a), later).await;
    assert_eq!(turn(&upstream, Some(&a), &body).await, StatusCode::OK);
    let replaced = upstream.last();
    assert!(carries(&replaced, later), "re-armed: {}", replaced.body);
    assert!(
        !carries(&replaced, first) && !carries(&replaced, quick),
        "the old summary is gone: {}",
        replaced.body
    );
}
