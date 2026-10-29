// SPDX-License-Identifier: Apache-2.0
//! How Hive helped in a project (X4, C0 1.9 `MetricsHiveV1`), per Hive
//! project and UTC day: counts and durations only, the same for sealed
//! projects. `<data>/hive/metrics/hive-use.json`, 0600, the last
//! `KEEP_DAYS` days.
//!
//! - M1 `knowledge_hits_referenced`: Hive facts an agent asked for and got
//!   (a recall that delivered facts pulled from a Hive vault).
//! - M2 `task_usage`: what the project's host sessions recorded (tokens,
//!   list price) while this machine held a task's claim, from the claim to
//!   its release, result or loss, per UTC minute; a minute in which several
//!   claims were held is shared among them, so no usage counts twice.
//! - M5 `first_context_ms`: per session, the time from its start to the
//!   first Hive knowledge it delivered.
//!
//! Recording never fails a caller: a write that does not happen is a count
//! that is missing, never an error in a tool.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::core::hive::state::{HiveStore, write_private};
use lean_ctx_protocol::hive::telemetry::{MAX_METRIC_TASK_ROWS, MetricsHiveV1, MetricsTaskUsageV1};

const FILE: &str = "hive-use.json";
/// Serializes the read-modify-write of [`FILE`] across processes.
const LOCK_FILE: &str = "hive-use.lock";
/// Days kept; an upload never reaches further back.
const KEEP_DAYS: i64 = 62;
/// First-context samples kept per project and day.
const MAX_SAMPLES: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DayUse {
    #[serde(default)]
    hits: u64,
    #[serde(default)]
    first_context_ms: Vec<u64>,
}

/// One claim this machine held: `[from, to]`, open while held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ClaimWindow {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    vault_id: String,
    number: u64,
    from: DateTime<Utc>,
    #[serde(default)]
    to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HiveUse {
    /// day → project id → use.
    #[serde(default)]
    days: BTreeMap<String, BTreeMap<String, DayUse>>,
    #[serde(default)]
    claims: Vec<ClaimWindow>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("metrics")
}

fn load(data_dir: &Path) -> HiveUse {
    std::fs::read(dir(data_dir).join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn change(data_dir: &Path, now: DateTime<Utc>, edit: impl FnOnce(&mut HiveUse)) {
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Every MCP server and CLI of this machine records here: without the
    // file lock two of them would drop each other's counts.
    if std::fs::create_dir_all(dir(data_dir)).is_err() {
        return;
    }
    let Ok(_file_lock) = crate::core::agents::FileLock::acquire(&dir(data_dir).join(LOCK_FILE))
    else {
        return;
    };
    let mut state = load(data_dir);
    edit(&mut state);
    let oldest = (now - chrono::Duration::days(KEEP_DAYS))
        .date_naive()
        .to_string();
    state.days.retain(|day, _| *day >= oldest);
    let cutoff = now - chrono::Duration::days(KEEP_DAYS);
    state
        .claims
        .retain(|claim| claim.to.is_none_or(|to| to >= cutoff));
    if let Ok(bytes) = serde_json::to_vec(&state) {
        let _ = write_private(&dir(data_dir), FILE, &bytes);
    }
}

fn day_of(at: DateTime<Utc>) -> String {
    at.date_naive().to_string()
}

/// The Hive project of a local root, when it is linked.
fn project_of(store: &HiveStore, project_root: &str) -> Option<String> {
    crate::core::hive::vault::VaultMap::load(store.data_dir())
        .for_root(project_root)
        .map(|p| p.vault_id.clone())
}

/// Whether a fact came from a Hive vault (a pulled revision).
#[must_use]
pub fn is_hive_fact(fact: &crate::core::knowledge::KnowledgeFact) -> bool {
    fact.imported_from
        .as_deref()
        .and_then(crate::core::hive::knowledge::learn::import_tag_source)
        .is_some()
}

/// Sessions whose first Hive context is recorded (once per session).
fn first_seen() -> &'static Mutex<HashSet<String>> {
    static SEEN: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// A delivery to an agent in `project_root` handed it `hive_facts` facts
/// from the Hive repo. `asked`: the agent asked for knowledge (a recall),
/// which counts as a hit (M1); every delivery may be the session's first
/// Hive context (M5).
pub fn record_delivery(
    project_root: &str,
    session: Option<(&str, DateTime<Utc>)>,
    hive_facts: usize,
    asked: bool,
) {
    if hive_facts == 0 {
        return;
    }
    let Ok(store) = HiveStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    let Some(vault_id) = project_of(&store, project_root) else {
        return;
    };
    let now = Utc::now();
    let first = session.and_then(|(id, started)| {
        first_seen()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned())
            .then(|| u64::try_from((now - started).num_milliseconds()).unwrap_or(0))
    });
    if !asked && first.is_none() {
        return;
    }
    record_at(
        store.data_dir(),
        &vault_id,
        now,
        asked.then_some(hive_facts),
        first,
    );
}

fn record_at(
    data_dir: &Path,
    vault_id: &str,
    now: DateTime<Utc>,
    hits: Option<usize>,
    first_context_ms: Option<u64>,
) {
    change(data_dir, now, |state| {
        let day = state
            .days
            .entry(day_of(now))
            .or_default()
            .entry(vault_id.to_owned())
            .or_default();
        if let Some(hits) = hits {
            day.hits = day.hits.saturating_add(hits as u64);
        }
        if let Some(ms) = first_context_ms
            && day.first_context_ms.len() < MAX_SAMPLES
        {
            day.first_context_ms.push(ms);
        }
    });
}

/// This machine claimed task `number` of `vault_id` at `at` (M2).
pub fn claim_started(data_dir: &Path, vault_id: &str, number: u64, at: DateTime<Utc>) {
    change(data_dir, at, |state| {
        if !state
            .claims
            .iter()
            .any(|c| c.vault_id == vault_id && c.number == number && c.to.is_none())
        {
            state.claims.push(ClaimWindow {
                vault_id: vault_id.to_owned(),
                number,
                from: at,
                to: None,
            });
        }
    });
}

/// The claim on task `number` ended at `at` (released, submitted, lost).
pub fn claim_ended(data_dir: &Path, vault_id: &str, number: u64, at: DateTime<Utc>) {
    change(data_dir, at, |state| {
        for claim in state
            .claims
            .iter_mut()
            .filter(|c| c.vault_id == vault_id && c.number == number && c.to.is_none())
        {
            claim.to = Some(at);
        }
    });
}

fn median(values: &[u64]) -> Option<u64> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => {
            let pair = u128::from(sorted[n / 2 - 1]) + u128::from(sorted[n / 2]);
            u64::try_from(pair / 2).ok()
        }
    }
}

/// M2 rows per task number: the usage of `[start, end)` per minute,
/// shared among the claims held in it.
fn task_rows(
    claims: &[ClaimWindow],
    vault_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    usage: &WindowUsage<'_>,
) -> BTreeMap<u64, MetricsTaskUsageV1> {
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let mut tasks: BTreeMap<u64, MetricsTaskUsageV1> = BTreeMap::new();
    for (from, to, numbers) in segments(claims, vault_id, start, end, now) {
        // `usage` counts whole minutes, both ends included.
        for (model, input, output, write, read) in usage(from, to - chrono::Duration::minutes(1)) {
            let usd = pricing
                .quote(Some(model.as_str()))
                .cost
                .estimate_usd(input, output, write, read);
            let micros = (usd * 1_000_000.0).round().max(0.0) as u64;
            let n = numbers.len() as u64;
            for (i, number) in (0u64..).zip(&numbers) {
                let row = tasks.entry(*number).or_insert(MetricsTaskUsageV1 {
                    number: *number,
                    input_tokens: 0,
                    output_tokens: 0,
                    list_usd_micros: 0,
                });
                row.input_tokens = row.input_tokens.saturating_add(share(input + write, n, i));
                row.output_tokens = row.output_tokens.saturating_add(share(output, n, i));
                row.list_usd_micros = row.list_usd_micros.saturating_add(share(micros, n, i));
            }
        }
    }
    tasks
}

/// C9: what the project's host sessions recorded since this machine claimed
/// task `number` (its open claim), at list price: the engine's own measure
/// of a task's cost, so a budget never rests on what an agent reports.
/// `None` while no claim is open or nothing was recorded.
pub(crate) fn claim_usage(
    data_dir: &Path,
    vault_id: &str,
    number: u64,
    now: DateTime<Utc>,
    usage: &WindowUsage<'_>,
) -> Option<lean_ctx_protocol::hive::tasks::UsageV1> {
    let state = load(data_dir);
    let from = state
        .claims
        .iter()
        .filter(|c| c.vault_id == vault_id && c.number == number && c.to.is_none())
        .map(|c| c.from)
        .min()?;
    let end = now + chrono::Duration::minutes(1);
    let row = task_rows(&state.claims, vault_id, from, end, now, usage).remove(&number)?;
    let tokens = row.input_tokens.saturating_add(row.output_tokens);
    (tokens > 0).then_some(lean_ctx_protocol::hive::tasks::UsageV1 {
        tokens,
        usd_micros: Some(row.list_usd_micros),
    })
}

/// [`WindowUsage`] over the host session files under `roots` in `home`; a
/// session under a deeper root of `others` belongs to that project.
pub(crate) fn host_window<'a>(
    home: &'a Path,
    roots: &'a [String],
    others: &'a [String],
    now: DateTime<Utc>,
) -> impl Fn(DateTime<Utc>, DateTime<Utc>) -> Vec<(String, u64, u64, u64, u64)> + 'a {
    move |from, to| {
        crate::core::dashboard_pro::host_usage::usage_between(home, roots, others, now, from, to)
            .into_iter()
            .map(|(model, c)| {
                let n = |v: i64| u64::try_from(v).unwrap_or(0);
                (
                    model,
                    n(c.input),
                    n(c.output),
                    n(c.cache_write),
                    n(c.cache_read),
                )
            })
            .collect()
    }
}

/// The `i`-th of `n` shares of `total`; the shares add up to `total`.
fn share(total: u64, n: u64, i: u64) -> u64 {
    total / n + u64::from(i < total % n)
}

/// The claims of `vault_id` cut into minute segments of `[start, end)`:
/// `(from, to, claims held)` with `to` exclusive and at least one claim.
/// A claim covers the minutes from that of its start through that of its
/// end (`now` while held); two claims that meet in a minute share it.
fn segments(
    claims: &[ClaimWindow],
    vault_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, DateTime<Utc>, Vec<u64>)> {
    use chrono::DurationRound;
    let minute = chrono::Duration::minutes(1);
    let floor = |t: DateTime<Utc>| t.duration_trunc(minute).unwrap_or(t);
    let windows: Vec<(u64, DateTime<Utc>, DateTime<Utc>)> = claims
        .iter()
        .filter(|c| c.vault_id == vault_id)
        .map(|c| {
            (
                c.number,
                floor(c.from).max(start),
                (floor(c.to.unwrap_or(now)) + minute).min(end),
            )
        })
        .filter(|(_, from, to)| from < to)
        .collect();
    let mut edges: Vec<DateTime<Utc>> = windows.iter().flat_map(|w| [w.1, w.2]).collect();
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            let mut held: Vec<u64> = windows
                .iter()
                .filter(|(_, a, b)| *a <= from && to <= *b)
                .map(|(number, _, _)| *number)
                .collect();
            held.sort_unstable();
            held.dedup();
            (!held.is_empty()).then_some((from, to, held))
        })
        .collect()
}

/// Usage per model in a window: `(model, input, output, cache_write, cache_read)`.
pub(crate) type WindowUsage<'a> =
    dyn Fn(DateTime<Utc>, DateTime<Utc>) -> Vec<(String, u64, u64, u64, u64)> + 'a;

/// The day's cumulative Hive use of `vault_id` (engine metrics v2 samples
/// it hourly): facts used and the first-context samples so far.
pub(crate) fn day_counters(hive_data: &Path, vault_id: &str, date: &str) -> (u64, Vec<u64>) {
    load(hive_data)
        .days
        .get(date)
        .and_then(|projects| projects.get(vault_id))
        .map(|day| (day.hits, day.first_context_ms.clone()))
        .unwrap_or_default()
}

/// Ledger usage per held task of `vault_id` in `[start, end)` (engine
/// metrics v2, one hour), sorted by number, tasks with usage only.
pub(crate) fn window_tasks(
    hive_data: &Path,
    vault_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    usage: &WindowUsage<'_>,
) -> Vec<MetricsTaskUsageV1> {
    let state = load(hive_data);
    task_rows(&state.claims, vault_id, start, end, now, usage)
        .into_values()
        .filter(|row| row.input_tokens + row.output_tokens > 0)
        .collect()
}

/// The `hive` entry of `vault_id` on `date`, or `None` when Hive did
/// nothing measurable there. `usage` reads the project's host sessions in
/// a time window; `now` closes a claim still held.
pub(crate) fn day_entry(
    hive_data: &Path,
    vault_id: &str,
    date: &str,
    now: DateTime<Utc>,
    usage: &WindowUsage<'_>,
) -> Option<MetricsHiveV1> {
    let state = load(hive_data);
    let day = state
        .days
        .get(date)
        .and_then(|projects| projects.get(vault_id))
        .cloned()
        .unwrap_or_default();
    let date_naive = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let start = date_naive.and_hms_opt(0, 0, 0)?.and_utc();
    let end = start + chrono::Duration::days(1);
    let tasks = task_rows(&state.claims, vault_id, start, end, now, usage);
    let task_usage: Vec<MetricsTaskUsageV1> = tasks
        .into_values()
        .filter(|row| row.input_tokens + row.output_tokens > 0)
        .take(MAX_METRIC_TASK_ROWS)
        .collect();
    let first_context_sessions = day.first_context_ms.len() as u64;
    let entry = MetricsHiveV1 {
        knowledge_hits_referenced: day.hits,
        task_usage,
        first_context_ms_p50: median(&day.first_context_ms),
        first_context_sessions,
    };
    (entry.knowledge_hits_referenced > 0
        || !entry.task_usage.is_empty()
        || entry.first_context_sessions > 0)
        .then_some(entry)
}

#[cfg(test)]
#[path = "hive_use_tests.rs"]
mod tests;

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `ClaimWindow` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaClaimWindow {
        project_id: String,
        number: u64,
        from: DateTime<Utc>,
        #[serde(default)]
        to: Option<DateTime<Utc>>,
    }

    #[test]
    fn claim_windows_keep_pre_va_names() {
        let old = r#"{"project_id":"v1","number":7,"from":"2026-10-22T10:00:00Z"}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<
            ClaimWindow,
            PreVaClaimWindow,
        >(old, "project_id", "vault_id");
        assert_eq!((read.vault_id.as_str(), read.number), ("v1", 7));
    }
}
