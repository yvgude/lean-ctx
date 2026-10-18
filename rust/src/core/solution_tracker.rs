use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::Local;
use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "solution_tracker.json";

#[derive(Default, Deserialize, Serialize)]
struct SolutionStore {
    decisions_total: u64,
    decisions_stdlib: u64,
    decisions_native: u64,
    decisions_reuse: u64,
    decisions_yagni: u64,
    decisions_oneline: u64,
    decisions_debt: u64,
    loc_added: u64,
    loc_removed: u64,
    loc_net_saved: i64,
    output_tokens_baseline: u64,
    output_tokens_actual: u64,
    #[serde(default)]
    daily_records: Vec<DayRecord>,
}

#[derive(Clone, Deserialize, Serialize)]
struct DayRecord {
    date: String,
    decisions: u64,
    loc_net_saved: i64,
}

#[derive(Clone, Serialize)]
pub struct SolutionSnapshot {
    pub decisions_total: u64,
    pub decisions_by_kind: HashMap<String, u64>,
    pub loc_added: u64,
    pub loc_removed: u64,
    pub loc_net_saved: i64,
    pub output_tokens_baseline: u64,
    pub output_tokens_actual: u64,
    pub output_reduction_pct: u8,
}

fn store_path() -> PathBuf {
    crate::core::paths::data_dir()
        .unwrap_or_else(|_| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".lean-ctx")
        })
        .join(STORE_FILE)
}

static STORE: OnceLock<Mutex<SolutionStore>> = OnceLock::new();

fn store() -> &'static Mutex<SolutionStore> {
    STORE.get_or_init(|| {
        let loaded = std::fs::read(store_path())
            .ok()
            .and_then(|contents| serde_json::from_slice(&contents).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// Reload the store from disk. Called by `snapshot()` so the dashboard
/// daemon sees changes written by short-lived `hook observe` processes.
fn reload_from_disk() {
    if let Some(fresh) = read_store_file() {
        if let Ok(mut guard) = store().lock() {
            *guard = fresh;
        }
    }
}

fn read_store_file() -> Option<SolutionStore> {
    let bytes = std::fs::read(store_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Exclusive lock on a stable companion file; released when the handle drops.
/// Writes still happen without it if the lock cannot be taken, as before.
fn lock_store_file() -> Option<std::fs::File> {
    use fs2::FileExt;

    let lock_path = store_path().with_extension("json.lock");
    std::fs::create_dir_all(lock_path.parent()?).ok()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .ok()?;
    lock.lock_exclusive().ok()?;
    Some(lock)
}

fn flush(store: &SolutionStore) {
    let path = store_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let Ok(contents) = serde_json::to_vec(store) else {
        return;
    };
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&temporary, contents).is_ok() {
        let _ = std::fs::rename(temporary, path);
    }
}

/// Several long-lived MCP servers record into the same file. Each mutation is
/// a locked read-modify-write so no process writes back a stale copy and
/// drops counts the others recorded since it started.
fn with_store<F: FnOnce(&mut SolutionStore)>(f: F) {
    let mut store = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _file_lock = lock_store_file();
    if let Some(fresh) = read_store_file() {
        *store = fresh;
    }
    f(&mut store);
    flush(&store);
}

fn current_day_record(store: &mut SolutionStore) -> &mut DayRecord {
    let date = Local::now().format("%F").to_string();
    if let Some(index) = store
        .daily_records
        .iter()
        .position(|record| record.date == date)
    {
        return &mut store.daily_records[index];
    }

    store.daily_records.push(DayRecord {
        date,
        decisions: 0,
        loc_net_saved: 0,
    });
    store
        .daily_records
        .last_mut()
        .expect("daily record was just inserted")
}

pub fn record_decision(kind: &str) {
    with_store(|store| {
        store.decisions_total = store.decisions_total.saturating_add(1);
        let record = current_day_record(store);
        record.decisions = record.decisions.saturating_add(1);
        match kind {
            "stdlib" => store.decisions_stdlib = store.decisions_stdlib.saturating_add(1),
            "native" => store.decisions_native = store.decisions_native.saturating_add(1),
            "reuse" => store.decisions_reuse = store.decisions_reuse.saturating_add(1),
            "yagni" => store.decisions_yagni = store.decisions_yagni.saturating_add(1),
            "oneline" => store.decisions_oneline = store.decisions_oneline.saturating_add(1),
            "debt" => store.decisions_debt = store.decisions_debt.saturating_add(1),
            _ => {}
        }
    });
}

pub fn record_loc_change(added: u64, removed: u64) {
    with_store(|store| {
        store.loc_added = store.loc_added.saturating_add(added);
        store.loc_removed = store.loc_removed.saturating_add(removed);
        let added = i64::try_from(added).unwrap_or(i64::MAX);
        let removed = i64::try_from(removed).unwrap_or(i64::MAX);
        let loc_net_saved = removed.saturating_sub(added);
        store.loc_net_saved = store.loc_net_saved.saturating_add(loc_net_saved);
        let record = current_day_record(store);
        record.loc_net_saved = record.loc_net_saved.saturating_add(loc_net_saved);
    });
}

pub fn trend_7d() -> Vec<(String, u64, i64)> {
    reload_from_disk();
    let store = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut records = store.daily_records.clone();
    records.sort_unstable_by(|left, right| left.date.cmp(&right.date));
    let mut trend: Vec<_> = records
        .into_iter()
        .rev()
        .take(7)
        .map(|record| (record.date, record.decisions, record.loc_net_saved))
        .collect();
    trend.reverse();
    trend
}

pub fn record_output_tokens(baseline: u64, actual: u64) {
    with_store(|store| {
        store.output_tokens_baseline = store.output_tokens_baseline.saturating_add(baseline);
        store.output_tokens_actual = store.output_tokens_actual.saturating_add(actual);
    });
}

pub fn snapshot() -> SolutionSnapshot {
    reload_from_disk();
    let store = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut decisions_by_kind = HashMap::new();
    if store.decisions_stdlib > 0 {
        decisions_by_kind.insert("stdlib".to_owned(), store.decisions_stdlib);
    }
    if store.decisions_native > 0 {
        decisions_by_kind.insert("native".to_owned(), store.decisions_native);
    }
    if store.decisions_reuse > 0 {
        decisions_by_kind.insert("reuse".to_owned(), store.decisions_reuse);
    }
    if store.decisions_yagni > 0 {
        decisions_by_kind.insert("yagni".to_owned(), store.decisions_yagni);
    }
    if store.decisions_oneline > 0 {
        decisions_by_kind.insert("oneline".to_owned(), store.decisions_oneline);
    }
    if store.decisions_debt > 0 {
        decisions_by_kind.insert("debt".to_owned(), store.decisions_debt);
    }
    let output_reduction_pct = store
        .output_tokens_baseline
        .checked_sub(store.output_tokens_actual)
        .and_then(|reduction| reduction.checked_mul(100))
        .and_then(|reduction| reduction.checked_div(store.output_tokens_baseline))
        .and_then(|reduction| u8::try_from(reduction).ok())
        .unwrap_or(0);

    SolutionSnapshot {
        decisions_total: store.decisions_total,
        decisions_by_kind,
        loc_added: store.loc_added,
        loc_removed: store.loc_removed,
        loc_net_saved: store.loc_net_saved,
        output_tokens_baseline: store.output_tokens_baseline,
        output_tokens_actual: store.output_tokens_actual,
        output_reduction_pct,
    }
}

pub fn reset() {
    with_store(|store| *store = SolutionStore::default());
}

pub fn gain_summary() -> String {
    let snapshot = snapshot();
    format!(
        "{} decisions; {} net LOC saved; {}% output-token reduction",
        snapshot.decisions_total, snapshot.loc_net_saved, snapshot.output_reduction_pct
    )
}
