// SPDX-License-Identifier: Apache-2.0
//! One engine-metrics pass: sharing state, gate, sample, build, buffer,
//! sign, send (`POST /v1/metrics/hours`), plus the local views (`hive
//! status`, `hive metrics preview`, `hive metrics delete`) and the
//! person's confirmation (`hive metrics confirm`).
//!
//! **Release switch (owner sequencing).** Until the lead's go after the
//! privacy block K1–K6, nothing is collected or sent to a real Hive:
//! [`released`] is false except for a loopback Hive (the fake wire of the
//! tests) with `LEAN_CTX_HIVE_ENGINE_METRICS=fake-wire`. The preview and the
//! status work regardless; they never send.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::Signer as _;

use super::buffer::{self, Buffer, Item, Refusal, State};
use super::collect::{self, Scope, Sources};
use super::gate::{self, OffReason, Sharing};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::instance_key::InstanceKeys;
use crate::core::hive::state::{HiveStore, InstanceRecord};
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::hive::lease::HiveLeaseV1;
use lean_ctx_protocol::hive::metrics_v2::{
    BucketV2, EngineOs, EngineV2, EngineVersion, HOURS_ROUTE, HourStamp, HoursAckV2, HoursBodyV2,
    HoursRequestV2, HoursSchema, KeyId, MAX_BUCKETS, MAX_HOURS_BODY_BYTES, PolicyLevel,
    SHARING_CONFIRM_ROUTE, SHARING_ROUTE, SharingStateV2, SharingValue, SignatureHex,
};

/// Whether engine metrics are released for real sends (owner go pending).
const RELEASED: bool = false;
/// Test switch: only a loopback Hive may receive before the release.
pub const TEST_SWITCH_ENV: &str = "LEAN_CTX_HIVE_ENGINE_METRICS";
/// Days the first period may reach back (and never before `since`).
const BACKFILL_DAYS: i64 = 30;
/// Minimum seconds between two regular passes.
const DEBOUNCE_SECS: i64 = 3_600;
const BACKOFF_START: i64 = 60;
const BACKOFF_MAX: i64 = 3_600;

/// Whether this engine may collect and send to the Hive at `origin`.
pub fn released(origin: &str) -> bool {
    let switch = std::env::var(TEST_SWITCH_ENV);
    RELEASED || (switch.as_deref() == Ok("fake-wire") && crate::core::hive::is_loopback(origin))
}

/// What a pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsV2Run {
    /// Held back until the release (owner sequencing).
    NotReleased,
    NotDue,
    Off(Sharing),
    /// Waiting for the back-off after 429/5xx.
    Waiting,
    Sent {
        buckets: u64,
        refused: u64,
    },
    Empty,
}

fn engine() -> EngineV2 {
    let os = match std::env::consts::OS {
        "macos" => EngineOs::Macos,
        "windows" => EngineOs::Windows,
        _ => EngineOs::Linux,
    };
    EngineV2 {
        engine_version: EngineVersion::new(env!("CARGO_PKG_VERSION"))
            .or_else(|| EngineVersion::new("0.0.0"))
            .expect("version"),
        os,
    }
}

fn hours_between(from: &HourStamp, to: &HourStamp) -> Vec<HourStamp> {
    let mut out = Vec::new();
    let mut at = from.start();
    let end = to.start();
    while at <= end {
        out.push(HourStamp::of(at));
        at += Duration::hours(1);
    }
    out
}

/// The vaults allowed and whether unattributed work goes.
pub(crate) struct Allowed<'a> {
    pub(crate) vaults_off: &'a std::collections::BTreeSet<String>,
    pub(crate) unattributed: bool,
}

/// The first hour that may be built: the sharing period's start, at most
/// 30 days back, never before the person agreed, and never before this
/// engine's own on period began (`local`, after an engine-side off; it may
/// lie in the next hour, which then is the first one built).
fn first_hour(
    since: Option<DateTime<Utc>>,
    local: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> HourStamp {
    let floor = now - Duration::days(BACKFILL_DAYS);
    let start = since.unwrap_or(now).max(floor).min(now);
    HourStamp::of(local.map_or(start, |local| start.max(local)))
}

/// Sample the cumulative counters, close the open hour, and (re)build the
/// buckets that may change: hours never built, the previous and the
/// current hour. Closed hours stay as they are.
pub(crate) fn build(
    sources: &Sources<'_>,
    scope: &Scope,
    state: &mut State,
    buffer: &Buffer,
    allowed: &Allowed<'_>,
    since: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), HiveError> {
    let now_hour = HourStamp::of(now);
    let prev_hour = HourStamp::of(now - Duration::hours(1));
    let snap = collect::snapshot(sources, scope);
    match state.open_hour.clone() {
        None => {
            let local = sources
                .hive
                .and_then(|hive| super::buffer::local_on_since(hive, now));
            state.collect_from = Some(first_hour(since, local, now));
            state.open_hour = Some(now_hour.clone());
            state.open_start = snap.clone();
        }
        Some(open) if open != now_hour => {
            let mut item = buffer.get(&open).unwrap_or_else(|| Item {
                hour: Some(open.clone()),
                ..Item::default()
            });
            item.sampled = collect::sampled(&state.open_start, &snap);
            if !item.sampled.is_empty() || item.bucket.is_some() {
                buffer.put(&item)?;
            }
            state.open_hour = Some(now_hour.clone());
            state.open_start = snap.clone();
        }
        Some(_) => {}
    }
    let current = collect::sampled(&state.open_start, &snap);
    let collect_from = state
        .collect_from
        .clone()
        .unwrap_or_else(|| now_hour.clone());
    let first = state
        .built_until
        .clone()
        .unwrap_or_else(|| collect_from.clone())
        .min(prev_hour.clone())
        .max(collect_from);
    let exact = collect::exact(
        sources,
        scope,
        first.start(),
        now_hour.start() + Duration::hours(1),
        prev_hour.start(),
    );
    for hour in hours_between(&first, &now_hour) {
        let mut item = buffer.get(&hour).unwrap_or_else(|| Item {
            hour: Some(hour.clone()),
            ..Item::default()
        });
        if hour == now_hour {
            item.sampled = current.clone();
        }
        let slices = collect::slices(
            scope,
            &hour,
            &exact,
            &item.sampled,
            allowed.vaults_off,
            allowed.unattributed,
        );
        if slices.is_empty() {
            if item.bucket.is_some() || !item.sampled.is_empty() {
                buffer.put(&item)?;
            }
            continue;
        }
        if item.bucket.as_ref().is_none_or(|b| b.slices != slices) {
            item.bucket = Some(BucketV2 {
                hour: hour.clone(),
                rev: state.rev(),
                slices,
            });
        }
        buffer.put(&item)?;
    }
    state.built_until = Some(now_hour);
    state.dropped_buckets = state
        .dropped_buckets
        .saturating_add(buffer.enforce_cap(buffer::MAX_ITEMS, buffer::MAX_BYTES));
    if let Some(hive) = sources.hive {
        super::secrets::prune(hive, prev_hour.start());
        super::signals::prune(hive, prev_hour.start());
    }
    Ok(())
}

/// The pending buckets, oldest first, without slices the gate no longer
/// allows (a vault turned off after the hour was built).
fn pending(buffer: &Buffer, allowed: &Allowed<'_>) -> Vec<BucketV2> {
    buffer
        .items()
        .into_iter()
        .filter(Item::pending)
        .filter_map(|item| {
            let mut bucket = item.bucket?;
            bucket.slices.retain(|s| match &s.vault {
                Some(v) => !allowed.vaults_off.contains(v.as_str()),
                None => allowed.unattributed,
            });
            (!bucket.slices.is_empty()).then_some(bucket)
        })
        .collect()
}

fn body(buckets: Vec<BucketV2>) -> HoursBodyV2 {
    HoursBodyV2 {
        schema: HoursSchema,
        engine: engine(),
        buckets,
    }
}

/// Batches of at most 168 buckets and 512 KiB (signature included).
pub(crate) fn batches(buckets: Vec<BucketV2>) -> Vec<HoursBodyV2> {
    // `key_id` and `signature` with their JSON framing.
    const SIGNED_EXTRA: usize = 200;
    let size = |b: &[BucketV2]| {
        serde_json::to_vec(&body(b.to_vec())).map_or(usize::MAX, |v| v.len() + SIGNED_EXTRA)
    };
    let mut out = Vec::new();
    let mut current: Vec<BucketV2> = Vec::new();
    for bucket in buckets {
        current.push(bucket);
        if current.len() > MAX_BUCKETS || size(&current) > MAX_HOURS_BODY_BYTES {
            let last = current.pop().expect("just pushed");
            if !current.is_empty() {
                out.push(body(std::mem::take(&mut current)));
            }
            current.push(last);
        }
    }
    if !current.is_empty() {
        out.push(body(current));
    }
    out
}

/// Sign a batch with the instance key (as `worker_register`).
pub(crate) fn sign(keys: &InstanceKeys, body: HoursBodyV2) -> Result<HoursRequestV2, HiveError> {
    let signer = keys.signing_key();
    let key_id = KeyId::new(&crate::core::hive::workers::key_id(signer))
        .ok_or_else(|| HiveError::Local("instance key id".into()))?;
    let input = body
        .signing_input(&key_id)
        .map_err(|e| HiveError::Local(e.to_string()))?;
    let signature = SignatureHex::new(&hex::encode(signer.sign(&input).to_bytes()))
        .ok_or_else(|| HiveError::Local("signature".into()))?;
    Ok(body.signed(key_id, signature))
}

/// The same batch always carries the same key, so a retry is idempotent.
fn idempotency_key(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(&sha2::Sha256::digest(bytes)[..16])
}

fn level_of(raw: Option<&str>) -> Option<PolicyLevel> {
    raw.and_then(PolicyLevel::from_wire)
}

/// Stop: record the refusal and drop everything buffered.
fn refused(
    hive_data: &Path,
    state: &mut State,
    refusal: Refusal,
) -> Result<MetricsV2Run, HiveError> {
    let (reason, level) = match &refusal {
        Refusal::Policy { level, .. } => (OffReason::Policy, *level),
        Refusal::Lease { reason, .. } => (*reason, None),
    };
    state.refusal = Some(refusal);
    state.store(hive_data)?;
    buffer::drop_all(hive_data)?;
    Ok(MetricsV2Run::Off(Sharing::Off { reason, level }))
}

/// `GET /v1/metrics/sharing`.
fn fetch_sharing(session: &Session) -> Result<SharingStateV2, HiveError> {
    let answer = session.with_client(|client| client.get(SHARING_ROUTE))?;
    serde_json::from_value(answer)
        .map_err(|e| HiveError::Protocol(format!("engine metrics sharing: {e}")))
}

/// Let the redaction path count redactions per detector (sharing is on).
fn ensure_secret_counting(session: &Session) -> Result<(), HiveError> {
    let hive = session.store.data_dir();
    if super::secrets::enabled(hive) {
        return Ok(());
    }
    super::secrets::enable(hive)
}

/// One pass at `now`; `force` skips the hourly debounce (`hive sync`).
pub fn run(
    session: &Session,
    config: &HiveConfig,
    now: DateTime<Utc>,
    force: bool,
) -> Result<MetricsV2Run, HiveError> {
    if !released(&session.instance.url) {
        return Ok(MetricsV2Run::NotReleased);
    }
    let store = &session.store;
    let hive_data = store.data_dir();
    let lease = &session.lease.lease;
    let _lock = crate::core::hive::state::lock_state(&buffer::dir_in(hive_data), "state.json")?;
    let mut state = State::load(hive_data);
    if state.org_id != session.instance.org_id {
        // Another workspace: nothing of the old one goes to the new one.
        buffer::forget(hive_data);
        state = State {
            org_id: session.instance.org_id.clone(),
            ..State::default()
        };
    }
    // K2 (L2): an off switch applies at once, on every pass, not only on
    // the hourly one: nothing stays buffered once any switch says off.
    if !gate::decide_with(
        store,
        config,
        &session.instance,
        lease,
        state.sharing.as_ref(),
        now,
    )
    .is_on()
    {
        buffer::drop_all(hive_data)?;
        state = State::load(hive_data);
        state.org_id.clone_from(&session.instance.org_id);
    }
    let due = force
        || state
            .last_run
            .is_none_or(|last| now.timestamp() < last || now.timestamp() - last >= DEBOUNCE_SECS)
        || state
            .open_hour
            .as_ref()
            .is_some_and(|open| *open != HourStamp::of(now));
    if !due {
        return Ok(MetricsV2Run::NotDue);
    }
    // The workspace's word first: nothing is collected unless it says on.
    if config.metrics_upload {
        match fetch_sharing(session) {
            Ok(sharing) => state.sharing = Some(sharing),
            Err(error) if error.is_transient() => {}
            Err(error) => {
                state.store(hive_data)?;
                return Err(error);
            }
        }
    }
    state.store(hive_data)?;
    let sharing = gate::decide_with(
        store,
        config,
        &session.instance,
        lease,
        state.sharing.as_ref(),
        now,
    );
    let Sharing::On {
        vaults_off,
        unattributed,
        policy_version,
        since,
        ..
    } = &sharing
    else {
        buffer::drop_all(hive_data)?;
        return Ok(MetricsV2Run::Off(sharing));
    };
    state.refusal = None;
    if let Err(error) = ensure_secret_counting(session) {
        store.record_error(&format!("engine metrics redaction counter: {error}"));
    }
    let data = crate::core::data_dir::lean_ctx_data_dir().map_err(HiveError::Local)?;
    let home = dirs::home_dir();
    let sources = Sources {
        data: &data,
        home: home.as_deref(),
        hive: Some(hive_data),
    };
    let scope_since = state
        .open_hour
        .as_ref()
        .map_or(now - Duration::days(BACKFILL_DAYS), |h| {
            h.start().min(now - Duration::days(3))
        });
    let scope = Scope::load(&data, &VaultMap::load(hive_data), scope_since);
    let buffer = Buffer::at(hive_data);
    let allowed = Allowed {
        vaults_off,
        unattributed: *unattributed,
    };
    build(&sources, &scope, &mut state, &buffer, &allowed, *since, now)?;
    state.last_run = Some(now.timestamp());
    state.store(hive_data)?;

    if state.retry_at.is_some_and(|at| now.timestamp() < at) && !force {
        return Ok(MetricsV2Run::Waiting);
    }
    let requests = batches(pending(&buffer, &allowed));
    if requests.is_empty() {
        return Ok(MetricsV2Run::Empty);
    }
    let keys = InstanceKeys::load(store)?;
    let (mut sent, mut refused_slices) = (0u64, 0u64);
    for body in requests {
        body.validate()
            .map_err(|e| HiveError::Local(format!("engine metrics withheld: {e}")))?;
        let request = sign(&keys, body)?;
        let bytes = serde_json::to_vec(&request).map_err(|e| HiveError::Local(e.to_string()))?;
        let wire: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| HiveError::Local(e.to_string()))?;
        let key = idempotency_key(&bytes);
        match session.with_client(|client| client.post_idempotent(HOURS_ROUTE, &wire, &key)) {
            Ok(answer) => {
                let ack: HoursAckV2 = serde_json::from_value(answer)
                    .map_err(|e| HiveError::Protocol(format!("engine metrics ack: {e}")))?;
                for bucket in &request.buckets {
                    if let Some(mut item) = buffer.get(&bucket.hour) {
                        item.acked_rev = bucket.rev;
                        buffer.put(&item)?;
                    }
                }
                sent += u64::from(ack.accepted);
                refused_slices += ack.refused.len() as u64;
                state.last_sent_at = Some(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                state.retry_at = None;
                state.attempts = 0;
                let off = ack.sharing.state != SharingValue::On;
                state.sharing = Some(ack.sharing);
                if off {
                    state.store(hive_data)?;
                    buffer::drop_all(hive_data)?;
                    return Ok(MetricsV2Run::Off(gate::decide(
                        store,
                        config,
                        &session.instance,
                        lease,
                        now,
                    )));
                }
            }
            Err(HiveError::PolicyDenied(denial)) => {
                return refused(
                    hive_data,
                    &mut state,
                    Refusal::Policy {
                        policy_version: *policy_version,
                        level: level_of(denial.level.as_deref()),
                    },
                );
            }
            Err(HiveError::UploadDisabled(_)) => {
                return refused(
                    hive_data,
                    &mut state,
                    Refusal::Lease {
                        lease_id: lease.lease_id.clone(),
                        reason: OffReason::Workspace,
                    },
                );
            }
            Err(HiveError::Forbidden) => {
                return refused(
                    hive_data,
                    &mut state,
                    Refusal::Lease {
                        lease_id: lease.lease_id.clone(),
                        reason: OffReason::Lease,
                    },
                );
            }
            Err(HiveError::Server { status: 403, .. }) => {
                // `sharing_not_confirmed` (or a newer refusal): ask again and stop.
                state.sharing = fetch_sharing(session).ok().or(state.sharing.take());
                state.store(hive_data)?;
                buffer::drop_all(hive_data)?;
                return Ok(MetricsV2Run::Off(gate::decide(
                    store,
                    config,
                    &session.instance,
                    lease,
                    now,
                )));
            }
            Err(error) if error.is_transient() => {
                state.attempts = state.attempts.saturating_add(1);
                let backoff = BACKOFF_START
                    .saturating_mul(1 << state.attempts.min(10))
                    .min(BACKOFF_MAX);
                let wait = error
                    .retry_after_at(now)
                    .and_then(|s| i64::try_from(s).ok())
                    .unwrap_or(backoff);
                state.retry_at = Some(now.timestamp().saturating_add(wait));
                state.store(hive_data)?;
                return Err(error);
            }
            Err(
                error @ (HiveError::Server {
                    status: 400 | 413, ..
                }
                | HiveError::Protocol(_)),
            ) => {
                // These hours would be refused again: give them up, counted.
                for bucket in &request.buckets {
                    buffer.remove(&bucket.hour);
                    state.dropped_buckets = state.dropped_buckets.saturating_add(1);
                }
                state.store(hive_data)?;
                return Err(error);
            }
            Err(error) => {
                state.store(hive_data)?;
                return Err(error);
            }
        }
    }
    // Acked hours older than the previous one are done.
    let prev = HourStamp::of(now - Duration::hours(1));
    for item in buffer.items() {
        if let Some(hour) = &item.hour
            && !item.pending()
            && *hour < prev
        {
            buffer.remove(hour);
        }
    }
    state.refused_slices = state.refused_slices.saturating_add(refused_slices);
    state.store(hive_data)?;
    Ok(MetricsV2Run::Sent {
        buckets: sent,
        refused: refused_slices,
    })
}

/// `hive metrics confirm`: the person agrees (Personal). Callers
/// make sure a person answered in a terminal; MCP tools never call this.
pub fn confirm(session: &Session) -> Result<SharingStateV2, HiveError> {
    if !released(&session.instance.url) {
        return Err(HiveError::Refused(
            "engine metrics are not released yet; nothing is shared".into(),
        ));
    }
    let answer =
        session.with_client(|client| client.post(SHARING_CONFIRM_ROUTE, &serde_json::json!({})))?;
    let sharing: SharingStateV2 = serde_json::from_value(answer)
        .map_err(|e| HiveError::Protocol(format!("engine metrics sharing: {e}")))?;
    let hive = session.store.data_dir();
    let mut state = State::load(hive);
    if state.org_id != session.instance.org_id {
        state = State {
            org_id: session.instance.org_id.clone(),
            ..State::default()
        };
    }
    state.sharing = Some(sharing.clone());
    state.store(hive)?;
    Ok(sharing)
}

// ── local views ──────────────────────────────────────────────────────────────

/// What `hive status` shows about engine metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsV2Status {
    pub sharing: Sharing,
    pub state: SharingStateV2,
    pub released: bool,
    pub last_sent_at: Option<String>,
    pub buffered: u64,
    pub dropped: u64,
}

impl MetricsV2Status {
    /// `Sharing engine metrics with <workspace>: on (plan default) · counts only, incl. full
    /// re-fetches and unregulated program names · last sent 17:05 UTC · 2 hours buffered`.
    pub fn line(&self, workspace: &str) -> String {
        let mut line = format!(
            "Sharing engine metrics with {workspace}: {}",
            self.sharing.describe()
        );
        if !self.released {
            line.push_str(" · not released yet, nothing is sent");
        }
        // K4: what the counts include, beyond the obvious.
        line.push_str(" · counts only, incl. full re-fetches and unregulated program names");
        if self.sharing.is_on() {
            if let Some(at) = self
                .last_sent_at
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            {
                line.push_str(&format!(" · last sent {} UTC", at.format("%H:%M")));
            }
            line.push_str(&format!(" · {} hours buffered", self.buffered));
        }
        if self.dropped > 0 {
            line.push_str(&format!(" · {} hours dropped (buffer full)", self.dropped));
        }
        line
    }

    pub fn wire(&self) -> lean_ctx_protocol::hive::status::HiveStatusMetricsV2 {
        let mut sharing = self.state.clone();
        // This engine sends nothing whatever the workspace says: say off.
        if !self.sharing.is_on() && sharing.state == SharingValue::On {
            sharing.state = SharingValue::Off;
        }
        lean_ctx_protocol::hive::status::HiveStatusMetricsV2 {
            sharing,
            last_sent_at: self.last_sent_at.clone(),
            buffered_buckets: self.buffered,
            dropped_buckets: self.dropped,
            released: Some(self.released),
        }
    }
}

/// Engine metrics as `hive status` reports them (local only).
pub fn status(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> MetricsV2Status {
    let state = State::load(store.data_dir());
    let known = state
        .sharing
        .clone()
        .filter(|_| state.org_id == instance.org_id);
    MetricsV2Status {
        sharing: gate::decide(store, config, instance, lease, now),
        state: known.unwrap_or_else(|| gate::plan_state(lease.plan)),
        released: released(&instance.url),
        last_sent_at: state.last_sent_at,
        buffered: Buffer::at(store.data_dir()).pending() as u64,
        dropped: state.dropped_buckets,
    }
}

/// The buckets of `hours` (oldest first) from the same collector that
/// uploads: exact counters from the local stores now, the sampled part and
/// the `rev` as buffered (rev 1 for an hour never built). Local only.
fn local_buckets(
    store: &HiveStore,
    instance: &InstanceRecord,
    sharing: &Sharing,
    hours: &[HourStamp],
    now: DateTime<Utc>,
) -> Result<Vec<BucketV2>, HiveError> {
    let (Some(first), Some(last)) = (hours.first(), hours.last()) else {
        return Ok(Vec::new());
    };
    let (vaults_off, unattributed) = match sharing {
        Sharing::On {
            vaults_off,
            unattributed,
            ..
        } => (vaults_off.clone(), *unattributed),
        Sharing::Off { .. } => (
            gate::policy(store.data_dir(), now)
                .map(|(off, _)| off)
                .unwrap_or_default(),
            instance.org_kind.as_deref() == Some("personal"),
        ),
    };
    let data = crate::core::data_dir::lean_ctx_data_dir().map_err(HiveError::Local)?;
    let home = dirs::home_dir();
    let sources = Sources {
        data: &data,
        home: home.as_deref(),
        hive: Some(store.data_dir()),
    };
    let scope = Scope::load(
        &data,
        &VaultMap::load(store.data_dir()),
        first.start() - Duration::days(3),
    );
    let end = last.start() + Duration::hours(1);
    // Spend is read by minute per hour: the last two hours, as uploads do.
    let exact = collect::exact(
        &sources,
        &scope,
        first.start(),
        end,
        end - Duration::hours(2),
    );
    let buffer = Buffer::at(store.data_dir());
    let mut out = Vec::new();
    for hour in hours {
        let buffered = buffer.get(hour);
        let sampled = buffered
            .as_ref()
            .map(|i| i.sampled.clone())
            .unwrap_or_default();
        let rev = buffered.and_then(|i| i.bucket.map(|b| b.rev)).unwrap_or(1);
        let slices = collect::slices(&scope, hour, &exact, &sampled, &vaults_off, unattributed);
        if !slices.is_empty() {
            out.push(BucketV2 {
                hour: hour.clone(),
                rev,
                slices,
            });
        }
    }
    Ok(out)
}

/// The next batch exactly as it would be sent, unsigned; changes nothing.
/// Sharing on: the oldest pending batch, else the last complete hour built
/// now. Sharing not on: no buckets (nothing would be sent).
pub fn preview(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> Result<(Sharing, HourStamp, HoursBodyV2), HiveError> {
    let sharing = gate::decide(store, config, instance, lease, now);
    let hour = HourStamp::of(now - Duration::hours(1));
    let buckets = match &sharing {
        Sharing::On {
            vaults_off,
            unattributed,
            ..
        } => {
            let allowed = Allowed {
                vaults_off,
                unattributed: *unattributed,
            };
            match batches(pending(&Buffer::at(store.data_dir()), &allowed))
                .into_iter()
                .next()
            {
                Some(next) => next.buckets,
                None => local_buckets(store, instance, &sharing, std::slice::from_ref(&hour), now)?,
            }
        }
        Sharing::Off { .. } => Vec::new(),
    };
    let request = body(buckets);
    if !request.buckets.is_empty() {
        request
            .validate()
            .map_err(|e| HiveError::Local(format!("engine metrics withheld: {e}")))?;
    }
    Ok((sharing, hour, request))
}

/// What `hive metrics preview` shows for the last complete hour whatever
/// the sharing state (for people: "this is what would go when on").
pub fn sample_hour(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> Result<HoursBodyV2, HiveError> {
    let sharing = gate::decide(store, config, instance, lease, now);
    let hour = HourStamp::of(now - Duration::hours(1));
    Ok(body(local_buckets(
        store,
        instance,
        &sharing,
        &[hour],
        now,
    )?))
}

/// `hive metrics hours --range 24h|7d|30d`: this engine's own buckets for
/// the span (the current hour included, at most 720), from the same
/// collector that uploads, sent or not: the local side of the K24 parity
/// check. Local only.
pub fn hours(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
    lease: &HiveLeaseV1,
    range_hours: i64,
    now: DateTime<Utc>,
) -> Result<HoursBodyV2, HiveError> {
    let sharing = gate::decide(store, config, instance, lease, now);
    let count = range_hours.clamp(1, 720);
    let list: Vec<HourStamp> = (0..count)
        .rev()
        .map(|back| HourStamp::of(now - Duration::hours(back)))
        .collect();
    Ok(body(local_buckets(store, instance, &sharing, &list, now)?))
}

/// `hive metrics delete`: everything this engine holds for the workspace
/// (buffer, state, fingerprint key and hits) is deleted locally.
pub fn delete_local(store: &HiveStore) {
    buffer::forget(store.data_dir());
    // Deleted hours are not rebuilt from the local stores (NEW-1).
    buffer::note_local_off(store.data_dir());
}

/// `hive metrics delete`, Hive side (C0 2.1 `DELETE /v1/metrics/hours`):
/// every bucket this engine stored. Only once released.
pub fn delete_remote(session: &Session) -> Result<bool, HiveError> {
    if !released(&session.instance.url) {
        return Ok(false);
    }
    session.with_client(|client| client.delete(HOURS_ROUTE))?;
    Ok(true)
}
