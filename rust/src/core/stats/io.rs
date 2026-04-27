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
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => StatsStore::default(),
    }
}

pub(super) fn write_to_disk(store: &StatsStore) {
    let Some(dir) = stats_dir() else { return };

    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }

    let path = dir.join("stats.json");
    if let Ok(json) = serde_json::to_string(store) {
        let tmp = dir.join(".stats.json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

pub(super) fn merge_and_save(current: &StatsStore, baseline: &StatsStore) -> StatsStore {
    let Some(dir) = stats_dir() else {
        let disk = load_from_disk();
        return apply_deltas(&disk, current, baseline);
    };

    let lock_path = dir.join(".stats.lock");
    let _lock = acquire_file_lock(&lock_path);

    let disk = load_from_disk();
    let merged = apply_deltas(&disk, current, baseline);
    write_to_disk(&merged);
    merged
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
        if let Ok(meta) = std::fs::metadata(lock_path) {
            if let Ok(modified) = meta.modified() {
                if modified.elapsed().unwrap_or_default().as_secs() > 5 {
                    let _ = std::fs::remove_file(lock_path);
                    continue;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

pub(super) fn apply_deltas(
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

    merged.total_commands += delta_commands;
    merged.total_input_tokens += delta_input;
    merged.total_output_tokens += delta_output;

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

    merge_daily(&mut merged.daily, &current.daily, &baseline.daily);

    if let Some(ref ts) = current.last_use {
        match merged.last_use {
            Some(ref existing) if existing >= ts => {}
            _ => merged.last_use = Some(ts.clone()),
        }
    }
    if merged.first_use.is_none() {
        merged.first_use.clone_from(&current.first_use);
    } else if let Some(ref cur_first) = current.first_use {
        if let Some(ref merged_first) = merged.first_use {
            if cur_first < merged_first {
                merged.first_use = Some(cur_first.clone());
            }
        }
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
        if dc == 0 && di == 0 && do_ == 0 {
            continue;
        }
        if let Some(existing) = merged.iter_mut().find(|d| d.date == day.date) {
            existing.commands += dc;
            existing.input_tokens += di;
            existing.output_tokens += do_;
        } else {
            merged.push(DayStats {
                date: day.date.clone(),
                commands: dc,
                input_tokens: di,
                output_tokens: do_,
            });
        }
    }

    if merged.len() > 90 {
        merged.sort_by(|a, b| a.date.cmp(&b.date));
        merged.drain(..merged.len() - 90);
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
    }
}
