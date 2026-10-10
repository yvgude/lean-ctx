// SPDX-License-Identifier: Apache-2.0

//! Privacy-safe daily telemetry aggregation.
//!
//! Every UTC day keeps cumulative totals. A send carries the full totals of
//! today (and of any closed day with unsent activity) under that day's bucket.
//! The server replaces per-day rows, so sending several times a day is
//! idempotent: tool usage reaches the server within the day, and a one-day
//! user is counted without having to come back.

use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::installation_id;
use super::telemetry_v2::{
    Architecture, ClientFamily, DecisionMetrics, DistributionChannel, ErrorCategory, ErrorMetrics,
    HeartbeatMetrics, Histogram, MAX_BATCH_EVENTS, MAX_COUNT, MAX_TOOL_ENTRIES, OccurrenceMetrics,
    OperatingSystem, SCHEMA_VERSION, SessionMetrics, SyncMetrics, TelemetryBatchV2,
    TelemetryEnvelopeV2, TelemetryEventV2, TokenMetrics, ToolCallCount, ToolCallMetrics,
    ToolUsageMetrics, VersionUpgradeMetrics, valid_feature_code, valid_tool_name,
};

mod counters;
mod environment;
mod features;
mod history;
use super::telemetry_failure::{KindCounts, sub_kinds, wire_kinds, wire_messages};
use counters::{add_counters, counter_delta};
use environment::{client_family, distribution_channel, setup_profile};
use features::{FeatureTally, add_feature, feature_metrics};

/// Most send attempts per installation and UTC day. The server admits ten;
/// two stay in reserve for clock skew between client and server.
pub const DAILY_SEND_CAP: u32 = 8;
/// Backoff base while today's first send has not been acknowledged yet.
const RETRY_BACKOFF_SECS: i64 = 60;
/// Spacing after the first acknowledged send of a day; doubles per attempt.
const RESEND_INTERVAL_SECS: i64 = 15 * 60;
const MAX_SEND_INTERVAL_SECS: i64 = 2 * 60 * 60;
/// Flat spacing for the exit flush, so a session's final calls still arrive.
const EXIT_RESEND_INTERVAL_SECS: i64 = 5 * 60;
/// Closed days with unsent activity kept locally besides today.
const RETAINED_CLOSED_DAYS: usize = 7;

/// What prompted a send attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendTrigger {
    /// Background cadence while tools are being called.
    Periodic,
    /// Final flush as the MCP server exits; may use the last daily slot.
    Exit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterCheckpoint {
    tool_calls: u64,
    tool_failures: u64,
    tool_latency_buckets: [u64; crate::core::telemetry::TOOL_LATENCY_BUCKET_UPPER_MS.len()],
    session_uptime_secs: u64,
    /// Per-tool counters. Absent in state written before per-tool counting.
    #[serde(default)]
    tools: BTreeMap<String, ToolCounterCheckpoint>,
    /// Tool-output tokens. Absent in state written before 3.11.1.
    #[serde(default)]
    tokens_input: u64,
    #[serde(default)]
    tokens_output: u64,
    /// Scrubbed failure templates, keyed `tool\ttemplate` (3.11.1).
    #[serde(default)]
    failure_messages: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCounterCheckpoint {
    calls: u64,
    failures: u64,
    #[serde(default)]
    latency_us: u64,
    #[serde(default)]
    failure_kinds: KindCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateState {
    #[serde(default)]
    installation_id: String,
    /// Legacy in-process baseline, kept so older state files still load.
    /// Unsent counters now live durably in [`QueuedOneShots::counters`].
    process_nonce: String,
    acknowledged: CounterCheckpoint,
    pending: Option<PendingBatch>,
    /// Bucket of the most recently acknowledged batch.
    #[serde(default)]
    last_sent_bucket: Option<String>,
    /// Unix time of the most recent admitted send attempt.
    #[serde(default)]
    last_attempt_unix: Option<i64>,
    /// UTC day that `attempts_in_bucket` counts for.
    #[serde(default)]
    attempt_bucket: Option<String>,
    #[serde(default)]
    attempts_in_bucket: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OneShotState {
    #[serde(default)]
    installation_id: String,
    setup_recorded: bool,
    configured_integrations: BTreeSet<String>,
    observed_major: Option<u16>,
    #[serde(default)]
    last_acknowledged_batch: Option<String>,
    /// Pre-daily-totals queue. Loaded for compatibility and migrated into
    /// today's totals; never written with content again.
    #[serde(default, skip_serializing_if = "QueuedOneShots::is_empty")]
    queued: QueuedOneShots,
    /// Cumulative totals per UTC day, keyed `YYYY-MM-DD`.
    #[serde(default)]
    days: BTreeMap<String, DayTotals>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DayTotals {
    totals: QueuedOneShots,
    /// The totals the server last acknowledged for this day.
    #[serde(default)]
    sent: QueuedOneShots,
}

impl DayTotals {
    fn unsent(&self) -> bool {
        self.totals != self.sent
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuedOneShots {
    setup_completed: bool,
    integrations_detected: u64,
    version_upgrade: Option<VersionTransition>,
    #[serde(default)]
    sync: SyncMetrics,
    #[serde(default)]
    autopilot: DecisionMetrics,
    #[serde(default)]
    autopilot_fallback: DecisionMetrics,
    #[serde(default)]
    checkout_started: u64,
    #[serde(default)]
    error_categories: [u64; 8],
    /// Shell commands that exited non-zero (3.11.1); kept apart from the
    /// fixed-size category array so older state still loads.
    #[serde(default)]
    command_errors: u64,
    /// Tool counters folded in by every process, so short sessions reach the
    /// server even when the process exits before the next send.
    #[serde(default)]
    counters: CounterCheckpoint,
    /// CLI commands and background features by registry code (3.11.1).
    #[serde(default)]
    features: BTreeMap<String, FeatureTally>,
}

impl QueuedOneShots {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Merge `extra` into these totals, saturating at the contract bounds.
    fn absorb(&mut self, extra: &Self) {
        self.setup_completed |= extra.setup_completed;
        self.integrations_detected = self
            .integrations_detected
            .saturating_add(extra.integrations_detected)
            .min(MAX_COUNT);
        self.version_upgrade = match (self.version_upgrade, extra.version_upgrade) {
            (Some(own), Some(other)) => Some(VersionTransition {
                from_major: own.from_major.min(other.from_major),
                to_major: own.to_major.max(other.to_major),
            }),
            (own, other) => own.or(other),
        };
        self.sync.attempts = self
            .sync
            .attempts
            .saturating_add(extra.sync.attempts)
            .min(MAX_COUNT);
        self.sync.successes = self
            .sync
            .successes
            .saturating_add(extra.sync.successes)
            .min(self.sync.attempts);
        self.sync.failures = self
            .sync
            .failures
            .saturating_add(extra.sync.failures)
            .min(self.sync.attempts - self.sync.successes);
        add_decisions(&mut self.autopilot, &extra.autopilot);
        add_decisions(&mut self.autopilot_fallback, &extra.autopilot_fallback);
        self.checkout_started = self
            .checkout_started
            .saturating_add(extra.checkout_started)
            .min(MAX_COUNT);
        for (own, other) in self.error_categories.iter_mut().zip(extra.error_categories) {
            *own = own.saturating_add(other).min(MAX_COUNT);
        }
        self.command_errors = self
            .command_errors
            .saturating_add(extra.command_errors)
            .min(MAX_COUNT);
        add_counters(&mut self.counters, &extra.counters);
        for (code, tally) in &extra.features {
            add_feature(&mut self.features, code, *tally);
        }
    }
}

fn add_decisions(total: &mut DecisionMetrics, extra: &DecisionMetrics) {
    total.admitted = total.admitted.saturating_add(extra.admitted).min(MAX_COUNT);
    total.denied = total.denied.saturating_add(extra.denied).min(MAX_COUNT);
    total.fallback = total.fallback.saturating_add(extra.fallback).min(MAX_COUNT);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTransition {
    from_major: u16,
    to_major: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingBatch {
    batch: TelemetryBatchV2,
    #[serde(default)]
    acknowledgement_id: String,
    observed: CounterCheckpoint,
    process_nonce: String,
    /// Legacy subtract-on-acknowledge snapshot; new batches use `included_days`.
    #[serde(default, skip_serializing_if = "QueuedOneShots::is_empty")]
    included_one_shots: QueuedOneShots,
    /// Exact per-day totals this batch carries, recorded as sent on success.
    #[serde(default)]
    included_days: BTreeMap<String, QueuedOneShots>,
}

pub struct DailySendLease {
    batch: TelemetryBatchV2,
    state_path: PathBuf,
    one_shot_path: PathBuf,
    _lock: std::fs::File,
}

impl DailySendLease {
    pub fn batch(&self) -> &TelemetryBatchV2 {
        &self.batch
    }

    pub fn commit(self) -> Result<(), String> {
        acknowledge_at(&self.state_path, &self.one_shot_path, &self.batch)
    }
}

pub fn pending_daily_batch() -> Result<TelemetryBatchV2, String> {
    preview_daily_batch()
}

/// Build the exact currently eligible payload without advancing durable state.
pub fn preview_daily_batch() -> Result<TelemetryBatchV2, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.try_lock_exclusive().map_err(|error| {
        if super::file_lock::is_contended(&error) {
            "exact telemetry preview unavailable while a send is in progress".to_string()
        } else {
            format!("cannot lock telemetry aggregate state for preview: {error}")
        }
    })?;
    let state = state_for_current_identity(load_state_at(&path)?)?;
    if let Some(pending) = state.pending {
        return Ok(pending.batch);
    }
    let sidecar_path = one_shot_path()?;
    ensure_parent(&sidecar_path)?;
    let sidecar_lock = open_sidecar_lock(&sidecar_path)?;
    // Tool calls hold this lock briefly to persist counters; wait out that
    // window instead of failing a concurrent preview.
    lock_telemetry_file(&sidecar_lock, "one-shot").map_err(|error| {
        format!("exact telemetry preview unavailable: cannot lock one-shot state: {error}")
    })?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&sidecar_path)?)?;
    // Fold in memory only: the preview must not advance durable state.
    let today = current_send_bucket();
    fold_process_counters(&sidecar_path, &mut one_shots, &today);
    build_batch(&one_shots, &today).map(|(batch, _)| batch)
}

/// Freeze one payload until the sender explicitly acknowledges success,
/// bypassing admission so tests can drive the two phases directly.
#[cfg(test)]
fn prepare_daily_batch() -> Result<TelemetryBatchV2, String> {
    with_locked_state(|mut state| {
        if let Some(pending) = &state.pending {
            return Ok((state.clone(), pending.batch.clone()));
        }
        let (batch, _) = freeze_batch(&mut state, &current_send_bucket())?;
        Ok((state, batch))
    })
}

/// Hold the cross-process state lease until network, ledger, and
/// acknowledgement finish. Refuses while the next send is not yet due.
pub fn begin_daily_send() -> Result<DailySendLease, String> {
    begin_send(SendTrigger::Periodic)
}

/// Admission and payload resolve one clock reading under the aggregate lock,
/// so competing senders cannot both pass a stale caller-side precheck.
pub fn begin_send(trigger: SendTrigger) -> Result<DailySendLease, String> {
    let path = state_path()?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let mut state = state_for_current_identity(load_state()?)?;
    let (bucket, now) = send_clock();
    if state.attempt_bucket.as_deref() != Some(bucket.as_str()) {
        state.attempt_bucket = Some(bucket.clone());
        state.attempts_in_bucket = 0;
    }
    let sent_today = state.last_sent_bucket.as_deref() == Some(bucket.as_str());
    admit(&state, now, trigger, sent_today)?;
    let batch = if let Some(pending) = &state.pending {
        // Retries remain at-least-once with the exact frozen bytes, including
        // across UTC day boundaries.
        pending.batch.clone()
    } else {
        let (batch, unsent) = freeze_batch(&mut state, &bucket)?;
        if sent_today && !unsent {
            // Nothing new since the last acknowledged send. The fold above
            // already persisted this process's counters, which is harmless.
            return Err(format!("telemetry for {bucket} is already up to date"));
        }
        batch
    };
    state.last_attempt_unix = Some(now);
    state.attempts_in_bucket = state.attempts_in_bucket.saturating_add(1);
    write_state(&path, &state)?;
    Ok(DailySendLease {
        batch,
        state_path: path,
        one_shot_path,
        _lock: lock,
    })
}

/// Whether the next attempt is due. Attempts in a day are capped below the
/// server's own limit; their spacing grows so a long session sends a handful
/// of cumulative snapshots and an offline machine backs off.
fn admit(
    state: &AggregateState,
    now: i64,
    trigger: SendTrigger,
    sent_today: bool,
) -> Result<(), String> {
    let cap = match trigger {
        SendTrigger::Periodic => DAILY_SEND_CAP - 1,
        SendTrigger::Exit => DAILY_SEND_CAP,
    };
    let attempts = state.attempts_in_bucket;
    if attempts >= cap {
        return Err(format!(
            "telemetry daily send limit reached ({attempts} of {cap} attempts)"
        ));
    }
    let interval = if attempts == 0 {
        0
    } else if state.pending.is_some() || !sent_today {
        backoff(RETRY_BACKOFF_SECS, attempts)
    } else if trigger == SendTrigger::Exit {
        EXIT_RESEND_INTERVAL_SECS
    } else {
        backoff(RESEND_INTERVAL_SECS, attempts)
    };
    // A clock that moved backwards must not stall sending until it catches up.
    let elapsed = state
        .last_attempt_unix
        .map_or(i64::MAX, |last| now.saturating_sub(last));
    if elapsed >= 0 && elapsed < interval {
        return Err(format!(
            "telemetry send not due for another {}s",
            interval - elapsed
        ));
    }
    Ok(())
}

fn backoff(base: i64, attempts: u32) -> i64 {
    base.saturating_mul(1_i64 << attempts.saturating_sub(1).min(16))
        .min(MAX_SEND_INTERVAL_SECS)
}

/// Fold this process's counters, then freeze the batch for `bucket` as the
/// pending payload. Also reports whether the batch carries any totals the
/// server has not acknowledged yet. Holds the one-shot lock only for the fold.
fn freeze_batch(
    state: &mut AggregateState,
    bucket: &str,
) -> Result<(TelemetryBatchV2, bool), String> {
    let one_shot_path = one_shot_path()?;
    ensure_parent(&one_shot_path)?;
    let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
    lock_telemetry_file(&one_shot_lock, "one-shot")?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&one_shot_path)?)?;
    let observed = fold_process_counters(&one_shot_path, &mut one_shots, bucket);
    write_one_shots(&one_shot_path, &one_shots)?;
    mark_folded(&one_shot_path, observed.clone());
    let (batch, included_days) = build_batch(&one_shots, bucket)?;
    // A day without an entry has no activity, so it has nothing unsent.
    let unsent = included_days
        .keys()
        .any(|day| one_shots.days.get(day).is_some_and(DayTotals::unsent));
    state.installation_id = batch_installation_id(&batch).to_string();
    state.pending = Some(PendingBatch {
        batch: batch.clone(),
        acknowledgement_id: uuid::Uuid::new_v4().to_string(),
        observed,
        process_nonce: process_nonce().to_string(),
        included_one_shots: QueuedOneShots::default(),
        included_days,
    });
    Ok((batch, unsent))
}

/// Advance durable state only when the exact frozen payload was accepted.
#[cfg(test)]
fn acknowledge_daily_batch(batch: &TelemetryBatchV2) -> Result<(), String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.lock_exclusive()
        .map_err(|error| format!("cannot lock telemetry aggregate state: {error}"))?;
    acknowledge_at(&path, &one_shot_path()?, batch)
}

fn acknowledge_at(
    state_path: &std::path::Path,
    one_shot_path: &std::path::Path,
    batch: &TelemetryBatchV2,
) -> Result<(), String> {
    let current_state = load_state_at(state_path)?;
    let pending = current_state
        .pending
        .as_ref()
        .ok_or_else(|| "no prepared telemetry batch to acknowledge".to_string())?;
    let included = pending.included_days.clone();
    let acknowledgement_id = pending_acknowledgement_id(pending)?;
    let state = acknowledge_state(current_state, batch)?;
    acknowledge_one_shots_at(one_shot_path, &included, &acknowledgement_id)?;
    write_state(state_path, &state)
}

fn pending_acknowledgement_id(pending: &PendingBatch) -> Result<String, String> {
    if !pending.acknowledgement_id.is_empty() {
        return Ok(pending.acknowledgement_id.clone());
    }
    let encoded = serde_json::to_vec(&pending.batch)
        .map_err(|error| format!("cannot encode telemetry acknowledgement: {error}"))?;
    Ok(hex::encode(sha2::Sha256::digest(encoded)))
}

fn acknowledge_state(
    mut state: AggregateState,
    batch: &TelemetryBatchV2,
) -> Result<AggregateState, String> {
    let pending = state
        .pending
        .take()
        .ok_or_else(|| "no prepared telemetry batch to acknowledge".to_string())?;
    if pending.batch != *batch {
        return Err("telemetry acknowledgement does not match pending batch".to_string());
    }
    state.process_nonce = pending.process_nonce;
    state.acknowledged = pending.observed;
    state.last_sent_bucket = batch
        .events
        .first()
        .map(|event| event.timestamp_bucket.clone());
    Ok(state)
}

pub fn last_sent_bucket() -> Option<String> {
    load_state()
        .and_then(state_for_current_identity)
        .ok()
        .and_then(|state| state.last_sent_bucket)
}

/// UTC bucket shared by payload generation and the background caller's precheck.
pub(crate) fn current_send_bucket() -> String {
    send_clock().0
}

/// UTC day bucket and unix time from one clock reading.
fn send_clock() -> (String, i64) {
    #[cfg(test)]
    if let Some(clock) = TEST_CLOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return clock;
    }
    let now = chrono::Utc::now();
    (now.format("%Y-%m-%d").to_string(), now.timestamp())
}

/// Fixed clock for tests; `None` uses the system clock.
#[cfg(test)]
static TEST_CLOCK: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// Pin the send clock until the guard drops.
#[cfg(test)]
struct TestClockGuard;

#[cfg(test)]
impl TestClockGuard {
    fn set(bucket: &str, unix: i64) -> Self {
        *TEST_CLOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((bucket.to_string(), unix));
        Self
    }
}

#[cfg(test)]
impl Drop for TestClockGuard {
    fn drop(&mut self) {
        *TEST_CLOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

fn state_for_current_identity(mut state: AggregateState) -> Result<AggregateState, String> {
    let (current, _) = installation_id::get_or_create_identity()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    let pending_mismatch = state.pending.as_ref().is_some_and(|pending| {
        let pending_id = batch_installation_id(&pending.batch);
        pending_id.is_empty() || pending_id != current
    });
    if pending_mismatch || (!state.installation_id.is_empty() && state.installation_id != current) {
        state = AggregateState {
            installation_id: current,
            ..AggregateState::default()
        };
    } else if state.installation_id.is_empty() {
        state.installation_id = current;
    }
    Ok(state)
}

fn batch_installation_id(batch: &TelemetryBatchV2) -> &str {
    batch
        .events
        .first()
        .map_or("", |event| event.installation_id.as_str())
}

/// Build today's cumulative totals under `today`, followed by every closed day
/// with unsent activity (oldest first) under its own bucket, as many as fit in
/// one batch. Today leads so the first event names the send's bucket. Returns
/// the exact totals included per day, to be recorded as sent on success.
fn build_batch(
    one_shots: &OneShotState,
    today: &str,
) -> Result<(TelemetryBatchV2, BTreeMap<String, QueuedOneShots>), String> {
    let (installation_id, deletion_token) = installation_id::get_or_create_identity()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    let deletion_token_hash = hex::encode(sha2::Sha256::digest(deletion_token.as_bytes()));
    let distribution_channel = distribution_channel();
    let client_family = client_family();
    let build_day = |day: &str, totals: &QueuedOneShots| {
        build_daily_aggregate(
            installation_id.clone(),
            deletion_token_hash.clone(),
            day.to_string(),
            distribution_channel,
            client_family,
            totals,
        )
    };
    let today_totals = one_shots
        .days
        .get(today)
        .map(|day| day.totals.clone())
        .unwrap_or_default();
    let mut batch = build_day(today, &today_totals)?;
    if let Some(history) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .ok()
        .and_then(history::usage_history)
    {
        let event = envelope_like(&batch.events[0], TelemetryEventV2::UsageHistory(history));
        batch.events.push(event);
    }
    let mut included = BTreeMap::from([(today.to_string(), today_totals)]);
    for (day, totals) in one_shots
        .days
        .range::<str, _>(before(today))
        .filter(|(_, day)| day.unsent())
    {
        let closed = build_day(day, &totals.totals)?;
        if batch.events.len() + closed.events.len() > MAX_BATCH_EVENTS {
            break;
        }
        batch.events.extend(closed.events);
        included.insert(day.clone(), totals.totals.clone());
    }
    batch
        .validate()
        .map_err(|error| format!("invalid telemetry batch: {error:?}"))?;
    Ok((batch, included))
}

fn build_daily_aggregate(
    installation_id: String,
    deletion_token_hash: String,
    timestamp_bucket: String,
    distribution_channel: DistributionChannel,
    client_family: ClientFamily,
    queued: &QueuedOneShots,
) -> Result<TelemetryBatchV2, String> {
    let observed = &queued.counters;
    let baseline = CounterCheckpoint::default();
    let latency_counts = bounded_histogram_delta(
        &observed.tool_latency_buckets,
        &baseline.tool_latency_buckets,
    );
    let calls = latency_counts.iter().sum();
    debug_assert_eq!(
        calls,
        observed
            .tool_calls
            .saturating_sub(baseline.tool_calls)
            .min(MAX_COUNT)
    );
    let failures = observed
        .tool_failures
        .saturating_sub(baseline.tool_failures)
        .min(calls);
    let duration = observed
        .session_uptime_secs
        .saturating_sub(baseline.session_uptime_secs)
        .min(MAX_COUNT);
    let mut batch = build_daily_heartbeat(
        installation_id,
        deletion_token_hash,
        timestamp_bucket,
        distribution_channel,
        client_family,
    )?;
    let common = batch.events[0].clone();
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::SetupProfile(setup_profile()),
    ));
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::SessionAggregate(SessionMetrics {
            sessions: 1,
            duration_seconds: single_observation_histogram(
                duration,
                &[
                    60,
                    300,
                    900,
                    3_600,
                    14_400,
                    86_400,
                    604_800,
                    31_536_000,
                    i64::MAX as u64,
                ],
                1,
            ),
        }),
    ));
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::ToolUsageAggregate(ToolUsageMetrics {
            calls,
            failures,
            latency_milliseconds: Histogram {
                upper_bounds: crate::core::telemetry::TOOL_LATENCY_BUCKET_UPPER_MS.to_vec(),
                counts: latency_counts.to_vec(),
            },
            tokens: Some(TokenMetrics {
                original: observed.tokens_input.min(MAX_COUNT),
                delivered: observed.tokens_output.min(MAX_COUNT),
            }),
        }),
    ));
    let tools = tool_call_deltas(&observed.tools, &baseline.tools, &observed.failure_messages);
    if !tools.is_empty() {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::ToolCallAggregate(ToolCallMetrics { tools }),
        ));
    }
    if queued.setup_completed {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::SetupCompleted(OccurrenceMetrics { count: 1 }),
        ));
    }
    if queued.integrations_detected > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::IntegrationDetected(OccurrenceMetrics {
                count: queued.integrations_detected.min(MAX_COUNT),
            }),
        ));
    }
    if let Some(transition) = queued.version_upgrade {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::VersionUpgrade(VersionUpgradeMetrics {
                from_major: transition.from_major,
                to_major: transition.to_major,
            }),
        ));
    }
    if queued.sync.attempts > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::SyncAggregate(queued.sync.clone()),
        ));
    }
    if queued.autopilot.admitted > 0 || queued.autopilot.denied > 0 || queued.autopilot.fallback > 0
    {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::AutopilotAggregate(queued.autopilot.clone()),
        ));
    }
    if queued.autopilot_fallback.fallback > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::AutopilotFallbackAggregate(queued.autopilot_fallback.clone()),
        ));
    }
    if queued.checkout_started > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::CheckoutStarted(OccurrenceMetrics {
                count: queued.checkout_started,
            }),
        ));
    }
    for (category, count) in ERROR_CATEGORIES
        .into_iter()
        .zip(queued.error_categories)
        .chain([(ErrorCategory::Command, queued.command_errors)])
    {
        let count = count.min(MAX_COUNT);
        if count > 0 {
            batch.events.push(envelope_like(
                &common,
                TelemetryEventV2::ErrorCategoryAggregate(ErrorMetrics { category, count }),
            ));
        }
    }
    if let Some(features) = feature_metrics(&queued.features) {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::FeatureAggregate(features),
        ));
    }
    batch
        .validate()
        .map_err(|error| format!("invalid telemetry batch: {error:?}"))?;
    Ok(batch)
}

fn bounded_histogram_delta<const N: usize>(observed: &[u64; N], baseline: &[u64; N]) -> [u64; N] {
    let mut remaining = MAX_COUNT;
    std::array::from_fn(|index| {
        let count = observed[index]
            .saturating_sub(baseline[index])
            .min(remaining);
        remaining -= count;
        count
    })
}

/// Per-tool calls since the baseline. Names failing the contract format are
/// dropped; past [`MAX_TOOL_ENTRIES`] the most-called tools are kept. The
/// result is sorted by name, as the contract requires.
fn tool_call_deltas(
    observed: &BTreeMap<String, ToolCounterCheckpoint>,
    baseline: &BTreeMap<String, ToolCounterCheckpoint>,
    messages: &BTreeMap<String, u64>,
) -> Vec<ToolCallCount> {
    let mut tools: Vec<ToolCallCount> = observed
        .iter()
        .filter(|(tool, _)| valid_tool_name(tool))
        .filter_map(|(tool, counter)| {
            let base = baseline.get(tool).copied().unwrap_or_default();
            let calls = counter.calls.saturating_sub(base.calls).min(MAX_COUNT);
            let failures = counter.failures.saturating_sub(base.failures).min(calls);
            (calls > 0).then(|| ToolCallCount {
                tool: tool.clone(),
                calls,
                failures,
                latency_milliseconds_total: Some(
                    (counter.latency_us.saturating_sub(base.latency_us) / 1_000).min(MAX_COUNT),
                ),
                failure_kinds: wire_kinds(
                    &sub_kinds(&counter.failure_kinds, &base.failure_kinds),
                    failures,
                ),
                failure_messages: wire_messages(messages, tool, failures),
            })
        })
        .collect();
    if tools.len() > MAX_TOOL_ENTRIES {
        tools.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.tool.cmp(&b.tool)));
        tools.truncate(MAX_TOOL_ENTRIES);
        tools.sort_by(|a, b| a.tool.cmp(&b.tool));
    }
    tools
}

fn envelope_like(template: &TelemetryEnvelopeV2, event: TelemetryEventV2) -> TelemetryEnvelopeV2 {
    TelemetryEnvelopeV2 {
        schema_version: template.schema_version,
        timestamp_bucket: template.timestamp_bucket.clone(),
        installation_id: template.installation_id.clone(),
        account_id: template.account_id.clone(),
        organization_id: template.organization_id.clone(),
        app_version: template.app_version.clone(),
        event,
    }
}

fn single_observation_histogram(value: u64, bounds: &[u64], count: u64) -> Histogram {
    let mut counts = vec![0; bounds.len()];
    let index = bounds
        .iter()
        .position(|bound| value <= *bound)
        .unwrap_or(bounds.len() - 1);
    counts[index] = count.min(MAX_COUNT);
    Histogram {
        upper_bounds: bounds.to_vec(),
        counts,
    }
}

fn current_checkpoint() -> CounterCheckpoint {
    let snapshot = crate::core::telemetry::global_metrics().daily_telemetry_snapshot();
    CounterCheckpoint {
        tool_calls: snapshot.tool_calls,
        tool_failures: snapshot.tool_failures,
        tool_latency_buckets: snapshot.tool_latency_buckets,
        session_uptime_secs: snapshot.session_uptime_secs,
        tools: snapshot
            .per_tool
            .into_iter()
            .map(|(tool, counter)| {
                (
                    tool.to_string(),
                    ToolCounterCheckpoint {
                        calls: counter.calls,
                        failures: counter.failures,
                        latency_us: counter.latency_us,
                        failure_kinds: counter.failure_kinds,
                    },
                )
            })
            .collect(),
        tokens_input: snapshot.tokens_input,
        tokens_output: snapshot.tokens_output,
        failure_messages: snapshot
            .failure_templates
            .into_iter()
            .map(|((tool, template), count)| (format!("{tool}\t{template}"), count))
            .collect(),
    }
}

fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// This process's counters already folded into each sidecar, keyed by path so
/// an isolated state directory starts from zero.
fn folded_baselines() -> &'static Mutex<HashMap<PathBuf, CounterCheckpoint>> {
    static FOLDED: OnceLock<Mutex<HashMap<PathBuf, CounterCheckpoint>>> = OnceLock::new();
    FOLDED.get_or_init(Mutex::default)
}

/// Add this process's not-yet-folded counters to the totals of `day` in memory
/// and return the observed checkpoint. Callers that persist the result must
/// then [`mark_folded`] it while still holding the sidecar lock.
fn fold_process_counters(
    path: &std::path::Path,
    one_shots: &mut OneShotState,
    day: &str,
) -> CounterCheckpoint {
    let observed = current_checkpoint();
    let folded = folded_baselines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .cloned()
        .unwrap_or_default();
    let delta = counter_delta(&observed, &folded);
    if delta != CounterCheckpoint::default() {
        add_counters(&mut day_totals(one_shots, day).counters, &delta);
    }
    observed
}

fn mark_folded(path: &std::path::Path, observed: CounterCheckpoint) {
    folded_baselines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), observed);
}

/// Persist this process's unsent tool counters so they survive process exit
/// and reach the next daily batch. While telemetry is disabled the counters
/// are skipped instead, so re-enabling never back-fills opted-out activity.
pub fn persist_process_counters() -> Result<(), String> {
    let path = one_shot_path()?;
    if !telemetry_collection_eligible() {
        mark_folded(&path, current_checkpoint());
        return Ok(());
    }
    ensure_parent(&path)?;
    let lock = open_sidecar_lock(&path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&path)?)?;
    let before = one_shots.clone();
    let observed = fold_process_counters(&path, &mut one_shots, &current_send_bucket());
    if one_shots.days != before.days || one_shots.queued != before.queued {
        write_one_shots(&path, &one_shots)?;
    }
    mark_folded(&path, observed);
    Ok(())
}

fn state_path() -> Result<PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_v2_aggregate.json"))
}

fn one_shot_path() -> Result<PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_v2_one_shots.json"))
}

pub fn record_setup_completion(
    integration_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        let mut setup_completed = false;
        if !state.setup_recorded {
            state.setup_recorded = true;
            setup_completed = true;
        }
        let mut inserted = 0_u64;
        for integration_id in integration_ids {
            if state.configured_integrations.len() >= 64 {
                break;
            }
            if integration_id.is_empty() || integration_id.len() > 128 {
                continue;
            }
            let stable_id = hex::encode(sha2::Sha256::digest(integration_id.as_bytes()));
            if state.configured_integrations.insert(stable_id) {
                inserted += 1;
            }
        }
        let today = today_totals(&mut state);
        today.setup_completed |= setup_completed;
        today.integrations_detected = today
            .integrations_detected
            .saturating_add(inserted)
            .min(MAX_COUNT);
        Ok((state, ()))
    })
}

pub fn record_sync_result(success: bool) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        let sync = &mut today_totals(&mut state).sync;
        if sync.attempts >= MAX_COUNT {
            return Ok((state, ()));
        }
        sync.attempts += 1;
        if success {
            sync.successes = sync.successes.saturating_add(1).min(sync.attempts);
        } else {
            sync.failures = sync
                .failures
                .saturating_add(1)
                .min(sync.attempts.saturating_sub(sync.successes));
        }
        Ok((state, ()))
    })
}

pub fn record_autopilot_decisions(admitted: u64, denied: u64) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        add_decisions(
            &mut today_totals(&mut state).autopilot,
            &DecisionMetrics {
                admitted,
                denied,
                fallback: 0,
            },
        );
        Ok((state, ()))
    })
}

pub fn record_autopilot_fallback() -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        let today = today_totals(&mut state);
        let fallback = DecisionMetrics {
            admitted: 0,
            denied: 0,
            fallback: 1,
        };
        add_decisions(&mut today.autopilot, &fallback);
        add_decisions(&mut today.autopilot_fallback, &fallback);
        Ok((state, ()))
    })
}

pub fn record_checkout_started() -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        let today = today_totals(&mut state);
        today.checkout_started = today.checkout_started.saturating_add(1).min(MAX_COUNT);
        Ok((state, ()))
    })
}

/// Counts one use of a registry feature (`core::telemetry_features`) for
/// today; `ok = false` also counts a failure. Invalid codes are ignored.
pub fn record_feature(code: &str, ok: bool) -> Result<(), String> {
    if !valid_feature_code(code) || !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        add_feature(
            &mut today_totals(&mut state).features,
            code,
            FeatureTally {
                count: 1,
                failures: u64::from(!ok),
            },
        );
        Ok((state, ()))
    })
}

const ERROR_CATEGORIES: [ErrorCategory; 8] = [
    ErrorCategory::Authentication,
    ErrorCategory::Authorization,
    ErrorCategory::Configuration,
    ErrorCategory::Network,
    ErrorCategory::Provider,
    ErrorCategory::Timeout,
    ErrorCategory::Validation,
    ErrorCategory::Internal,
];

pub fn record_error_category(category: ErrorCategory) -> Result<(), String> {
    record_error_category_inner(category)
}

fn record_error_category_inner(category: ErrorCategory) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    let index = ERROR_CATEGORIES
        .iter()
        .position(|candidate| *candidate == category);
    with_locked_one_shots(|mut state| {
        let totals = today_totals(&mut state);
        let count = match index {
            Some(index) => &mut totals.error_categories[index],
            None => &mut totals.command_errors,
        };
        *count = count.saturating_add(1).min(MAX_COUNT);
        Ok((state, ()))
    })
}

/// Collection stays off whenever the opt-out cannot be read: an unreadable or
/// corrupt config must never be mistaken for consent.
fn telemetry_collection_eligible() -> bool {
    let Ok(config) = crate::core::config::Config::try_load_global() else {
        return false;
    };
    let do_not_track = crate::core::host_env::var("DO_NOT_TRACK");
    let telemetry_override = crate::core::host_env::var("LEAN_CTX_TELEMETRY");
    !config.telemetry.explicitly_disabled()
        && !crate::core::config::TelemetryConfig::environment_disables(
            do_not_track.as_deref(),
            telemetry_override.as_deref(),
        )
        && !crate::core::telemetry_consent::running_in_ci()
}

pub fn record_current_version() -> Result<(), String> {
    record_current_version_value(env!("CARGO_PKG_VERSION"))
}

fn record_current_version_value(version: &str) -> Result<(), String> {
    let current = parse_major(version)
        .ok_or_else(|| "current app version has no valid major component".to_string())?;
    with_locked_one_shots(|mut state| {
        let previous = match state.observed_major {
            Some(previous) => Some(previous),
            None => crate::core::telemetry_ledger::latest_valid_version()?
                .as_deref()
                .and_then(parse_major),
        };
        match previous {
            Some(previous) if current > previous => {
                // Several upgrades within one day collapse into one transition.
                let today = today_totals(&mut state);
                let from_major = today
                    .version_upgrade
                    .map_or(previous, |transition| transition.from_major);
                today.version_upgrade = Some(VersionTransition {
                    from_major,
                    to_major: current,
                });
                state.observed_major = Some(current);
            }
            Some(previous) => state.observed_major = Some(previous.max(current)),
            None => state.observed_major = Some(current),
        }
        Ok((state, ()))
    })
}

fn parse_major(version: &str) -> Option<u16> {
    version.split('.').next()?.parse().ok()
}

pub fn purge_local_state() -> Result<(), String> {
    purge_local_state_then(|| Ok(()))
}

pub fn purge_local_state_then<T>(
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&one_shot_path)?;
    let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
    lock_telemetry_file(&one_shot_lock, "one-shot")?;
    remove_state_file(&path, "aggregate")?;
    remove_state_file(&one_shot_path, "one-shot")?;
    operation()
}

pub fn rotate_identity_state_then<T>(
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = state_path()?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
    lock_telemetry_file(&one_shot_lock, "one-shot")?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&one_shot_path)?)?;
    write_one_shots(&one_shot_path, &one_shots)?;
    let value = operation()?;
    one_shots.installation_id = installation_id::get_or_create()
        .map_err(|error| format!("installation ID unavailable after rotation: {error}"))?;
    requeue_for_new_identity(&mut one_shots);
    remove_state_file(&path, "aggregate")?;
    write_one_shots(&one_shot_path, &one_shots)?;
    Ok(value)
}

/// A new identity starts with no history, so only setup facts carry over:
/// its first batch reports the existing setup as that identity's own.
fn requeue_for_new_identity(state: &mut OneShotState) {
    state.queued = QueuedOneShots::default();
    state.days.clear();
    state.last_acknowledged_batch = None;
    let setup_completed = state.setup_recorded;
    let integrations = state
        .configured_integrations
        .len()
        .try_into()
        .unwrap_or(MAX_COUNT)
        .min(MAX_COUNT);
    let today = today_totals(state);
    today.setup_completed = setup_completed;
    today.integrations_detected = integrations;
}

/// Totals for the current UTC day, created on first use.
fn today_totals(state: &mut OneShotState) -> &mut QueuedOneShots {
    day_totals(state, &current_send_bucket())
}

fn day_totals<'a>(state: &'a mut OneShotState, day: &str) -> &'a mut QueuedOneShots {
    &mut state.days.entry(day.to_string()).or_default().totals
}

fn remove_state_file(path: &std::path::Path, kind: &str) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot purge telemetry {kind} state: {error}")),
    }
}

fn load_state() -> Result<AggregateState, String> {
    let path = state_path()?;
    load_state_at(&path)
}

fn load_state_at(path: &std::path::Path) -> Result<AggregateState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid telemetry aggregate state: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AggregateState::default()),
        Err(error) => Err(format!("cannot read telemetry aggregate state: {error}")),
    }
}

fn load_one_shots_at(path: &std::path::Path) -> Result<OneShotState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid telemetry one-shot state: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OneShotState::default()),
        Err(error) => Err(format!("cannot read telemetry one-shot state: {error}")),
    }
}

fn one_shots_for_current_identity(mut state: OneShotState) -> Result<OneShotState, String> {
    let current = installation_id::get_or_create()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    if state.installation_id.is_empty() {
        state.installation_id = current;
    } else if state.installation_id != current {
        state.installation_id = current;
        requeue_for_new_identity(&mut state);
    }
    normalize_days(&mut state, &current_send_bucket());
    Ok(state)
}

/// Range over the day keys strictly before `day` (closed days).
fn before(day: &str) -> (std::ops::Bound<&str>, std::ops::Bound<&str>) {
    (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(day))
}

/// Migrate the pre-daily-totals queue into today and drop closed days the
/// server already holds, keeping the most recent unsent ones.
fn normalize_days(state: &mut OneShotState, today: &str) {
    if !state.queued.is_empty() {
        let legacy = std::mem::take(&mut state.queued);
        day_totals(state, today).absorb(&legacy);
    }
    state
        .days
        .retain(|day, totals| day.as_str() >= today || totals.unsent());
    let closed = state.days.range::<str, _>(before(today)).count();
    let stale: Vec<String> = state
        .days
        .keys()
        .take(closed.saturating_sub(RETAINED_CLOSED_DAYS))
        .cloned()
        .collect();
    for day in stale {
        state.days.remove(&day);
    }
}

fn with_locked_one_shots<T>(
    operation: impl FnOnce(OneShotState) -> Result<(OneShotState, T), String>,
) -> Result<T, String> {
    let path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_sidecar_lock(&path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let state = one_shots_for_current_identity(load_one_shots_at(&path)?)?;
    let (state, value) = operation(state)?;
    write_one_shots(&path, &state)?;
    Ok(value)
}

/// Record the exact per-day totals the server accepted. Totals that grew while
/// the batch was in flight stay unsent and go out with the next send.
fn acknowledge_one_shots_at(
    path: &std::path::Path,
    included: &BTreeMap<String, QueuedOneShots>,
    acknowledgement_id: &str,
) -> Result<(), String> {
    ensure_parent(path)?;
    let lock = open_sidecar_lock(path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let mut state = one_shots_for_current_identity(load_one_shots_at(path)?)?;
    if state.last_acknowledged_batch.as_deref() == Some(acknowledgement_id) {
        return Ok(());
    }
    // A day pruned or reset by an identity change in the meantime stays gone.
    for (day, sent) in included {
        if let Some(totals) = state.days.get_mut(day) {
            totals.sent = sent.clone();
        }
    }
    state.last_acknowledged_batch = Some(acknowledgement_id.to_string());
    normalize_days(&mut state, &current_send_bucket());
    write_one_shots(path, &state)
}

#[cfg(test)]
fn with_locked_state<T>(
    operation: impl FnOnce(AggregateState) -> Result<(AggregateState, T), String>,
) -> Result<T, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.lock_exclusive()
        .map_err(|error| format!("cannot lock telemetry aggregate state: {error}"))?;
    let (state, value) = operation(load_state_at(&path)?)?;
    write_state(&path, &state)?;
    Ok(value)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "telemetry state path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create telemetry state directory: {error}"))
}

fn write_state(path: &std::path::Path, state: &AggregateState) -> Result<(), String> {
    let bytes = serde_json::to_vec(&state)
        .map_err(|error| format!("cannot serialize telemetry aggregate state: {error}"))?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(&path, &bytes, permissions.as_ref())
        .map_err(|error| format!("cannot persist telemetry aggregate state: {error}"))
}

fn write_one_shots(path: &std::path::Path, state: &OneShotState) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("cannot serialize telemetry one-shot state: {error}"))?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(path, &bytes, permissions.as_ref())
        .map_err(|error| format!("cannot persist telemetry one-shot state: {error}"))
}

fn open_state_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|error| format!("cannot open telemetry state lock: {error}"))
}

fn open_sidecar_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    open_state_lock(path)
}

/// Contention fails without mutating state; failed acknowledgements keep the
/// frozen batch retryable. This bounds acquisition, not filesystem I/O.
/// Tests use the production bound: a contender must outwait a lease holder
/// even under coverage instrumentation, where 150 ms timed out.
const SEND_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
const SEND_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Shared acquisition for aggregate, one-shot and ledger locks, in that order.
pub(super) fn lock_telemetry_file(file: &std::fs::File, kind: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + SEND_LOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if super::file_lock::is_contended(&error) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "telemetry {kind} lock timed out after {}ms; another operation is active",
                        SEND_LOCK_TIMEOUT.as_millis()
                    ));
                }
                std::thread::sleep(SEND_LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(format!("cannot lock telemetry {kind} state: {error}")),
        }
    }
}

pub fn build_daily_heartbeat(
    installation_id: String,
    deletion_token_hash: String,
    timestamp_bucket: String,
    distribution_channel: DistributionChannel,
    client_family: ClientFamily,
) -> Result<TelemetryBatchV2, String> {
    let install_age = environment::install_age(std::time::SystemTime::now());
    let active_days = environment::active_days(&installation_id, &timestamp_bucket);
    let batch = TelemetryBatchV2 {
        schema_version: SCHEMA_VERSION,
        deletion_token_hash,
        events: vec![TelemetryEnvelopeV2 {
            schema_version: SCHEMA_VERSION,
            timestamp_bucket,
            installation_id,
            account_id: None,
            organization_id: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            event: TelemetryEventV2::Heartbeat(HeartbeatMetrics {
                distribution_channel,
                client_family,
                operating_system: OperatingSystem::current(),
                architecture: Architecture::current(),
                install_age,
                active_days: Some(active_days),
                runtime_environment: Some(environment::runtime_environment()),
            }),
        }],
    };
    batch
        .validate()
        .map_err(|error| format!("invalid telemetry batch: {error:?}"))?;
    Ok(batch)
}

#[cfg(test)]
mod tests;
