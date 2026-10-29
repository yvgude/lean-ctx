use std::collections::HashMap;
use std::path::PathBuf;

use super::model::{CepStats, DayStats, StatsStore};

fn stats_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir().ok()
}

fn stats_path() -> Option<PathBuf> {
    stats_dir().map(|d| d.join("stats.json"))
}

pub(super) fn load_from_disk() -> StatsStore {
    let Some(path) = stats_path() else {
        return StatsStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => sanitize_extrapolated_rereads(store),
            // Corrupt display cache (#706): NEVER silently reset — the next
            // flush would overwrite the good file with an empty store and the
            // whole history would be gone. Quarantine the corrupt bytes so
            // they stay recoverable, warn loudly, and start fresh.
            Err(e) => {
                quarantine_corrupt(&path, &e);
                StatsStore::default()
            }
        },
        Err(_) => StatsStore::default(),
    }
}

/// One-time sanitation (#1283). Stores written before the extrapolation fix
/// can carry `reread_tokens_saved` values orders of magnitude larger than
/// every input token ever processed (measured: ~119 trillion vs 2.4B input).
/// Those values are products of turn arithmetic, not measurements, and cannot
/// be decomposed retroactively — reset with a loud log.
///
/// Cap = 1x total input (was 100x): during the mixed-version window a still-
/// running pre-fix server keeps re-inflating the store between sanitations,
/// sawtoothing anywhere below the cap — measured live at 59.8B (~24x input),
/// which rendered as ~$18k "cost saved" on the dashboard. Measured cache-hit
/// savings sit around 8% of daily input, so 1x is still generous; kept as a
/// floor of 1M tokens for young stores.
fn sanitize_extrapolated_rereads(mut store: StatsStore) -> StatsStore {
    let plausible_cap = store.total_input_tokens.max(1_000_000);
    if store.reread_tokens_saved > plausible_cap {
        tracing::warn!(
            "reread_tokens_saved={} exceeds plausibility cap {} (total input {}) — \
             value stems from the pre-#1283 turn extrapolation and is reset; \
             future re-read savings are measured from real cache hits",
            store.reread_tokens_saved,
            plausible_cap,
            store.total_input_tokens
        );
        store.reread_tokens_saved = 0;
    }
    store
}

/// Moves an unparseable `stats.json` aside to `stats.json.corrupt` (#706)
/// instead of letting the next flush overwrite it with an empty store. An
/// existing quarantine file is never overwritten — the OLDER corrupt copy is
/// the one closest to the lost history, so it wins. Pure display-cache
/// hygiene: the savings ledger (`savings/ledger.jsonl`) remains the
/// append-only source of truth either way.
fn quarantine_corrupt(path: &std::path::Path, err: &serde_json::Error) {
    let quarantine = path.with_extension("json.corrupt");
    let preserved = if quarantine.exists() {
        false
    } else {
        std::fs::rename(path, &quarantine).is_ok()
    };
    tracing::warn!(
        "stats.json is corrupt ({err}) — starting a fresh stats store. {}",
        if preserved {
            format!(
                "The corrupt file was preserved at {} for recovery; \
                 `lean-ctx doctor` will flag it",
                quarantine.display()
            )
        } else {
            format!(
                "A previous corrupt copy already exists at {} and was kept \
                 (older = closer to the lost history)",
                quarantine.display()
            )
        }
    );
}

/// Loads `stats.json` from a *specific* directory (no in-process buffer applied).
/// Used by [`crate::core::stats::load_for_display`] to fold sibling data dirs
/// into the displayed total when an XDG split (#408/#414/#500) spread savings
/// across more than one tree. Missing files degrade to an empty store; corrupt
/// files warn (#706) but are NOT quarantined here — this is a read-only peek
/// into a sibling tree that another lean-ctx instance owns.
pub(super) fn load_from_dir(dir: &std::path::Path) -> StatsStore {
    match std::fs::read_to_string(dir.join("stats.json")) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(
                    "sibling stats file {} is corrupt ({e}) — folding it in as empty",
                    dir.join("stats.json").display()
                );
                StatsStore::default()
            }
        },
        Err(_) => StatsStore::default(),
    }
}

fn write_to_disk(store: &StatsStore) -> bool {
    let Some(dir) = stats_dir() else { return false };
    write_to_dir(&dir, store)
}

fn write_to_dir(dir: &std::path::Path, store: &StatsStore) -> bool {
    if !dir.exists() && std::fs::create_dir_all(dir).is_err() {
        return false;
    }

    let path = dir.join("stats.json");
    let tmp = dir.join(".stats.json.tmp");
    serde_json::to_string(store).is_ok_and(|json| {
        std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_ok()
    })
}

/// The folder of one project's own `stats.json` (`<data>/project-stats/<key>`).
/// `None` for a key that is not a ledger project key (16 hex digits).
pub(super) fn project_dir(key: &str) -> Option<PathBuf> {
    (key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| stats_dir().map(|d| d.join("project-stats").join(key)))
        .flatten()
}

/// Applies one accounting event to a project's own store, under its lock.
/// The device store (`stats.json`) keeps counting everything; this copy is
/// the project's share of it.
pub(super) fn update_project(key: &str, apply: impl FnOnce(&mut StatsStore)) -> bool {
    let Some(dir) = project_dir(key) else {
        return false;
    };
    if !dir.exists() && std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let Some(_lock) = acquire_file_lock(&dir.join(".stats.lock")) else {
        return false;
    };
    let mut store = load_from_dir(&dir);
    apply(&mut store);
    write_to_dir(&dir, &store)
}

/// The stores of the given project keys that exist.
pub(super) fn load_projects<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<StatsStore> {
    keys.filter_map(project_dir)
        .filter(|dir| dir.join("stats.json").exists())
        .map(|dir| load_from_dir(&dir))
        .collect()
}

/// Every project key with its own store.
pub(super) fn project_keys() -> Vec<String> {
    let Some(root) = stats_dir().map(|d| d.join("project-stats")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|key| project_dir(key).is_some())
        .collect();
    keys.sort();
    keys
}

pub(super) fn locked_write(store: &StatsStore) {
    let Some(dir) = stats_dir() else { return };
    let lock_path = dir.join(".stats.lock");
    let _lock = acquire_file_lock(&lock_path);
    if _lock.is_none() {
        return;
    }
    let _ = write_to_disk(store);
}

pub(super) fn merge_and_save(current: &StatsStore, baseline: &StatsStore) -> Option<StatsStore> {
    let dir = stats_dir()?;
    let lock_path = dir.join(".stats.lock");
    let _lock = acquire_file_lock(&lock_path)?;

    let disk = load_from_disk();
    let merged = apply_deltas(&disk, current, baseline);
    write_to_disk(&merged).then_some(merged)
}

struct FileLockGuard(PathBuf);

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn acquire_file_lock(lock_path: &std::path::Path) -> Option<FileLockGuard> {
    for _ in 0..20 {
        if std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(lock_path)
            .is_ok()
        {
            return Some(FileLockGuard(lock_path.to_path_buf()));
        }
        if let Ok(meta) = std::fs::metadata(lock_path)
            && let Ok(modified) = meta.modified()
            && modified.elapsed().unwrap_or_default().as_secs() > 5
        {
            let _ = std::fs::remove_file(lock_path);
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

pub(crate) fn apply_deltas(
    disk: &StatsStore,
    current: &StatsStore,
    baseline: &StatsStore,
) -> StatsStore {
    let mut merged = disk.clone();

    let delta_commands = current
        .total_commands
        .saturating_sub(baseline.total_commands);
    let delta_input = current
        .total_input_tokens
        .saturating_sub(baseline.total_input_tokens);
    let delta_output = current
        .total_output_tokens
        .saturating_sub(baseline.total_output_tokens);

    merged.total_commands = merged.total_commands.saturating_add(delta_commands);
    merged.total_input_tokens = merged.total_input_tokens.saturating_add(delta_input);
    merged.total_output_tokens = merged.total_output_tokens.saturating_add(delta_output);
    merged.first_inject_tokens_saved = merged.first_inject_tokens_saved.saturating_add(
        current
            .first_inject_tokens_saved
            .saturating_sub(baseline.first_inject_tokens_saved),
    );
    merged.reread_tokens_saved = merged.reread_tokens_saved.saturating_add(
        current
            .reread_tokens_saved
            .saturating_sub(baseline.reread_tokens_saved),
    );
    merged.active_tool_result_tokens_saved = merged.active_tool_result_tokens_saved.saturating_add(
        current
            .active_tool_result_tokens_saved
            .saturating_sub(baseline.active_tool_result_tokens_saved),
    );
    merged.last_tool_result_turn = merged
        .last_tool_result_turn
        .max(current.last_tool_result_turn);
    merged.stream_tracked_results = merged.stream_tracked_results.saturating_add(
        current
            .stream_tracked_results
            .saturating_sub(baseline.stream_tracked_results),
    );

    for (cmd, stats) in &current.commands {
        let base = baseline.commands.get(cmd);
        let dc = stats.count.saturating_sub(base.map_or(0, |b| b.count));
        let di = stats
            .input_tokens
            .saturating_sub(base.map_or(0, |b| b.input_tokens));
        let do_ = stats
            .output_tokens
            .saturating_sub(base.map_or(0, |b| b.output_tokens));
        if dc > 0 || di > 0 || do_ > 0 {
            let entry = merged.commands.entry(cmd.clone()).or_default();
            entry.count += dc;
            entry.input_tokens += di;
            entry.output_tokens += do_;
        }
    }

    // Tags are metadata rather than counters: carry the newest classification
    // through disk merges and sibling-directory display aggregation.
    for (cmd, class) in &current.command_classes {
        merged.command_classes.insert(cmd.clone(), *class);
    }

    merge_daily(&mut merged.daily, &current.daily, &baseline.daily);

    if let Some(ref ts) = current.last_use {
        match merged.last_use {
            Some(ref existing) if existing >= ts => {}
            _ => merged.last_use = Some(ts.clone()),
        }
    }
    if merged.first_use.is_none() {
        merged.first_use.clone_from(&current.first_use);
    } else if let Some(ref cur_first) = current.first_use
        && let Some(ref merged_first) = merged.first_use
        && cur_first < merged_first
    {
        merged.first_use = Some(cur_first.clone());
    }

    merge_cep(&mut merged.cep, &current.cep, &baseline.cep);

    merged
}

pub(super) fn merge_daily(merged: &mut Vec<DayStats>, current: &[DayStats], baseline: &[DayStats]) {
    let base_map: HashMap<String, &DayStats> =
        baseline.iter().map(|d| (d.date.clone(), d)).collect();

    for day in current {
        let base = base_map.get(&day.date);
        let dc = day.commands.saturating_sub(base.map_or(0, |b| b.commands));
        let di = day
            .input_tokens
            .saturating_sub(base.map_or(0, |b| b.input_tokens));
        let do_ = day
            .output_tokens
            .saturating_sub(base.map_or(0, |b| b.output_tokens));
        let dci = day
            .compressible_input_tokens
            .saturating_sub(base.map_or(0, |b| b.compressible_input_tokens));
        let dco = day
            .compressible_output_tokens
            .saturating_sub(base.map_or(0, |b| b.compressible_output_tokens));
        if dc == 0 && di == 0 && do_ == 0 {
            continue;
        }
        if let Some(existing) = merged.iter_mut().find(|d| d.date == day.date) {
            existing.commands += dc;
            existing.input_tokens += di;
            existing.output_tokens += do_;
            existing.compressible_input_tokens += dci;
            existing.compressible_output_tokens += dco;
            if !day.version.is_empty() {
                existing.version.clone_from(&day.version);
            }
        } else {
            merged.push(DayStats {
                date: day.date.clone(),
                commands: dc,
                input_tokens: di,
                output_tokens: do_,
                compressible_input_tokens: dci,
                compressible_output_tokens: dco,
                version: day.version.clone(),
            });
        }
    }

    if merged.len() > super::MAX_DAILY_HISTORY_DAYS {
        merged.sort_by_key(|item| item.date.clone());
        merged.drain(..merged.len() - super::MAX_DAILY_HISTORY_DAYS);
    }
}

fn merge_cep(merged: &mut CepStats, current: &CepStats, baseline: &CepStats) {
    merged.sessions += current.sessions.saturating_sub(baseline.sessions);
    merged.total_cache_hits += current
        .total_cache_hits
        .saturating_sub(baseline.total_cache_hits);
    merged.total_cache_reads += current
        .total_cache_reads
        .saturating_sub(baseline.total_cache_reads);
    merged.total_tokens_original += current
        .total_tokens_original
        .saturating_sub(baseline.total_tokens_original);
    merged.total_tokens_compressed += current
        .total_tokens_compressed
        .saturating_sub(baseline.total_tokens_compressed);

    for (mode, count) in &current.modes {
        let base_count = baseline.modes.get(mode).copied().unwrap_or(0);
        let delta = count.saturating_sub(base_count);
        if delta > 0 {
            *merged.modes.entry(mode.clone()).or_insert(0) += delta;
        }
    }

    let base_scores_len = baseline.scores.len();
    if current.scores.len() > base_scores_len {
        for snapshot in &current.scores[base_scores_len..] {
            merged.scores.push(snapshot.clone());
        }
    }
    if merged.scores.len() > 100 {
        merged.scores.drain(..merged.scores.len() - 100);
    }

    if current.last_session_pid.is_some() {
        merged.last_session_pid = current.last_session_pid;
        merged.last_session_original = current.last_session_original;
        merged.last_session_compressed = current.last_session_compressed;
        merged.last_session_cache_hits = current.last_session_cache_hits;
        merged.last_session_cache_reads = current.last_session_cache_reads;
    }
}

#[cfg(test)]
mod sanitation_tests {
    use super::*;

    /// #1283 mixed-version window: a still-running pre-fix server sawtooths
    /// the persisted value anywhere above real measurements (live: 59.8B at
    /// 2.5B total input rendered as ~$18k "cost saved"). The cap is 1x total
    /// input; plausible measured values pass through untouched.
    #[test]
    fn sanitation_resets_above_total_input_and_keeps_plausible_values() {
        let inflated = StatsStore {
            total_input_tokens: 2_500_000_000,
            reread_tokens_saved: 59_800_000_000, // 24x input — sawtooth
            ..Default::default()
        };
        assert_eq!(
            sanitize_extrapolated_rereads(inflated).reread_tokens_saved,
            0
        );

        let plausible = StatsStore {
            total_input_tokens: 2_500_000_000,
            reread_tokens_saved: 200_000_000, // 8% of input — measured scale
            ..Default::default()
        };
        assert_eq!(
            sanitize_extrapolated_rereads(plausible).reread_tokens_saved,
            200_000_000
        );

        // Young stores keep the 1M floor so tiny installs are never reset.
        let young = StatsStore {
            total_input_tokens: 10_000,
            reread_tokens_saved: 900_000,
            ..Default::default()
        };
        assert_eq!(
            sanitize_extrapolated_rereads(young).reread_tokens_saved,
            900_000
        );
    }
}
